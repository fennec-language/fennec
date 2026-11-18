//! The toolchain's own store: what depends only on this build of Fennec and the libraries beside
//! it, worked out once for every home rather than once in each. The standard library's parse is
//! kept there, and which build each runtime library is from. An empty home, as each test's and each
//! CI job's is, did both again, about half of what building a small program costs.
//!
//! It sits beside `fennec`, as `fennec-store`, and is keyed as a home's cache is: by this build of
//! Fennec, and within it by what each entry is of, so nothing another build or another library made
//! is ever read. A home reads it when its own cache hasn't got an entry, and what the toolchain
//! works out is kept in both. One that can't be written, as in a folder only the system writes, is
//! only read. `FENNEC_TOOLCHAIN_STORE` names another folder, or, set empty, turns it off, as the
//! tests of an empty home's own work do.

use std::sync::OnceLock;

use fennec_build::Cache;

/// The toolchain's store, if there's one to use.
fn store() -> Option<&'static Cache> {
    static STORE: OnceLock<Option<Cache>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            if std::env::var_os("FENNEC_NO_CACHE").is_some() {
                return None;
            }
            let folder = match std::env::var_os("FENNEC_TOOLCHAIN_STORE") {
                Some(folder) if folder.is_empty() => return None,
                Some(folder) => std::path::PathBuf::from(folder),
                None => std::env::current_exe().ok()?.parent()?.join("fennec-store"),
            };
            Some(Cache::new(&folder))
        })
        .as_ref()
}

/// An entry the toolchain's store has.
pub(crate) fn read(kind: &str, name: &str) -> Option<Vec<u8>> {
    store()?.read(kind, name)
}

/// Keeps an entry in the toolchain's store, for every home after this one. A store that can't be
/// written is one to read only, which is no fault of the command's, so nothing is said.
pub(crate) fn keep(kind: &str, name: &str, bytes: &[u8]) {
    if let Some(store) = store() {
        let _ = store.write(kind, name, bytes);
    }
}
