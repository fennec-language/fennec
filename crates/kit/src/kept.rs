//! Loads kept for the next load of a program, as the daemon keeps them from one build to the next.
//! Each file keeps its parse and its rewriting of names across files, with what that rewriting
//! read of the other files' names. The next load uses again the parse of each file that's as it
//! was, and its rewriting while what it read still reads the same, and does the rest itself. An
//! edit to a function's body parses and rewrites one file, and a name added or taken away rewrites
//! the files that asked for it, and no others.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fennec_syntax::{Diagnostic, Import, Module, Span};

use crate::load::{self, Keeping};
use crate::names::Reads;
use crate::{Entry, Overlay, Problems, Program};

/// What a load kept for the next load of the program: each file's parse and rewriting, by its
/// module's name. Only a load with no problems is kept. One that has some leaves what was kept
/// before it as it was.
#[derive(Default)]
pub struct KeptLoad {
    pub(crate) files: HashMap<String, KeptFile>,
    /// The files of the load, in the order it visited them, with each one's kit's prefix.
    pub(crate) order: Vec<(String, String)>,
    pub(crate) used: Used,
}

/// What a load kept of one file.
pub(crate) struct KeptFile {
    pub(crate) key: FileKey,
    /// Its imports as written, which a load that finds the file as it was follows.
    pub(crate) written: Arc<[Import]>,
    /// Its module, its imports by their whole names, before names across files are rewritten.
    pub(crate) parsed: Arc<Module>,
    /// Its module after.
    pub(crate) resolved: Arc<Module>,
    /// What its rewriting read of the other files' names.
    pub(crate) reads: Arc<Reads>,
}

/// What a file is kept by: everything its parse depends on.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FileKey {
    pub(crate) shown: String,
    /// Where its spans start, which a kept module's spans hold.
    pub(crate) base: u32,
    pub(crate) text: fennec_blake3::Hash,
    pub(crate) doc_examples: bool,
    /// What its parse depends on of its kit (`Place::kept_as`).
    pub(crate) place: String,
}

/// How much of a kept load came from the one before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Used {
    /// The program's files.
    pub files: usize,
    /// Those whose parse was kept.
    pub parses: usize,
    /// Those whose rewriting of names across files was kept.
    pub rewritings: usize,
}

impl KeptLoad {
    /// How much of the load that kept this came from the one before it.
    #[must_use]
    pub fn used(&self) -> Used {
        self.used
    }
}

/// When a kept load is compared with loading it all again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    Never,
    Always,
    /// In a dev build, one load in 8 that used anything kept, picked by its files' text, so the
    /// same files always are or aren't. A release compiler never compares.
    InDevBuilds,
}

impl Compare {
    fn wanted(self, program: &Program) -> bool {
        match self {
            Compare::Never => false,
            Compare::Always => true,
            Compare::InDevBuilds => {
                let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec kept load compared");
                for (path, text) in program.sources.hashed_files() {
                    hasher.update(path.as_bytes());
                    hasher.update(text.as_bytes());
                }
                let first = hasher.finalize().as_bytes()[0];
                cfg!(debug_assertions) && first.is_multiple_of(8)
            }
        }
    }
}

/// Loads a program as [`crate::load_with`] does with no overlay, using what `kept` holds from the
/// load before for each file that's as it was, and leaving in it what this load found for the
/// next. With `compare`, a load that used anything kept is compared with loading it all again,
/// and a difference is the compiler's mistake, said as one.
///
/// # Errors
///
/// What stops the program loading, as `load_with` says it. Then `kept` is left as it was.
pub fn load_keeping(
    path: &Path,
    shown_as: &str,
    entry: Entry,
    kept: &mut KeptLoad,
    compare: Compare,
) -> Result<Program, Problems> {
    let overlay = Overlay::new();
    let keeping = Keeping { before: Some(kept), keep: true };
    let (program, next) = if path.is_dir() {
        load::kit(path, shown_as, entry, &overlay, keeping)?
    } else {
        load::file(path, shown_as, entry, &overlay, keeping)?
    };
    let next = next.unwrap_or_default();
    let used = next.used;
    if (used.parses > 0 || used.rewritings > 0) && compare.wanted(&program) {
        let again = crate::load_with(path, shown_as, entry, &overlay);
        if let Some(part) = differs(&program, again.as_ref().ok()) {
            return Err(Problems {
                sources: program.sources,
                diagnostics: vec![kept_load_differs(part)],
                unplaced: Vec::new(),
            });
        }
    }
    *kept = next;
    Ok(program)
}

/// Where a kept load differs from loading it all again, first, if it does.
fn differs(kept: &Program, again: Option<&Program>) -> Option<&'static str> {
    let Some(again) = again else { return Some("whether it loads") };
    if kept.module != again.module {
        return Some("the merged module");
    }
    let files = |program: &Program| -> Vec<(String, String)> {
        program.sources.files().map(|(path, text)| (path.to_owned(), text.to_owned())).collect()
    };
    if files(kept) != files(again) || kept.sources.spans() != again.sources.spans() {
        return Some("the files read");
    }
    let embedded = |program: &Program| -> Vec<(std::path::PathBuf, fennec_syntax::Blob)> {
        program.embedded.iter().map(|file| (file.path.clone(), file.data.clone())).collect()
    };
    if embedded(kept) != embedded(again) {
        return Some("the files embedded");
    }
    if kept.dependency_files != again.dependency_files
        || kept.kit_src != again.kit_src
        || kept.manifest != again.manifest
    {
        return Some("what's known of its kits");
    }
    None
}

fn kept_load_differs(part: &str) -> Diagnostic {
    let message = format!(
        "internal error: a load that used what the one before it kept disagrees with loading it all again, in {part}"
    );
    Diagnostic::new(message, Span::default())
        .with_help("this is a mistake in the compiler, not in the program: please report it with the program")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Compare, KeptLoad, Used, differs, load_keeping};
    use crate::{Entry, Overlay, Program};

    /// A kit of its own, holding these files under `src/`.
    fn kit(name: &str, files: &[(&str, &str)]) -> fennec_temp::Temp {
        let folder = fennec_temp::Temp::folder(name).unwrap();
        std::fs::write(folder.join("kit.toml"), "name = \"kept\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
            .unwrap();
        std::fs::create_dir_all(folder.join("src")).unwrap();
        for (file, text) in files {
            std::fs::write(folder.join("src").join(file), text).unwrap();
        }
        folder
    }

    fn write(folder: &Path, file: &str, text: &str) {
        std::fs::write(folder.join("src").join(file), text).unwrap();
    }

    /// A load using what's kept, compared with loading it all again by `load_keeping` itself.
    fn kept_load(folder: &Path, kept: &mut KeptLoad) -> Result<Program, String> {
        load_keeping(folder, "kit", Entry::Main, kept, Compare::Always).map_err(|problems| problems.render())
    }

    fn fresh_load(folder: &Path) -> Result<Program, String> {
        crate::load_with(folder, "kit", Entry::Main, &Overlay::new()).map_err(|problems| problems.render())
    }

    fn used(files: usize, parses: usize, rewritings: usize) -> Used {
        Used { files, parses, rewritings }
    }

    const MAIN: &str = "from shapes import area\nfrom paint import warm\n\ndef main():\n    print(area(2))\n    print(warm())\n";
    const SHAPES: &str = "def area(side: int) -> int:\n    return side * side\n";
    const PAINT: &str = "enum Color:\n    Red\n    Blue\n\ndef warm() -> Color:\n    return .Red\n";

    /// An edit to a body parses and rewrites the file it's in, and nothing else. A case name
    /// added in one file rewrites the file whose leading-dot case it makes ambiguous to name, and
    /// not the one that never asked about it. Each load is what loading it all again gives.
    #[test]
    fn a_kept_load_does_again_only_what_an_edit_reaches() {
        let folder = kit("kept-reach", &[("main.fen", MAIN), ("shapes.fen", SHAPES), ("paint.fen", PAINT)]);
        let mut kept = KeptLoad::default();
        kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(3, 0, 0), "nothing's kept at first");
        kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(3, 3, 3), "nothing changed");

        write(&folder, "shapes.fen", &SHAPES.replace("side * side", "side * side + 1"));
        let before = kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(3, 2, 2), "a body edited");
        assert_eq!(before.module, fresh_load(&folder).unwrap().module);

        write(&folder, "shapes.fen", &format!("{SHAPES}\nenum Light:\n    Red\n    Green\n"));
        let after = kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(3, 2, 1), "only main never asked about `Red`");
        assert_eq!(after.module, fresh_load(&folder).unwrap().module);
        assert_ne!(
            warm(&before),
            warm(&after),
            "paint's `.Red` is written as Color's once Light has one too"
        );
    }

    /// The body of `warm`, as the merged module has it.
    fn warm(program: &Program) -> String {
        let warm = program.module.items.iter().find_map(|item| match item {
            fennec_syntax::Item::Function(def) if def.name == "warm" => Some(format!("{:?}", def.body)),
            _ => None,
        });
        warm.unwrap_or_default()
    }

    /// A name one file adds is found used without an import in another, which the load before
    /// kept as it was: the load says so as loading it all again does, and keeps what it kept.
    #[test]
    fn a_name_added_is_found_missing_its_import_where_it_was_used_already() {
        let main = "from a import one\nfrom b import two\n\ndef main():\n    print(one() + two())\n";
        let one = "def one() -> int:\n    return 1\n";
        let two = "def two() -> int:\n    return helper()\n";
        let folder = kit("kept-missing", &[("main.fen", main), ("a.fen", one), ("b.fen", two)]);
        let mut kept = KeptLoad::default();
        kept_load(&folder, &mut kept).unwrap();
        kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(3, 3, 3));

        write(&folder, "a.fen", &format!("{one}\ndef helper() -> int:\n    return 2\n"));
        let found = kept_load(&folder, &mut kept).unwrap_err();
        assert!(found.contains("which this file doesn't import"), "{found}");
        assert_eq!(found, fresh_load(&folder).unwrap_err());
        assert_eq!(kept.used(), used(3, 3, 3), "a load with problems keeps nothing");

        write(&folder, "b.fen", &format!("from a import helper\n\n{two}"));
        let program = kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(3, 1, 1), "main is as the last kept load had it");
        assert_eq!(program.module, fresh_load(&folder).unwrap().module);
    }

    /// A file that embeds another's bytes isn't kept, since they can change while it doesn't.
    #[test]
    fn a_file_that_embeds_is_loaded_again_with_what_it_embeds_now() {
        let main = "def main():\n    data = embed_file(\"data.txt\")\n    print(len(data))\n";
        let folder = kit("kept-embed", &[("main.fen", main), ("data.txt", "one")]);
        let mut kept = KeptLoad::default();
        kept_load(&folder, &mut kept).unwrap();
        write(&folder, "data.txt", "two");
        let program = kept_load(&folder, &mut kept).unwrap();
        assert_eq!(kept.used(), used(1, 0, 0));
        let again = fresh_load(&folder).unwrap();
        assert_eq!(program.module, again.module);
        assert_eq!(
            program.embedded.first().map(|file| file.data.clone()),
            again.embedded.first().map(|file| file.data.clone())
        );
    }

    /// What tells a kept load from loading it all again finds a load that failed, a module that
    /// differs, and files that do.
    #[test]
    fn a_kept_load_unlike_loading_it_again_is_told_apart() {
        let folder = kit("kept-differs", &[("main.fen", MAIN), ("shapes.fen", SHAPES), ("paint.fen", PAINT)]);
        let program = fresh_load(&folder).unwrap();
        assert_eq!(differs(&program, Some(&program)), None);
        assert_eq!(differs(&program, None), Some("whether it loads"));
        let mut edited = program.clone();
        edited.module.items.pop();
        assert_eq!(differs(&edited, Some(&program)), Some("the merged module"));
        let mut moved = program.clone();
        moved.sources = crate::Sources::new();
        assert_eq!(differs(&moved, Some(&program)), Some("the files read"));
    }
}
