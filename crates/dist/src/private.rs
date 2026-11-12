//! Fennec's folder is its user's alone. What's in its cache is run: a program kept there is
//! linked out and started, and an object kept there is linked into the next build. A toolchain
//! kept there is run too. So the folder is made open to nobody else, and one another user owns,
//! or one anyone can write to, isn't used at all: anyone who can write it can write a program the
//! next `fennec run` starts. A folder of the user's own whose group can write to it, as a umask of
//! 002 makes every folder, is the user's, as it is for a kit, and is made private.
//!
//! Everything that makes Fennec's folder makes it through here: the cache, the launcher, and the
//! installers, which make it under a umask of 077.

use std::path::Path;

/// Why a folder isn't used, with what fixes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(pub String);

/// Makes `folder` if it isn't there, open to its user alone, and refuses one that's another
/// user's, or that anyone can write to, or that's a link, which could point anywhere. One of the
/// user's own that its group or others may read, enter or write is made private.
///
/// # Errors
///
/// When the folder can't be made, or isn't one to use, saying what fixes it.
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
            return Err(Refused(format!(
                "{shown} is a link or a file, not a folder: set FENNEC_HOME to a folder of your own"
            )));
        }
        let me = crate::trust::own_uid().ok_or_else(|| Refused("this user can't be told".to_owned()))?;
        if found.uid() != me {
            return Err(Refused(format!(
                "{shown} is another user's: set FENNEC_HOME to a folder of your own"
            )));
        }
        // Not made private here: what anyone may have put in it already would still be used.
        if found.mode() & 0o002 != 0 {
            return Err(Refused(format!(
                "{shown} can be written by anyone, so what's in it may not be yours: `chmod -R go-w {shown}` fixes it, or remove it"
            )));
        }
        // Only what others may do is taken away: a folder its user made read-only stays so.
        if found.mode() & 0o077 != 0 {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(found.mode() & 0o7700))
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
