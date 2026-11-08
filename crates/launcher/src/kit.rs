//! What a kit asks for: the version of Fennec in its `kit.toml`, and the hash `kit.lock` pins it
//! to, which is the hash of that release's signed manifest. One manifest names every platform's
//! archive, so teammates on different machines share one pin.

use std::path::{Path, PathBuf};

/// The kit around a folder: the nearest folder above it, or it, with a `kit.toml`.
pub(crate) fn root(from: &Path) -> Option<PathBuf> {
    from.ancestors().find(|dir| dir.join("kit.toml").is_file()).map(Path::to_path_buf)
}

/// The version `kit.toml` asks for: `fennec = "0.4.2"`, at its top, before any table.
///
/// # Errors
///
/// When there's none, or it isn't a version.
pub(crate) fn version(manifest: &str) -> Result<String, String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() != "fennec" {
            continue;
        }
        let value = value.trim();
        let version = value.strip_prefix('"').and_then(|rest| rest.strip_suffix('"'));
        return match version {
            Some(version) if plain_version(version) => Ok(version.to_owned()),
            _ => Err(format!("kit.toml's `fennec = {value}` isn't a version, like `fennec = \"0.4.2\"`")),
        };
    }
    Err("kit.toml doesn't say which Fennec it needs: add `fennec = \"0.4.2\"`, with the version".to_owned())
}

/// Whether a word is a version: digits first, then digits, letters, dots, dashes and `+`.
pub(crate) fn plain_version(version: &str) -> bool {
    version.starts_with(|first: char| first.is_ascii_digit())
        && version.len() <= 64
        && version.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
}

/// What `kit.lock` pins the toolchain to: its version and its manifest's hash, from the line
/// `toolchain 0.4.2 1f0c…`.
///
/// # Errors
///
/// When the line is there but can't be read.
pub(crate) fn pinned(lock: &str) -> Result<Option<(String, fennec_blake3::Hash)>, String> {
    for line in lock.lines() {
        let mut words = line.split(' ');
        if words.next() != Some("toolchain") {
            continue;
        }
        let (Some(version), Some(hash), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!("kit.lock's `{line}` isn't `toolchain <version> <hash>`"));
        };
        let hash = fennec_blake3::Hash::from_hex(hash)
            .filter(|_| {
                plain_version(version)
                    && hash.len() == 64
                    && hash.bytes().all(|byte| !byte.is_ascii_uppercase())
            })
            .ok_or_else(|| format!("kit.lock's `{line}` isn't `toolchain <version> <hash>`"))?;
        return Ok(Some((version.to_owned(), hash)));
    }
    Ok(None)
}

/// `kit.lock` with the toolchain pinned as given, and every other line as it was.
pub(crate) fn with_pin(lock: &str, version: &str, hash: fennec_blake3::Hash) -> String {
    let pin = format!("toolchain {version} {}\n", hash.to_hex());
    let kept: String = lock
        .split_inclusive('\n')
        .filter(|line| !line.starts_with("toolchain "))
        .map(|line| if line.ends_with('\n') { line.to_owned() } else { format!("{line}\n") })
        .collect();
    if kept.is_empty() {
        return format!("# What this kit builds with, by hash. Only `fennec update` changes it.\n{pin}");
    }
    format!("{kept}{pin}")
}

#[cfg(test)]
mod tests {
    use super::{pinned, version, with_pin};

    #[test]
    fn the_version_is_the_one_at_the_top_of_kit_toml() {
        assert_eq!(version("name = \"game\"\nfennec = \"0.4.2\"\n"), Ok("0.4.2".to_owned()));
        assert_eq!(version("fennec=\"1.0.0-rc.1\""), Ok("1.0.0-rc.1".to_owned()));
        assert!(
            version("name = \"game\"\n[dependencies]\nfennec = \"0.4.2\"\n")
                .unwrap_err()
                .contains("doesn't say")
        );
        assert!(version("fennec = \"latest\"\n").unwrap_err().contains("isn't a version"));
        assert!(version("fennec = \"../0.4.2\"\n").unwrap_err().contains("isn't a version"));
        assert!(version("fennec = 0.4\n").unwrap_err().contains("isn't a version"));
    }

    #[test]
    fn a_pin_is_one_line_and_the_rest_of_kit_lock_stays() {
        let hash = fennec_blake3::hash(b"manifest");
        let fresh = with_pin("", "0.4.2", hash);
        assert_eq!(pinned(&fresh), Ok(Some(("0.4.2".to_owned(), hash))));
        let other = "kit owner/name 1.2.0 abc\n";
        let moved = with_pin(&format!("{other}{fresh}"), "0.4.3", hash);
        let comment = "# What this kit builds with, by hash. Only `fennec update` changes it.\n";
        assert_eq!(moved, format!("{other}{comment}toolchain 0.4.3 {}\n", hash.to_hex()));
        assert_eq!(pinned("kit owner/name 1.2.0 abc"), Ok(None));
        assert!(pinned("toolchain 0.4.2").is_err());
        assert!(pinned(&format!("toolchain 0.4.2 {}", hash.to_hex().to_uppercase())).is_err());
    }
}
