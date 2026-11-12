//! Which files around a folder Fennec may take as a kit's, and how it writes a kit's own files.
//!
//! A kit is the nearest folder above with a `kit.toml`. Taken whoever owns it, another user could
//! plant `/tmp/kit.toml`, and anyone running Fennec in a folder under `/tmp` with no `kit.toml` of
//! its own, as agents' scratch folders are, would run the toolchain and dependencies it names. So
//! the walk up goes only through folders its user or the system owns, which nobody else can write
//! to, and stops at the first that isn't one: `/tmp`, which everyone can write to, is never passed.
//!
//! `kit.toml` and `kit.lock` are written whole, through a temporary file renamed into place, and
//! never through a link: a cloned repository could ship `kit.lock` as a link to a file of the
//! user's, and the first `fennec run` would write into it.

use std::path::{Path, PathBuf};

/// The kit around `from`: the nearest folder above it, or it, with a `kit.toml`, reached only
/// through folders that can be trusted. None when there's none.
pub fn kit_root(from: &Path) -> Option<PathBuf> {
    let from = std::fs::canonicalize(from).ok()?;
    for folder in from.ancestors() {
        if !trusted(folder) {
            return None;
        }
        let manifest = folder.join("kit.toml");
        if std::fs::symlink_metadata(&manifest).is_ok_and(|found| found.is_file()) && trusted(&manifest) {
            return Some(folder.to_path_buf());
        }
    }
    None
}

/// Whether a folder or file can be trusted: its user's or the system's, and nobody else can write
/// to it. Where there are no owners to tell, as on Windows, everything is.
pub fn trusted(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(found) = std::fs::symlink_metadata(path) else { return false };
        let owner_ok = found.uid() == 0 || Some(found.uid()) == own_uid();
        owner_ok && found.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// Writes a kit's file whole: to a temporary file beside it, made new, then renamed into place. A
/// file that's a link is refused, not written through.
///
/// # Errors
///
/// When it's a link, or it can't be written.
pub fn write_kit_file(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;
    let shown = path.display();
    if std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_symlink()) {
        return Err(format!("{shown} is a link, so it isn't written: a kit's own files are plain files"));
    }
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let temporary = path.with_file_name(format!(".{name}.{}-{nanos:x}.tmp", std::process::id()));
    let written = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(text.as_bytes()).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, path));
    written.map_err(|err| {
        let _ = std::fs::remove_file(&temporary);
        format!("couldn't write {shown}: {err}")
    })
}

/// The user this process runs as: the owner of a file it makes, since the standard library has no
/// call that says so. Found once.
#[cfg(unix)]
pub(crate) fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    static OWN: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *OWN.get_or_init(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        let probe = std::env::temp_dir().join(format!(".fennec-who-{}-{nanos:x}", std::process::id()));
        let made = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).ok()?;
        let owner = made.metadata().ok().map(|metadata| metadata.uid());
        drop(made);
        let _ = std::fs::remove_file(&probe);
        owner
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::{kit_root, write_kit_file};

    /// A `kit.toml` in a folder others can write to, or above one, isn't taken as the kit: anyone
    /// could have put it there. One in the user's own folders is, however deep below it a command
    /// runs.
    #[test]
    fn a_kit_toml_above_in_a_folder_others_can_write_isnt_the_kit() {
        let base = fennec_temp::Temp::folder("kit-trust").unwrap();
        let shared = base.join("shared");
        let inner = shared.join("mine/deeper");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(shared.join("kit.toml"), "fennec = \"0.0.1\"\n").unwrap();
        assert_eq!(kit_root(&inner), Some(std::fs::canonicalize(&shared).unwrap()));
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(kit_root(&inner), None, "a kit.toml where anyone could put one was taken");
        // A kit of the user's own below it is still a kit.
        std::fs::write(shared.join("mine/kit.toml"), "fennec = \"0.0.1\"\n").unwrap();
        assert_eq!(kit_root(&inner), Some(std::fs::canonicalize(shared.join("mine")).unwrap()));
        // And a kit.toml others can write isn't taken either.
        std::fs::set_permissions(shared.join("mine/kit.toml"), std::fs::Permissions::from_mode(0o666))
            .unwrap();
        assert_eq!(kit_root(&inner), None);
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A kit's file that's a link isn't written through: the file it leads to stays as it was.
    /// One that's a plain file is written whole.
    #[test]
    fn a_lock_that_is_a_link_is_refused() {
        let base = fennec_temp::Temp::folder("kit-lock-link").unwrap();
        let theirs = base.join("dotfile");
        std::fs::write(&theirs, "the user's own\n").unwrap();
        let lock = base.join("kit.lock");
        std::os::unix::fs::symlink(&theirs, &lock).unwrap();
        let refused = write_kit_file(&lock, "fennec = \"0.0.1\"\n").unwrap_err();
        assert!(refused.contains("is a link"), "{refused}");
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "the user's own\n");
        std::fs::remove_file(&lock).unwrap();
        write_kit_file(&lock, "fennec = \"0.0.2\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "fennec = \"0.0.2\"\n");
        let left: Vec<_> =
            std::fs::read_dir(&*base).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(left.len(), 2, "a temporary file was left: {left:?}");
    }
}
