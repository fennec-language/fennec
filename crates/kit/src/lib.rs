//! Fennec projects. A kit is a folder with a `kit.toml` and its code under `src`, where
//! `src/main.fen` holds `main` and the module `enemies.ai` is the file `src/enemies/ai.fen`.
//!
//! Loading reads a file and everything it imports, checks the imports, and merges the files
//! into one module that the checker and the compiler take as they take a single file. Every file is parsed at its own byte offset, so `Program::sources` can say which
//! file a diagnostic is in.

// A hash map's order changes with every process, so iterating one would make what the compiler
// emits, or the order it checks in, differ from run to run. Iterate a sorted copy or a BTreeMap.
#![deny(clippy::iter_over_hash_type)]

mod embed;
mod init;
mod load;

pub use load::PACK_LOCK;
mod scope;
mod starter;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fennec_syntax::{Blob, Diagnostic, Module, Sources};

pub use fennec_deps::{Dependency, Manifest, Source, valid_name};
pub use init::{GUIDANCE, Guidance, Initialized, init};
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
    /// Where a kit's modules are, and how messages show that, for looking in the ones not loaded.
    pub kit_src: Option<(PathBuf, String)>,
    /// The files loaded from other kits, as messages show them.
    pub dependency_files: HashSet<String>,
}

/// A file `embed` read, and what it held then.
#[derive(Clone, Debug)]
pub struct Embedded {
    pub path: PathBuf,
    pub data: Blob,
}

impl Program {
    /// This program with `module` in place of its own, which isn't copied.
    #[must_use]
    pub fn with_module(&self, module: Module) -> Program {
        Program {
            module,
            sources: self.sources.clone(),
            manifest: self.manifest.clone(),
            embedded: self.embedded.clone(),
            kit_src: self.kit_src.clone(),
            dependency_files: self.dependency_files.clone(),
        }
    }

    /// Whether something at `span` is in this kit's own files, rather than a dependency's, whose
    /// lints aren't for this kit to act on.
    #[must_use]
    pub fn is_own(&self, span: fennec_syntax::Span) -> bool {
        self.sources.file_at(span).is_none_or(|(file, ..)| !self.dependency_files.contains(file))
    }

    /// The file of the kit that gives a type this method with `extend`, which this program didn't
    /// load, as messages show it, with its module's name. `who` is how a message names the type.
    #[must_use]
    pub fn extension_elsewhere(&self, who: &str, method: &str) -> Option<(String, String)> {
        let (src, shown_src) = self.kit_src.as_ref()?;
        let names_it =
            |name: &str| who.split(|c: char| !c.is_alphanumeric() && c != '_').any(|word| word == name);
        let mut modules = Vec::new();
        load::find_modules(src, &mut Vec::new(), &mut modules);
        modules.sort();
        modules.into_iter().find_map(|module| {
            let mut path = src.clone();
            path.extend(&module);
            path.set_extension("fen");
            let text = std::fs::read_to_string(path).ok()?;
            if !text.contains("extend ") {
                return None;
            }
            let extends = fennec_syntax::parse(&text).ok()?.items.iter().any(|item| match item {
                fennec_syntax::Item::Extend(extension) => {
                    names_it(&extension.name) && extension.methods.iter().any(|def| def.name == method)
                }
                _ => false,
            });
            extends.then(|| (format!("{shown_src}{}.fen", module.join("/")), module.join(".")))
        })
    }

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
    /// Every file of the kit.
    Everything,
    /// Every file of the kit, with the examples in its doc comments as tests, for running them.
    Tests,
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
        if texts.iter().any(|(_, text)| text.contains("embed_file(") || text.contains("embed_folder(")) {
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

/// Like `load_modules`, with the examples in the doc comments of what's loaded as tests.
pub fn load_modules_to_test(dir: &Path, shown_as: &str, modules: &[String]) -> Result<Program, Problems> {
    load::kit_modules_to_test(dir, shown_as, modules)
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
        load::file(path, shown_as, entry, overlay)
    }
}

/// Whether `program`, loaded from `path` as `shown_as` with `Entry::Main`, holds every file of its
/// kit, so that it's just what `Entry::Everything` would load: the same files, visited in the same
/// order. A single file loads the same either way.
#[must_use]
pub fn holds_every_file(program: &Program, path: &Path, shown_as: &str) -> bool {
    !path.is_dir() || load::holds_every_file(program, path, shown_as)
}

/// Parses kept from earlier loads, by what was parsed, for loading to use rather than parsing a
/// file again: a parse of the same text is the same module, wherever among a program's files it is.
pub trait Parses: Send + Sync {
    /// The module the text whose hash is `text`, shown as `shown`, parses to at `base`, if a
    /// parse of it is kept.
    fn kept(&self, text: &fennec_blake3::Hash, shown: &str, doc_examples: bool, base: u32) -> Option<Module>;

    /// Keeps the module the text whose hash is `text` parsed to at `base`.
    fn keep(&self, text: &fennec_blake3::Hash, shown: &str, doc_examples: bool, base: u32, module: &Module);
}

/// Where loading finds and keeps parses, once `keep_parses_in` says.
static PARSES: std::sync::OnceLock<Box<dyn Parses>> = std::sync::OnceLock::new();

/// Has every load in this process find and keep its files' parses in `parses`. Only the first
/// call does anything.
pub fn keep_parses_in(parses: Box<dyn Parses>) {
    let _ = PARSES.set(parses);
}

/// Why a program couldn't be loaded.
#[derive(Clone, Debug, Default)]
pub struct Problems {
    /// The files read before the problems were found.
    pub sources: Sources,
    /// Problems in those files, like a parse error, an import of nothing or a bad `kit.toml`.
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
