//! Where snapshots are kept: `~/.fennec/cache/runs/`, one per program, named by a hash of the
//! program's absolute path. A snapshot records the last run of that program; it isn't a
//! cached result, so naming it by a path is fine. Nothing is written inside the project.

use std::path::{Path, PathBuf};

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

/// The file that holds `program`'s last snapshot.
fn snapshot_path(home: &Path, program: &Path) -> PathBuf {
    let absolute = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    let name = fennec_blake3::hash(absolute.to_string_lossy().as_bytes());
    home.join("cache").join("runs").join(format!("{name}.txt"))
}

/// `program`'s last snapshot, or an empty one if there's none or it can't be read.
pub fn load(home: &Path, program: &Path) -> Snapshot {
    std::fs::read_to_string(snapshot_path(home, program))
        .ok()
        .and_then(|text| text.strip_prefix(HEADER)?.strip_prefix('\n').and_then(Snapshot::from_text))
        .unwrap_or_default()
}

/// Stores `program`'s snapshot. It's written to a temporary file and renamed into place, so
/// a reader never sees half of one.
pub fn save(home: &Path, program: &Path, snapshot: &Snapshot) -> std::io::Result<()> {
    let path = snapshot_path(home, program);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, format!("{HEADER}\n{}", snapshot.to_text()))?;
    std::fs::rename(&temporary, &path)
}
