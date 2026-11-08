//! Unpacking a release's `.tar.gz`, which happens only after its hash has been checked against a
//! signed manifest. Even so, nothing in it may land outside the folder it's unpacked into: every
//! entry has to be a plain file or folder under the archive's one top folder, with no `..`, no
//! absolute path and no links.

use std::path::{Component, Path, PathBuf};

/// The most an archive may unpack to. A release is a few hundred megabytes.
const MOST_UNPACKED: usize = 1 << 31;

/// A file the archive holds: where it goes under the top folder, whether it can be run, and its
/// bytes. A folder has no bytes.
pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    pub(crate) runnable: bool,
    pub(crate) bytes: Option<Vec<u8>>,
}

/// The entries of a `.tar.gz` whose one top folder is `top`, with `top` taken off their paths.
///
/// # Errors
///
/// When it isn't a gzip of a tar, or holds anything but plain files and folders under `top`.
pub(crate) fn tar_gz(archive: &[u8], top: &str) -> Result<Vec<Entry>, String> {
    tar(&gunzip(archive)?, top)
}

/// The entries of a `.zip` whose one top folder is `top`, with `top` taken off their paths, as
/// Windows' releases are packed.
///
/// # Errors
///
/// When it isn't a zip that's stored or deflated, or holds anything but plain files and folders
/// under `top`.
pub(crate) fn zip(archive: &[u8], top: &str) -> Result<Vec<Entry>, String> {
    const END: [u8; 4] = [0x50, 0x4b, 5, 6];
    const LISTED: [u8; 4] = [0x50, 0x4b, 1, 2];
    const LOCAL: [u8; 4] = [0x50, 0x4b, 3, 4];
    const UNIX: u8 = 3;
    const LINK: u32 = 0o120_000;
    const KIND: u32 = 0o170_000;
    let bad = || "the archive isn't a zip that can be read".to_owned();
    let number = |at: usize, width: usize| -> Option<usize> {
        let bytes = archive.get(at..at + width)?;
        Some(bytes.iter().rev().fold(0, |number, byte| number << 8 | usize::from(*byte)))
    };
    // The end record is the last thing, after a comment of up to 64 KiB.
    let end = (0..archive.len().saturating_sub(21))
        .rev()
        .take(65_557)
        .find(|at| archive.get(*at..*at + 4) == Some(&END[..]))
        .ok_or_else(bad)?;
    let count = number(end + 10, 2).ok_or_else(bad)?;
    let mut at = number(end + 16, 4).ok_or_else(bad)?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if archive.get(at..at + 4) != Some(&LISTED[..]) {
            return Err(bad());
        }
        let made_on = *archive.get(at + 5).ok_or_else(bad)?;
        let method = number(at + 10, 2).ok_or_else(bad)?;
        let (packed, size) = (number(at + 20, 4).ok_or_else(bad)?, number(at + 24, 4).ok_or_else(bad)?);
        let name_length = number(at + 28, 2).ok_or_else(bad)?;
        let skipped = number(at + 30, 2).ok_or_else(bad)? + number(at + 32, 2).ok_or_else(bad)?;
        let attributes = u32::try_from(number(at + 38, 4).ok_or_else(bad)?).map_err(|_| bad())?;
        let local = number(at + 42, 4).ok_or_else(bad)?;
        let name = archive.get(at + 46..at + 46 + name_length).ok_or_else(bad)?;
        let name = std::str::from_utf8(name).map_err(|_| bad())?.to_owned();
        at += 46 + name_length + skipped;
        let mode = if made_on == UNIX { attributes >> 16 } else { 0 };
        if mode & KIND == LINK || name.contains('\\') {
            return Err(format!("the archive holds `{name}`, which is a link, or isn't named plainly"));
        }
        let Some(path) = under(&name, top)? else { continue };
        if name.ends_with('/') {
            entries.push(Entry { path, runnable: false, bytes: None });
            continue;
        }
        if archive.get(local..local + 4) != Some(&LOCAL[..]) {
            return Err(bad());
        }
        let start =
            local + 30 + number(local + 26, 2).ok_or_else(bad)? + number(local + 28, 2).ok_or_else(bad)?;
        let data = archive.get(start..start.checked_add(packed).ok_or_else(bad)?).ok_or_else(bad)?;
        let bytes = match method {
            0 => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, MOST_UNPACKED)
                .map_err(|_| format!("`{name}` in the archive doesn't inflate"))?,
            _ => {
                return Err(format!(
                    "`{name}` in the archive is packed in a way this launcher doesn't unpack"
                ));
            }
        };
        if bytes.len() != size {
            return Err(format!("`{name}` in the archive isn't the size it says"));
        }
        // A zip made on Windows says nothing of what can run, so there it's what's named `.exe`.
        let runnable =
            if made_on == UNIX { mode & 0o111 != 0 } else { name.to_ascii_lowercase().ends_with(".exe") };
        entries.push(Entry { path, runnable, bytes: Some(bytes) });
    }
    Ok(entries)
}

/// The bytes a gzip holds.
fn gunzip(archive: &[u8]) -> Result<Vec<u8>, String> {
    const CRC: u8 = 2;
    const EXTRA: u8 = 4;
    const NAME: u8 = 8;
    const COMMENT: u8 = 16;
    let bad = || "the archive isn't a gzip".to_owned();
    let header = archive.get(..10).ok_or_else(bad)?;
    if header.get(..3) != Some(&[0x1f, 0x8b, 8][..]) {
        return Err(bad());
    }
    let flags = *header.get(3).ok_or_else(bad)?;
    let mut at = 10;
    if flags & EXTRA != 0 {
        let length: [u8; 2] = archive.get(at..at + 2).and_then(|two| two.try_into().ok()).ok_or_else(bad)?;
        at += 2 + usize::from(u16::from_le_bytes(length));
    }
    for field in [NAME, COMMENT] {
        if flags & field != 0 {
            let rest = archive.get(at..).ok_or_else(bad)?;
            at += rest.iter().position(|byte| *byte == 0).ok_or_else(bad)? + 1;
        }
    }
    if flags & CRC != 0 {
        at += 2;
    }
    let body = archive.get(at..archive.len().checked_sub(8).ok_or_else(bad)?).ok_or_else(bad)?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, MOST_UNPACKED)
        .map_err(|_| "the archive's gzip doesn't inflate".to_owned())
}

/// The entries of a tar, as `tar_gz` gives them.
fn tar(bytes: &[u8], top: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut at = 0;
    // A path a pax header or a GNU long name gives for the entry after it.
    let mut long_name: Option<String> = None;
    while let Some(header) = bytes.get(at..at + 512) {
        if header.iter().all(|byte| *byte == 0) {
            return Ok(entries);
        }
        let field = |from: usize, to: usize| header.get(from..to).unwrap_or_default();
        let size = octal(field(124, 136)).ok_or("a tar header's size can't be read")?;
        let kind = field(156, 157).first().copied().unwrap_or(0);
        let start = at + 512;
        let end = start.checked_add(size).ok_or("the tar ends inside a file")?;
        let data = bytes.get(start..end).ok_or("the tar ends inside a file")?;
        at = start + size.div_ceil(512) * 512;
        let name = long_name.take().unwrap_or_else(|| {
            let (prefix, name) = (text(field(345, 500)), text(field(0, 100)));
            if prefix.is_empty() { name } else { format!("{prefix}/{name}") }
        });
        match kind {
            // A pax header, whose `path` names the next entry; its other keys say nothing that matters.
            b'x' => long_name = pax_path(data),
            b'L' => long_name = Some(text(data)),
            // A pax header for every entry after it, which is nothing that matters here either.
            b'g' => {}
            b'0' | 0 | b'5' => {
                let Some(path) = under(&name, top)? else { continue };
                let mode = octal(field(100, 108)).unwrap_or(0);
                let bytes = (kind != b'5').then(|| data.to_vec());
                entries.push(Entry { path, runnable: mode & 0o111 != 0, bytes });
            }
            _ => return Err(format!("the archive holds `{name}`, which is a link or a device, not a file")),
        }
    }
    Err("the tar has no end".to_owned())
}

/// Where an entry named `name` goes under the top folder `top`, or None for the top folder
/// itself, and for the `._` files macOS's tar adds to hold a file's attributes.
fn under(name: &str, top: &str) -> Result<Option<PathBuf>, String> {
    let outside = || Err(format!("the archive holds `{name}`, which isn't in its folder `{top}`"));
    let path = Path::new(name.trim_end_matches('/'));
    // macOS's tar puts one beside the top folder too. It's never written, wherever it is.
    if path.file_name().is_some_and(|file| file.to_string_lossy().starts_with("._")) {
        return Ok(None);
    }
    let mut components = path.components();
    if components.next() != Some(Component::Normal(top.as_ref())) {
        return outside();
    }
    let rest: PathBuf = components.clone().collect();
    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return outside();
    }
    Ok((!rest.as_os_str().is_empty()).then_some(rest))
}

fn pax_path(records: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(records);
    text.lines().find_map(|record| {
        let (_, entry) = record.split_once(' ')?;
        entry.strip_prefix("path=").map(str::to_owned)
    })
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|byte| *byte == 0).unwrap_or(field.len());
    String::from_utf8_lossy(field.get(..end).unwrap_or_default()).into_owned()
}

fn octal(field: &[u8]) -> Option<usize> {
    let digits = text(field);
    let digits = digits.trim_matches([' ', '\0']);
    if digits.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(digits, 8).ok()
}

/// Writes the entries under `dir`, which has to be new.
///
/// # Errors
///
/// When a file can't be written.
pub(crate) fn write(entries: &[Entry], dir: &Path) -> Result<(), String> {
    let failed = |path: &Path, err: std::io::Error| format!("couldn't write {}: {err}", path.display());
    std::fs::create_dir(dir).map_err(|err| failed(dir, err))?;
    for entry in entries {
        let path = dir.join(&entry.path);
        let Some(bytes) = &entry.bytes else {
            std::fs::create_dir_all(&path).map_err(|err| failed(&path, err))?;
            continue;
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| failed(parent, err))?;
        }
        std::fs::write(&path, bytes).map_err(|err| failed(&path, err))?;
        #[cfg(unix)]
        if entry.runnable {
            use std::os::unix::fs::PermissionsExt as _;
            let runnable = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&path, runnable).map_err(|err| failed(&path, err))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::under;

    #[test]
    fn only_what_is_under_the_top_folder_is_taken() {
        let top = "fennec-v1-x";
        assert_eq!(under("fennec-v1-x/fennec", top).unwrap(), Some("fennec".into()));
        assert_eq!(under("fennec-v1-x/llvm/bin/opt", top).unwrap(), Some("llvm/bin/opt".into()));
        assert_eq!(under("fennec-v1-x/", top).unwrap(), None);
        assert_eq!(under("fennec-v1-x/._fennec", top).unwrap(), None);
        assert_eq!(under("._fennec-v1-x", top).unwrap(), None);
        for outside in [
            "fennec",
            "other/fennec",
            "/fennec-v1-x/fennec",
            "fennec-v1-x/../evil",
            "fennec-v1-x/a/../../evil",
            "../fennec-v1-x/a",
        ] {
            assert!(under(outside, top).is_err(), "{outside}");
        }
    }
}
