//! Fennec projects. A kit is a folder with a `kit.toml` and its code under `src`, where
//! `src/main.fen` holds `main` and the module `enemies.ai` is the file `src/enemies/ai.fen`.
//!
//! Loading reads a file and everything it imports, checks the imports, and merges the files
//! into one module that the checker, the interpreter and the JIT run as they run a single
//! file. Every file is parsed at its own byte offset, so `Program::sources` can say which
//! file a diagnostic is in.

mod load;
mod manifest;
mod scope;
mod starter;

use std::path::Path;

use fennec_syntax::{Module, Sources};

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
}

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
    if path.is_dir() { load::kit(path, shown_as, entry) } else { load::file(path, shown_as) }
}
