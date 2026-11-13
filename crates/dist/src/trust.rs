//! Which files around a folder Fennec may take as a kit's, and how it writes a kit's own files.
//!
//! A kit is the nearest folder above with a `kit.toml`. Taken whoever owns it, another user could
//! plant `/tmp/kit.toml`, and anyone running Fennec in a folder under `/tmp` with no `kit.toml` of
//! its own, as agents' scratch folders are, would run the toolchain and dependencies it names. So
//! a kit is taken only through folders that are trusted, as git takes a repository:
//!
//! - one this user owns, whatever its group may do, as a umask of 002 makes every folder, since a
//!   group of the user's own lets nobody else in, but not one anyone may write to;
//! - one the system owns that neither its group nor anyone else may write to;
//! - one below a folder named in `FENNEC_TRUST`, whoever owns it, as a project mounted into a
//!   container is the host user's, but not one anyone may write to.
//!
//! A `kit.toml` that's there but not trusted stops the command and says why, with what fixes it.
//! Going on as if there were no kit would run the default toolchain where the kit pins another.
//! The one left alone is another user's in a folder anyone may write to, as `/tmp/kit.toml`: that's
//! the one planted, and a command there runs as if it weren't.
//!
//! `kit.toml` and `kit.lock` are written whole, through a temporary file renamed into place, and
//! never through a link: a cloned repository could ship `kit.lock` as a link to a file of the
//! user's, and the first `fennec run` would write into it.

use std::path::{Path, PathBuf};

/// The kit around `from`: the nearest folder above it, or it, with a `kit.toml`, reached only
/// through folders that are trusted. None when there's none, or when the only one is planted.
///
/// # Errors
///
/// When there's a `kit.toml` above that isn't trusted, or is reached through a folder that isn't,
/// saying why and what fixes it.
pub fn kit_root(from: &Path) -> Result<Option<PathBuf>, String> {
    let Ok(from) = std::fs::canonicalize(from) else { return Ok(None) };
    // The first folder on the way up that isn't trusted, and why: any `kit.toml` above it isn't
    // taken, but is said.
    let mut in_the_way: Option<(PathBuf, String)> = None;
    for folder in from.ancestors() {
        let folder_is = judge(folder);
        let manifest = folder.join("kit.toml");
        let Ok(found) = std::fs::symlink_metadata(&manifest) else {
            if in_the_way.is_none()
                && let Err(why) = folder_is
            {
                in_the_way = Some((folder.to_path_buf(), why));
            }
            continue;
        };
        let shown = manifest.display();
        if planted(&manifest, folder) {
            return Ok(None);
        }
        if let Some((blocking, why)) = in_the_way {
            return Err(format!("{shown} isn't taken as the kit, as {why}: {}", fix(&blocking, &why)));
        }
        if let Err(why) = folder_is {
            return Err(format!("{shown} isn't taken as the kit, as {why}: {}", fix(folder, &why)));
        }
        if !found.is_file() {
            return Err(format!("{shown} isn't taken as the kit, as it's a link, not a plain file"));
        }
        if let Err(why) = judge(&manifest) {
            return Err(format!("{shown} isn't taken as the kit, as {why}: {}", fix(&manifest, &why)));
        }
        return Ok(Some(folder.to_path_buf()));
    }
    Ok(None)
}

/// Whether a folder or file can be trusted: see the top of this file. Where there are no owners
/// to tell, as on Windows, everything is.
pub fn trusted(path: &Path) -> bool {
    judge(path).is_ok()
}

/// Why a path isn't trusted, said as "… can be written by anyone".
fn judge(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let shown = path.display();
        let found = std::fs::symlink_metadata(path).map_err(|err| format!("{shown} can't be read: {err}"))?;
        if found.mode() & 0o002 != 0 {
            return Err(format!("{shown} can be written by anyone"));
        }
        if Some(found.uid()) == own_uid() || listed(path) {
            return Ok(());
        }
        if found.uid() != 0 {
            return Err(format!("{shown} is another user's"));
        }
        if found.mode() & 0o020 != 0 {
            return Err(format!("{shown} is the system's, and its group can write to it"));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Whether a `kit.toml` is one another user planted: theirs, in a folder anyone may write to.
fn planted(manifest: &Path, folder: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let theirs = std::fs::symlink_metadata(manifest)
            .is_ok_and(|found| found.uid() != 0 && Some(found.uid()) != own_uid() && !listed(manifest));
        let open = std::fs::symlink_metadata(folder).is_ok_and(|found| found.mode() & 0o002 != 0);
        theirs && open
    }
    #[cfg(not(unix))]
    {
        let _ = (manifest, folder);
        false
    }
}

/// What makes a path trusted, for the reason `judge` gave.
fn fix(path: &Path, why: &str) -> String {
    let shown = path.display();
    if why.ends_with("anyone") {
        format!("`chmod o-w {shown}` fixes it")
    } else if why.ends_with("can write to it") {
        format!("`chmod g-w {shown}` fixes it")
    } else {
        format!(
            "if it's yours to trust, as a project mounted into a container is, name it in FENNEC_TRUST, as `FENNEC_TRUST={shown}`"
        )
    }
}

/// Whether a path is at or below a folder named in `FENNEC_TRUST`, a list of folders as `PATH` is.
/// Only where there are owners to tell, as on Windows there aren't.
#[cfg(unix)]
fn listed(path: &Path) -> bool {
    std::env::var_os("FENNEC_TRUST").is_some_and(|named| listed_in(path, &named))
}

/// Whether a path is at or below one of the folders `named` lists.
#[cfg(unix)]
fn listed_in(path: &Path, named: &std::ffi::OsStr) -> bool {
    std::env::split_paths(named)
        .filter(|folder| !folder.as_os_str().is_empty())
        .filter_map(|folder| std::fs::canonicalize(folder).ok())
        .any(|folder| path.starts_with(folder))
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

    /// A `kit.toml` in a folder anyone can write to, or above one, isn't taken as the kit, and
    /// says so with the `chmod` that fixes it, rather than running as if there were no kit. One in
    /// the user's own folders is, however deep below it a command runs, and whatever the folders'
    /// group may do, as a umask of 002 makes them.
    #[test]
    fn a_kit_toml_above_in_a_folder_anyone_can_write_isnt_the_kit() {
        let base = fennec_temp::Temp::folder("kit-trust").unwrap();
        let shared = base.join("shared");
        let inner = shared.join("mine/deeper");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(shared.join("kit.toml"), "fennec = \"0.0.1\"\n").unwrap();
        let shared_real = std::fs::canonicalize(&shared).unwrap();
        assert_eq!(kit_root(&inner), Ok(Some(shared_real.clone())));
        // Group-writable, as a umask of 002 leaves every folder and file, is still the user's own.
        for (path, mode) in
            [(&shared, 0o775), (&shared.join("mine"), 0o775), (&shared.join("kit.toml"), 0o664)]
        {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(kit_root(&inner), Ok(Some(shared_real.clone())));
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let refused = kit_root(&inner).unwrap_err();
        let shown = shared_real.display();
        assert!(
            refused.contains(&format!("{shown} can be written by anyone: `chmod o-w {shown}` fixes it")),
            "{refused}"
        );
        // Below it, through folders that are trusted, a kit of the user's own is still a kit.
        std::fs::write(shared.join("mine/kit.toml"), "fennec = \"0.0.1\"\n").unwrap();
        assert_eq!(kit_root(&inner), Ok(Some(std::fs::canonicalize(shared.join("mine")).unwrap())));
        // A kit.toml anyone can write isn't taken either, and says so.
        std::fs::set_permissions(shared.join("mine/kit.toml"), std::fs::Permissions::from_mode(0o666))
            .unwrap();
        let refused = kit_root(&inner).unwrap_err();
        assert!(refused.contains("kit.toml can be written by anyone: `chmod o-w"), "{refused}");
        // A folder anyone can write to with no kit.toml above it is no kit, and no error.
        std::fs::remove_file(shared.join("mine/kit.toml")).unwrap();
        std::fs::remove_file(shared.join("kit.toml")).unwrap();
        assert_eq!(kit_root(&inner), Ok(None));
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `FENNEC_TRUST` names folders as `PATH` does, and trusts what's at or below each, not
    /// beside it; an empty entry trusts nothing.
    #[test]
    fn fennec_trust_names_folders_and_what_is_below_them() {
        let base = fennec_temp::Temp::folder("kit-trust-listed").unwrap();
        let (mounted, beside) = (base.join("work"), base.join("workshop"));
        std::fs::create_dir_all(mounted.join("game/src")).unwrap();
        std::fs::create_dir_all(&beside).unwrap();
        let real = |path: &std::path::Path| std::fs::canonicalize(path).unwrap();
        let named = std::env::join_paths(["/nowhere/at/all".into(), mounted.clone()]).unwrap();
        assert!(super::listed_in(&real(&mounted.join("game/src")), &named));
        assert!(super::listed_in(&real(&mounted), &named));
        assert!(!super::listed_in(&real(&beside), &named));
        assert!(!super::listed_in(&real(&mounted), std::ffi::OsStr::new("")));
    }

    /// A `kit.toml` that's a link isn't taken, and says so.
    #[test]
    fn a_kit_toml_that_is_a_link_is_said() {
        let base = fennec_temp::Temp::folder("kit-trust-link").unwrap();
        std::fs::write(base.join("elsewhere.toml"), "fennec = \"0.0.1\"\n").unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere.toml"), base.join("kit.toml")).unwrap();
        let refused = kit_root(&base).unwrap_err();
        assert!(refused.contains("as it's a link, not a plain file"), "{refused}");
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
