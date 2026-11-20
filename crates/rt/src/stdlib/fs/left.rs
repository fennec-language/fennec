//! Files runs left beside their targets: what a run was writing when it was killed, which nothing
//! put in its place or threw away. The first time a run writes in a folder, it clears the folder
//! of those whose run is gone (#2238), so they don't pile up. Once a folder, not once a write, so a
//! program saving many files in one folder reads its names once.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

/// The names of the files this run is writing beside their targets, from just before each is
/// made until it's put in its place or thrown away. A name holds the run's process number and a
/// count of the run's own, so it's no other run's.
static HELD: LazyLock<Mutex<HashSet<OsString>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The folders this run has cleared, by their paths.
static CLEARED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Holds `name` as this run's before its file is made, so a clearing never takes it for one left.
pub(super) fn hold(name: &OsStr) {
    HELD.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_owned());
}

/// Lets go of `name`, once its file is put in its place or thrown away, or wasn't made.
pub(super) fn let_go(name: &OsStr) {
    HELD.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
}

/// Whether this is the first time this run writes in `folder`, counting it as written in.
fn first_time(folder: PathBuf) -> bool {
    CLEARED.lock().unwrap_or_else(PoisonError::into_inner).insert(folder)
}

/// The process number of the run that made a file written beside its target, from its name,
/// `.NAME.PROCESS.COUNT.fennec-partial`, or `None` for a name of another kind.
fn maker(name: &OsStr) -> Option<u32> {
    let rest = name.to_str()?.strip_prefix('.')?.strip_suffix(".fennec-partial")?;
    let mut parts = rest.rsplitn(3, '.');
    parts.next()?.parse::<u64>().ok()?;
    let process = parts.next()?.parse::<u32>().ok()?;
    parts.next().filter(|target| !target.is_empty())?;
    Some(process)
}

/// Whether the file named `name` was left by a run that's gone. One this run holds isn't.
///
/// On Unix, its maker's process number is looked up. One that's this run's but isn't held was
/// left by an earlier run that had the same number, as a program run again in a container does.
/// A number another live process has is taken as its maker's, so a file is never cleared while
/// it may be written, and is cleared by a later run.
fn left_by_a_gone_run(name: &OsStr, opens: impl FnOnce() -> bool) -> bool {
    let Some(made_by) = maker(name) else { return false };
    if HELD.lock().unwrap_or_else(PoisonError::into_inner).contains(name) {
        return false;
    }
    gone(made_by, opens)
}

#[cfg(unix)]
fn gone(made_by: u32, _opens: impl FnOnce() -> bool) -> bool {
    if made_by == std::process::id() {
        return true;
    }
    let Some(process) = i32::try_from(made_by).ok().and_then(rustix::process::Pid::from_raw) else {
        return false;
    };
    rustix::process::test_kill_process(process) == Err(rustix::io::Errno::SRCH)
}

/// On Windows, a file being written is opened without letting another open it to read
/// (`new_beside`), so one that opens to read has no writer left, whatever its process number.
#[cfg(windows)]
fn gone(_made_by: u32, opens: impl FnOnce() -> bool) -> bool {
    opens()
}

/// Elsewhere there's no telling, so nothing is cleared.
#[cfg(not(any(unix, windows)))]
fn gone(_made_by: u32, _opens: impl FnOnce() -> bool) -> bool {
    false
}

/// The first time this run writes in `folder`, clears it of what gone runs left beside their
/// targets. A clearing that can't be done is left for a later run: it's no failure of the write.
pub(super) fn clear(folder: &Path) {
    if !first_time(folder.to_path_buf()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_file())
            && left_by_a_gone_run(&name, || std::fs::File::open(&path).is_ok())
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// [`clear`], for the folder `here`, opened beneath an `Fs` narrowed with `only`, which is at
/// `folder`.
pub(super) fn clear_beneath(here: &cap_std::fs::Dir, folder: PathBuf) {
    if !first_time(folder) {
        return;
    }
    let Ok(entries) = here.entries() else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if entry.file_type().is_ok_and(|kind| kind.is_file())
            && left_by_a_gone_run(&name, || here.open(&name).is_ok())
        {
            let _ = here.remove_file(&name);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};

    use super::{hold, left_by_a_gone_run, let_go, maker};

    /// A file this run holds is never taken for one a gone run left, though its process number is
    /// this run's: another task of this run is writing it. Let go of, it's one left.
    #[test]
    fn a_file_this_run_holds_is_never_taken_for_one_left() {
        let name = OsString::from(format!(".x.{}.424242.fennec-partial", std::process::id()));
        assert!(left_by_a_gone_run(&name, || true));
        hold(&name);
        assert!(!left_by_a_gone_run(&name, || true));
        let_go(&name);
        assert!(left_by_a_gone_run(&name, || true));
    }

    /// What one thread holds, every other thread of the run sees held: a task on one worker never
    /// clears a file a task on another is writing.
    #[test]
    fn a_file_another_thread_holds_is_never_taken_for_one_left() {
        let name = OsString::from(format!(".y.{}.434343.fennec-partial", std::process::id()));
        hold(&name);
        let seen = std::thread::scope(|scope| scope.spawn(|| left_by_a_gone_run(&name, || true)).join().ok());
        let_go(&name);
        assert_eq!(seen, Some(false));
        let freed =
            std::thread::scope(|scope| scope.spawn(|| left_by_a_gone_run(&name, || true)).join().ok());
        assert_eq!(freed, Some(true));
    }

    #[test]
    fn a_files_maker_is_read_from_its_name() {
        assert_eq!(maker(OsStr::new(".save.txt.4242.7.fennec-partial")), Some(4242));
        assert_eq!(maker(OsStr::new(".a.b.c.1.2.fennec-partial")), Some(1));
        for other in [
            "save.txt",
            ".save.txt.fennec-partial",
            ".save.txt.x.7.fennec-partial",
            ".save.txt.4242.x.fennec-partial",
            "..4242.7.fennec-partial",
            "save.txt.4242.7.fennec-partial",
        ] {
            assert_eq!(maker(OsStr::new(other)), None, "{other}");
        }
    }
}
