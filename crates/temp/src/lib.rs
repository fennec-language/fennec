//! A file or folder in the system's temporary folder that's removed when the value naming it
//! goes, however the code holding it ends: by finishing, by failing an assertion, or by returning
//! early with `?`. Tests make their files here, so a run leaves nothing behind.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A path that's removed, with all it holds, when this is dropped.
#[derive(Debug)]
pub struct Temp {
    path: PathBuf,
}

impl Temp {
    /// Removes `path` when it's dropped, whether it's a folder or a file, and whatever made it.
    /// Nothing is made here.
    #[must_use]
    pub fn at(path: PathBuf) -> Temp {
        Temp { path }
    }

    /// An empty folder of its own in the system's temporary folder, named `fennec-{name}-` and
    /// this process's id.
    pub fn folder(name: &str) -> std::io::Result<Temp> {
        let folder = Temp::at(std::env::temp_dir().join(format!("fennec-{name}-{}", std::process::id())));
        // One left by a process that had this id and was killed before it tidied up.
        let _ = std::fs::remove_dir_all(&folder.path);
        std::fs::create_dir_all(&folder.path)?;
        Ok(folder)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Deref for Temp {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Temp {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for Temp {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        // What's already gone, or can't be removed, is left as it is: a test's own result matters
        // more than its tidying up.
        if std::fs::remove_dir_all(&self.path).is_err() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Temp;

    #[test]
    fn a_folder_and_a_file_are_gone_when_dropped() {
        let folder = Temp::folder("temp-folder").unwrap();
        std::fs::write(folder.join("inside.txt"), "x").unwrap();
        let kept = folder.to_path_buf();
        drop(folder);
        assert!(!kept.exists());
        let file = Temp::at(std::env::temp_dir().join(format!("fennec-temp-file-{}", std::process::id())));
        std::fs::write(&file, "x").unwrap();
        let kept = file.to_path_buf();
        drop(file);
        assert!(!kept.exists());
    }

    #[test]
    fn a_folder_is_gone_when_its_test_fails() {
        let kept = std::env::temp_dir().join(format!("fennec-temp-failing-{}", std::process::id()));
        let failed = std::panic::catch_unwind(|| {
            let folder = Temp::folder("temp-failing").unwrap();
            std::fs::write(folder.join("inside.txt"), "x").unwrap();
            assert!(folder.join("missing").exists(), "fails on purpose");
        });
        assert!(failed.is_err());
        assert!(!kept.exists());
    }
}
