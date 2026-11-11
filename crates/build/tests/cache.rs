//! The cache: entries written whole, reads marking use, and cleanup's order and lock.

use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime};

use fennec_build::{Cache, Cleanup, FOLDER};

/// A fresh Fennec folder for one test.
fn home(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let home = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cache-{name}-{}", std::process::id())),
    );
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

/// Makes a folder look last changed `age` seconds ago.
fn age_folder(folder: &Path, age: u64) -> std::io::Result<()> {
    let mut options = File::options();
    options.read(true);
    // Windows opens a folder only when asked for its backup semantics, and changes its times
    // only through a handle that may write its attributes.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES and FILE_FLAG_BACKUP_SEMANTICS.
        options.read(false).access_mode(0x0100).custom_flags(0x0200_0000);
    }
    options.open(folder)?.set_modified(SystemTime::now() - Duration::from_secs(age))
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
    assert_eq!(path, home.join("cache").join(FOLDER).join("runs").join("a.txt"));
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

/// A build of Fennec reads nothing another wrote, even of the same version and under the same key:
/// a key that leaves out something the compiler does would otherwise bring back what an older
/// compiler made, and run it.
#[test]
fn an_entry_another_build_wrote_is_never_read() {
    let home = home("other build").unwrap();
    let cache = Cache::new(&home);
    assert_eq!(FOLDER, format!("{}-{}", fennec_build::VERSION, env!("FENNEC_BUILD")));
    let other = home.join("cache").join(format!("{}-{}", fennec_build::VERSION, "0".repeat(64)));
    entry(&other.join("llvm").join("same key"), 10, 3_600).unwrap();
    assert_eq!(cache.read("llvm", "same key"), None);
    cache.write("llvm", "same key", b"this build's").unwrap();
    assert_eq!(cache.read("llvm", "same key").as_deref(), Some(&b"this build's"[..]));
    // Used before this build's entry, it's the first a cleanup over the cap removes.
    cache.cleanup(20).unwrap();
    assert!(!other.exists());
    assert_eq!(cache.read("llvm", "same key").as_deref(), Some(&b"this build's"[..]));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn cleanup_drops_the_least_recently_used_whichever_build_wrote_them() {
    let home = home("over").unwrap();
    let cache = Cache::new(&home);
    // Each used longer ago than an entry is always kept for.
    let old = fennec_build::KEPT_RECENT.as_secs();
    let other = home.join("cache").join("0.0.0-other").join("functions").join("x");
    entry(&other, 300, old + 10).unwrap();
    entry(&cache.path("functions", "oldest"), 300, old + 300).unwrap();
    entry(&cache.path("functions", "middle"), 300, old + 200).unwrap();
    entry(&cache.path("functions", "newest"), 300, old + 100).unwrap();

    // 1,200 bytes against a cap of 1,000: down to 800, so two entries go. Another build used
    // since may be one in use elsewhere, so it isn't put first.
    let cleanup = cache.cleanup(1_000).unwrap();
    assert_eq!(cleanup, Cleanup { removed: 2, freed: 600, size: 600, skipped: false });
    assert!(other.exists(), "another build's entry used since stays");
    assert!(!cache.path("functions", "oldest").exists());
    assert!(!cache.path("functions", "middle").exists());
    assert!(cache.path("functions", "newest").exists());
    assert!(!home.join("cache").join("cleanup.lock").exists(), "the lock is released");

    // Emptying this build's folders leaves them, since a command may be writing into one, while
    // another build's emptied folder goes.
    cache.cleanup(0).unwrap();
    assert!(!cache.path("functions", "newest").exists());
    assert!(home.join("cache").join(FOLDER).join("functions").is_dir());
    assert!(!home.join("cache").join("0.0.0-other").exists());
    std::fs::remove_dir_all(&home).unwrap();
}

/// The cap is for the whole cache: many builds' folders, each well under it, are cut down
/// together, the least recently used entries first whichever folder they're in.
#[test]
fn the_cap_is_for_every_build_folder_together() {
    let home = home("builds").unwrap();
    let cache = Cache::new(&home);
    let build = |at: usize| home.join("cache").join(format!("{}-{at:064}", fennec_build::VERSION));
    // Five other builds and this one, 200 bytes each, each used a minute later than the last.
    let old = fennec_build::KEPT_RECENT.as_secs() + 600;
    for at in 0..5_u64 {
        entry(&build(usize::try_from(at).unwrap()).join("llvm").join("f"), 200, old - at * 60).unwrap();
    }
    entry(&cache.path("llvm", "f"), 200, 1).unwrap();
    assert_eq!(cache.size(), 1_200);

    // A cap of 1,000 for all of them: down to 800, so the two least recently used go.
    let cleanup = cache.cleanup(1_000).unwrap();
    assert_eq!(cleanup, Cleanup { removed: 2, freed: 400, size: 800, skipped: false });
    assert!(!build(0).exists() && !build(1).exists(), "the two used longest ago went, folders and all");
    assert!((2..5).all(|at| build(at).join("llvm").join("f").exists()));
    assert!(cache.path("llvm", "f").exists());
    std::fs::remove_dir_all(&home).unwrap();
}

/// However far over its cap the cache is, an entry stays while it was stored or used in the last
/// [`fennec_build::KEPT_RECENT`], or while a run has its program put out: a command may be about
/// to read it, or to start it. A program put out is marked as used then, read-only as it is, so a
/// sweep of what earlier runs built, which goes by that time, never takes one about to start.
#[test]
fn an_entry_just_stored_used_or_put_out_is_never_removed() {
    let home = home("held").unwrap();
    let cache = Cache::new(&home);
    let old = fennec_build::KEPT_RECENT.as_secs() + 600;
    entry(&cache.path("llvm", "just stored"), 100, 5).unwrap();
    entry(&cache.path("llvm", "used"), 100, old).unwrap();
    assert!(cache.read("llvm", "used").is_some());
    entry(&cache.path("llvm", "old"), 100, old).unwrap();
    let program = home.join("launched");
    std::fs::write(&program, b"a program").unwrap();
    cache.keep_file("programs", "root", &program).unwrap();
    entry(&cache.path("llvm", "spare"), 0, 0).unwrap();
    let kept = cache.path("programs", "root");
    // Kept long ago, read-only, then put out for a run now.
    File::open(&kept).unwrap().set_modified(SystemTime::now() - Duration::from_secs(old)).unwrap();
    let run = home.join("work").join("fennec-run-1");
    std::fs::create_dir_all(run.parent().unwrap()).unwrap();
    assert!(cache.link_out("programs", "root", &run).unwrap());
    assert!(
        modified(&run).unwrap().elapsed().unwrap() < Duration::from_secs(60),
        "put out, it's marked used"
    );
    std::fs::remove_file(&program).unwrap();
    let cleanup = cache.cleanup(0).unwrap();
    assert_eq!(cleanup.removed, 1, "{cleanup:?}");
    assert!(!cache.path("llvm", "old").exists());
    for name in ["just stored", "used"] {
        assert!(cache.path("llvm", name).exists(), "{name}");
    }
    assert!(kept.exists() && run.exists());
    // Long unused, it still stays while the run has it put out, and goes once it doesn't.
    let long_ago = SystemTime::now() - Duration::from_secs(old);
    File::open(&kept).unwrap().set_modified(long_ago).unwrap();
    cache.cleanup(0).unwrap();
    assert!(kept.exists(), "a program a run has put out stays");
    std::fs::remove_file(&run).unwrap();
    cache.cleanup(0).unwrap();
    assert!(!kept.exists());
    std::fs::remove_dir_all(&home).unwrap();
}

/// Another build's folder that's gone unused for a day goes whole, even with the cache under its
/// cap; this build's folder stays however long it's been, and so does a build used in the day.
#[test]
fn a_build_folder_unused_for_a_day_goes_whole() {
    let home = home("unused").unwrap();
    let cache = Cache::new(&home);
    let day = fennec_build::UNUSED_BUILD.as_secs();
    let gone = home.join("cache").join("0.0.1");
    entry(&gone.join("llvm").join("a"), 100, day + 60).unwrap();
    entry(&gone.join("programs").join("b"), 100, day + 3_600).unwrap();
    let used = home.join("cache").join("0.0.1-used");
    // One entry used in the day keeps its whole folder.
    entry(&used.join("llvm").join("a"), 100, day + 3_600).unwrap();
    entry(&used.join("llvm").join("b"), 100, 60).unwrap();
    entry(&cache.path("llvm", "a"), 100, day * 7).unwrap();

    let cleanup = cache.cleanup(1_000_000).unwrap();
    assert_eq!(cleanup, Cleanup { removed: 2, freed: 200, size: 300, skipped: false });
    assert!(!gone.exists());
    assert!(used.join("llvm").join("a").exists() && used.join("llvm").join("b").exists());
    assert!(cache.path("llvm", "a").exists());
    std::fs::remove_dir_all(&home).unwrap();
}

/// A command starts a cleanup once none has run for an hour, and only one command of those that
/// follow does. A new cache only gets marked.
#[test]
fn a_cleanup_is_due_once_an_hour() {
    let home = home("due").unwrap();
    let cache = Cache::new(&home);
    assert!(!cache.cleanup_due(), "a new cache has nothing to clean");
    assert!(!cache.cleanup_due());
    let mark = home.join("cache").join("cleaned");
    entry(&mark, 0, fennec_build::CLEANUP_EVERY.as_secs() + 60).unwrap();
    assert!(cache.cleanup_due());
    assert!(!cache.cleanup_due(), "the one that said yes marked it");
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_cleanup_already_running_is_left_to_finish() {
    let home = home("locked").unwrap();
    let cache = Cache::new(&home);
    entry(&cache.path("functions", "a"), 500, 3_600).unwrap();
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

/// What a command stopped before it tidied up leaves in `work` goes once it's old, and what a
/// command working now has there stays.
#[test]
fn old_scratch_work_is_swept_and_current_work_stays() -> std::io::Result<()> {
    let home = home("sweep")?;
    entry(&home.join("work/fennec-llvm-1-a-0/f0.ll"), 10, 0)?;
    let left = home.join("work/fennec-llvm-2-b-0");
    entry(&left.join("f0.ll"), 10, 7200)?;
    age_folder(&left, 7200)?;
    entry(&home.join("work/fennec-run-3-c-0"), 10, 7200)?;
    assert_eq!(fennec_build::sweep_work(&home, fennec_build::WORK_KEPT), 2);
    assert!(home.join("work/fennec-llvm-1-a-0/f0.ll").is_file(), "current work stays");
    assert!(!left.exists() && !home.join("work/fennec-run-3-c-0").exists());
    assert_eq!(fennec_build::sweep_work(&home.join("nowhere"), Duration::ZERO), 0);
    std::fs::remove_dir_all(&home)
}
