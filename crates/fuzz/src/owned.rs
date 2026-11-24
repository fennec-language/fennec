//! Folders a fuzzing process makes for itself in the temporary folder, and how they're removed
//! once it's ended. Each process holds a lock on a file in its folder for as long as it lives, and
//! the operating system lets go of the lock when the process ends, however it ends. A folder is
//! only removed by someone who can take that lock, so a folder whose process is still running is
//! never removed, whenever it was made and whatever its process id. Asking which processes are
//! running, by their ids, isn't enough: a process that starts just after it was asked isn't on the
//! list, and an id can be taken again by a new process.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// The file in each folder its process holds a lock on.
const HELD: &str = ".held";

/// How old a folder with no file to lock has to be before it's removed: one its process was
/// stopped in before it took the lock, or one made before there were locks.
const UNHELD_FOR: Duration = Duration::from_hours(24);

/// The folders this process owns, each with the file it holds the lock on.
static OWNED: Mutex<Option<HashMap<PathBuf, File>>> = Mutex::new(None);

/// Makes `folder`, where it isn't there, and holds it as this process's own until the process ends
/// or lets go of it.
pub(crate) fn own(folder: &Path) -> io::Result<()> {
    let mut owned = OWNED.lock().unwrap_or_else(PoisonError::into_inner);
    let owned = owned.get_or_insert_with(HashMap::new);
    if owned.contains_key(folder) {
        return Ok(());
    }
    loop {
        match std::fs::create_dir_all(folder) {
            Err(err) if !swept(&err, folder) => return Err(err),
            Err(_) => continue,
            Ok(()) => {}
        }
        let held = folder.join(HELD);
        let file = match OpenOptions::new().create(true).truncate(false).write(true).open(&held) {
            Ok(file) => file,
            Err(err) if swept(&err, folder) => continue,
            Err(err) => return Err(err),
        };
        file.lock()?;
        // Someone removing the folder holds the lock while they remove it, so once it's ours, the
        // file is either still there and ours, or it was removed with the folder while this waited,
        // and the folder is made again.
        if still_at(&file, &held) {
            owned.insert(folder.to_owned(), file);
            return Ok(());
        }
    }
}

/// Whether `err`, from making `folder` or its file, came of a sweep removing the folder meanwhile,
/// so making it again is the answer: the folder's gone, and the error is one that a removal under
/// way gives. That's `NotFound` for the file, `AlreadyExists` from `create_dir_all` when the
/// folder it found is gone before it looks again, and, on macOS, `InvalidInput` for a file
/// created in a folder being removed, which Linux says is `NotFound`.
fn swept(err: &io::Error, folder: &Path) -> bool {
    let kind = err.kind();
    matches!(kind, io::ErrorKind::NotFound | io::ErrorKind::AlreadyExists | io::ErrorKind::InvalidInput)
        && !folder.is_dir()
}

/// Lets go of a folder this process owns, and removes it.
pub(crate) fn let_go(folder: &Path) {
    let mut owned = OWNED.lock().unwrap_or_else(PoisonError::into_inner);
    let file = owned.as_mut().and_then(|owned| owned.remove(folder));
    let _ = std::fs::remove_dir_all(folder);
    drop(file);
}

/// Removes `folder` when no running process holds it: once its process has ended, or when it has
/// no file to lock and is a day old.
pub(crate) fn remove_ended(folder: &Path) {
    let held = folder.join(HELD);
    match File::open(&held) {
        Ok(file) => match file.try_lock() {
            // A file removed since it was opened, by another sweep, isn't the folder's any more:
            // its process may already hold the one made in its place.
            Ok(()) if still_at(&file, &held) => {
                // The lock is held until the folder is gone, so a process making it again waits.
                let _ = std::fs::remove_dir_all(folder);
                drop(file);
            }
            Ok(()) | Err(TryLockError::WouldBlock | TryLockError::Error(_)) => {}
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let changed = std::fs::metadata(folder).and_then(|meta| meta.modified()).ok();
            if changed.and_then(|changed| changed.elapsed().ok()).is_some_and(|age| age > UNHELD_FOR) {
                let _ = std::fs::remove_dir_all(folder);
            }
        }
        Err(_) => {}
    }
}

/// Whether `file` is still the file at `path`, and not one removed since it was opened.
fn still_at(file: &File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), std::fs::metadata(path)) {
            (Ok(open), Ok(there)) => open.dev() == there.dev() && open.ino() == there.ino(),
            _ => false,
        }
    }
    // An open file can't be removed on Windows, so what's at the path is what was opened.
    #[cfg(not(unix))]
    {
        let _ = file;
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{HELD, let_go, own, remove_ended};

    /// A folder of its own for a test, in the temporary folder, not named as a fuzzing process's.
    fn test_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("fennec-owned-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        folder
    }

    fn held(folder: &Path) -> bool {
        folder.join(HELD).exists()
    }

    /// A folder is kept while its owner holds it and removed once it's let go; one with a file to
    /// lock that nobody holds is removed; a new one with no file to lock is kept.
    #[test]
    fn a_folder_is_removed_only_once_nobody_holds_it() {
        let folder = test_folder("owned");
        assert!(own(&folder).is_ok());
        assert!(own(&folder).is_ok(), "owning it again is owning it still");
        remove_ended(&folder);
        assert!(held(&folder));
        let_go(&folder);
        assert!(!folder.exists());

        // An owner that ended: its file is there, and nobody holds the lock.
        let ended = test_folder("ended");
        assert!(std::fs::create_dir_all(&ended).is_ok());
        assert!(File::create(ended.join(HELD)).is_ok());
        remove_ended(&ended);
        assert!(!ended.exists());

        let unheld = test_folder("unheld");
        assert!(std::fs::create_dir_all(&unheld).is_ok());
        remove_ended(&unheld);
        assert!(unheld.exists());
        let _ = std::fs::remove_dir_all(&unheld);
    }

    /// A process that makes its folder while a sweep holds the lock on it, to remove it, waits for
    /// the sweep, finds its folder gone and makes it again, and the next sweep keeps it. The owner
    /// may make the folder or open its file at any moment of the removal, which on macOS can fail
    /// with `InvalidInput`, so it's done 50 times.
    #[test]
    fn a_folder_made_while_a_sweep_removes_it_is_made_again_and_kept() {
        let folder = test_folder("swept");
        for round in 0..50 {
            assert!(std::fs::create_dir_all(&folder).is_ok());
            let sweeping = File::create(folder.join(HELD));
            let Ok(sweeping) = sweeping else { unreachable!("{sweeping:?}") };
            assert!(sweeping.try_lock().is_ok());
            let made = std::thread::scope(|scope| {
                let owner = scope.spawn(|| own(&folder));
                let _ = std::fs::remove_dir_all(&folder);
                drop(sweeping);
                owner.join().unwrap_or_else(|_| Err(std::io::Error::other("the owner panicked")))
            });
            assert!(made.is_ok(), "round {round}: {made:?}");
            assert!(held(&folder), "round {round}");
            remove_ended(&folder);
            assert!(held(&folder), "round {round}");
            let_go(&folder);
        }
    }

    /// Processes making their folders while sweeps run over and over beside them keep every one.
    #[test]
    fn folders_made_during_sweeps_are_all_kept() {
        let parent = test_folder("sweeps");
        assert!(std::fs::create_dir_all(&parent).is_ok());
        let folders: Vec<PathBuf> = (0..64).map(|at| parent.join(format!("owner-{at}"))).collect();
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    for entry in std::fs::read_dir(&parent).into_iter().flatten().flatten() {
                        remove_ended(&entry.path());
                    }
                }
            });
            let owners: Vec<_> = folders
                .chunks(16)
                .map(|some| scope.spawn(|| some.iter().all(|folder| own(folder).is_ok())))
                .collect();
            let owned = owners.into_iter().all(|owner| owner.join().unwrap_or(false));
            done.store(true, Ordering::Relaxed);
            assert!(owned);
        });
        for folder in &folders {
            remove_ended(folder);
            assert!(held(folder), "{} was removed while it was held", folder.display());
            let_go(folder);
        }
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// The folder `holds_a_folder_for_its_parent` holds for the test process `parent`.
    #[cfg(unix)]
    fn childs_folder(parent: u32) -> PathBuf {
        std::env::temp_dir().join(format!("fennec-owned-test-child-of-{parent}"))
    }

    /// Run by `a_process_started_mid_sweep_keeps_its_folder` as a process of its own: it holds its
    /// folder until the test leaves a file named `go` in it, or for a minute when it's run by hand.
    #[cfg(unix)]
    #[test]
    #[ignore = "run by a_process_started_mid_sweep_keeps_its_folder"]
    fn holds_a_folder_for_its_parent() {
        let folder = childs_folder(std::os::unix::process::parent_id());
        assert!(own(&folder).is_ok());
        println!("held");
        let started = std::time::Instant::now();
        while !folder.join("go").exists() && started.elapsed() < std::time::Duration::from_secs(60) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// A process that starts while sweeps run over and over, and makes its folder in the middle of
    /// them, keeps it for as long as it runs; once it's ended, the next sweep removes it.
    #[cfg(unix)]
    #[test]
    fn a_process_started_mid_sweep_keeps_its_folder() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};

        let folder = childs_folder(std::process::id());
        let _ = std::fs::remove_dir_all(&folder);
        let done = AtomicBool::new(false);
        let (mut child, mut said) = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    remove_ended(&folder);
                }
            });
            let child = Command::new(std::env::current_exe().unwrap_or_default())
                .args(["--ignored", "--exact", "owned::tests::holds_a_folder_for_its_parent", "--nocapture"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else { unreachable!("{child:?}") };
            let Some(mut said) = child.stdout.take().map(std::io::BufReader::new) else {
                unreachable!("the child has no output")
            };
            let held = (&mut said).lines().map_while(Result::ok).any(|line| line == "held");
            done.store(true, Ordering::Relaxed);
            assert!(held, "the child never held its folder");
            (child, said)
        });
        remove_ended(&folder);
        assert!(held(&folder), "a sweep removed the folder of a process still running");
        assert!(File::create(folder.join("go")).is_ok());
        // What it says after, read to its end so it isn't stopped writing to a closed pipe.
        let _ = std::io::copy(&mut said, &mut std::io::sink());
        let ended = child.wait();
        assert!(ended.as_ref().is_ok_and(std::process::ExitStatus::success), "{ended:?}");
        remove_ended(&folder);
        assert!(!folder.exists(), "the folder of an ended process was kept");
    }
}
