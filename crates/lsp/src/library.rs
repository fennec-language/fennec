//! The standard library written in Fennec, as files an editor can open. Its source is built into
//! `fennec`, so each file is written once to `~/.fennec/std/<version>-<hash>/`, and going to a
//! library declaration goes there. The hash is of the library's text, so two builds whose library
//! differs, like two branches built at once, never write over each other's files. It's parsed here a second time, with spans of its own, apart from the
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

/// Where this build's library files go: named by the version and a hash of every file's name and
/// text, so the files there are only ever this library's.
fn folder() -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    for (name, text) in fennec_syntax::prelude_files() {
        name.hash(&mut hasher);
        text.hash(&mut hasher);
    }
    let name = format!("{}-{:016x}", env!("CARGO_PKG_VERSION"), hasher.finish());
    let folder = fennec_build::fennec_home()?.join("std").join(name);
    std::fs::create_dir_all(&folder).ok()?;
    Some(folder)
}

/// The path of the library file `name`, written with `text` unless it's there already. It's
/// written beside its place and then moved there, so a reader never sees half a file, and left
/// read-only, since an edit to it would change nothing.
fn written(folder: &std::path::Path, name: &str, text: &str) -> String {
    let path = folder.join(format!("{name}.fen"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
        let beside = folder.join(format!(".{name}.{}.fen", std::process::id()));
        if std::fs::write(&beside, text).is_ok() {
            if let Ok(metadata) = std::fs::metadata(&beside) {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(true);
                let _ = std::fs::set_permissions(&beside, permissions);
            }
            if std::fs::rename(&beside, &path).is_err() {
                let _ = std::fs::remove_file(&beside);
            }
        }
    }
    path.to_string_lossy().into_owned()
}
