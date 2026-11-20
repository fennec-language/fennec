//! Finding a kit, reading each file once, and following imports from file to file.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use fennec_syntax::{Diagnostic, Import, Item, Module, Sources, Span};

use crate::kept::{FileKey, KeptFile, KeptLoad, Used};
use crate::{Embedded, Entry, Manifest, Overlay, Problems, Program, embed, scope};

/// A parsed file of a program.
pub(crate) struct File {
    /// Its module name, like `enemies.ai`.
    pub(crate) name: String,
    /// Its path as messages show it.
    pub(crate) shown: String,
    /// Shared with what a load keeps for the next, so a rewriting copies it before changing it.
    pub(crate) module: Arc<Module>,
    /// What starts the names its kit defines in the merged program: its name and the
    /// `fennec_syntax::KIT_MARK` for a dependency, and nothing for the program's own kit.
    pub(crate) prefix: String,
    /// The names it marks `export`, which other kits can use. Only a dependency's are read.
    pub(crate) exports: HashSet<String>,
    /// What's kept of it for the next load, in a load that keeps.
    keep: Option<Keep>,
}

/// What a load keeps of a file for the next: what it's kept by, its imports as written, and its
/// module before names across files were rewritten. A file that embeds another's bytes isn't
/// kept, since what it holds can change while it doesn't.
struct Keep {
    key: FileKey,
    written: Arc<[Import]>,
    parsed: Arc<Module>,
}

/// What a load uses again from the load before, and whether it keeps what it finds for the next.
#[derive(Clone, Copy, Default)]
pub(crate) struct Keeping<'a> {
    pub(crate) before: Option<&'a KeptLoad>,
    pub(crate) keep: bool,
}

/// A file's module: parsed, or found as the load before kept it.
enum Parsed {
    Fresh(Module),
    /// Its module with its imports by their whole names, and its imports as written.
    Kept(Arc<Module>, Arc<[Import]>),
}

/// A file read and parsed, or found kept, with what it's kept by in a load that keeps or uses
/// what's kept.
struct Loaded {
    key: Option<FileKey>,
    parsed: Result<Parsed, Diagnostic>,
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

    /// What a file's parse depends on of its kit, past its own text: its name, and the kits it
    /// names, which together say how its imports are written by their whole names.
    fn kept_as(&self) -> String {
        let mut uses: Vec<&str> = self.uses.iter().map(String::as_str).collect();
        uses.sort_unstable();
        format!("{}\0{}", self.name, uses.join("\0"))
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
    /// What `kit.lock` says each kit holds, which the checker holds the program to.
    holds: fennec_syntax::Holds,
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
    keeping: Keeping<'_>,
) -> Result<(Program, Option<KeptLoad>), Problems> {
    let src = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut loader =
        Loader::new(src, folder_prefix(Path::new(shown_as).parent()), None, overlay, Dependencies::default());
    loader.keeping = keeping;
    loader.doc_examples = entry == Entry::Tests;
    let name = path.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    loader.visit_own(&name, path, shown_as.to_owned(), None);
    loader.finish(None)
}

/// Loads a kit from its folder or any folder inside it.
pub(crate) fn kit(
    dir: &Path,
    shown_as: &str,
    entry: Entry,
    overlay: &Overlay,
    keeping: Keeping<'_>,
) -> Result<(Program, Option<KeptLoad>), Problems> {
    let Some((root, shown_root)) = find_root(dir, shown_as).map_err(|why| refused(&why))? else {
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
    loader.keeping = keeping;
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
        // A kit with no file of its own has nothing to check, lint or test, and passing it
        // without a word hid that its files were beside `kit.toml`, where no command reads them.
        if modules.is_empty() && !overlay.keys().any(|path| path.starts_with(&src)) {
            return Err(Problems::unplaced(no_files(&root, &shown_root)));
        }
        modules.sort();
        loader.parse_ahead(loader.own_modules(&modules));
        for module in modules {
            let (real, shown) = loader.own.path(&module);
            loader.visit_own(&module.join("."), &real, shown, None);
        }
    }
    let (mut program, kept) = loader.finish(Some(manifest))?;
    with_pack_lock(&mut program, &root);
    Ok((program, kept))
}

/// What a kit with no `.fen` file in `src/` says: where its files go, and which `.fen` files are
/// beside `kit.toml` instead, as a kit's are in other languages' layouts.
fn no_files(root: &Path, shown_root: &str) -> String {
    let mut beside: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "fen"))
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .collect();
    beside.sort();
    let help = if beside.is_empty() {
        format!("a kit's files are in {shown_root}src/, with `main` in src/main.fen")
    } else {
        let moves: Vec<String> =
            beside.iter().map(|name| format!("{shown_root}{name} to {shown_root}src/{name}")).collect();
        format!("a kit's files are in {shown_root}src/, so no command reads these: move {}", moves.join(", "))
    };
    format!("error: {shown_root}src/ has no .fen file, so there's nothing in this kit\n  = help: {help}\n")
}

/// Whether a program loaded from the kit at `dir` has every one of the kit's own modules, as
/// `kit` shows them.
pub(crate) fn holds_every_file(program: &Program, dir: &Path, shown_as: &str) -> bool {
    let Ok(Some((root, shown_root))) = find_root(dir, shown_as) else { return false };
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
    let (root, _) = find_root(dir, ".").ok()??;
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
    let Some((root, shown_root)) = find_root(dir, shown_as).map_err(|why| refused(&why))? else {
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
    loader.finish(Some(manifest)).map(|(program, _)| program)
}

/// Why a `kit.toml` that's there isn't taken, as the command's error.
fn refused(why: &str) -> Problems {
    Problems::unplaced(format!("error: {why}\n"))
}

/// The nearest folder at or above `dir` with a `kit.toml`, and how messages show it: by
/// trimming `shown_as` when it names that folder, and as an absolute path when it doesn't. An
/// error says why a `kit.toml` that's there isn't trusted.
fn find_root(dir: &Path, shown_as: &str) -> Result<Option<(PathBuf, String)>, String> {
    let Ok(absolute) = std::fs::canonicalize(dir) else { return Ok(None) };
    // Reached only through folders that are trusted: see `fennec_dist::kit_root`.
    let Some(found) = fennec_dist::kit_root(&absolute)? else { return Ok(None) };
    let Some((levels, root)) = absolute.ancestors().enumerate().find(|(_, folder)| *folder == found) else {
        return Ok(None);
    };
    let mut shown = PathBuf::from(shown_as);
    for _ in 0..levels {
        // Going up from `.` or `..` isn't trimming a name, so show the absolute path.
        if !matches!(shown.components().next_back(), Some(Component::Normal(_))) {
            return Ok(Some((root.to_path_buf(), folder_prefix(Some(root)))));
        }
        shown.pop();
    }
    Ok(Some((root.to_path_buf(), folder_prefix(Some(&shown)))))
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
    // Loading fetches nothing: opening, checking or running a project reaches no host it names.
    // `fennec fetch` gets what kit.lock pins, and each kit is checked against its pin here.
    let store = fennec_deps::Kits::of_this_machine()
        .ok_or_else(|| {
            Problems::unplaced(
                "error: there's no home folder to keep fetched kits in; set FENNEC_HOME\n".to_owned(),
            )
        })?
        .for_loading();
    let found = store.ensure(root).map_err(|message| Problems::unplaced(format!("error: {message}\n")))?;
    // What each kit was accepted as holding, which a kit handed more is stopped at.
    let lock = std::fs::read_to_string(root.join("kit.lock")).unwrap_or_default();
    let lines =
        fennec_deps::holds(&lock).map_err(|message| Problems::unplaced(format!("error: {message}\n")))?;
    // A line belongs to the source it was accepted for: a kit from elsewhere under the same name
    // holds nothing until its grants are accepted again.
    let mut holds = fennec_syntax::Holds::default();
    for line in lines {
        match found.iter().find(|kit| kit.name == line.kit) {
            Some(kit) if fennec_deps::source_of(kit, root) == line.source => {
                holds.accepted.push((line.kit, line.holds));
            }
            Some(_) => holds.moved.push(line.kit),
            None => {}
        }
    }
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
    Ok(Dependencies { uses, kits, holds })
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

/// A test is reported, kept as passed and run by its name, so two of the program's own tests of
/// one name, which two files may fairly use to say different things alike, are each told apart by
/// their module, and within one module by their line as well: `it works (in one)`. By the module,
/// not the path, so a test's name is the same however the kit is named to `fennec test`. Before,
/// neither ran (#1860).
fn name_tests_apart(files: &mut [File], sources: &Sources) {
    let mut places: HashMap<String, Vec<String>> = HashMap::new();
    for file in files.iter().filter(|file| file.prefix.is_empty()) {
        for item in &file.module.items {
            if let Item::Test(test) = item {
                places.entry(test.name.clone()).or_default().push(file.name.clone());
            }
        }
    }
    for file in files.iter_mut().filter(|file| file.prefix.is_empty()) {
        let renamed: Vec<(usize, String)> = file
            .module
            .items
            .iter()
            .enumerate()
            .filter_map(|(at, item)| {
                let Item::Test(test) = item else { return None };
                let found = places.get(&test.name).filter(|found| found.len() > 1)?;
                let place = if found.iter().filter(|module| **module == file.name).count() > 1 {
                    let line = sources.locate(test.span).map_or(0, |(_, line, ..)| line);
                    format!("in {}, line {line}", file.name)
                } else {
                    format!("in {}", file.name)
                };
                Some((at, format!("{} ({place})", test.name)))
            })
            .collect();
        if renamed.is_empty() {
            continue;
        }
        // A module kept for the next load is copied, since tests are named apart in each load.
        let module = Arc::make_mut(&mut file.module);
        for (at, name) in renamed {
            if let Some(Item::Test(test)) = module.items.get_mut(at) {
                test.name = name;
            }
        }
    }
}

/// Every `.fen` file under `dir`, as module paths.
///
/// A folder that's a link isn't gone into: one linked into itself, as `src/a -> .`, would be walked
/// again at every level until the path is too long, and two such double the walk at each. A file
/// that's a link is a module like any other.
pub(crate) fn find_modules(dir: &Path, prefix: &mut Vec<String>, found: &mut Vec<Vec<String>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { continue };
        // The entry's own kind, not what a link leads to.
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() && path.is_dir() {
            continue;
        }
        if kind.is_dir() {
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
/// Parses the file added to `sources` at `base`, or finds its parse kept, keyed by its text's
/// hash, which `sources` keeps for whatever else asks for it.
fn parse_file(sources: &Sources, base: u32, shown: &str, doc_examples: bool) -> Result<Module, Diagnostic> {
    let Some((text, hash)) = sources.hashed_text_at(base) else {
        return Err(Diagnostic::new(format!("`{shown}` wasn't read"), Span { start: base, end: base }));
    };
    let parses = crate::PARSES.get();
    if let Some(kept) = parses.and_then(|parses| parses.kept(&hash, shown, doc_examples, base)) {
        return Ok(kept);
    }
    let fresh = parsed(text, base, shown, doc_examples);
    if let (Some(parses), Ok(module)) = (parses, &fresh) {
        parses.keep(&hash, shown, doc_examples, base, module);
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

/// Parses each file of `sources` at the offsets given, shown as named, in order, on as many
/// threads as there are CPUs.
fn parse_all(
    sources: &Sources,
    files: &[(u32, String)],
    doc_examples: bool,
) -> Vec<Result<Module, Diagnostic>> {
    let parse = |chunk: &[(u32, String)]| -> Vec<Result<Module, Diagnostic>> {
        chunk.iter().map(|(base, shown)| parse_file(sources, *base, shown, doc_examples)).collect()
    };
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if threads < 2 || files.len() < 2 {
        return parse(files);
    }
    let parse = &parse;
    std::thread::scope(|scope| {
        let workers: Vec<_> = files
            .chunks(files.len().div_ceil(threads))
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
    /// What the load before kept, used again for each file that's as it was, and whether this
    /// load keeps what it finds for the next.
    keeping: Keeping<'a>,
    /// How many files' parses were found kept.
    parses_kept: usize,
    /// The kit being loaded.
    own: Rc<Place>,
    /// The kits it depends on, and those they depend on, by name.
    kits: HashMap<String, Rc<Place>>,
    /// What `kit.lock` says each kit holds, which the checker holds the program to.
    holds: fennec_syntax::Holds,
    /// Whether the examples in the doc comments of the kit being loaded become tests.
    doc_examples: bool,
    embedded: Vec<Embedded>,
    sources: Sources,
    files: Vec<File>,
    seen: HashSet<String>,
    /// The modules being loaded, outermost first, to find imports that go in a circle.
    stack: Vec<String>,
    /// Modules parsed ahead of being visited, by name.
    parsed: HashMap<String, Loaded>,
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
            keeping: Keeping::default(),
            parses_kept: 0,
            own: Rc::new(Place { name: String::new(), src, shown_src, root, uses: dependencies.uses }),
            kits: dependencies.kits,
            holds: dependencies.holds,
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
        let loaded = if let Some(loaded) = self.parsed.remove(name) {
            loaded
        } else {
            match self.read(real) {
                Ok(text) => {
                    let base = self.sources.add(shown.clone(), text);
                    let doc_examples = self.doc_examples && !place.is_dependency();
                    let key = self.key(place, base, &shown, doc_examples);
                    let parsed = match self.found_kept(name, key.as_ref()) {
                        Some(found) => Ok(found),
                        None => parse_file(&self.sources, base, &shown, doc_examples).map(Parsed::Fresh),
                    };
                    Loaded { key, parsed }
                }
                Err(err) => {
                    match from {
                        Some(span) => self.errors.push(
                            Diagnostic::new(format!("there's no module called `{name}`"), span).with_help(
                                fennec_syntax::python_module(name)
                                    .unwrap_or_else(|| format!("it would be the file {shown}")),
                            ),
                        ),
                        None => self.unplaced.push(format!("error: couldn't read {shown}: {err}\n")),
                    }
                    return;
                }
            }
        };
        let Loaded { key, parsed } = loaded;
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(diagnostic) => {
                self.errors.push(diagnostic);
                return;
            }
        };
        let (module, keep, imports) = self.taken(place, parsed, key, real, &shown);
        let prefix = place.prefix();
        self.files.push(File { name: name.to_owned(), shown, module, prefix, exports: HashSet::new(), keep });
        self.parse_ahead(imports.iter().map(|(wanted, _)| wanted.clone()).collect());
        self.stack.push(name.to_owned());
        for (wanted, span) in imports {
            let (real, shown) = wanted.place.path(&wanted.module);
            self.visit(&wanted.place, &wanted.name, &real, shown, Some(span));
        }
        self.stack.pop();
    }

    /// A file's module as the program takes it, with what's kept of it for the next load and the
    /// modules it imports. A parse has the files it embeds read in, and its imports written by
    /// their whole names. A kept module has had both: only its imports are followed again, since
    /// what they find can change.
    fn taken(
        &mut self,
        place: &Rc<Place>,
        parsed: Parsed,
        key: Option<FileKey>,
        real: &Path,
        shown: &str,
    ) -> (Arc<Module>, Option<Keep>, Vec<(Wanted, Span)>) {
        let mut imports = Vec::new();
        match parsed {
            Parsed::Fresh(mut module) => {
                let at =
                    embed::Place { file: real, shown, root: place.root.as_deref(), overlay: self.overlay };
                let embeds = embed::read_files(&mut module, &at, &mut self.embedded, &mut self.errors);
                let mut written = Vec::new();
                for item in &mut module.items {
                    let Item::Import(import) = item else { continue };
                    if !is_std(import) {
                        if self.keeping.keep {
                            written.push(import.clone());
                        }
                        imports.extend(self.imported_modules(place, import));
                        by_its_whole_name(place, import);
                    }
                }
                let module = Arc::new(module);
                let keep = key.filter(|_| self.keeping.keep && !embeds).map(|key| Keep {
                    key,
                    written: written.into(),
                    parsed: Arc::clone(&module),
                });
                (module, keep, imports)
            }
            Parsed::Kept(module, written) => {
                self.parses_kept += 1;
                for import in written.iter() {
                    imports.extend(self.imported_modules(place, import));
                }
                let keep = key.filter(|_| self.keeping.keep).map(|key| Keep {
                    key,
                    written,
                    parsed: Arc::clone(&module),
                });
                (module, keep, imports)
            }
        }
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
        let mut names: Vec<(String, Option<FileKey>)> = Vec::new();
        let mut texts = Vec::new();
        for wanted in modules {
            let name = wanted.name;
            if self.seen.contains(&name)
                || self.parsed.contains_key(&name)
                || names.iter().any(|(listed, _)| *listed == name)
            {
                continue;
            }
            let (real, shown) = wanted.place.path(&wanted.module);
            if let Ok(text) = self.read(&real) {
                let base = self.sources.add(shown.clone(), text);
                let key = self.key(&wanted.place, base, &shown, doc_examples);
                if let Some(found) = self.found_kept(&name, key.as_ref()) {
                    self.parsed.insert(name, Loaded { key, parsed: Ok(found) });
                    continue;
                }
                names.push((name, key));
                texts.push((base, shown));
            }
        }
        let parsed = parse_all(&self.sources, &texts, doc_examples);
        for ((name, key), parsed) in names.into_iter().zip(parsed) {
            self.parsed.insert(name, Loaded { key, parsed: parsed.map(Parsed::Fresh) });
        }
    }

    /// What the file added to `sources` at `base` is kept by, in a load that keeps or uses what's
    /// kept: what it is, where, and what its parse depends on of its kit.
    fn key(&self, place: &Place, base: u32, shown: &str, doc_examples: bool) -> Option<FileKey> {
        if self.keeping.before.is_none() && !self.keeping.keep {
            return None;
        }
        let (_, text) = self.sources.hashed_text_at(base)?;
        Some(FileKey { shown: shown.to_owned(), base, text, doc_examples, place: place.kept_as() })
    }

    /// The module `name` as the load before kept it, when it was kept by the same key.
    fn found_kept(&self, name: &str, key: Option<&FileKey>) -> Option<Parsed> {
        let kept = self.keeping.before?.files.get(name)?;
        (Some(&kept.key) == key).then(|| Parsed::Kept(Arc::clone(&kept.parsed), Arc::clone(&kept.written)))
    }

    /// The rewriting the load before kept for each file whose module is the one it kept, when the
    /// load has the same files in the same order: a file's place in it is what the rewriting read
    /// of the others by.
    fn rewritings_before(&self) -> Vec<Option<scope::Before>> {
        let Some(before) = self.keeping.before else { return Vec::new() };
        let same_files = before.order.len() == self.files.len()
            && before
                .order
                .iter()
                .zip(&self.files)
                .all(|((name, prefix), file)| *name == file.name && *prefix == file.prefix);
        if !same_files {
            return Vec::new();
        }
        self.files
            .iter()
            .map(|file| {
                let kept = before.files.get(&file.name)?;
                Arc::ptr_eq(&kept.parsed, &file.module).then(|| scope::Before {
                    module: Arc::clone(&kept.resolved),
                    reads: Arc::clone(&kept.reads),
                })
            })
            .collect()
    }

    /// What this load keeps for the next: each file kept, with its rewriting, and the files in
    /// order. Taken after names across files are rewritten, before tests are named apart.
    fn kept_for_next(&self, resolved: scope::Resolved) -> KeptLoad {
        let files = self
            .files
            .iter()
            .zip(resolved.reads)
            .filter_map(|(file, reads)| {
                let keep = file.keep.as_ref()?;
                let kept = KeptFile {
                    key: keep.key.clone(),
                    written: Arc::clone(&keep.written),
                    parsed: Arc::clone(&keep.parsed),
                    resolved: Arc::clone(&file.module),
                    reads: reads?,
                };
                Some((file.name.clone(), kept))
            })
            .collect();
        KeptLoad {
            files,
            order: self.files.iter().map(|file| (file.name.clone(), file.prefix.clone())).collect(),
            used: Used { files: self.files.len(), parses: self.parses_kept, rewritings: resolved.kept },
        }
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

    /// Checks names across the kits' files and merges them into one module, with what it keeps
    /// for the next load in a load that keeps.
    fn finish(mut self, manifest: Option<Manifest>) -> Result<(Program, Option<KeptLoad>), Problems> {
        // Past this, a program's own text would be read as what the compiler writes in.
        if self.sources.too_long() {
            let message = "the program and its kits are more source than Fennec builds: past 6,144 files of up to 256 KiB, the rest have 256 MiB together";
            let help = "split it into programs of their own (#2114)";
            self.errors.push(Diagnostic::new(message, Span::default()).with_help(help));
        }
        if !self.unplaced.is_empty() {
            return Err(Problems {
                sources: self.sources,
                diagnostics: self.errors,
                unplaced: self.unplaced,
            });
        }
        let mut resolved = None;
        if self.errors.is_empty() {
            let texts: HashMap<&str, &str> = self.sources.files().collect();
            for file in self.files.iter_mut().filter(|file| !file.prefix.is_empty()) {
                file.exports = texts.get(file.shown.as_str()).map(|text| exported(text)).unwrap_or_default();
            }
            let before = self.rewritings_before();
            resolved = Some(scope::resolve(&mut self.files, &mut self.errors, before, self.keeping.keep));
        }
        if !self.errors.is_empty() {
            self.errors.sort_by_key(|diagnostic| diagnostic.span.start);
            return Err(Problems { sources: self.sources, diagnostics: self.errors, unplaced: Vec::new() });
        }
        let kept = resolved.filter(|_| self.keeping.keep).map(|resolved| self.kept_for_next(resolved));
        let dependency_files =
            self.files.iter().filter(|file| !file.prefix.is_empty()).map(|file| file.shown.clone()).collect();
        name_tests_apart(&mut self.files, &self.sources);
        // A dependency's tests are its own to run.
        let items = self
            .files
            .into_iter()
            .flat_map(|file| {
                let own = file.prefix.is_empty();
                // Copied when it's kept for the next load.
                Arc::unwrap_or_clone(file.module)
                    .items
                    .into_iter()
                    .filter(move |item| own || !matches!(item, Item::Test(_)))
            })
            .filter(|item| !matches!(item, Item::Import(import) if !is_std(import)))
            .collect();
        let kit_src = manifest.as_ref().map(|_| (self.own.src.clone(), self.own.shown_src.clone()));
        let program = Program {
            module: Module { items, holds: self.holds, files: self.sources.spans(), ..Default::default() },
            sources: self.sources,
            manifest,
            embedded: self.embedded,
            kit_src,
            dependency_files,
        };
        Ok((program, kept))
    }
}
