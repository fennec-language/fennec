// What names a build of Fennec: a hash of the source of every crate and of `Cargo.lock`, as
// `fennec`'s build script and the runtime library's both work it out, so `fennec` can tell a
// runtime library built from other source. Each build script is in a crate beside the others, so
// `..` is the same folder of crates for both, and the hash comes out the same.

/// Every file under `folder`.
fn files(folder: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
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

/// The hash of every crate's source and `Cargo.lock`, telling Cargo to run the build script again
/// when any of them changes.
fn build_hash() -> String {
    let crates = std::path::Path::new("..");
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
    hasher.finalize().to_hex()
}
