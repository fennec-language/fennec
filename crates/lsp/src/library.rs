//! The standard library written in Fennec, as files an editor can open. Its source is built into
//! `fennec`, so each file is written once to `~/.fennec/std/<version>/`, and going to a library
//! declaration goes there. It's parsed here a second time, with spans of its own, apart from the
//! program's: what a name in the library refers to is never the program's own, so renaming it is
//! refused, and nothing in it is ever reported.

use std::path::PathBuf;
use std::sync::OnceLock;

use fennec_syntax::{Module, Sources};

/// The library's files, each with the module it's parsed to and the name it goes by, like `math`.
pub(crate) struct Library {
    pub(crate) sources: Sources,
    pub(crate) modules: Vec<(&'static str, Module)>,
}

/// The library, written out and parsed the first time it's asked for.
pub(crate) fn library() -> &'static Library {
    static LIBRARY: OnceLock<Library> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let folder = folder();
        let mut sources = Sources::new();
        let mut modules = Vec::new();
        for (name, text) in fennec_syntax::prelude_files() {
            let path = folder
                .as_ref()
                .map_or_else(|| format!("std/{name}.fen"), |folder| written(folder, name, text));
            let base = sources.add(path, text);
            if let Ok(module) = fennec_syntax::parse_at(text, base) {
                modules.push((name, module));
            }
        }
        Library { sources, modules }
    })
}

/// Where this version's library files go.
fn folder() -> Option<PathBuf> {
    let folder = fennec_build::fennec_home()?.join("std").join(env!("CARGO_PKG_VERSION"));
    std::fs::create_dir_all(&folder).ok()?;
    Some(folder)
}

/// The path of the library file `name`, written with `text` unless it's there already.
fn written(folder: &std::path::Path, name: &str, text: &str) -> String {
    let path = folder.join(format!("{name}.fen"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
        // It's for reading, since an edit to it would change nothing, so it's made writable only
        // to be written again by a build of this version whose library has changed.
        if let Ok(metadata) = std::fs::metadata(&path) {
            let mut permissions = metadata.permissions();
            #[expect(clippy::permissions_set_readonly_false, reason = "only to write it again")]
            permissions.set_readonly(false);
            let _ = std::fs::set_permissions(&path, permissions);
        }
        let _ = std::fs::write(&path, text);
        if let Ok(metadata) = std::fs::metadata(&path) {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            let _ = std::fs::set_permissions(&path, permissions);
        }
    }
    path.to_string_lossy().into_owned()
}
