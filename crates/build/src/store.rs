//! Where snapshots are kept: in the cache, `~/.fennec/cache/<version>/runs/`, one per program,
//! named by a hash of the program's absolute path. A snapshot records the last run of that program; it isn't a
//! cached result, so naming it by a path is fine. Nothing is written inside the project.

use std::path::{Path, PathBuf};

use crate::cache::Cache;
use crate::snapshot::Snapshot;

/// The first line of a stored snapshot. A snapshot from another version of Fennec is
/// ignored, since the way trees are hashed may have changed.
const HEADER: &str = concat!("fennec snapshot ", env!("CARGO_PKG_VERSION"));

/// Fennec's folder: `$FENNEC_HOME`, or `.fennec` in the home folder.
pub fn fennec_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("FENNEC_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".fennec"))
}

/// Where a command does its scratch work, like a build's object files before they're linked:
/// `work` in Fennec's folder, or the system's temporary folder when there's no Fennec folder to
/// write in. See `docs/store.md`.
pub fn work_folder() -> PathBuf {
    match fennec_home().map(|home| home.join("work")) {
        Some(folder) if std::fs::create_dir_all(&folder).is_ok() => folder,
        _ => std::env::temp_dir(),
    }
}

/// How long scratch work no running command owns may stay in `work` before it's taken for what a
/// command that was stopped left behind. A command tidies its own as soon as it's done.
pub const WORK_KEPT: std::time::Duration = std::time::Duration::from_secs(3600);

/// Removes what's in `home`'s `work` folder that no running command owns: what commands that were
/// stopped before they tidied up left there. Gives back how many entries it removed.
///
/// An entry a command named for itself (`fennec-<what>-<id>-<time>-<count>`, see the `fennec`
/// crate's `own_temp`) is that command's while it runs, however long its folder has gone without
/// changing: a tool writing into a file it made doesn't change its folder, and the machine may
/// sleep through a build. Once the command is gone, its entry goes. A process running under the
/// same id isn't its owner if it started after the entry was named, as one given the id again
/// after the owner went did. Any other entry, and every entry where a process can't be asked
/// after, goes once it hasn't changed for `older_than`.
pub fn sweep_work(home: &Path, older_than: std::time::Duration) -> usize {
    let Ok(listing) = std::fs::read_dir(home.join("work")) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut left = Vec::new();
    // Old entries whose id is running, with when each was named: their owner's start is asked.
    let mut asked = Vec::new();
    for entry in listing.flatten() {
        let path = entry.path();
        let changed = entry.metadata().and_then(|metadata| metadata.modified()).ok();
        let old = changed.and_then(|at| now.duration_since(at).ok()).is_some_and(|age| age > older_than);
        match owner(&path).map(|(id, named)| (id, named, running(id))) {
            Some((_, _, Some(false))) => left.push(path),
            Some((id, named, Some(true))) => {
                if old {
                    asked.push((path, id, named));
                }
            }
            _ => {
                if old {
                    left.push(path);
                }
            }
        }
    }
    if !asked.is_empty() {
        let started = started_at(&asked.iter().map(|(_, id, _)| *id).collect::<Vec<_>>(), now);
        // A second's slack, as `ps` gives a start to the second.
        let slack = std::time::Duration::from_secs(1);
        for (path, id, named) in asked {
            if started.get(&id).is_some_and(|started| *started > named + slack) {
                left.push(path);
            }
        }
    }
    left.into_iter()
        .filter(|path| {
            if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }.is_ok()
        })
        .count()
}

/// The process id and the time in the name of a `work` entry a command named for itself: `None`
/// for an entry named otherwise.
fn owner(path: &Path) -> Option<(i32, std::time::SystemTime)> {
    let name = path.file_stem()?.to_str()?.strip_prefix("fennec-")?;
    // What it's for may hold dashes, as `llvm-again` does: the id is the third field from the end.
    let mut fields = name.rsplitn(4, '-');
    let (_count, time, id, _what) = (fields.next()?, fields.next()?, fields.next()?, fields.next()?);
    let nanos = u64::try_from(u128::from_str_radix(time, 16).ok()?).ok()?;
    Some((id.parse().ok()?, std::time::UNIX_EPOCH + std::time::Duration::from_nanos(nanos)))
}

/// Whether the process `id` is running: a signal of none sent to it finds it, or finds it isn't
/// this user's, which is running too. `None` where a process can't be asked after.
#[cfg(unix)]
fn running(id: i32) -> Option<bool> {
    let pid = rustix::process::Pid::from_raw(id)?;
    Some(match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(err) => err != rustix::io::Errno::SRCH,
    })
}

#[cfg(not(unix))]
fn running(_: i32) -> Option<bool> {
    None
}

/// When each of the processes `ids` started, as `ps` gives how long each has run: one that's gone
/// by the time it's asked isn't in it.
fn started_at(
    ids: &[i32],
    now: std::time::SystemTime,
) -> std::collections::HashMap<i32, std::time::SystemTime> {
    let list: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let Ok(output) =
        std::process::Command::new("ps").args(["-o", "pid=,etime=", "-p", &list.join(",")]).output()
    else {
        return std::collections::HashMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let id = fields.next()?.parse().ok()?;
            Some((id, now.checked_sub(elapsed(fields.next()?)?)?))
        })
        .collect()
}

/// A time `ps` gives as `[[days-]hours:]minutes:seconds`.
fn elapsed(written: &str) -> Option<std::time::Duration> {
    let (days, clock) = match written.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, written),
    };
    let mut seconds = 0_u64;
    for part in clock.split(':') {
        seconds = seconds.checked_mul(60)?.checked_add(part.parse().ok()?)?;
    }
    Some(std::time::Duration::from_secs(days.checked_mul(86_400)?.checked_add(seconds)?))
}

/// The name of the entry that holds `program`'s last snapshot.
fn snapshot_name(program: &Path) -> String {
    let absolute = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    format!("{}.txt", fennec_blake3::hash(absolute.to_string_lossy().as_bytes()))
}

/// `program`'s last snapshot, or an empty one if there's none or it can't be read.
pub fn load(home: &Path, program: &Path) -> Snapshot {
    Cache::new(home)
        .read("runs", &snapshot_name(program))
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.strip_prefix(HEADER)?.strip_prefix('\n').and_then(Snapshot::from_text))
        .unwrap_or_default()
}

/// Stores `program`'s snapshot.
pub fn save(home: &Path, program: &Path, snapshot: &Snapshot) -> std::io::Result<()> {
    let text = format!("{HEADER}\n{}", snapshot.to_text());
    Cache::new(home).write("runs", &snapshot_name(program), text.as_bytes())
}
