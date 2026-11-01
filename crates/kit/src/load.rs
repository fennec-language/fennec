//! Finding a kit, reading each file once, and following imports from file to file.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use fennec_syntax::{Diagnostic, Import, Item, Module, Sources, Span};

use crate::{Entry, Manifest, Overlay, Problems, Program, manifest, scope};

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
pub(crate) fn file(path: &Path, shown_as: &str, overlay: &Overlay) -> Result<Program, Problems> {
    let src = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut loader = Loader::new(src, folder_prefix(Path::new(shown_as).parent()), overlay);
    let name = path.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    loader.visit(&name, path, shown_as.to_owned(), None);
    loader.finish(None)
}

/// Loads a kit from its folder or any folder inside it.
pub(crate) fn kit(dir: &Path, shown_as: &str, entry: Entry, overlay: &Overlay) -> Result<Program, Problems> {
    let Some((root, shown_root)) = find_root(dir, shown_as) else {
        return Err(Problems::unplaced(format!(
            "error: there's no kit.toml in {shown_as} or any folder above it\n  = help: `fennec new <name>` starts a kit, and `fennec run <file.fen>` runs a single file\n"
        )));
    };
    let manifest = read_manifest(&root, &shown_root).map_err(Problems::unplaced)?;
    let src = root.join("src");
    let mut loader = Loader::new(src.clone(), format!("{shown_root}src/"), overlay);
    let main = src.join("main.fen");
    if main.is_file() || overlay.contains_key(&main) {
        loader.visit("main", &main, format!("{shown_root}src/main.fen"), None);
    } else if entry == Entry::Main {
        return Err(Problems::unplaced(format!(
            "error: {shown_root}src/main.fen doesn't exist\n  = help: a kit runs `main` from src/main.fen\n"
        )));
    }
    if entry == Entry::Everything {
        let mut modules = Vec::new();
        find_modules(&src, &mut Vec::new(), &mut modules);
        modules.sort();
        loader.parse_ahead(modules.clone());
        for module in modules {
            let (real, shown) = loader.module_path(&module);
            loader.visit(&module.join("."), &real, shown, None);
        }
    }
    loader.finish(Some(manifest))
}

/// The text of every file of a kit, without parsing any of it, by path. None if `dir` isn't in a
/// kit, or a file can't be read: loading it will say why.
pub(crate) fn kit_texts(dir: &Path) -> Option<Vec<(String, String)>> {
    let (root, _) = find_root(dir, ".")?;
    let src = root.join("src");
    let mut modules = Vec::new();
    find_modules(&src, &mut Vec::new(), &mut modules);
    modules.sort();
    let mut texts = Vec::with_capacity(modules.len() + 1);
    let manifest = root.join("kit.toml");
    let paths = modules.iter().map(|module| {
        let mut path = src.clone();
        path.extend(module);
        path.set_extension("fen");
        path
    });
    for path in paths.chain(std::iter::once(manifest)) {
        let text = std::fs::read_to_string(&path).ok()?;
        texts.push((path.to_string_lossy().into_owned(), text));
    }
    Some(texts)
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

/// The stack each thread parsing files gets, as much as the thread parsing one file has. It's
/// only reserved, not used, until a file nests deeply.
const PARSING_STACK: usize = 1 << 30;

/// Parses each text at its offset, in order, on as many threads as there are CPUs.
fn parse_all(texts: &[(String, u32)]) -> Vec<Result<Module, Diagnostic>> {
    let parse = |chunk: &[(String, u32)]| -> Vec<Result<Module, Diagnostic>> {
        chunk.iter().map(|(text, base)| fennec_syntax::parse_at(text, *base)).collect()
    };
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if threads < 2 || texts.len() < 2 {
        return parse(texts);
    }
    let parse = &parse;
    std::thread::scope(|scope| {
        let workers: Vec<_> = texts
            .chunks(texts.len().div_ceil(threads))
            .map(|chunk| {
                let worker = std::thread::Builder::new()
                    .stack_size(PARSING_STACK)
                    .spawn_scoped(scope, move || parse(chunk));
                (chunk, worker)
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|(chunk, worker)| match worker.map(std::thread::ScopedJoinHandle::join) {
                Ok(Ok(parsed)) => parsed,
                // A thread that couldn't start or that died: this one finds out what happened.
                _ => parse(chunk),
            })
            .collect()
    })
}

struct Loader<'a> {
    /// Text read in place of files on disk.
    overlay: &'a Overlay,
    /// The folder module paths resolve against.
    src: PathBuf,
    /// How `src` is shown before a module's path.
    shown_src: String,
    sources: Sources,
    files: Vec<File>,
    seen: HashSet<String>,
    /// The modules being loaded, outermost first, to find imports that go in a circle.
    stack: Vec<String>,
    /// Modules parsed ahead of being visited, by name.
    parsed: HashMap<String, Result<Module, Diagnostic>>,
    errors: Vec<Diagnostic>,
    /// Problems with no place in a file to point at.
    unplaced: Vec<String>,
}

impl<'a> Loader<'a> {
    fn new(src: PathBuf, shown_src: String, overlay: &'a Overlay) -> Self {
        Loader {
            overlay,
            src,
            shown_src,
            sources: Sources::new(),
            files: Vec::new(),
            seen: HashSet::new(),
            stack: Vec::new(),
            parsed: HashMap::new(),
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
        let parsed = if let Some(parsed) = self.parsed.remove(name) {
            parsed
        } else {
            match self.read(real) {
                Ok(text) => fennec_syntax::parse_at(&text, self.sources.add(shown.clone(), text.as_str())),
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
            }
        };
        let module = match parsed {
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
                Item::Import(import) if !is_std(import) => Some(self.imported_modules(import)),
                _ => None,
            })
            .flatten()
            .collect();
        self.files.push(File { name: name.to_owned(), shown, module });
        self.parse_ahead(imports.iter().map(|(module, _)| module.clone()).collect());
        self.stack.push(name.to_owned());
        for (module, span) in imports {
            let (real, shown) = self.module_path(&module);
            self.visit(&module.join("."), &real, shown, Some(span));
        }
        self.stack.pop();
    }

    /// Parses the modules that are about to be visited, side by side. One that can't be read
    /// is left for its visit to report.
    fn parse_ahead(&mut self, modules: Vec<Vec<String>>) {
        let mut names = Vec::new();
        let mut texts = Vec::new();
        for module in modules {
            let name = module.join(".");
            if self.seen.contains(&name) || self.parsed.contains_key(&name) || names.contains(&name) {
                continue;
            }
            let (real, shown) = self.module_path(&module);
            if let Ok(text) = self.read(&real) {
                let base = self.sources.add(shown, text.as_str());
                names.push(name);
                texts.push((text, base));
            }
        }
        self.parsed.extend(names.into_iter().zip(parse_all(&texts)));
    }

    /// The modules an import loads. `from game import physics` loads `game/physics.fen` when
    /// there is one, as in Python, and otherwise takes `physics` from `game.fen`.
    fn imported_modules(&self, import: &Import) -> Vec<(Vec<String>, Span)> {
        let mut modules = Vec::new();
        let mut from_parent = import.names.is_empty();
        for name in &import.names {
            let submodule: Vec<String> = import.module.iter().chain([name]).cloned().collect();
            if self.exists(&self.module_path(&submodule).0) {
                modules.push((submodule, import.span));
            } else {
                from_parent = true;
            }
        }
        if from_parent {
            modules.push((import.module.clone(), import.span));
        }
        modules
    }

    /// A file's text, from the overlay when it's there.
    fn read(&self, path: &Path) -> std::io::Result<String> {
        self.overlay.get(path).map_or_else(|| std::fs::read_to_string(path), |text| Ok(text.clone()))
    }

    fn exists(&self, path: &Path) -> bool {
        self.overlay.contains_key(path) || path.is_file()
    }

    /// Checks names across the files and merges them into one module.
    fn finish(mut self, manifest: Option<Manifest>) -> Result<Program, Problems> {
        if !self.unplaced.is_empty() {
            return Err(Problems {
                sources: self.sources,
                diagnostics: self.errors,
                unplaced: self.unplaced,
            });
        }
        if self.errors.is_empty() {
            scope::resolve(&mut self.files, &mut self.errors);
        }
        if !self.errors.is_empty() {
            self.errors.sort_by_key(|diagnostic| diagnostic.span.start);
            return Err(Problems { sources: self.sources, diagnostics: self.errors, unplaced: Vec::new() });
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
