//! Fennec's folder is its user's alone. What's in its cache is run: a program kept there is
//! linked out and started, and an object kept there is linked into the next build. So the folder
//! is made open to nobody else, and a cache another user owns, or one others can write to, isn't
//! used at all: anyone who can write it can write a program the next `fennec run` starts.

use std::path::Path;

/// Why a folder isn't used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(pub String);

/// Makes `folder` if it isn't there, open to its user alone, and refuses one that's another
/// user's, or that others can write to, or that's a link, which could point anywhere. One of the
/// user's own that others can only read or enter is made private.
pub fn private_folder(folder: &Path) -> Result<(), Refused> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
        let shown = folder.display();
        match std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder) {
            Ok(()) => {}
            Err(err) => return Err(Refused(format!("{shown} can't be made: {err}"))),
        }
        let found = std::fs::symlink_metadata(folder).map_err(|err| Refused(format!("{shown}: {err}")))?;
        if !found.is_dir() {
            return Err(Refused(format!("{shown} is a link or a file, not a folder")));
        }
        let me = own_uid().ok_or_else(|| Refused("this user can't be told".to_owned()))?;
        if found.uid() != me {
            return Err(Refused(format!("{shown} is another user's")));
        }
        if found.mode() & 0o022 != 0 {
            return Err(Refused(format!("{shown} can be written by others")));
        }
        if found.mode() & 0o077 != 0 {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
                .map_err(|err| Refused(format!("{shown} can't be made private: {err}")))?;
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(folder)
            .map_err(|err| Refused(format!("{} can't be made: {err}", folder.display())))?;
    }
    Ok(())
}

/// The user this process runs as: the owner of a file it makes, since the standard library has no
/// call that says so. Found once.
#[cfg(unix)]
pub fn own_uid() -> Option<u32> {
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

/// Where there are no user ids to compare, there's no owner to tell.
#[cfg(not(unix))]
pub fn own_uid() -> Option<u32> {
    None
}
