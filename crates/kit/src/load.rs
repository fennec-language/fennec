//! Finding a kit, reading each file once, and following imports from file to file.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Import, Item, Module, Sources, Span};

use crate::{Embedded, Entry, Manifest, Overlay, Problems, Program, embed, scope};

/// A parsed file of a program.
pub(crate) struct File {
    /// Its module name, like `enemies.ai`.
    pub(crate) name: String,
    /// Its path as messages show it.
    pub(crate) shown: String,
    pub(crate) module: Module,
    /// What starts the names its kit defines in the merged program: its name and the
    /// `fennec_syntax::KIT_MARK` for a dependency, and nothing for the program's own kit.
    pub(crate) prefix: String,
    /// The names it marks `export`, which other kits can use. Only a dependency's are read.
    pub(crate) exports: HashSet<String>,
}

/// A kit whose files are being loaded: the program's own, or one it depends on.
struct Place {
    /// What other kits import it as, like `physics`. Nothing for the program's own kit.
    name: String,
    /// The folder its module paths resolve against.
    src: PathBuf,
    /// How `src` is shown before a module's path.
    shown_src: String,
    /// Its folder, which `embed` can't reach out of. A single file has none.
    root: Option<PathBuf>,
    /// The kits its `kit.toml` names, which are the only ones it can import.
    uses: HashSet<String>,
}

impl Place {
    /// Where a module's file is, and how it's shown.
    fn path(&self, module: &[String]) -> (PathBuf, String) {
        let mut real = self.src.clone();
        real.extend(module);
        real.set_extension("fen");
        (real, format!("{}{}.fen", self.shown_src, module.join("/")))
    }

    /// The name a module has across every kit: `shapes` in the program's own kit, `physics.shapes`
    /// in the kit `physics`, and just `physics` for that kit's `lib`.
    fn key(&self, module: &[String]) -> String {
        let path = module.join(".");
        match (self.name.as_str(), path.as_str()) {
            ("", _) => path,
            (kit, "lib") => kit.to_owned(),
            (kit, _) => format!("{kit}.{path}"),
        }
    }

    fn prefix(&self) -> String {
        if self.name.is_empty() { String::new() } else { format!("{}{}", self.name, fennec_syntax::KIT_MARK) }
    }

    fn is_dependency(&self) -> bool {
        !self.name.is_empty()
    }
}

/// A module to load, wherever it is.
#[derive(Clone)]
struct Wanted {
    place: Rc<Place>,
    /// Its path inside its kit.
    module: Vec<String>,
    /// Its name across every kit: see [`Place::key`].
    name: String,
}

/// The kits a kit can reach.
#[derive(Default)]
struct Dependencies {
    /// The ones it names itself.
    uses: HashSet<String>,
    /// All of them, and those they name, by name.
    kits: HashMap<String, Rc<Place>>,
}

/// Whether an import is of the standard library rather than of the program's own files.
pub(crate) fn is_std(import: &Import) -> bool {
    import.module.first().is_some_and(|first| first == "std")
}

/// Loads a single `.fen` file. Its imports resolve against its own folder.
pub(crate) fn file(
    path: &Path,
    shown_as: &str,
    entry: Entry,
    overlay: &Overlay,
) -> Result<Program, Problems> {
    let src = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut loader =
        Loader::new(src, folder_prefix(Path::new(shown_as).parent()), None, overlay, Dependencies::default());
    loader.doc_examples = entry == Entry::Tests;
    let name = path.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    loader.visit_own(&name, path, shown_as.to_owned(), None);
    loader.finish(None)
}

/// Loads a kit from its folder or any folder inside it.
pub(crate) fn kit(dir: &Path, shown_as: &str, entry: Entry, overlay: &Overlay) -> Result<Program, Problems> {
    let Some((root, shown_root)) = find_root(dir, shown_as) else {
        return Err(Problems::unplaced(format!(
            "error: there's no kit.toml in {shown_as} or any folder above it\n  = help: `fennec new <name>` starts a kit, and `fennec run <file.fen>` runs a single file\n"
        )));
    };
    let manifest = read_manifest(&root, &shown_root)?;
    let src = root.join("src");
    let dependencies = dependencies(&root, &manifest)?;
    let mut loader =
        Loader::new(src.clone(), format!("{shown_root}src/"), Some(&root), overlay, dependencies);
    loader.doc_examples = entry == Entry::Tests;
    let main = src.join("main.fen");
    if main.is_file() || overlay.contains_key(&main) {
        loader.visit_own("main", &main, format!("{shown_root}src/main.fen"), None);
    } else if entry == Entry::Main {
        return Err(Problems::unplaced(format!(
            "error: {shown_root}src/main.fen doesn't exist\n  = help: a kit runs `main` from src/main.fen\n"
        )));
    }
    if entry != Entry::Main {
        let mut modules = Vec::new();
        find_modules(&src, &mut Vec::new(), &mut modules);
        modules.sort();
        loader.parse_ahead(loader.own_modules(&modules));
        for module in modules {
            let (real, shown) = loader.own.path(&module);
            loader.visit_own(&module.join("."), &real, shown, None);
        }
    }
    let mut program = loader.finish(Some(manifest))?;
    with_pack_lock(&mut program, &root);
    Ok(program)
}

/// Whether a program loaded from the kit at `dir` has every one of the kit's own modules, as
/// `kit` shows them.
pub(crate) fn holds_every_file(program: &Program, dir: &Path, shown_as: &str) -> bool {
    let Some((root, shown_root)) = find_root(dir, shown_as) else { return false };
    let mut modules = Vec::new();
    find_modules(&root.join("src"), &mut Vec::new(), &mut modules);
    let loaded: HashSet<&str> = program.sources.files().map(|(shown, _)| shown).collect();
    modules.iter().all(|module| loaded.contains(format!("{shown_root}src/{}.fen", module.join("/")).as_str()))
}

/// The name a kit's `pack.lock` is held under in its program: one no program can write.
pub const PACK_LOCK: &str = "\0pack_lock";

/// Gives the program its kit's `pack.lock`, the layouts of what it saved in `std.pack` before,
/// as an embedded file, so a change to it is a change to the program.
fn with_pack_lock(program: &mut Program, root: &Path) {
    let path = root.join("pack.lock");
    let Ok(data) = std::fs::read(&path) else { return };
    let blob = fennec_syntax::Blob::from(data);
    let at = Span { start: Span::PRELUDE, end: Span::PRELUDE };
    program.module.items.push(Item::Const {
        name: PACK_LOCK.to_owned(),
        ty: None,
        value: fennec_syntax::Expr { kind: fennec_syntax::ExprKind::Bytes(blob.clone()), span: at },
        span: at,
    });
    program.embedded.push(Embedded { path, data: blob });
}

/// The text of every file of a kit, without parsing any of it, by its module name, with the
/// manifest as `kit.toml`. None if `dir` isn't in a kit, or a file can't be read: loading it
/// will say why.
pub(crate) fn kit_texts(dir: &Path) -> Option<Vec<(String, String)>> {
    let paths = kit_paths(dir)?;
    paths.into_iter().map(|(module, path)| Some((module, std::fs::read_to_string(path).ok()?))).collect()
}

/// Where each file of a kit is, by module name, with the manifest as `kit.toml`.
pub(crate) fn kit_paths(dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    let (root, _) = find_root(dir, ".")?;
    let src = root.join("src");
    let mut modules = Vec::new();
    find_modules(&src, &mut Vec::new(), &mut modules);
    modules.sort();
    let mut paths = Vec::with_capacity(modules.len() + 1);
    for module in modules {
        let mut path = src.clone();
        path.extend(&module);
        path.set_extension("fen");
        paths.push((module.join("."), path));
    }
    paths.push(("kit.toml".to_owned(), root.join("kit.toml")));
    // The program holds it, so one appearing or changing changes the program.
    let lock = root.join("pack.lock");
    if lock.is_file() {
        paths.push(("pack.lock".to_owned(), lock));
    }
    paths.extend(dependency_paths(&root));
    Some(paths)
}

/// The files outside a kit's own that a change to changes the program: its `kit.lock`, and the files
/// of its dependencies from paths, since those from git are pinned by `kit.lock`. They're named as
/// the files `embed` reads are, so a change to one means looking at the whole kit again.
fn dependency_paths(root: &Path) -> Vec<(String, PathBuf)> {
    let mut paths = Vec::new();
    let lock = root.join("kit.lock");
    if lock.is_file() {
        paths.push((crate::embedded_name(&lock), lock));
    }
    let has_some = read_manifest(root, "").is_ok_and(|manifest| !manifest.dependencies.is_empty());
    // Dependencies that can't be found are for loading the kit to report.
    let found = fennec_deps::Kits::of_this_machine()
        .filter(|_| has_some)
        .and_then(|kits| kits.ensure(root).ok())
        .unwrap_or_default();
    for kit in found.iter().filter(|kit| kit.pin.is_none()) {
        let src = kit.folder.join("src");
        let mut modules = Vec::new();
        find_modules(&src, &mut Vec::new(), &mut modules);
        modules.sort();
        let files = modules.into_iter().map(|module| {
            let mut path = src.clone();
            path.extend(&module);
            path.with_extension("fen")
        });
        for path in files.chain([kit.folder.join("kit.toml")]) {
            paths.push((crate::embedded_name(&path), path));
        }
    }
    paths
}

/// Loads some of a kit's modules and what they import, by name. What they don't import can't
/// reach them.
pub(crate) fn kit_modules(
    dir: &Path,
    shown_as: &str,
    modules: &[String],
    overlay: &Overlay,
) -> Result<Program, Problems> {
    some_of_a_kit(dir, shown_as, modules, overlay, false)
}

/// Some of a kit's modules, with the examples in their doc comments as tests.
pub(crate) fn kit_modules_to_test(
    dir: &Path,
    shown_as: &str,
    modules: &[String],
) -> Result<Program, Problems> {
    some_of_a_kit(dir, shown_as, modules, &Overlay::new(), true)
}

fn some_of_a_kit(
    dir: &Path,
    shown_as: &str,
    modules: &[String],
    overlay: &Overlay,
    doc_examples: bool,
) -> Result<Program, Problems> {
    let Some((root, shown_root)) = find_root(dir, shown_as) else {
        return Err(Problems::unplaced("error: this isn't in a kit\n".to_owned()));
    };
    let manifest = read_manifest(&root, &shown_root)?;
    let dependencies = dependencies(&root, &manifest)?;
    let mut loader =
        Loader::new(root.join("src"), format!("{shown_root}src/"), Some(&root), overlay, dependencies);
    loader.doc_examples = doc_examples;
    let modules: Vec<Vec<String>> =
        modules.iter().map(|module| module.split('.').map(str::to_owned).collect()).collect();
    loader.parse_ahead(loader.own_modules(&modules));
    for module in modules {
        let (real, shown) = loader.own.path(&module);
        loader.visit_own(&module.join("."), &real, shown, None);
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

fn read_manifest(root: &Path, shown_root: &str) -> Result<Manifest, Problems> {
    let shown = format!("{shown_root}kit.toml");
    let text = std::fs::read_to_string(root.join("kit.toml"))
        .map_err(|err| Problems::unplaced(format!("error: couldn't read {shown}: {err}\n")))?;
    fennec_deps::manifest::parse(&text, 0).map_err(|diagnostics| {
        let mut sources = Sources::new();
        sources.add(shown, text);
        Problems { sources, diagnostics, unplaced: Vec::new() }
    })
}

/// Finds the kits a kit depends on, fetching any that `kit.lock` pins and this machine hasn't got.
/// A kit that depends on nothing reads nothing more.
fn dependencies(root: &Path, manifest: &Manifest) -> Result<Dependencies, Problems> {
    if manifest.dependencies.is_empty() {
        return Ok(Dependencies::default());
    }
    let store = fennec_deps::Kits::of_this_machine().ok_or_else(|| {
        Problems::unplaced(
            "error: there's no home folder to keep fetched kits in; set FENNEC_HOME\n".to_owned(),
        )
    })?;
    let found = store.ensure(root).map_err(|message| Problems::unplaced(format!("error: {message}\n")))?;
    let kits = found
        .into_iter()
        .map(|kit| {
            let place = Place {
                src: kit.folder.join("src"),
                shown_src: format!("{}src/", folder_prefix(Some(&kit.folder))),
                root: Some(kit.folder),
                uses: kit.uses.into_iter().collect(),
                name: kit.name.clone(),
            };
            (kit.name, Rc::new(place))
        })
        .collect();
    let uses = manifest.dependencies.iter().map(|dependency| dependency.name.clone()).collect();
    Ok(Dependencies { uses, kits })
}

/// The names a file of a dependency marks `export`. A kit keeps the first word of each such line,
/// as the standard library's modules are read: only a name it also defines means anything.
fn exported(text: &str) -> HashSet<String> {
    const KEYWORDS: [&str; 7] = ["def ", "struct ", "shared ", "enum ", "error ", "interface ", "type "];
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.lines()
        .filter_map(|line| line.strip_prefix("export "))
        .map(|rest| KEYWORDS.iter().find_map(|keyword| rest.strip_prefix(keyword)).unwrap_or(rest))
        .map(|rest| rest.chars().take_while(|c| word(*c)).collect())
        .collect()
}

/// Every `.fen` file under `dir`, as module paths.
pub(crate) fn find_modules(dir: &Path, prefix: &mut Vec<String>, found: &mut Vec<Vec<String>>) {
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

/// Parses a file at its offset, with the examples in its doc comments as tests if asked, or takes
/// its parse from where parses are kept, when one of its text is.
fn parse_file(text: &str, base: u32, shown: &str, doc_examples: bool) -> Result<Module, Diagnostic> {
    let parses = crate::PARSES.get();
    if let Some(kept) = parses.and_then(|parses| parses.kept(text, shown, doc_examples, base)) {
        return Ok(kept);
    }
    let fresh = parsed(text, base, shown, doc_examples);
    if let (Some(parses), Ok(module)) = (parses, &fresh) {
        parses.keep(text, shown, doc_examples, base, module);
    }
    fresh
}

/// Parses a file at its offset, with the examples in its doc comments as tests if asked.
fn parsed(text: &str, base: u32, shown: &str, doc_examples: bool) -> Result<Module, Diagnostic> {
    let mut module = fennec_syntax::parse_at(text, base)?;
    if doc_examples {
        module.items.extend(fennec_syntax::doc_examples(text, base, shown)?);
    }
    Ok(module)
}

/// Parses each text, shown as named, at its offset, in order, on as many threads as there are CPUs.
fn parse_all(texts: &[(String, u32, String)], doc_examples: bool) -> Vec<Result<Module, Diagnostic>> {
    let parse = |chunk: &[(String, u32, String)]| -> Vec<Result<Module, Diagnostic>> {
        chunk.iter().map(|(text, base, shown)| parse_file(text, *base, shown, doc_examples)).collect()
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

/// Writes an import in a dependency's own files by its whole name, so `import shapes` in `physics`
/// is of the module `physics.shapes`, which it still calls `shapes`.
fn by_its_whole_name(place: &Place, import: &mut Import) {
    let another_kit = import.module.first().is_some_and(|first| place.uses.contains(first));
    if !place.is_dependency() || another_kit {
        return;
    }
    if import.names.is_empty() && import.alias.is_none() {
        import.alias = Some(import.module.join("."));
    }
    import.module = place.key(&import.module).split('.').map(str::to_owned).collect();
}

struct Loader<'a> {
    /// Text read in place of files on disk.
    overlay: &'a Overlay,
    /// The kit being loaded.
    own: Rc<Place>,
    /// The kits it depends on, and those they depend on, by name.
    kits: HashMap<String, Rc<Place>>,
    /// Whether the examples in the doc comments of the kit being loaded become tests.
    doc_examples: bool,
    embedded: Vec<Embedded>,
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
    fn new(
        src: PathBuf,
        shown_src: String,
        root: Option<&Path>,
        overlay: &'a Overlay,
        dependencies: Dependencies,
    ) -> Self {
        let root = root.map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()));
        Loader {
            overlay,
            own: Rc::new(Place { name: String::new(), src, shown_src, root, uses: dependencies.uses }),
            kits: dependencies.kits,
            doc_examples: false,
            embedded: Vec::new(),
            sources: Sources::new(),
            files: Vec::new(),
            seen: HashSet::new(),
            stack: Vec::new(),
            parsed: HashMap::new(),
            errors: Vec::new(),
            unplaced: Vec::new(),
        }
    }

    /// These modules of the kit being loaded.
    fn own_modules(&self, modules: &[Vec<String>]) -> Vec<Wanted> {
        modules.iter().map(|module| self.target(&self.own, module)).collect()
    }

    /// The module that a path written in a file of `place` means: one of another kit when it
    /// starts with the name of a kit that `place` depends on, and otherwise one of its own.
    fn target(&self, place: &Rc<Place>, written: &[String]) -> Wanted {
        if let Some((first, rest)) = written.split_first()
            && place.uses.contains(first)
            && let Some(kit) = self.kits.get(first)
        {
            let module = if rest.is_empty() { vec!["lib".to_owned()] } else { rest.to_vec() };
            return Wanted { name: kit.key(&module), place: Rc::clone(kit), module };
        }
        Wanted { name: place.key(written), place: Rc::clone(place), module: written.to_vec() }
    }

    fn visit_own(&mut self, name: &str, real: &Path, shown: String, from: Option<Span>) {
        let own = Rc::clone(&self.own);
        self.visit(&own, name, real, shown, from);
    }

    /// Loads a module of `place` and what it imports, unless it's loaded already. `from` is the
    /// import that asked for it.
    fn visit(&mut self, place: &Rc<Place>, name: &str, real: &Path, shown: String, from: Option<Span>) {
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
                Ok(text) => {
                    let base = self.sources.add(shown.clone(), text.as_str());
                    parse_file(&text, base, &shown, self.doc_examples && !place.is_dependency())
                }
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
        let mut module = match parsed {
            Ok(module) => module,
            Err(diagnostic) => {
                self.errors.push(diagnostic);
                return;
            }
        };
        let at =
            embed::Place { file: real, shown: &shown, root: place.root.as_deref(), overlay: self.overlay };
        embed::read_files(&mut module, &at, &mut self.embedded, &mut self.errors);
        let mut imports = Vec::new();
        for item in &mut module.items {
            let Item::Import(import) = item else { continue };
            if !is_std(import) {
                imports.extend(self.imported_modules(place, import));
                by_its_whole_name(place, import);
            }
        }
        let prefix = place.prefix();
        self.files.push(File { name: name.to_owned(), shown, module, prefix, exports: HashSet::new() });
        self.parse_ahead(imports.iter().map(|(wanted, _)| wanted.clone()).collect());
        self.stack.push(name.to_owned());
        for (wanted, span) in imports {
            let (real, shown) = wanted.place.path(&wanted.module);
            self.visit(&wanted.place, &wanted.name, &real, shown, Some(span));
        }
        self.stack.pop();
    }

    /// Parses the modules that are about to be visited, side by side. One that can't be read
    /// is left for its visit to report. Only the kit being loaded has its doc examples run.
    fn parse_ahead(&mut self, modules: Vec<Wanted>) {
        let (others, own): (Vec<Wanted>, Vec<Wanted>) =
            modules.into_iter().partition(|wanted| wanted.place.is_dependency());
        self.parse_these(own, self.doc_examples);
        self.parse_these(others, false);
    }

    fn parse_these(&mut self, modules: Vec<Wanted>, doc_examples: bool) {
        let mut names = Vec::new();
        let mut texts = Vec::new();
        for wanted in modules {
            let name = wanted.name;
            if self.seen.contains(&name) || self.parsed.contains_key(&name) || names.contains(&name) {
                continue;
            }
            let (real, shown) = wanted.place.path(&wanted.module);
            if let Ok(text) = self.read(&real) {
                let base = self.sources.add(shown.clone(), text.as_str());
                names.push(name);
                texts.push((text, base, shown));
            }
        }
        self.parsed.extend(names.into_iter().zip(parse_all(&texts, doc_examples)));
    }

    /// The modules an import loads. `from game import physics` loads `game/physics.fen` when
    /// there is one, as in Python, and otherwise takes `physics` from `game.fen`.
    fn imported_modules(&mut self, place: &Rc<Place>, import: &Import) -> Vec<(Wanted, Span)> {
        if let Some(first) = import.module.first().filter(|first| place.uses.contains(*first)) {
            let (file, _) = place.path(std::slice::from_ref(first));
            if self.exists(&file) || file.with_extension("").is_dir() {
                self.errors.push(
                    Diagnostic::new(
                        format!("`{first}` is both a kit this one depends on and a module of its own"),
                        import.span,
                    )
                    .with_help("rename the module, or name the dependency something else in kit.toml"),
                );
            }
        }
        let mut modules = Vec::new();
        let mut from_parent = import.names.is_empty();
        for name in &import.names {
            let submodule: Vec<String> = import.module.iter().chain([name]).cloned().collect();
            let wanted = self.target(place, &submodule);
            if self.exists(&wanted.place.path(&wanted.module).0) {
                modules.push((wanted, import.span));
            } else {
                from_parent = true;
            }
        }
        if from_parent {
            modules.push((self.target(place, &import.module), import.span));
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

    /// Checks names across the kits' files and merges them into one module.
    fn finish(mut self, manifest: Option<Manifest>) -> Result<Program, Problems> {
        if !self.unplaced.is_empty() {
            return Err(Problems {
                sources: self.sources,
                diagnostics: self.errors,
                unplaced: self.unplaced,
            });
        }
        if self.errors.is_empty() {
            let texts: HashMap<&str, &str> = self.sources.files().collect();
            for file in self.files.iter_mut().filter(|file| !file.prefix.is_empty()) {
                file.exports = texts.get(file.shown.as_str()).map(|text| exported(text)).unwrap_or_default();
            }
            scope::resolve(&mut self.files, &mut self.errors);
        }
        if !self.errors.is_empty() {
            self.errors.sort_by_key(|diagnostic| diagnostic.span.start);
            return Err(Problems { sources: self.sources, diagnostics: self.errors, unplaced: Vec::new() });
        }
        let dependency_files =
            self.files.iter().filter(|file| !file.prefix.is_empty()).map(|file| file.shown.clone()).collect();
        // A dependency's tests are its own to run.
        let items = self
            .files
            .into_iter()
            .flat_map(|file| {
                let own = file.prefix.is_empty();
                file.module.items.into_iter().filter(move |item| own || !matches!(item, Item::Test(_)))
            })
            .filter(|item| !matches!(item, Item::Import(import) if !is_std(import)))
            .collect();
        let kit_src = manifest.as_ref().map(|_| (self.own.src.clone(), self.own.shown_src.clone()));
        Ok(Program {
            module: Module { items, ..Default::default() },
            sources: self.sources,
            manifest,
            embedded: self.embedded,
            kit_src,
            dependency_files,
        })
    }
}
