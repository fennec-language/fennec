//! A file or folder in the system's temporary folder that's removed when the value naming it
//! goes, however the code holding it ends: by finishing, by failing an assertion, or by returning
//! early with `?`. Tests make their files here, so a run leaves nothing behind.
//!
//! But with `FENNEC_KEEP_FAILED` set, as the gate sets it, one dropped while its test is failing
//! is kept, and its path said on standard error, which the test's output holds: the program a
//! failing test built, its source, and what it printed say more than its panic does, and the gate
//! keeps them with its log.

use std::io::Write as _;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A path that's removed, with all it holds, when this is dropped. It shows as its path does, so
/// a test can write it into a program with `{:?}`.
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

impl std::fmt::Debug for Temp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.path.fmt(f)
    }
}

/// Whether a failing test's files are kept: `FENNEC_KEEP_FAILED` set to anything but nothing or `0`.
fn keep_failed() -> bool {
    std::env::var_os("FENNEC_KEEP_FAILED").is_some_and(|keep| !keep.is_empty() && keep != "0")
}

impl Drop for Temp {
    fn drop(&mut self) {
        if std::thread::panicking() && keep_failed() && self.path.exists() {
            let _ = writeln!(std::io::stderr(), "fennec-temp: kept {} for the failure", self.path.display());
            return;
        }
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
    fn it_shows_as_its_path() {
        let path = std::env::temp_dir().join("fennec-temp-shown");
        assert_eq!(format!("{:?}", Temp::at(path.clone())), format!("{path:?}"));
    }

    /// A folder goes when its test fails, unless `FENNEC_KEEP_FAILED` asks for it to be kept, as
    /// the gate does: then it's there to look at, and this test takes it away itself.
    #[test]
    fn a_folder_is_gone_when_its_test_fails_unless_its_kept() {
        let kept = std::env::temp_dir().join(format!("fennec-temp-failing-{}", std::process::id()));
        let failed = std::panic::catch_unwind(|| {
            let folder = Temp::folder("temp-failing").unwrap();
            std::fs::write(folder.join("inside.txt"), "x").unwrap();
            assert!(folder.join("missing").exists(), "fails on purpose");
        });
        assert!(failed.is_err());
        if super::keep_failed() {
            assert!(kept.join("inside.txt").exists());
            std::fs::remove_dir_all(&kept).unwrap();
        } else {
            assert!(!kept.exists());
        }
    }
}
