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
//! sees half of one, and reading and writing take no locks. Each entry written as bytes ends in a
//! hash of them, and a read that doesn't match is a miss: a torn or changed entry is never used. A
//! kept object, below, is checked as it's written instead.
//!
//! A program kept as the file that was launched can't end in its hash, so it's kept under a name
//! that holds it, `<name>.<hash>`, a file never written again, and the entry is a file beside it,
//! `<name>.sum`, that names that hash. Keeping a new program puts its file in place first and then
//! renames the entry's file over the old one, in one step, so a reader finds the old program or the
//! new one, each whole, never one program beside the other's hash: that mix, read under load, was a
//! miss said on standard error, which a recorded run printed and its replay didn't. Putting one out
//! for a run checks its hash once, notes how its file looked then in `<name>.<hash>.seen`, and
//! checks it whole again only once that's changed or an hour has gone by. The hash guards against
//! accidents; against people, Fennec's folder is its user's alone, and a cache folder another
//! user owns or can write to isn't used at all (see `private.rs`). Reading an entry sets its modified time,
//! which is how cleanup knows what was used least recently; access times are unreliable.
//!
//! A compiled function's object is kept as a file of its own too, `<name>.o`, for the linker to
//! read where it lies, and found by its name alone: one look at the file, never opened, since a
//! dev build of a big program links tens of thousands, and on a Mac opening each one, and a file
//! naming its hash, was most of an edit's build. So it's checked as it's written rather than as
//! it's read: whole before it has its name, as every entry is; synced first, so a power cut can't
//! leave the name on less than the whole; written once and never again; and on Unix read-only
//! from the moment it's made, so nothing but root writes into it by accident. One that's empty, as
//! a disk that said it synced and didn't can leave it, is a miss, and written again.
//! Cleanup is the one thing that takes a lock, so two cleanups never run at once. The daemon runs
//! one every five minutes while it's idle, and any command starts one when none has run for an hour.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fennec_blake3::Hash;

/// This Fennec's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The folder this build's entries live in, `<version>-<hash of all its source>`.
pub const FOLDER: &str = concat!(env!("CARGO_PKG_VERSION"), "-", env!("FENNEC_BUILD"));

/// The cache's size cap unless `FENNEC_CACHE_CAP` sets one, in bytes: room for many programs'
/// builds, as a 200,000-line program's dev build keeps about half a gigabyte, so cleanup doesn't
/// take the objects of one being edited. A tenth of the space free on the cache's disk is the cap
/// when that's less (see [`Cache::cap`]).
pub const DEFAULT_CAP: u64 = 20 << 30;

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

/// The file, in the cache's folder, counting the cleanups that took anything.
const REMOVALS: &str = "removals";

/// The file, in the cache's folder, counting the analyses the build daemon kept that it found wrong.
const ANALYSES_FOUND_WRONG: &str = "analyses-found-wrong";

/// A cleanup lock older than this was left by a cleanup that died, and is taken over.
const STALE_LOCK: Duration = Duration::from_secs(600);

/// The cache under one Fennec folder.
#[derive(Clone, Debug)]
pub struct Cache {
    root: PathBuf,
    /// Whether the folder can be trusted: its user's alone. One that isn't is never read from or
    /// written to.
    trusted: Result<(), fennec_dist::Refused>,
}

/// How many bytes of hash end each entry written as bytes.
const HASH_LEN: usize = 32;

/// The ending of the file beside a kept program that holds its hash.
const SUM: &str = "sum";

/// The kind of entry that names what a program holds: its kind of object on the first line, then
/// each object's name, a line each (see [`Cache::hold`]).
pub const HOLDS: &str = "holds";

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
    /// For a kept program, the entry's file that names it, which goes with it.
    sum: Option<PathBuf>,
    /// For a kept program, its note of its last whole check, which goes with it.
    seen: Option<PathBuf>,
    /// For an object a program holds, the newest list naming it (see [`Cache::hold`]), and when
    /// that was last used as the walk saw it: the object is used as recently as that.
    holder: Option<(PathBuf, SystemTime)>,
}

impl Cache {
    pub fn new(home: &Path) -> Self {
        let root = home.join("cache");
        let trusted = fennec_dist::private_folder(home).and_then(|()| fennec_dist::private_folder(&root));
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

    /// The size cap: `FENNEC_CACHE_CAP` in bytes, or [`DEFAULT_CAP`], or a tenth of the space free
    /// on the disk Fennec's home is on if that's less, so the cache never fills a disk that's
    /// nearly full. It's asked at each cleanup, so the cap falls as the disk fills.
    pub fn cap() -> u64 {
        if let Some(cap) = std::env::var("FENNEC_CACHE_CAP").ok().and_then(|text| text.parse().ok()) {
            return cap;
        }
        cap_for(crate::fennec_home().and_then(|home| free_space(&home)))
    }

    /// Where an entry of this build lives: `cache/<version>-<build>/<kind>/<name>`.
    pub fn path(&self, kind: &str, name: &str) -> PathBuf {
        self.root.join(FOLDER).join(kind).join(name)
    }

    /// Reads an entry, and marks it as just used. One whose bytes don't match its hash is a
    /// miss, said once a command: it was torn or changed since it was written. So is one that's
    /// there but can't be read: only one that isn't there is a miss without a word.
    pub fn read(&self, kind: &str, name: &str) -> Option<Vec<u8>> {
        self.read_checked(kind, name).map(|(bytes, _)| bytes)
    }

    /// [`Self::read`], with the hash its bytes were checked against: for an entry written with
    /// [`Self::write`], their BLAKE3 hash, so whoever wants one needn't hash them again. None for a
    /// kept program, which is checked another way.
    pub fn read_hashed(&self, kind: &str, name: &str) -> Option<(Vec<u8>, Hash)> {
        let (bytes, hash) = self.read_checked(kind, name)?;
        Some((bytes, hash?))
    }

    /// The BLAKE3 hash an entry written with [`Self::write`] ends with, read without its bytes,
    /// for whoever may not need them: a run whose program is stored already names it by its
    /// image and never reads the image. Nothing here checks the bytes against it, which
    /// [`Self::read_hashed`] does once they're read. The entry is marked as used, as a read
    /// marks it. None for a kept program, or an entry too short to hold a hash.
    pub fn written_hash(&self, kind: &str, name: &str) -> Option<Hash> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        if !self.usable() {
            return None;
        }
        let path = self.path(kind, name);
        if beside(&path).is_some() {
            return None;
        }
        let read = |file: &mut File| {
            let end = file.seek(SeekFrom::End(0))?;
            let start = end.checked_sub(HASH_LEN as u64).ok_or(ErrorKind::UnexpectedEof)?;
            file.seek(SeekFrom::Start(start))?;
            let mut hash = [0; HASH_LEN];
            file.read_exact(&mut hash)?;
            Ok::<_, std::io::Error>(Hash(hash))
        };
        let (hash, age) =
            match File::open(&path).and_then(|mut file| Ok((read(&mut file)?, file.metadata()?))) {
                Ok((hash, metadata)) => (hash, metadata.modified().ok()),
                Err(err) => {
                    crate::touched("read an entry", Err(err));
                    return None;
                }
            };
        // Marked as `read_checked` marks what it reads.
        if age.and_then(|time| time.elapsed().ok()).is_none_or(|age| age > MARKED_RECENTLY) {
            mark_used(&path);
        }
        Some(hash)
    }

    /// [`Self::read`], with the hash of the bytes when it's the one their entry is checked against.
    fn read_checked(&self, kind: &str, name: &str) -> Option<(Vec<u8>, Option<Hash>)> {
        if !self.usable() {
            return None;
        }
        let named = self.path(kind, name);
        // A kept program is the whole of the file its entry names.
        let sum = beside(&named);
        let path = sum.as_deref().map_or_else(|| named.clone(), |sum| kept_at(&named, sum));
        let mut bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                crate::touched("read an entry", Err(err));
                return None;
            }
        };
        let mut checked = None;
        let whole = match &sum {
            Some(sum) => sum_of(&bytes) == *sum,
            None => match bytes.len().checked_sub(HASH_LEN).and_then(|end| bytes.split_at_checked(end)) {
                Some((text, hash)) => {
                    let found = fennec_blake3::hash(text);
                    let whole = hash == found.as_bytes();
                    checked = Some(found);
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
        Some((bytes, checked))
    }

    /// Where the object kept under `name` is, if it's there: the file itself, for the linker to
    /// read where it lies, found by one look at its name and never opened, and marked as just
    /// used, as [`Self::read`] marks an entry. It was checked as it was written (see the module's
    /// notes). Cleanup never takes an entry used in the last [`KEPT_RECENT`], so one a build just
    /// found stays while it links.
    pub fn kept_object(&self, kind: &str, name: &str) -> Option<PathBuf> {
        self.found_object(kind, name, true)
    }

    /// [`Self::kept_object`], for an object a program's list keeps (see [`Self::hold`]), as a
    /// unit a build uses again without writing it has: it's found by one look, and not marked.
    /// The build marked the list as it started, which marks every object it names at once, so a
    /// big program's edit marks one file rather than every object it holds.
    pub fn held_object(&self, kind: &str, name: &str) -> Option<PathBuf> {
        self.found_object(kind, name, false)
    }

    /// Where the object kept under `name` is, if it's there, marked as just used if `mark` says.
    fn found_object(&self, kind: &str, name: &str, mark: bool) -> Option<PathBuf> {
        if !self.usable() {
            return None;
        }
        let path = object_at(&self.path(kind, name));
        // No object is empty, so an empty file is one a power cut left, written again.
        let found = std::fs::metadata(&path).and_then(|metadata| {
            if metadata.is_file() && metadata.len() > 0 {
                metadata.modified()
            } else {
                Err(ErrorKind::NotFound.into())
            }
        });
        let changed = match found {
            Ok(changed) => changed,
            Err(err) => {
                crate::touched("look for an object", Err(err));
                return None;
            }
        };
        // Marking costs a write, so an object marked in the last few minutes isn't marked again.
        if mark && changed.elapsed().ok().is_none_or(|age| age > MARKED_RECENTLY) {
            crate::touched("mark what it used as used", set_time(&path, SystemTime::now()));
        }
        Some(path)
    }

    /// Keeps the list of the objects of kind `kind` the program `name` holds, as an entry of its
    /// own: cleanup counts each as used as recently as the list was, so a build marks the list
    /// (see [`Self::mark`]) rather than each object it finds again.
    pub fn hold(&self, name: &str, kind: &str, held: &[String]) -> std::io::Result<()> {
        let mut text = format!("{kind}\n");
        for object in held {
            text.push_str(object);
            text.push('\n');
        }
        self.write(HOLDS, name, text.as_bytes())
    }

    /// Marks an entry as just used without reading it, as [`Self::read`] marks one, unless it was
    /// marked in the last few minutes. An entry that isn't there is nothing to mark.
    pub fn mark(&self, kind: &str, name: &str) {
        if !self.usable() {
            return;
        }
        let path = self.path(kind, name);
        let Ok(changed) = std::fs::metadata(&path).and_then(|metadata| metadata.modified()) else { return };
        if changed.elapsed().ok().is_none_or(|age| age > MARKED_RECENTLY) {
            mark_used(&path);
        }
    }

    /// Keeps a compiled object as a file of its own under `name`, written once and never again,
    /// and gives back that file, for the linker to read where it lies: rather than a copy written
    /// out for each build and deleted after, which on a Mac was most of a build after a one-line
    /// edit. An object kept under `name` before is the same bytes already, since `name` holds the
    /// hash of all it's made from.
    pub fn keep_object(&self, kind: &str, name: &str, bytes: &[u8]) -> std::io::Result<Option<PathBuf>> {
        if !self.usable() {
            return Ok(None);
        }
        let path = object_at(&self.path(kind, name));
        let whole = |path: &Path| {
            std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        };
        if !whole(&path) {
            // Another build may have kept it meanwhile, and on Windows, where a file a linker has
            // open can't be renamed over, that's a failed rename here, though the object is there.
            put_with(&path, &[bytes], Made::ReadOnly)
                .or_else(|err| if whole(&path) { Ok(()) } else { Err(err) })?;
        }
        Ok(Some(path))
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
    /// The program itself is run and can't end in its hash, so it's kept under a name holding its
    /// hash, and the entry is a file naming that, put in place after it, in one step: a reader finds
    /// the program kept before or this one, each whole.
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
        let program = kept_at(&path, &sum);
        // The same program kept before is these bytes already, and never written again.
        if !program.is_file() {
            let temporary = linked_beside(from, &program)?;
            read_only_program(&temporary)?;
            std::fs::rename(&temporary, &program)?;
        }
        put(&sum_path(&path), &[sum.as_bytes()])
    }

    /// The file a kept program's bytes are in, as its entry names it now, if it has one.
    pub fn kept_program(&self, kind: &str, name: &str) -> Option<PathBuf> {
        let path = self.path(kind, name);
        beside(&path).map(|sum| kept_at(&path, &sum))
    }

    /// Puts the entry at `to` as that very file rather than a copy, marked as just used: linked
    /// out, where a link can be made. Gives back whether there was one, and one that doesn't match
    /// its hash wasn't. An entry written as bytes, which ends in its hash, is written out whole
    /// without it.
    pub fn link_out(&self, kind: &str, name: &str, to: &Path) -> std::io::Result<bool> {
        if !self.usable() {
            return Ok(false);
        }
        let named = self.path(kind, name);
        let Some(sum) = beside(&named) else {
            if !named.is_file() {
                return Ok(false);
            }
            let Some(bytes) = self.read(kind, name) else { return Ok(false) };
            let _ = std::fs::remove_file(to);
            put(to, &[&bytes])?;
            read_only_program(to)?;
            return Ok(true);
        };
        let path = kept_at(&named, &sum);
        if !checked_for_a_run(&path, &sum) {
            return Ok(false);
        }
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
        // A kept program and the entry's file naming it are one entry, its size both of theirs. A
        // program file no entry names any more, one a newer program replaced, is an entry of its
        // own, for cleanup to take once nothing holds it.
        let at: HashMap<PathBuf, usize> =
            found.iter().enumerate().map(|(index, entry)| (entry.path.clone(), index)).collect();
        let mut sums = Vec::new();
        for (index, entry) in found.iter().enumerate() {
            if let Some(&program) = program_named_by(&entry.path).and_then(|program| at.get(&program)) {
                sums.push((index, program));
            }
        }
        for &(sum, program) in &sums {
            let (size, path) = found.get(sum).map(|sum| (sum.size, sum.path.clone())).unwrap_or_default();
            if let Some(program) = found.get_mut(program) {
                program.size += size;
                program.sum = Some(path);
            }
        }
        // A program's note of its last whole check goes with it too.
        let mut seen = Vec::new();
        for (index, entry) in found.iter().enumerate() {
            let program =
                entry.path.as_os_str().to_str().and_then(|path| path.strip_suffix(&format!(".{SEEN}")));
            if let Some(&program) = program.and_then(|program| at.get(Path::new(program))) {
                seen.push((index, program));
            }
        }
        for &(note, program) in &seen {
            let (size, path) = found.get(note).map(|note| (note.size, note.path.clone())).unwrap_or_default();
            if let Some(program) = found.get_mut(program) {
                program.size += size;
                program.seen = Some(path);
            }
        }
        let folded: BTreeSet<usize> = sums.iter().chain(&seen).map(|&(beside, _)| beside).collect();
        found
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !folded.contains(index))
            .map(|(_, entry)| entry)
            .collect()
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
        self.cleanup_entries(entries, cap)
    }

    /// How many cleanups have taken anything from the cache, as cleanup counts them before it takes
    /// anything: a build that knows an object was there when the count was this knows it's there
    /// still.
    #[must_use]
    pub fn removals(&self) -> u64 {
        std::fs::read_to_string(self.root.join(REMOVALS))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    /// How many times the build daemon has found an analysis it kept for builds wrong, for this
    /// cache. It's part of what names a stored program, so none stored before one was found is put
    /// out after, and it's kept beside them, where no cleanup takes it: it goes only with them. One
    /// that's there but can't be read is `u64::MAX`, which names nothing stored.
    #[must_use]
    pub fn analyses_found_wrong(&self) -> u64 {
        match std::fs::read_to_string(self.root.join(ANALYSES_FOUND_WRONG)) {
            Ok(text) => text.trim().parse().unwrap_or(u64::MAX),
            Err(err) if err.kind() == ErrorKind::NotFound => 0,
            Err(_) => u64::MAX,
        }
    }

    /// Counts an analysis the build daemon kept that it found wrong, written whole or not at all.
    /// After one that couldn't be read, the count starts again from the time, which names nothing
    /// stored before.
    ///
    /// # Errors
    /// When the count can't be written.
    pub fn count_analysis_found_wrong(&self) -> std::io::Result<()> {
        let next = match self.analyses_found_wrong() {
            u64::MAX => {
                SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(1, |since| since.as_secs())
            }
            count => count + 1,
        };
        put(&self.root.join(ANALYSES_FOUND_WRONG), &[next.to_string().as_bytes()])
    }

    /// Counts a cleanup that's about to take something, before it takes anything, under its lock:
    /// a build that reads the count after anything has gone sees a new count.
    fn removed(&self) -> std::io::Result<()> {
        let next = self.removals().wrapping_add(1);
        put(&self.root.join(REMOVALS), &[next.to_string().as_bytes()])
    }

    /// Cleanup, from the entries as a walk of the cache found them. The walk can take seconds on a
    /// big cache, and a build can find and mark an entry meanwhile, to link it next, so each
    /// entry's time is looked at again just before it goes.
    fn cleanup_entries(&self, entries: Vec<Entry>, cap: u64) -> std::io::Result<Cleanup> {
        let mut cleanup = Cleanup::default();
        let mut counted = false;
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
        if !unused.is_empty() {
            self.removed()?;
            counted = true;
        }
        for build in &unused {
            std::fs::remove_dir_all(build)?;
        }
        let (gone, mut kept): (Vec<Entry>, Vec<Entry>) =
            entries.into_iter().partition(|entry| unused.contains(&entry.build));
        held_as_recently_as_their_lists(&mut kept);
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
                // Used since the walk saw it: a build found it, and may be about to link it.
                if entry.used_since() {
                    continue;
                }
                if !counted {
                    self.removed()?;
                    counted = true;
                }
                // The entry's file first, so no entry ever names a program that's gone.
                if let Some(sum) = &entry.sum {
                    let _ = std::fs::remove_file(sum);
                }
                std::fs::remove_file(&entry.path)?;
                if let Some(seen) = &entry.seen {
                    let _ = std::fs::remove_file(seen);
                }
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
    /// Whether the entry was used since the walk saw it, or is a second name for a program a run
    /// started meanwhile: what a build that found it did, as it marked it as just used. One that's
    /// gone already wasn't.
    fn used_since(&self) -> bool {
        // The list holding it was marked since, by a build starting that may link it.
        if let Some((list, used)) = &self.holder
            && std::fs::metadata(list).and_then(|metadata| metadata.modified()).is_ok_and(|now| now > *used)
        {
            return true;
        }
        let Ok(now) = std::fs::metadata(&self.path) else { return false };
        #[cfg(unix)]
        let links = std::os::unix::fs::MetadataExt::nlink(&now);
        #[cfg(not(unix))]
        let links = 1;
        now.modified().is_ok_and(|time| time > self.used) || links > 1
    }

    /// Whether the entry can't go yet: it was stored or used in the last [`KEPT_RECENT`], or a
    /// program it is has been put out where a run started it, as a second name for the same file.
    fn held(&self) -> bool {
        let recent = self.used.elapsed().map_or(true, |age| age < KEPT_RECENT);
        recent || self.links > 1
    }
}

/// How long a kept program's last whole check stands while its file looks as it did then.
const CHECK_STANDS: Duration = Duration::from_hours(1);

/// How old a kept program's time may be when a run puts it out without marking it again. A later
/// run sweeps what a run put out once its time is 10 minutes old, and this is well inside that.
const RUN_MARKED: Duration = Duration::from_mins(1);

/// The ending of the file beside a kept program noting its last whole check.
const SEEN: &str = "seen";

/// Whether a kept program's bytes are the ones its entry names, marking it as used for a run whose
/// program it's about to be. A whole check hashes every byte, which was most of a no-change run: 17M
/// instructions for a hello program and 38M for a 5,000-line kit. So a whole check is noted beside
/// the program, in `<program>.seen` (see [`Seen`]), with how the file looked: while it looks the
/// same and the check is under [`CHECK_STANDS`] old, the check stands and nothing is read.
///
/// A file is read-only once kept, and a write into it moves its modified time. Its changed time
/// isn't looked at: putting it out links it, and a sweep unlinks it, and either moves that time, so
/// every run would check it whole. What no time shows, as a disk going bad under it, macOS finds as
/// it pages the program in, by its code signature, and kills it; the hour is how long that can go
/// on before a whole check finds it and the program is built again.
///
/// Marked before it's put out: a linked file is the entry, with its time, and a sweep of what
/// earlier runs built goes by that time, so it has to be at most [`RUN_MARKED`] old.
fn checked_for_a_run(path: &Path, sum: &str) -> bool {
    let seen = seen_path(path);
    let Ok(before) = std::fs::metadata(path) else { return false };
    let noted = std::fs::read_to_string(&seen).ok().and_then(|text| Seen::read(&text));
    let stands = noted.filter(|noted| noted.stands(&before)).map(|noted| noted.checked);
    let marked =
        before.modified().ok().and_then(|time| time.elapsed().ok()).is_none_or(|age| age > RUN_MARKED);
    if marked {
        mark_used(path);
    }
    // Looked at after the mark, which moves its modified time, and before any read: a write after
    // this look is a change the next run finds, whether or not this run's read saw it.
    let Ok(after) = std::fs::metadata(path) else { return false };
    let checked = if let Some(checked) = stands {
        checked
    } else {
        let Ok(bytes) = std::fs::read(path) else { return false };
        if sum_of(&bytes) != sum {
            crate::unsaved::doubted(path);
            return false;
        }
        SystemTime::now()
    };
    if marked || stands.is_none() {
        // A note that isn't written, or is lost to a power cut, only means a whole check next time.
        let _ = note(&seen, &Seen::of(&after, checked).written());
    }
    true
}

/// How a kept program's file looked after its last whole check, and when that was. The system's
/// own numbers for the file: its device and inode, its size, and its modified time to the
/// nanosecond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    file: [i64; 5],
    checked: SystemTime,
}

impl Seen {
    #[cfg(unix)]
    fn file(metadata: &std::fs::Metadata) -> [i64; 5] {
        use std::os::unix::fs::MetadataExt as _;
        let unsigned = |number: u64| number.cast_signed();
        [
            unsigned(metadata.dev()),
            unsigned(metadata.ino()),
            unsigned(metadata.size()),
            metadata.mtime(),
            metadata.mtime_nsec(),
        ]
    }

    /// Nothing that can't be told apart: every file looks new, so every check is a whole one.
    #[cfg(not(unix))]
    fn file(_: &std::fs::Metadata) -> [i64; 5] {
        [-1; 5]
    }

    fn of(metadata: &std::fs::Metadata, checked: SystemTime) -> Self {
        Self { file: Self::file(metadata), checked }
    }

    /// Whether this check stands for the file as it looks now.
    fn stands(&self, now: &std::fs::Metadata) -> bool {
        self.file != [-1; 5]
            && self.file == Self::file(now)
            && self.checked.elapsed().is_ok_and(|age| age < CHECK_STANDS)
    }

    /// The note's text: the file's numbers, then the check's time in nanoseconds since 1970, on one line.
    fn written(&self) -> String {
        let checked = self.checked.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_nanos());
        let file: Vec<String> = self.file.iter().map(i64::to_string).collect();
        format!("{} {checked}\n", file.join(" "))
    }

    /// A note's text read back: none unless it's whole, six numbers and the line's end.
    fn read(text: &str) -> Option<Self> {
        let line = text.strip_suffix('\n')?;
        let mut numbers = line.split(' ');
        let mut file = [0; 5];
        for number in &mut file {
            *number = numbers.next()?.parse().ok()?;
        }
        let checked: u64 = numbers.next()?.parse().ok()?;
        if numbers.next().is_some() {
            return None;
        }
        let checked = SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(checked))?;
        Some(Self { file, checked })
    }
}

/// Where a kept program's [`Seen`] note is: the program's name with `.seen` after it.
fn seen_path(program: &Path) -> PathBuf {
    let mut name = program.as_os_str().to_owned();
    name.push(".");
    name.push(SEEN);
    PathBuf::from(name)
}

/// Writes a note whole, put in place by a rename, but not synced: one lost costs a whole check.
fn note(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let temporary = temporary_beside(path);
    let written = File::options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// A kept program's entry: the file naming its hash, the entry's whole name with `.sum` after it,
/// so `root` and `root.functions` never share one.
fn sum_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(SUM);
    PathBuf::from(name)
}

/// Where a kept program with hash `sum` is: the entry's name with the hash after it.
fn kept_at(path: &Path, sum: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(sum);
    PathBuf::from(name)
}

/// The cap when nothing sets one, with `free` bytes free on the cache's disk, when that's known.
fn cap_for(free: Option<u64>) -> u64 {
    free.map_or(DEFAULT_CAP, |free| DEFAULT_CAP.min(free / 10))
}

/// How many bytes this user may still write on the disk `path` is on, as the system says.
#[cfg(unix)]
fn free_space(path: &Path) -> Option<u64> {
    let stats = rustix::fs::statvfs(path).ok()?;
    stats.f_bavail.checked_mul(stats.f_frsize)
}

/// Not asked where there's no safe call for it: the cap is [`DEFAULT_CAP`] there.
#[cfg(not(unix))]
fn free_space(_: &Path) -> Option<u64> {
    None
}

/// Where a kept object is: the entry's name with `.o` after it, which no other entry's name ends in.
fn object_at(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".o");
    PathBuf::from(name)
}

/// The program an entry's `.sum` file names, for a path ending in `.sum`.
fn program_named_by(path: &Path) -> Option<PathBuf> {
    let entry = path.as_os_str().to_str()?.strip_suffix(&format!(".{SUM}"))?;
    beside(Path::new(entry)).map(|sum| kept_at(Path::new(entry), &sum))
}

/// The hash a kept program's entry names, if it's one. One that's there but can't be read is said.
fn beside(path: &Path) -> Option<String> {
    match std::fs::read_to_string(sum_path(path)) {
        Ok(sum) => Some(sum.trim().to_owned()),
        Err(err) => {
            crate::touched("read an entry", Err(err));
            None
        }
    }
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
    put_with(path, parts, Made::Writable)
}

/// How [`put_with`] makes a file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Made {
    Writable,
    /// Read-only from the moment it's made, on Unix, where a file's mode is set as it's made at no
    /// cost: a kept object, which nothing writes into once it has its name.
    ReadOnly,
}

/// [`put`], with the file made as `made` says.
fn put_with(path: &Path, parts: &[&[u8]], made: Made) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    for _ in 0..8 {
        let temporary = temporary_beside(path);
        let mut options = File::options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if made == Made::ReadOnly {
            // The mode is for later opens: this one writes the file as it makes it.
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o444);
        }
        #[cfg(not(unix))]
        let _ = made;
        let mut file = match options.open(&temporary) {
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
    // A kept program's entry goes with it: the file naming it, the program's name less its hash.
    if let Some(sum) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once('.'))
        .map(|(entry, _)| path.with_file_name(format!("{entry}.{SUM}")))
        .filter(|sum| sum.is_file())
    {
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
            found.push(Entry {
                path,
                size: metadata.len(),
                used,
                build: build.to_path_buf(),
                links,
                sum: None,
                seen: None,
                holder: None,
            });
        }
    }
}

/// Counts each object a program's list holds (see [`Cache::hold`]) as used as recently as the
/// newest list naming it was, as cleanup's walk found them. A list that can't be read, or whose
/// bytes don't match its hash, holds nothing.
fn held_as_recently_as_their_lists(entries: &mut [Entry]) {
    let mut held: HashMap<PathBuf, (PathBuf, SystemTime)> = HashMap::new();
    for list in
        entries.iter().filter(|entry| entry.path.parent().is_some_and(|folder| folder.ends_with(HOLDS)))
    {
        let Ok(bytes) = std::fs::read(&list.path) else { continue };
        let Some((text, hash)) =
            bytes.len().checked_sub(HASH_LEN).and_then(|end| bytes.split_at_checked(end))
        else {
            continue;
        };
        if hash != fennec_blake3::hash(text).as_bytes() {
            continue;
        }
        let mut lines = String::from_utf8_lossy(text)
            .into_owned()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter();
        let Some(kind) = lines.next() else { continue };
        for name in lines.filter(|name| !name.is_empty()) {
            let object = object_at(&list.build.join(&kind).join(&name));
            let newer = held.get(&object).is_none_or(|(_, used)| *used < list.used);
            if newer {
                held.insert(object, (list.path.clone(), list.used));
            }
        }
    }
    for entry in entries {
        if let Some((list, used)) = held.get(&entry.path) {
            entry.used = entry.used.max(*used);
            entry.holder = Some((list.clone(), *used));
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

    /// The cap with nothing setting it is 20 GiB, or a tenth of the space free on the disk if that's
    /// less; and on Unix the free space of a folder that's there is found.
    #[test]
    fn the_cap_is_twenty_gibibytes_or_a_tenth_of_the_free_space() {
        let gib = 1_u64 << 30;
        assert_eq!(super::cap_for(None), 20 * gib);
        assert_eq!(super::cap_for(Some(1000 * gib)), 20 * gib);
        assert_eq!(super::cap_for(Some(50 * gib)), 5 * gib);
        assert_eq!(super::cap_for(Some(0)), 0);
        #[cfg(unix)]
        assert!(super::free_space(&std::env::temp_dir()).is_some_and(|free| free > 0));
    }

    /// An object kept is a file holding just its bytes, for the linker to read where it lies, found
    /// again by its name and never written again: kept a second time under the same name, it's the
    /// file kept first. On Unix it's read-only from the start. An entry written as bytes, which end
    /// in their hash, isn't an object, and a name nothing was kept under finds nothing.
    #[test]
    fn an_object_kept_is_found_by_its_name_where_it_lies() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-keep-object-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let kept = cache.keep_object("llvm", "key", b"an object").unwrap().unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), b"an object");
        assert_eq!(cache.kept_object("llvm", "key"), Some(kept.clone()));
        assert_eq!(cache.keep_object("llvm", "key", b"other bytes").unwrap(), Some(kept.clone()));
        assert_eq!(std::fs::read(&kept).unwrap(), b"an object", "a kept object was written again");
        #[cfg(unix)]
        assert!(std::fs::metadata(&kept).unwrap().permissions().readonly(), "a kept object can be written");
        cache.write("llvm", "written", b"another object").unwrap();
        assert_eq!(cache.kept_object("llvm", "written"), None);
        assert_eq!(cache.read("llvm", "written").as_deref(), Some(&b"another object"[..]));
        assert_eq!(cache.kept_object("llvm", "gone"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An object a build finds after cleanup's walk saw it stays, though the walk saw it unused
    /// for long: the build marked it, to link it next. One nothing used since goes. A cleanup that
    /// went by what its walk saw alone took objects from under a link, which failed for a file
    /// that wasn't there.
    #[test]
    fn an_object_found_while_cleanup_walks_the_cache_stays() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-cleanup-walk-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let found = cache.keep_object("llvm", "found", b"an object a build finds").unwrap().unwrap();
        let old = cache.keep_object("llvm", "old", b"an object nothing uses").unwrap().unwrap();
        let long_ago = std::time::SystemTime::now() - super::KEPT_RECENT - Duration::from_secs(600);
        super::set_time(&found, long_ago).unwrap();
        super::set_time(&old, long_ago).unwrap();
        let walked = cache.entries();
        assert_eq!(cache.kept_object("llvm", "found"), Some(found.clone()));
        let cleanup = cache.cleanup_entries(walked, 0).unwrap();
        assert!(found.exists(), "cleanup took an object a build found while it walked the cache");
        assert!(!old.exists(), "an object nothing used stayed: {cleanup:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An object a program's list holds is used as recently as the list: cleanup leaves it while
    /// the list was used lately, however long ago the object itself was marked, and takes it once
    /// the list is old too. An object no list holds goes by its own time, as before.
    #[test]
    fn an_object_a_recent_list_holds_stays_with_no_mark_of_its_own() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-held-objects-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let held = cache.keep_object("llvm", "held", b"an object a program holds").unwrap().unwrap();
        let loose = cache.keep_object("llvm", "loose", b"an object no program holds").unwrap().unwrap();
        let long_ago = std::time::SystemTime::now() - super::KEPT_RECENT - Duration::from_secs(600);
        super::set_time(&held, long_ago).unwrap();
        super::set_time(&loose, long_ago).unwrap();
        cache.hold("program", "llvm", &["held".to_owned()]).unwrap();
        let cleanup = cache.cleanup(0).unwrap();
        assert!(held.exists(), "cleanup took an object a list used just now holds: {cleanup:?}");
        assert!(!loose.exists(), "an object nothing holds or used stayed: {cleanup:?}");
        super::set_time(&cache.path(super::HOLDS, "program"), long_ago).unwrap();
        let cleanup = cache.cleanup(0).unwrap();
        assert!(!held.exists(), "an object only an old list holds stayed: {cleanup:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A cleanup that takes anything is counted before it takes it, and one that takes nothing
    /// isn't: a build that knew an object was there under the count it reads now knows it's there.
    #[test]
    fn a_cleanup_that_takes_anything_is_counted() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-removals-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        assert_eq!(cache.removals(), 0);
        let kept = cache.keep_object("llvm", "kept", b"an object").unwrap().unwrap();
        let cleanup = cache.cleanup(1 << 30).unwrap();
        assert_eq!((cleanup.removed, cache.removals()), (0, 0), "a cleanup that took nothing was counted");
        let long_ago = std::time::SystemTime::now() - super::KEPT_RECENT - Duration::from_secs(600);
        super::set_time(&kept, long_ago).unwrap();
        let cleanup = cache.cleanup(0).unwrap();
        assert!(!kept.exists(), "{cleanup:?}");
        assert_eq!(cache.removals(), 1, "a cleanup that took an object wasn't counted");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A unit used again finds its object without marking it, as the program's list marks it,
    /// while finding one any other way marks it as just used.
    #[test]
    fn a_held_object_is_found_without_marking_it() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-held-object-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let kept = cache.keep_object("llvm", "key", b"an object").unwrap().unwrap();
        let long_ago = std::time::SystemTime::now() - super::KEPT_RECENT - Duration::from_secs(600);
        super::set_time(&kept, long_ago).unwrap();
        let time = |path: &std::path::Path| std::fs::metadata(path).unwrap().modified().unwrap();
        assert_eq!(cache.held_object("llvm", "key"), Some(kept.clone()));
        assert_eq!(time(&kept), long_ago, "finding a held object marked it");
        assert_eq!(cache.held_object("llvm", "gone"), None);
        assert_eq!(cache.kept_object("llvm", "key"), Some(kept.clone()));
        assert!(time(&kept) > long_ago, "finding a kept object didn't mark it");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An object whose program's list a build marks while cleanup walks the cache stays, though
    /// the walk saw the list and the object both unused for long: the build is starting, and may
    /// link it. One nothing holds goes.
    #[test]
    fn an_object_whose_list_a_build_marks_while_cleanup_walks_stays() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-held-walk-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let held = cache.keep_object("llvm", "held", b"an object a program holds").unwrap().unwrap();
        let loose = cache.keep_object("llvm", "loose", b"an object no program holds").unwrap().unwrap();
        cache.hold("program", "llvm", &["held".to_owned()]).unwrap();
        let long_ago = std::time::SystemTime::now() - super::KEPT_RECENT - Duration::from_secs(600);
        for path in [&held, &loose, &cache.path(super::HOLDS, "program")] {
            super::set_time(path, long_ago).unwrap();
        }
        let walked = cache.entries();
        cache.mark(super::HOLDS, "program");
        let cleanup = cache.cleanup_entries(walked, 0).unwrap();
        assert!(held.exists(), "cleanup took an object whose list a build marked as it walked: {cleanup:?}");
        assert!(!loose.exists(), "an object nothing holds or used stayed: {cleanup:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
        assert_eq!(inode(&launched), inode(&cache.kept_program("programs", "root").unwrap()));
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

    /// A program checked whole as it's put out for a run isn't read again while its file looks as
    /// it did, and the note of that check goes with it at cleanup. It's checked whole again once
    /// the check is an hour old, or once the file has changed.
    #[test]
    #[cfg(unix)]
    fn a_whole_check_stands_while_the_program_looks_the_same() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-check-stands-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = super::Cache::new(&dir.join("home"));
        let launched = dir.join("launched");
        std::fs::write(&launched, b"a program").unwrap();
        cache.keep_file("programs", "root", &launched).unwrap();
        std::fs::remove_file(&launched).unwrap();
        let kept = cache.kept_program("programs", "root").unwrap();
        let seen = super::seen_path(&kept);
        let next = dir.join("next");
        assert!(cache.link_out("programs", "root", &next).unwrap());
        let noted = super::Seen::read(&std::fs::read_to_string(&seen).unwrap()).unwrap();
        assert!(noted.stands(&std::fs::metadata(&kept).unwrap()));
        // The note goes with its program: one entry, its size both of theirs.
        let entries = cache.entries();
        assert!(entries.iter().all(|entry| entry.path != seen), "the note is an entry of its own");
        let entry = entries.iter().find(|entry| entry.path == kept).unwrap();
        assert_eq!(entry.seen.as_deref(), Some(seen.as_path()));
        // Made unreadable: a run that read it would fail, and one that puts it out didn't read it.
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&kept).is_ok() {
            // Run as root, which reads it anyway: nothing here can tell a read from none.
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        assert!(cache.link_out("programs", "root", &next).unwrap(), "a check that stands read the program");
        // An hour old, the check is made whole again, and fails on what can't be read.
        let looks = std::fs::metadata(&kept).unwrap();
        let long_ago = std::time::SystemTime::now() - super::CHECK_STANDS - Duration::from_secs(1);
        super::note(&seen, &super::Seen::of(&looks, long_ago).written()).unwrap();
        assert!(!cache.link_out("programs", "root", &next).unwrap(), "an hour-old check stood");
        // Noted as just checked, then written into: its modified time has moved, so it's checked whole.
        super::note(&seen, &super::Seen::of(&looks, std::time::SystemTime::now()).written()).unwrap();
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&kept, b"a pregram").unwrap();
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(!cache.link_out("programs", "root", &next).unwrap(), "a changed program was put out");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A note of a check is whole or nothing: cut short or with more after it, it's none.
    #[test]
    fn a_note_of_a_check_reads_back_only_whole() {
        let checked = std::time::SystemTime::UNIX_EPOCH + Duration::from_nanos(1_790_000_000_123_456_789);
        let seen = super::Seen { file: [1, 2, 3, 4, -5], checked };
        let text = seen.written();
        assert_eq!(super::Seen::read(&text), Some(seen));
        assert_eq!(super::Seen::read(text.trim_end()), None);
        assert_eq!(super::Seen::read(text.get(..text.len() / 2).unwrap()), None);
        assert_eq!(super::Seen::read(&format!("{} 6\n", text.trim_end())), None);
    }

    /// A program kept under a name another was kept under replaces it with a file of its own: a
    /// link to the old one still holds the old program, the next one put out holds the new, and
    /// neither can be written into. The old program's file stays whole beside the new one, so a
    /// reader that found the entry naming it just before reads it whole: no reader ever finds one
    /// program with the other's hash.
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
        let kept_first = cache.kept_program("programs", "root").unwrap();
        let (old, new) = (dir.join("old"), dir.join("new"));
        assert!(cache.link_out("programs", "root", &old).unwrap());
        std::fs::write(&second, b"the new program").unwrap();
        cache.keep_file("programs", "root", &second).unwrap();
        let kept_second = cache.kept_program("programs", "root").unwrap();
        assert_ne!(kept_first, kept_second);
        assert_eq!(std::fs::read(&kept_first).unwrap(), b"the old program");
        assert_eq!(cache.read("programs", "root").as_deref(), Some(&b"the new program"[..]));
        assert!(cache.link_out("programs", "root", &new).unwrap());
        assert_eq!(std::fs::read(&old).unwrap(), b"the old program");
        assert_eq!(std::fs::read(&new).unwrap(), b"the new program");
        let metadata = |path: &std::path::Path| std::fs::metadata(path).unwrap();
        assert_ne!(metadata(&old).ino(), metadata(&new).ino());
        for path in [&old, &new, &kept_first, &kept_second] {
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
