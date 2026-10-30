//! Finding a kit, reading each file once, and following imports from file to file.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use fennec_syntax::{Diagnostic, Import, Item, Module, Sources, Span};

use crate::{Entry, Manifest, Program, manifest, scope};

/// A parsed file of a program.
pub(crate) struct File {
    /// Its module name, like `enemies.ai`.
    pub(crate) name: String,
    /// Its path as messages show it.
    pub(crate) shown: String,
    pub(crate) module: Module,
}

/// Whether an import is of the standard library rather than of the program's own files.
pub(crate) fn is_std(import: &Import) -> bool {
    import.module.first().is_some_and(|first| first == "std")
}

/// Loads a single `.fen` file. Its imports resolve against its own folder.
pub(crate) fn file(path: &Path, shown_as: &str) -> Result<Program, String> {
    let src = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut loader = Loader::new(src, folder_prefix(Path::new(shown_as).parent()));
    let name = path.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    loader.visit(&name, path, shown_as.to_owned(), None);
    loader.finish(None)
}

/// Loads a kit from its folder or any folder inside it.
pub(crate) fn kit(dir: &Path, shown_as: &str, entry: Entry) -> Result<Program, String> {
    let Some((root, shown_root)) = find_root(dir, shown_as) else {
        return Err(format!(
            "error: there's no kit.toml in {shown_as} or any folder above it\n  = help: `fennec new <name>` starts a kit, and `fennec run <file.fen>` runs a single file\n"
        ));
    };
    let manifest = read_manifest(&root, &shown_root)?;
    let src = root.join("src");
    let mut loader = Loader::new(src.clone(), format!("{shown_root}src/"));
    let main = src.join("main.fen");
    if main.is_file() {
        loader.visit("main", &main, format!("{shown_root}src/main.fen"), None);
    } else if entry == Entry::Main {
        return Err(format!(
            "error: {shown_root}src/main.fen doesn't exist\n  = help: a kit runs `main` from src/main.fen\n"
        ));
    }
    if entry == Entry::Everything {
        let mut modules = Vec::new();
        find_modules(&src, &mut Vec::new(), &mut modules);
        modules.sort();
        for module in modules {
            let (real, shown) = loader.module_path(&module);
            loader.visit(&module.join("."), &real, shown, None);
        }
    }
    loader.finish(Some(manifest))
}

/// The nearest folder at or above `dir` with a `kit.toml`, and how messages show it: by
/// trimming `shown_as` when it names that folder, and as an absolute path when it doesn't.
fn find_root(dir: &Path, shown_as: &str) -> Option<(PathBuf, String)> {
    let absolute = std::fs::canonicalize(dir).ok()?;
    let (levels, root) =
        absolute.ancestors().enumerate().find(|(_, folder)| folder.join("kit.toml").is_file())?;
    let mut shown = PathBuf::from(shown_as);
    for _ in 0..levels {
        // Going up from `.` or `..` isn't trimming a name, so show the absolute path.
        if !matches!(shown.components().next_back(), Some(Component::Normal(_))) {
            return Some((root.to_path_buf(), folder_prefix(Some(root))));
        }
        shown.pop();
    }
    Some((root.to_path_buf(), folder_prefix(Some(&shown))))
}

/// How a folder is shown before the files in it: `games/arena/`, or nothing for `.`.
fn folder_prefix(folder: Option<&Path>) -> String {
    let shown = folder.map_or_else(String::new, |folder| folder.to_string_lossy().replace('\\', "/"));
    // Windows writes absolute paths it has resolved as `\\?\C:\...`.
    let shown = shown.strip_prefix("//?/").unwrap_or(&shown);
    match shown.trim_end_matches('/') {
        "" | "." => String::new(),
        trimmed => format!("{trimmed}/"),
    }
}

fn read_manifest(root: &Path, shown_root: &str) -> Result<Manifest, String> {
    let shown = format!("{shown_root}kit.toml");
    let text = std::fs::read_to_string(root.join("kit.toml"))
        .map_err(|err| format!("error: couldn't read {shown}: {err}\n"))?;
    manifest::parse(&text, 0).map_err(|diagnostics| {
        let mut sources = Sources::new();
        sources.add(shown, text);
        sources.render_all(&diagnostics)
    })
}

/// Every `.fen` file under `dir`, as module paths.
fn find_modules(dir: &Path, prefix: &mut Vec<String>, found: &mut Vec<Vec<String>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { continue };
        if path.is_dir() {
            prefix.push(stem);
            find_modules(&path, prefix, found);
            prefix.pop();
        } else if path.extension().is_some_and(|ext| ext == "fen") {
            found.push(prefix.iter().cloned().chain([stem]).collect());
        }
    }
}

struct Loader {
    /// The folder module paths resolve against.
    src: PathBuf,
    /// How `src` is shown before a module's path.
    shown_src: String,
    sources: Sources,
    files: Vec<File>,
    seen: HashSet<String>,
    /// The modules being loaded, outermost first, to find imports that go in a circle.
    stack: Vec<String>,
    errors: Vec<Diagnostic>,
    /// Problems with no place in a file to point at.
    unplaced: Vec<String>,
}

impl Loader {
    fn new(src: PathBuf, shown_src: String) -> Self {
        Loader {
            src,
            shown_src,
            sources: Sources::new(),
            files: Vec::new(),
            seen: HashSet::new(),
            stack: Vec::new(),
            errors: Vec::new(),
            unplaced: Vec::new(),
        }
    }

    /// Where a module's file is, and how it's shown.
    fn module_path(&self, module: &[String]) -> (PathBuf, String) {
        let mut real = self.src.clone();
        real.extend(module);
        real.set_extension("fen");
        (real, format!("{}{}.fen", self.shown_src, module.join("/")))
    }

    /// Loads a module and what it imports, unless it's loaded already. `from` is the import
    /// that asked for it.
    fn visit(&mut self, name: &str, real: &Path, shown: String, from: Option<Span>) {
        if let Some(at) = self.stack.iter().position(|loading| loading == name) {
            let chain: Vec<&str> = self.stack.iter().skip(at).map(String::as_str).chain([name]).collect();
            let diagnostic = if chain.len() == 2 {
                Diagnostic::new(format!("`{name}` imports itself"), Span::default())
                    .with_help("a file sees its own names without importing them")
            } else {
                Diagnostic::new(
                    format!("these imports go in a circle: {}", chain.join(" → ")),
                    Span::default(),
                )
                .with_help("move what the files share into a module of its own, and import that from each")
            };
            if let Some(span) = from {
                self.errors.push(Diagnostic { span, ..diagnostic });
            }
            return;
        }
        if !self.seen.insert(name.to_owned()) {
            return;
        }
        let text = match std::fs::read_to_string(real) {
            Ok(text) => text,
            Err(err) => {
                match from {
                    Some(span) => self.errors.push(
                        Diagnostic::new(format!("there's no module called `{name}`"), span)
                            .with_help(format!("it would be the file {shown}")),
                    ),
                    None => self.unplaced.push(format!("error: couldn't read {shown}: {err}\n")),
                }
                return;
            }
        };
        let base = self.sources.add(shown.clone(), text.as_str());
        let module = match fennec_syntax::parse_at(&text, base) {
            Ok(module) => module,
            Err(diagnostic) => {
                self.errors.push(diagnostic);
                return;
            }
        };
        let imports: Vec<(Vec<String>, Span)> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Import(import) if !is_std(import) => Some((import.module.clone(), import.span)),
                _ => None,
            })
            .collect();
        self.files.push(File { name: name.to_owned(), shown, module });
        self.stack.push(name.to_owned());
        for (module, span) in imports {
            let (real, shown) = self.module_path(&module);
            self.visit(&module.join("."), &real, shown, Some(span));
        }
        self.stack.pop();
    }

    /// Checks names across the files and merges them into one module.
    fn finish(mut self, manifest: Option<Manifest>) -> Result<Program, String> {
        if !self.unplaced.is_empty() {
            return Err(self.unplaced.concat());
        }
        if self.errors.is_empty() {
            scope::resolve(&mut self.files, &mut self.errors);
        }
        if !self.errors.is_empty() {
            self.errors.sort_by_key(|diagnostic| diagnostic.span.start);
            return Err(self.sources.render_all(&self.errors));
        }
        let items = self
            .files
            .into_iter()
            .flat_map(|file| file.module.items)
            .filter(|item| !matches!(item, Item::Import(import) if !is_std(import)))
            .collect();
        Ok(Program { module: Module { items }, sources: self.sources, manifest })
    }
}
