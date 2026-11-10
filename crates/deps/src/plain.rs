//! A path as other programs take it. On Windows, `canonicalize` gives a path in its verbatim form,
//! `\\?\C:\Users\...` or `\\?\UNC\server\share\...`, which Windows' own calls take but which
//! some programs refuse: newer versions of `git` can't make a folder under one. Such a path is
//! handed over in its plain form, `C:\Users\...` or `\\server\share\...`.

use std::path::{Path, PathBuf};

/// `path` in its plain form, which it already is anywhere but on Windows.
#[must_use]
pub fn plain(path: &Path) -> PathBuf {
    path.to_str().and_then(plain_text).map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// A verbatim path's plain form, or None when it isn't one that has a plain form: a verbatim
/// path to a drive or a share. Any other verbatim path, like `\\?\Volume{...}`, has none, and
/// any path that isn't verbatim is plain already.
#[must_use]
pub fn plain_text(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    let rest = path.strip_prefix(r"\\?\")?;
    let mut letters = rest.chars();
    let drive =
        letters.next().is_some_and(|letter| letter.is_ascii_alphabetic()) && letters.next() == Some(':');
    // `\\?\` also lets a path hold what a plain one can't, like a `/`, or `.` and `..` as names,
    // which a plain one reads differently.
    let different = rest.contains('/') || rest.split('\\').any(|part| part == "." || part == "..");
    (drive && !different).then(|| rest.to_owned())
}

#[cfg(test)]
mod tests {
    use super::plain_text;

    #[test]
    fn a_verbatim_drive_or_share_path_is_given_plain_and_any_other_path_is_left_alone() {
        assert_eq!(plain_text(r"\\?\C:\Users\runner\fennec").as_deref(), Some(r"C:\Users\runner\fennec"));
        assert_eq!(plain_text(r"\\?\UNC\server\share\kit").as_deref(), Some(r"\\server\share\kit"));
        assert_eq!(plain_text(r"C:\Users\runner"), None);
        assert_eq!(plain_text("/home/runner/kit"), None);
        assert_eq!(plain_text(r"\\?\Volume{1b3b1146-4076-11e1-84aa-806e6f6e6963}\kit"), None);
        assert_eq!(plain_text(r"\\?\C:\kit\..\other"), None);
        assert_eq!(plain_text(r"\\?\C:\a/b"), None);
    }
}
