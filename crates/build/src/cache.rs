//! The cache in `~/.fennec/cache`: one folder per Fennec version, entries written whole, and
//! a cleanup that keeps it under a size cap.
//!
//! An entry is written to a temporary file and renamed into place, so a reader never sees half
//! of one, and reading and writing take no locks. Reading an entry sets its modified time,
//! which is how cleanup knows what was used least recently; access times are unreliable.
//! Cleanup is the one thing that takes a lock, so two cleanups never run at once.

use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// This Fennec's version. Each version's entries live in a folder of their own.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The cache's size cap unless `FENNEC_CACHE_CAP` sets one, in bytes.
pub const DEFAULT_CAP: u64 = 1 << 30;

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
    current: bool,
}

impl Cache {
    pub fn new(home: &Path) -> Self {
        Cache { root: home.join("cache") }
    }

    /// The size cap: `FENNEC_CACHE_CAP` in bytes, or 1 GiB.
    pub fn cap() -> u64 {
        std::env::var("FENNEC_CACHE_CAP").ok().and_then(|text| text.parse().ok()).unwrap_or(DEFAULT_CAP)
    }

    /// Where an entry of this version lives: `cache/<version>/<kind>/<name>`.
    pub fn path(&self, kind: &str, name: &str) -> PathBuf {
        self.root.join(VERSION).join(kind).join(name)
    }

    /// Reads an entry, and marks it as just used.
    pub fn read(&self, kind: &str, name: &str) -> Option<Vec<u8>> {
        let path = self.path(kind, name);
        let bytes = std::fs::read(&path).ok()?;
        if let Ok(file) = File::options().write(true).open(&path) {
            // Only cleanup's order depends on this, so a failure here doesn't matter.
            let _ = file.set_modified(SystemTime::now());
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

    /// How many bytes the cache holds, across every version.
    pub fn size(&self) -> u64 {
        self.entries().iter().map(|entry| entry.size).sum()
    }

    fn entries(&self) -> Vec<Entry> {
        let mut found = Vec::new();
        let Ok(versions) = std::fs::read_dir(&self.root) else { return found };
        for version in versions.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
            let current = version.file_name().is_some_and(|name| name == VERSION);
            files(&version, current, &mut found);
        }
        found
    }

    /// Keeps the cache under `cap` bytes. Once it's over, entries go until it's down to 80%
    /// of the cap: every other version's first, since nothing can use them again, then this
    /// version's least recently used.
    pub fn cleanup(&self, cap: u64) -> std::io::Result<Cleanup> {
        let Some(_lock) = Lock::take(&self.root.join("cleanup.lock"))? else {
            return Ok(Cleanup { skipped: true, ..Cleanup::default() });
        };
        let mut entries = self.entries();
        let mut size: u64 = entries.iter().map(|entry| entry.size).sum();
        let mut cleanup = Cleanup::default();
        if size > cap {
            let goal = cap / 5 * 4;
            entries.sort_by_key(|entry| (entry.current, entry.used));
            for entry in entries {
                if size <= goal {
                    break;
                }
                std::fs::remove_file(&entry.path)?;
                size -= entry.size;
                cleanup.removed += 1;
                cleanup.freed += entry.size;
            }
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
            if version.is_dir() && version.file_name().is_some_and(|name| name != VERSION) {
                remove_empty_folders(&version);
            }
        }
    }
}

/// Every file under `dir`.
fn files(dir: &Path, current: bool, found: &mut Vec<Entry>) {
    let Ok(listing) = std::fs::read_dir(dir) else { return };
    for item in listing.flatten() {
        let path = item.path();
        let Ok(metadata) = item.metadata() else { continue };
        if metadata.is_dir() {
            files(&path, current, found);
        } else {
            let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            found.push(Entry { path, size: metadata.len(), used, current });
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

#[cfg(test)]
mod tests {
    use super::is_stale;
    use std::time::Duration;

    #[test]
    fn a_lock_is_stale_after_ten_minutes() {
        assert!(!is_stale(Duration::from_secs(600)));
        assert!(is_stale(Duration::from_secs(601)));
    }
}
