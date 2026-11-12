//! The cache in `~/.fennec/cache`: one folder per build of Fennec, entries written whole, and
//! a cleanup that keeps it under a size cap.
//!
//! A build is named by its version and a hash of all of its source (`FENNEC_BUILD`), so no entry
//! one build wrote is ever read by another. A key that leaves out something the compiler does,
//! like the flags it hands LLVM, then can't bring back what an older compiler made: a changed
//! compiler starts from an empty cache. An old build's folder goes whole once it's gone unused
//! for a day.
//!
//! An entry is written to a temporary file of its own and renamed into place, so a reader never
//! sees half of one, and reading and writing take no locks. Each ends in a hash of its bytes, or,
//! for a program kept as the file that was launched, has the hash in a file beside it, and a read
//! that doesn't match is a miss: a torn or changed entry is never used. The hash guards against
//! accidents; against people, Fennec's folder is its user's alone, and a cache folder another
//! user owns or can write to isn't used at all (see `private.rs`). Reading an entry sets its modified time,
//! which is how cleanup knows what was used least recently; access times are unreliable.
//! Cleanup is the one thing that takes a lock, so two cleanups never run at once. The daemon runs
//! one every five minutes while it's idle, and any command starts one when none has run for an hour.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// This Fennec's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The folder this build's entries live in, `<version>-<hash of all its source>`.
pub const FOLDER: &str = concat!(env!("CARGO_PKG_VERSION"), "-", env!("FENNEC_BUILD"));

/// The cache's size cap unless `FENNEC_CACHE_CAP` sets one, in bytes.
pub const DEFAULT_CAP: u64 = 1 << 30;

/// An entry used this recently is already near the end of cleanup's order.
const MARKED_RECENTLY: Duration = Duration::from_secs(600);

/// An entry stored or used this recently is never removed, however far over the cap the cache
/// is: a command may be about to read it, or to start the program it is. It's twice the time an
/// entry goes unmarked, so one used since is always inside it.
pub const KEPT_RECENT: Duration = Duration::from_mins(20);

/// Another build's folder that nothing has used for this long goes whole at any cleanup, under the
/// cap or over it: only that build of Fennec could read it, and it's most likely gone.
pub const UNUSED_BUILD: Duration = Duration::from_hours(24);

/// How long a command lets go by without a cleanup before it starts one.
pub const CLEANUP_EVERY: Duration = Duration::from_hours(1);

/// A cleanup lock older than this was left by a cleanup that died, and is taken over.
const STALE_LOCK: Duration = Duration::from_secs(600);

/// The cache under one Fennec folder.
#[derive(Clone, Debug)]
pub struct Cache {
    root: PathBuf,
    /// Whether the folder can be trusted: its user's alone. One that isn't is never read from or
    /// written to.
    trusted: Result<(), crate::private::Refused>,
}

/// How many bytes of hash end each entry written as bytes.
const HASH_LEN: usize = 32;

/// The ending of the file beside a kept program that holds its hash.
const SUM: &str = "sum";

/// What a cleanup did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleanup {
    pub removed: usize,
    pub freed: u64,
    /// The cache's size afterwards.
    pub size: u64,
    /// Whether another cleanup was already running, so this one did nothing.
    pub skipped: bool,
}

/// One file in the cache.
struct Entry {
    path: PathBuf,
    size: u64,
    used: SystemTime,
    /// The build folder it's in.
    build: PathBuf,
    /// How many names the file has: more than one once a run has a program put out as it.
    links: u64,
}

impl Cache {
    pub fn new(home: &Path) -> Self {
        let root = home.join("cache");
        let trusted =
            crate::private::private_folder(home).and_then(|()| crate::private::private_folder(&root));
        Cache { root, trusted }
    }

    /// Whether the cache can be used, saying once a command why when it can't.
    fn usable(&self) -> bool {
        match &self.trusted {
            Ok(()) => true,
            Err(why) => {
                crate::unsaved::refused(&why.0);
                false
            }
        }
    }

    /// The size cap: `FENNEC_CACHE_CAP` in bytes, or 1 GiB.
    pub fn cap() -> u64 {
        std::env::var("FENNEC_CACHE_CAP").ok().and_then(|text| text.parse().ok()).unwrap_or(DEFAULT_CAP)
    }

    /// Where an entry of this build lives: `cache/<version>-<build>/<kind>/<name>`.
    pub fn path(&self, kind: &str, name: &str) -> PathBuf {
        self.root.join(FOLDER).join(kind).join(name)
    }

    /// Reads an entry, and marks it as just used. One whose bytes don't match its hash is a
    /// miss, said once a command: it was torn or changed since it was written.
    pub fn read(&self, kind: &str, name: &str) -> Option<Vec<u8>> {
        if !self.usable() {
            return None;
        }
        let path = self.path(kind, name);
        let mut bytes = std::fs::read(&path).ok()?;
        let whole = match beside(&path) {
            // A kept program is the whole file, with its hash beside it.
            Some(sum) => sum_of(&bytes) == sum,
            None => match bytes.len().checked_sub(HASH_LEN).and_then(|end| bytes.split_at_checked(end)) {
                Some((text, hash)) => {
                    let whole = hash == fennec_blake3::hash(text).as_bytes();
                    let end = text.len();
                    bytes.truncate(end);
                    whole
                }
                None => false,
            },
        };
        if !whole {
            crate::unsaved::doubted(&path);
            return None;
        }
        // Marking costs a write, so an entry marked in the last few minutes isn't marked again.
        let age = std::fs::metadata(&path).and_then(|metadata| metadata.modified()).ok();
        if age.and_then(|time| time.elapsed().ok()).is_none_or(|age| age > MARKED_RECENTLY) {
            mark_used(&path);
        }
        Some(bytes)
    }

    /// Writes an entry whole, ending in a hash of its bytes: to a temporary file of its own,
    /// synced, then renamed into place.
    pub fn write(&self, kind: &str, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        if !self.usable() {
            return Ok(());
        }
        let path = self.path(kind, name);
        let hash = fennec_blake3::hash(bytes);
        put(&path, &[bytes, hash.as_bytes()])
    }

    /// Keeps the program at `from` as an entry, as that very file rather than a copy of it:
    /// linked in, so the entry and `from` are one file. A program kept this way is the file that
    /// was launched, which macOS has checked already: it holds each new file's first launch for
    /// seconds while it does. Where a link can't be made, as across disks, it's copied.
    ///
    /// Nothing is ever written into a file once kept: it's made read-only, and a new program kept
    /// under the same name is a new file put in its place, so a link to the old one goes on
    /// holding the old program, never the new one. `from` has to be a file nothing else writes.
    ///
    /// Its hash goes in a file beside it, since the program itself is run and can't end in one.
    pub fn keep_file(&self, kind: &str, name: &str, from: &Path) -> std::io::Result<()> {
        if !self.usable() {
            return Ok(());
        }
        let path = self.path(kind, name);
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        // On the disk before it's an entry, so a power cut can't leave one with no bytes.
        synced(from)?;
        let sum = sum_of(&std::fs::read(from)?);
        let temporary = linked_beside(from, &path)?;
        read_only_program(&temporary)?;
        // The hash first: a reader between the two finds the old program with the new hash, a
        // mismatch, which is a miss.
        put(&sum_path(&path), &[sum.as_bytes()])?;
        // Windows won't rename over a read-only file, as an older program kept at this name is.
        #[cfg(windows)]
        if let Ok(found) = std::fs::metadata(&path) {
            let mut permissions = found.permissions();
            permissions.set_readonly(false);
            let _ = std::fs::set_permissions(&path, permissions);
        }
        std::fs::rename(&temporary, &path)
    }

    /// Puts the entry at `to` as that very file rather than a copy, marked as just used: linked
    /// out, where a link can be made. Gives back whether there was one, and one that doesn't match
    /// its hash wasn't. An entry written as bytes, which ends in its hash, is written out whole
    /// without it.
    pub fn link_out(&self, kind: &str, name: &str, to: &Path) -> std::io::Result<bool> {
        if !self.usable() {
            return Ok(false);
        }
        let path = self.path(kind, name);
        if !path.is_file() {
            return Ok(false);
        }
        let Some(sum) = beside(&path) else {
            let Some(bytes) = self.read(kind, name) else { return Ok(false) };
            let _ = std::fs::remove_file(to);
            put(to, &[&bytes])?;
            read_only_program(to)?;
            return Ok(true);
        };
        if sum_of(&std::fs::read(&path)?) != sum {
            crate::unsaved::doubted(&path);
            return Ok(false);
        }
        // Marked before it's put out: a linked file is the entry, with its time, and a sweep of
        // what earlier runs built goes by that time, so it has to be the time of this run.
        mark_used(&path);
        let _ = std::fs::remove_file(to);
        if std::fs::hard_link(&path, to).is_err() {
            std::fs::copy(&path, to)?;
            read_only_program(to)?;
        }
        Ok(true)
    }

    /// How many bytes the cache holds, across every version.
    pub fn size(&self) -> u64 {
        self.entries().iter().map(|entry| entry.size).sum()
    }

    fn entries(&self) -> Vec<Entry> {
        let mut found = Vec::new();
        let Ok(builds) = std::fs::read_dir(&self.root) else { return found };
        for build in builds.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
            files(&build, &build, &mut found);
        }
        found
    }

    /// Whether a command should start a cleanup: none has run for [`CLEANUP_EVERY`]. Saying yes
    /// marks it as started, so the commands that follow don't start one too. A cache with no mark
    /// yet only gets one, since it's new, or from before cleanups were marked.
    pub fn cleanup_due(&self) -> bool {
        let mark = self.root.join("cleaned");
        let since = std::fs::metadata(&mark).and_then(|metadata| metadata.modified());
        let due = match since {
            Ok(time) => time.elapsed().is_ok_and(|age| age > CLEANUP_EVERY),
            Err(_) => false,
        };
        if due || since.is_err() {
            // A mark that can't be written means asking again next time, which is harmless, but
            // it's said.
            let marked = std::fs::create_dir_all(&self.root)
                .and_then(|()| File::create(&mark))
                .and_then(|file| file.set_modified(SystemTime::now()));
            crate::kept("mark when it was last cleaned up", marked);
        }
        due
    }

    /// Keeps the whole cache, every build's folder together, under `cap` bytes. First, other
    /// builds' folders that have gone unused for [`UNUSED_BUILD`] go whole. Then, if it's still
    /// over, entries go until it's down to 80% of the cap, the least recently used first,
    /// whichever build they're from, since another build may be one that's in use elsewhere.
    pub fn cleanup(&self, cap: u64) -> std::io::Result<Cleanup> {
        let Some(_lock) = Lock::take(&self.root.join("cleanup.lock"))? else {
            return Ok(Cleanup { skipped: true, ..Cleanup::default() });
        };
        let entries = self.entries();
        let mut cleanup = Cleanup::default();
        let current = self.root.join(FOLDER);
        let mut last_used: HashMap<&Path, SystemTime> = HashMap::new();
        for entry in entries.iter().filter(|entry| entry.build != current) {
            let used = last_used.entry(&entry.build).or_insert(entry.used);
            *used = (*used).max(entry.used);
        }
        let unused: BTreeSet<PathBuf> = last_used
            .into_iter()
            .filter(|(_, used)| used.elapsed().is_ok_and(|age| age > UNUSED_BUILD))
            .map(|(build, _)| build.to_path_buf())
            .collect();
        for build in &unused {
            std::fs::remove_dir_all(build)?;
        }
        let (gone, mut kept): (Vec<Entry>, Vec<Entry>) =
            entries.into_iter().partition(|entry| unused.contains(&entry.build));
        cleanup.removed = gone.len();
        cleanup.freed = gone.iter().map(|entry| entry.size).sum();
        let mut size: u64 = kept.iter().map(|entry| entry.size).sum();
        if size > cap {
            let goal = cap / 5 * 4;
            kept.sort_by_key(|entry| entry.used);
            for entry in kept.iter().filter(|entry| !entry.held()) {
                if size <= goal {
                    break;
                }
                std::fs::remove_file(&entry.path)?;
                size -= entry.size;
                cleanup.removed += 1;
                cleanup.freed += entry.size;
            }
        }
        if cleanup.removed > 0 {
            self.remove_empty_versions();
        }
        cleanup.size = size;
        Ok(cleanup)
    }

    /// Removes the folders of other versions that cleanup emptied.
    /// This version's folders stay, even empty: a command may be about to write into one.
    fn remove_empty_versions(&self) {
        let Ok(versions) = std::fs::read_dir(&self.root) else { return };
        for version in versions.flatten().map(|entry| entry.path()) {
            if version.is_dir() && version.file_name().is_some_and(|name| name != FOLDER) {
                remove_empty_folders(&version);
            }
        }
    }
}

impl Entry {
    /// Whether the entry can't go yet: it was stored or used in the last [`KEPT_RECENT`], or a
    /// program it is has been put out where a run started it, as a second name for the same file.
    fn held(&self) -> bool {
        let recent = self.used.elapsed().map_or(true, |age| age < KEPT_RECENT);
        recent || self.links > 1
    }
}

/// The file beside a kept program that holds its hash: its whole name with `.sum` after it, so
/// `root` and `root.functions` never share one.
fn sum_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(SUM);
    PathBuf::from(name)
}

/// The hash in the file beside a kept program, if it has one.
fn beside(path: &Path) -> Option<String> {
    std::fs::read_to_string(sum_path(path)).ok().map(|sum| sum.trim().to_owned())
}

/// A kept program's hash, as its file beside it holds it.
fn sum_of(bytes: &[u8]) -> String {
    fennec_blake3::hash(bytes).to_hex()
}

/// A name beside `path` for a temporary file no other writer has: this process's id, the time,
/// and a count, so two threads, or two processes of one id in two containers, never share one.
fn temporary_beside(path: &Path) -> PathBuf {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_nanos());
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{nanos:x}-{count}.tmp", std::process::id()))
}

/// Writes `parts` to `path` whole: to a temporary file of its own, made new, which refuses a file
/// or a link already at that name, then synced, then renamed into place.
fn put(path: &Path, parts: &[&[u8]]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    for _ in 0..8 {
        let temporary = temporary_beside(path);
        let mut file = match File::options().write(true).create_new(true).open(&temporary) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let written = parts
            .iter()
            .try_for_each(|part| file.write_all(part))
            .and_then(|()| file.sync_all())
            .and_then(|()| std::fs::rename(&temporary, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        return written;
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "no temporary name was free"))
}

/// `from` linked in at a temporary name of its own beside `path`, or copied there where a link
/// can't be made, as across disks. A link fails rather than replace what's at the name.
fn linked_beside(from: &Path, path: &Path) -> std::io::Result<PathBuf> {
    for _ in 0..8 {
        let temporary = temporary_beside(path);
        match std::fs::hard_link(from, &temporary) {
            Ok(()) => return Ok(temporary),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => {
                let bytes = std::fs::read(from)?;
                put(&temporary, &[&bytes])?;
                return Ok(temporary);
            }
        }
    }
    Err(std::io::Error::new(ErrorKind::AlreadyExists, "no temporary name was free"))
}

/// Marks an entry as just used, for cleanup's order. A program kept read-only can't be opened to
/// write, but its owner may still set its time. Cleanup's order and the sweep of old runs depend
/// on this, so a failure is said; an entry another command's cleanup took meanwhile is none.
fn mark_used(path: &Path) {
    crate::touched("mark what it used as used", set_time(path, SystemTime::now()));
    // A kept program's hash goes with it, or cleanup would take the hash and leave the program.
    let sum = sum_path(path);
    if sum.is_file() {
        crate::touched("mark what it used as used", set_time(&sum, SystemTime::now()));
    }
}

/// Sets when a file or folder was last changed, as cleanup orders entries by. Windows sets a time
/// only through a handle that may write the file's attributes, which a read-only file still
/// allows, and opens a folder only with its backup semantics; elsewhere, opening it to read will
/// do, for its owner.
///
/// # Errors
///
/// When it isn't there, or its time can't be set.
pub fn set_time(path: &Path, time: SystemTime) -> std::io::Result<()> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS.
        options.read(false).access_mode(0x0100).custom_flags(0x0200_0000);
    }
    options.open(path)?.set_modified(time)
}

/// Puts a file's bytes on the disk. Windows flushes a file only through a handle that may write it.
fn synced(path: &Path) -> std::io::Result<()> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    options.write(true);
    options.open(path)?.sync_all()
}

/// Every file under `dir`, in the build folder `build`.
fn files(dir: &Path, build: &Path, found: &mut Vec<Entry>) {
    let Ok(listing) = std::fs::read_dir(dir) else { return };
    for item in listing.flatten() {
        let path = item.path();
        let Ok(metadata) = item.metadata() else { continue };
        if metadata.is_dir() {
            files(&path, build, found);
        } else {
            let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            #[cfg(unix)]
            let links = std::os::unix::fs::MetadataExt::nlink(&metadata);
            #[cfg(not(unix))]
            let links = 1;
            found.push(Entry { path, size: metadata.len(), used, build: build.to_path_buf(), links });
        }
    }
}

/// Removes the folders under `dir` that hold no files, then `dir` itself if it's empty. A
/// folder that still holds something can't be removed, so that's simply skipped.
fn remove_empty_folders(dir: &Path) {
    if let Ok(listing) = std::fs::read_dir(dir) {
        for path in listing.flatten().map(|item| item.path()).filter(|path| path.is_dir()) {
            remove_empty_folders(&path);
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// The cleanup lock: a file only one cleanup can create. Dropping it releases it.
struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Takes the lock, or None if another cleanup holds it. A stale lock is taken over.
    fn take(path: &Path) -> std::io::Result<Option<Lock>> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        for _ in 0..2 {
            match File::options().write(true).create_new(true).open(path) {
                Ok(_) => return Ok(Some(Lock { path: path.to_path_buf() })),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok();
                    if !age.and_then(|time| time.elapsed().ok()).is_some_and(is_stale) {
                        return Ok(None);
                    }
                    crate::touched(
                        "take over a cleanup lock left by one that died",
                        std::fs::remove_file(path),
                    );
                }
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }
}

/// Whether a cleanup lock this old was left by a cleanup that died.
fn is_stale(age: Duration) -> bool {
    age > STALE_LOCK
}

impl Drop for Lock {
    fn drop(&mut self) {
        crate::touched("let go of its cleanup lock", std::fs::remove_file(&self.path));
    }
}

/// Lets a program run and keeps anything from writing into it.
fn read_only_program(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_stale;
    use std::time::Duration;

    /// A file kept and then put out again is one file in all three places, where the system can
    /// link files: what was launched, the entry, and what the next run launches.
    #[test]
    #[cfg(unix)]
    fn a_file_kept_and_put_out_again_is_the_same_file() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-keep-file-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let launched = dir.join("launched");
        std::fs::write(&launched, b"a program").unwrap();
        cache.keep_file("programs", "root", &launched).unwrap();
        let next = dir.join("next");
        assert!(cache.link_out("programs", "root", &next).unwrap());
        let inode = |path: &std::path::Path| std::fs::metadata(path).unwrap().ino();
        assert_eq!(inode(&launched), inode(&cache.path("programs", "root")));
        assert_eq!(inode(&launched), inode(&next));
        assert_eq!(std::fs::read(&next).unwrap(), b"a program");
        // Put out again over what's there, it's still the one file.
        assert!(cache.link_out("programs", "root", &next).unwrap());
        assert_eq!(inode(&launched), inode(&next));
        assert!(!cache.link_out("programs", "gone", &dir.join("none")).unwrap());
        // One written as any other entry is put out runnable too.
        cache.write("programs", "written", b"another program").unwrap();
        let written = dir.join("written");
        assert!(cache.link_out("programs", "written", &written).unwrap());
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&written).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o555);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A program kept under a name another was kept under replaces it with a file of its own: a
    /// link to the old one still holds the old program, the next one put out holds the new, and
    /// neither can be written into.
    #[test]
    #[cfg(unix)]
    fn a_program_kept_again_never_changes_one_put_out_before() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-kept-again-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let (first, second) = (dir.join("first"), dir.join("second"));
        std::fs::write(&first, b"the old program").unwrap();
        cache.keep_file("programs", "root", &first).unwrap();
        let (old, new) = (dir.join("old"), dir.join("new"));
        assert!(cache.link_out("programs", "root", &old).unwrap());
        std::fs::write(&second, b"the new program").unwrap();
        cache.keep_file("programs", "root", &second).unwrap();
        assert!(cache.link_out("programs", "root", &new).unwrap());
        assert_eq!(std::fs::read(&old).unwrap(), b"the old program");
        assert_eq!(std::fs::read(&new).unwrap(), b"the new program");
        let metadata = |path: &std::path::Path| std::fs::metadata(path).unwrap();
        assert_ne!(metadata(&old).ino(), metadata(&new).ino());
        for path in [&old, &new, &cache.path("programs", "root")] {
            assert_eq!(metadata(path).permissions().mode() & 0o222, 0, "{} can be written", path.display());
            assert!(std::fs::OpenOptions::new().write(true).open(path).is_err());
        }
        // Read-only files go as any other.
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_lock_is_stale_after_ten_minutes() {
        assert!(!is_stale(Duration::from_secs(600)));
        assert!(is_stale(Duration::from_secs(601)));
    }
}
