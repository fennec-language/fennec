//! The cache: entries written whole, reads marking use, and cleanup's order and lock.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fennec_build::{Cache, Cleanup, VERSION};

/// A fresh Fennec folder for one test.
fn home(name: &str) -> std::io::Result<PathBuf> {
    let home = std::env::temp_dir().join(format!("fennec-cache-{name}-{}", std::process::id()));
    if home.exists() {
        std::fs::remove_dir_all(&home)?;
    }
    std::fs::create_dir_all(&home)?;
    Ok(home)
}

/// Writes a file of `size` bytes, last used `age` seconds ago.
fn entry(path: &Path, size: usize, age: u64) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(path, vec![0_u8; size])?;
    File::options().write(true).open(path)?.set_modified(SystemTime::now() - Duration::from_secs(age))
}

fn modified(path: &Path) -> std::io::Result<SystemTime> {
    std::fs::metadata(path)?.modified()
}

#[test]
fn entries_are_written_whole_and_reading_marks_them_used() {
    let home = home("entries").unwrap();
    let cache = Cache::new(&home);
    cache.write("runs", "a.txt", b"hello").unwrap();
    let path = cache.path("runs", "a.txt");
    assert_eq!(path, home.join("cache").join(VERSION).join("runs").join("a.txt"));
    entry(&path, 5, 3_600).unwrap();
    let before = modified(&path).unwrap();
    assert_eq!(cache.read("runs", "a.txt"), Some(vec![0; 5]));
    assert!(modified(&path).unwrap() > before, "reading should mark the entry as used");
    assert_eq!(cache.read("runs", "missing.txt"), None);
    assert_eq!(cache.size(), 5);
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_cache_under_its_cap_is_left_alone() {
    let home = home("under").unwrap();
    let cache = Cache::new(&home);
    entry(&cache.path("functions", "a"), 100, 10).unwrap();
    assert_eq!(cache.cleanup(1_000).unwrap(), Cleanup { size: 100, ..Cleanup::default() });
    assert_eq!(
        cache.cleanup(100).unwrap(),
        Cleanup { size: 100, ..Cleanup::default() },
        "exactly at the cap"
    );
    assert!(cache.path("functions", "a").exists());
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn cleanup_drops_old_versions_then_the_least_recently_used() {
    let home = home("over").unwrap();
    let cache = Cache::new(&home);
    let old = home.join("cache").join("0.0.0-old").join("functions").join("x");
    entry(&old, 300, 1).unwrap();
    entry(&cache.path("functions", "oldest"), 300, 300).unwrap();
    entry(&cache.path("functions", "middle"), 300, 200).unwrap();
    entry(&cache.path("functions", "newest"), 300, 100).unwrap();

    // 1,200 bytes against a cap of 1,000: down to 800, so two entries go.
    let cleanup = cache.cleanup(1_000).unwrap();
    assert_eq!(cleanup, Cleanup { removed: 2, freed: 600, size: 600, skipped: false });
    assert!(!old.exists(), "another version's entries go first, however recently used");
    assert!(!home.join("cache").join("0.0.0-old").exists(), "its emptied folder goes too");
    assert!(!cache.path("functions", "oldest").exists());
    assert!(cache.path("functions", "middle").exists());
    assert!(cache.path("functions", "newest").exists());
    assert!(!home.join("cache").join("cleanup.lock").exists(), "the lock is released");

    // Emptying this version's folders leaves them, since a command may be writing into one.
    cache.cleanup(0).unwrap();
    assert!(!cache.path("functions", "newest").exists());
    assert!(home.join("cache").join(VERSION).join("functions").is_dir());
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_cleanup_already_running_is_left_to_finish() {
    let home = home("locked").unwrap();
    let cache = Cache::new(&home);
    entry(&cache.path("functions", "a"), 500, 10).unwrap();
    let lock = home.join("cache").join("cleanup.lock");
    entry(&lock, 0, 1).unwrap();
    assert!(cache.cleanup(100).unwrap().skipped);
    assert!(cache.path("functions", "a").exists());

    // A lock left for over ten minutes was left by a cleanup that died.
    entry(&lock, 0, 3_600).unwrap();
    assert_eq!(cache.cleanup(100).unwrap().removed, 1);
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(unix)]
#[test]
fn a_cache_that_cant_be_written_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let home = home("read-only").unwrap();
    let cache = Cache::new(&home);
    entry(&cache.path("functions", "a"), 500, 10).unwrap();
    let root = home.join("cache");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
    let cleanup = cache.cleanup(100);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(cleanup.is_err(), "taking the lock failed, so cleanup can't be said to have run: {cleanup:?}");
    std::fs::remove_dir_all(&home).unwrap();
}
