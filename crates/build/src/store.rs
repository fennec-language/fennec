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

/// How long scratch work may stay in `work` before it's taken for what a command that was
/// stopped left behind. A command tidies its own as soon as it's done.
pub const WORK_KEPT: std::time::Duration = std::time::Duration::from_secs(3600);

/// Removes what's in `home`'s `work` folder that hasn't changed for `older_than`: what commands
/// that were stopped before they tidied up left there. Gives back how many entries it removed.
pub fn sweep_work(home: &Path, older_than: std::time::Duration) -> usize {
    let Ok(listing) = std::fs::read_dir(home.join("work")) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in listing.flatten() {
        let path = entry.path();
        let changed = entry.metadata().and_then(|metadata| metadata.modified()).ok();
        let old = changed.and_then(|at| now.duration_since(at).ok()).is_some_and(|age| age > older_than);
        if !old {
            continue;
        }
        let gone = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        removed += usize::from(gone.is_ok());
    }
    removed
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
