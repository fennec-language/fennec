//! Names this build of Fennec by a hash of the source of every crate, as `FENNEC_BUILD`. Which
//! tests passed and which programs checked is only worth remembering for the build that found
//! it out, and a version number says nothing while Fennec itself is changing. Only this crate
//! sees the name, so a change elsewhere rebuilds nothing more than linking does already.

use std::path::{Path, PathBuf};

/// Every file under `folder`.
fn files(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn main() {
    let crates = Path::new("..");
    let lock = crates.join("../Cargo.lock");
    println!("cargo::rerun-if-changed={}", crates.display());
    println!("cargo::rerun-if-changed={}", lock.display());
    let mut found = vec![lock];
    files(crates, &mut found);
    found.sort();
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec build 2026-10-31 source");
    for path in found {
        let bytes = std::fs::read(&path).unwrap_or_default();
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    println!("cargo::rustc-env=FENNEC_BUILD={}", hasher.finalize().to_hex());
}
