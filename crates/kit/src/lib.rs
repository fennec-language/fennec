//! Fennec projects. A kit is a folder with a `kit.toml` and its code under `src`, where
//! `src/main.fen` holds `main` and the module `enemies.ai` is the file `src/enemies/ai.fen`.
//!
//! Loading reads a file and everything it imports, checks the imports, and merges the files
//! into one module that the checker, the interpreter and the JIT run as they run a single
//! file. Every file is parsed at its own byte offset, so `Program::sources` can say which
//! file a diagnostic is in.

mod embed;
mod init;
mod load;
mod manifest;
mod scope;
mod starter;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fennec_syntax::{Blob, Diagnostic, Module, Sources};

pub use init::{GUIDANCE, Guidance, Initialized, init};
pub use manifest::{Manifest, valid_name};
pub use starter::create;

/// A program ready to check and run: every file merged into one module.
#[derive(Clone, Debug)]
pub struct Program {
    pub module: Module,
    /// Every file, for rendering diagnostics against the file they point into.
    pub sources: Sources,
    /// The kit's `kit.toml`, when the program is a kit rather than a single file.
    pub manifest: Option<Manifest>,
    /// The files `embed` read, since a change to one changes the program.
    pub embedded: Vec<Embedded>,
}

/// A file `embed` read, and what it held then.
#[derive(Clone, Debug)]
pub struct Embedded {
    pub path: PathBuf,
    pub data: Blob,
}

impl Program {
    /// The files `embed` read, by a name no module has, with a hash for their text.
    #[must_use]
    pub fn embedded_texts(&self) -> Vec<(String, String)> {
        self.embedded.iter().map(|file| (embedded_name(&file.path), file.data.hash().to_hex())).collect()
    }
}

/// How the caches name a file `embed` read, next to a kit's modules.
#[must_use]
pub fn embedded_name(path: &Path) -> String {
    format!("{EMBEDDED}{}", path.display())
}

/// What starts the name of a file `embed` read.
pub const EMBEDDED: &str = "embed:";

/// Where loading starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `main` and what it imports, for running.
    Main,
    /// Every file of the kit, for running all of its tests.
    Everything,
}

/// Loads a program from a `.fen` file, or from a kit's folder or any folder inside it.
/// `shown_as` is how messages name `path`. Errors come back rendered, ready to print.
pub fn load(path: &Path, shown_as: &str, entry: Entry) -> Result<Program, String> {
    load_with(path, shown_as, entry, &Overlay::new()).map_err(|problems| problems.render())
}

/// The text of every file a program's tests can see: all of a kit's by module name, read
/// without parsing them, or a single file's and what it imports by path. None if they can't be read or loaded,
/// which loading it properly will explain. The files `embed` reads count too, so a kit that might
/// use it is loaded after all.
pub fn all_texts(path: &Path) -> Option<Vec<(String, String)>> {
    let shown = path.to_string_lossy();
    if path.is_dir() {
        let mut texts = load::kit_texts(path)?;
        if texts.iter().any(|(_, text)| text.contains("embed(")) {
            texts.extend(load(path, &shown, Entry::Everything).ok()?.embedded_texts());
        }
        return Some(texts);
    }
    let program = load(path, &shown, Entry::Everything).ok()?;
    let files = program.sources.files().map(|(path, text)| (path.to_owned(), text.to_owned()));
    Some(files.chain(program.embedded_texts()).collect())
}

/// Where each file of a kit is, by module name, with the manifest as `kit.toml`. None if `dir`
/// isn't in a kit.
pub fn kit_paths(dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    load::kit_paths(dir)
}

/// Loads some of a kit's modules and everything they import, by their names.
pub fn load_modules(dir: &Path, shown_as: &str, modules: &[String]) -> Result<Program, Problems> {
    load::kit_modules(dir, shown_as, modules, &Overlay::new())
}

/// Like `load_modules`, reading the files in `overlay` from it rather than from disk.
pub fn load_modules_with(
    dir: &Path,
    shown_as: &str,
    modules: &[String],
    overlay: &Overlay,
) -> Result<Program, Problems> {
    load::kit_modules(dir, shown_as, modules, overlay)
}

/// Text to read in place of files on disk, by path: an editor's unsaved changes. A path is
/// matched exactly as loading builds it, from the folder it's given.
pub type Overlay = HashMap<PathBuf, String>;

/// Like `load`, reading the files in `overlay` from it rather than from disk, and giving
/// back what went wrong as diagnostics rather than rendered.
pub fn load_with(path: &Path, shown_as: &str, entry: Entry, overlay: &Overlay) -> Result<Program, Problems> {
    if path.is_dir() {
        load::kit(path, shown_as, entry, overlay)
    } else {
        load::file(path, shown_as, overlay)
    }
}

/// Why a program couldn't be loaded.
#[derive(Clone, Debug, Default)]
pub struct Problems {
    /// The files read before the problems were found.
    pub sources: Sources,
    /// Problems in those files, like a parse error or an import of nothing.
    pub diagnostics: Vec<Diagnostic>,
    /// Problems with no place in a file, like a missing `kit.toml`, already rendered.
    pub unplaced: Vec<String>,
}

impl Problems {
    fn unplaced(message: String) -> Self {
        Problems { unplaced: vec![message], ..Problems::default() }
    }

    /// Every problem, rendered, ready to print.
    pub fn render(&self) -> String {
        if self.unplaced.is_empty() {
            self.sources.render_all(&self.diagnostics)
        } else {
            self.unplaced.concat()
        }
    }
}
