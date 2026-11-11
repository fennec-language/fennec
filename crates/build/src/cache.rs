//! The cache in `~/.fennec/cache`: one folder per build of Fennec, entries written whole, and
//! a cleanup that keeps it under a size cap.
//!
//! A build is named by its version and a hash of all of its source (`FENNEC_BUILD`), so no entry
//! one build wrote is ever read by another. A key that leaves out something the compiler does,
//! like the flags it hands LLVM, then can't bring back what an older compiler made: a changed
//! compiler starts from an empty cache. An old build's folder goes whole once it's gone unused
//! for a day.
//!
//! An entry is written to a temporary file and renamed into place, so a reader never sees half
//! of one, and reading and writing take no locks. Reading an entry sets its modified time,
//! which is how cleanup knows what was used least recently; access times are unreliable.
//! Cleanup is the one thing that takes a lock, so two cleanups never run at once. The daemon runs
//! one every five minutes while it's idle, and any command starts one when none has run for an hour.

use std::collections::{HashMap, HashSet};
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
}

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
        Cache { root: home.join("cache") }
    }

    /// The size cap: `FENNEC_CACHE_CAP` in bytes, or 1 GiB.
    pub fn cap() -> u64 {
        std::env::var("FENNEC_CACHE_CAP").ok().and_then(|text| text.parse().ok()).unwrap_or(DEFAULT_CAP)
    }

    /// Where an entry of this build lives: `cache/<version>-<build>/<kind>/<name>`.
    pub fn path(&self, kind: &str, name: &str) -> PathBuf {
        self.root.join(FOLDER).join(kind).join(name)
    }

    /// Reads an entry, and marks it as just used.
    pub fn read(&self, kind: &str, name: &str) -> Option<Vec<u8>> {
        let path = self.path(kind, name);
        let bytes = std::fs::read(&path).ok()?;
        // Marking costs a write, so an entry marked in the last few minutes isn't marked again.
        let age = std::fs::metadata(&path).and_then(|metadata| metadata.modified()).ok();
        if age.and_then(|time| time.elapsed().ok()).is_none_or(|age| age > MARKED_RECENTLY) {
            mark_used(&path);
        }
        Some(bytes)
    }

    /// Writes an entry whole: to a temporary file, then renamed into place.
    pub fn write(&self, kind: &str, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.path(kind, name);
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, &path)
    }

    /// Keeps the program at `from` as an entry, as that very file rather than a copy of it:
    /// linked in, so the entry and `from` are one file. A program kept this way is the file that
    /// was launched, which macOS has checked already: it holds each new file's first launch for
    /// seconds while it does. Where a link can't be made, as across disks, it's copied.
    ///
    /// Nothing is ever written into a file once kept: it's made read-only, and a new program kept
    /// under the same name is a new file put in its place, so a link to the old one goes on
    /// holding the old program, never the new one. `from` has to be a file nothing else writes.
    pub fn keep_file(&self, kind: &str, name: &str, from: &Path) -> std::io::Result<()> {
        let path = self.path(kind, name);
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&temporary);
        if std::fs::hard_link(from, &temporary).is_err() {
            std::fs::copy(from, &temporary)?;
        }
        read_only_program(&temporary)?;
        std::fs::rename(&temporary, &path)
    }

    /// Puts the entry at `to` as that very file rather than a copy, marked as just used: linked
    /// out, where a link can be made. Gives back whether there was one.
    pub fn link_out(&self, kind: &str, name: &str, to: &Path) -> std::io::Result<bool> {
        let path = self.path(kind, name);
        if !path.is_file() {
            return Ok(false);
        }
        // An entry kept some other way, as by `write`, may not be runnable yet.
        read_only_program(&path)?;
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
            // A mark that can't be written means asking again next time, which is harmless.
            let _ = std::fs::create_dir_all(&self.root);
            let _ = File::create(&mark).and_then(|file| file.set_modified(SystemTime::now()));
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
        let unused: HashSet<PathBuf> = last_used
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

/// Marks an entry as just used, for cleanup's order. A program kept read-only can't be opened to
/// write, but its owner may still set its time. Only cleanup's order and the sweep of old runs
/// depend on this, so a failure here doesn't matter.
fn mark_used(path: &Path) {
    if let Ok(file) = File::open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
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
                    let _ = std::fs::remove_file(path);
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
        let _ = std::fs::remove_file(&self.path);
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
