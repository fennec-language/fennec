//! Getting from an edit to a verdict without looking at the rest of the kit. When an edit only
//! changes the bodies of functions, nothing outside those bodies can have broken, so it's enough
//! to load the changed files, the tests that ran those functions, and what they import.
//!
//! This only works from a kit whose tests all passed, and it gives up on anything else, which
//! leaves the whole program to load and check: an edit that reaches further, a program that
//! isn't a kit, or a mistake, which the whole program reports best.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use fennec_build::{Cache, KitFile, KitState, Row, TestRecord};
use fennec_check::Signatures;
use fennec_kit::Program;
use fennec_syntax::{Item, Span};

const BUILD: &str = env!("FENNEC_BUILD");

/// What a run has to do: the program to run it on, the tests to run, and the rest.
pub(crate) struct Plan {
    pub(crate) program: Program,
    pub(crate) signatures: Signatures,
    pub(crate) wanted: Vec<String>,
    pub(crate) unchanged: Vec<TestRecord>,
}

/// A kit when its tests last passed, if they did.
pub(crate) fn last_state((cache, name): (&Cache, &str)) -> Option<KitState> {
    cache.kit_state(name, BUILD)
}

/// One of a kit's files as it is now. Its text is only read if its size and when it changed don't
/// already say what it is.
fn kit_file(module: String, path: &Path, state: Option<&KitState>) -> Option<KitFile> {
    let metadata = std::fs::metadata(path).ok()?;
    let changed_at =
        metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos());
    let size = metadata.len();
    let known = state.and_then(|state| state.same_text(&module, size, changed_at));
    let text = match known {
        Some(text) => text,
        None => fennec_blake3::hash(std::fs::read(path).ok()?.as_slice()),
    };
    Some(KitFile { module, text, size, changed_at })
}

/// Each of a kit's files as they are now, or None if this isn't a kit.
pub(crate) fn kit_files(path: &str, state: Option<&KitState>) -> Option<Vec<KitFile>> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return None;
    }
    let paths = fennec_kit::kit_paths(dir)?;
    paths.into_iter().map(|(module, path)| kit_file(module, &path, state)).collect()
}

/// Whether a kit's files have the text they had when `state` was noted.
pub(crate) fn same_files(state: &KitState, files: &[KitFile]) -> bool {
    state.files.len() == files.len()
        && files.iter().all(|file| state.files.get(&file.module).is_some_and(|known| known.text == file.text))
}

/// A kit's modules, to tell which one a file holds from how it's shown.
pub(crate) struct Modules<'f>(HashSet<&'f str>);

impl<'f> Modules<'f> {
    pub(crate) fn of(files: &'f [KitFile]) -> Self {
        Modules(files.iter().map(|file| file.module.as_str()).collect())
    }

    /// The module a file shown like this holds: what follows `src/`, wherever that is in a path
    /// that may have another `src` folder before it.
    pub(crate) fn of_file(&self, shown: &str) -> Option<&'f str> {
        let path = shown.strip_suffix(".fen")?;
        path.match_indices("src/").find_map(|(at, _)| {
            let module = path.get(at + 4..)?.replace('/', ".");
            self.0.get(module.as_str()).copied()
        })
    }
}

/// Where an item is written.
fn span_of(item: &Item) -> Span {
    match item {
        Item::Function(def) => def.span,
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.span,
        Item::Interface(interface) => interface.span,
        Item::Extend(extension) => extension.span,
        Item::TypeAlias { span, .. } | Item::Const { span, .. } => *span,
        Item::Import(import) => import.span,
        Item::Test(test) => test.span,
        Item::Stmt(stmt) => stmt.span,
    }
}

/// The definitions of each module a loaded program has.
fn rows_by_module(program: &Program, files: &[KitFile]) -> HashMap<String, Vec<Row>> {
    let modules = Modules::of(files);
    let mut items: HashMap<&str, (Option<&str>, Vec<&Item>)> = HashMap::new();
    for item in &program.module.items {
        let Some((shown, ..)) = program.sources.file_at(span_of(item)) else { continue };
        items.entry(shown).or_insert_with(|| (modules.of_file(shown), Vec::new())).1.push(item);
    }
    let mut found: HashMap<String, Vec<Row>> = HashMap::new();
    for (module, of_module) in items.values() {
        if let Some(module) = module {
            found.insert((*module).to_owned(), fennec_build::rows(of_module.iter().copied()));
        }
    }
    found
}

/// What a run needs, or None to look at the whole program.
pub(crate) fn plan(
    path: &str,
    cache: &Cache,
    (state, files): (&KitState, &[KitFile]),
    passed: &HashMap<String, TestRecord>,
) -> Option<Plan> {
    if state.files.len() != files.len() {
        return None;
    }
    let mut changed: Vec<String> = Vec::new();
    for file in files {
        if state.files.get(&file.module)?.text != file.text {
            changed.push(file.module.clone());
        }
    }
    if changed.iter().any(|module| module == "kit.toml") {
        return None;
    }
    let dir = Path::new(path);
    let loaded = fennec_kit::load_modules(dir, path, &changed).ok()?;
    let rows = rows_by_module(&loaded, files);
    let mut bodies: HashSet<String> = HashSet::new();
    for module in &changed {
        let old = cache.file_rows(state.files.get(module)?.text)?;
        // A file with nothing left in it has no items to find.
        let new = rows.get(module).map_or(&[][..], Vec::as_slice);
        bodies.extend(fennec_build::changed_bodies(&old, new)?);
    }
    let affected = |record: &TestRecord| {
        bodies.contains(&format!("test \"{}\"", record.test))
            || bodies
                .iter()
                .any(|key| key.strip_prefix("def ").is_some_and(|function| record.depends_on(function)))
    };
    let (wanted, unchanged): (Vec<&TestRecord>, Vec<&TestRecord>) =
        passed.values().partition(|record| affected(record));
    let mut needed = changed;
    for record in &wanted {
        if !needed.contains(&record.module) {
            needed.push(record.module.clone());
        }
    }
    let mut program = fennec_kit::load_modules(dir, path, &needed).ok()?;
    let signatures = fennec_check::check_to_run(&mut program.module).ok()?;
    Some(Plan {
        program,
        signatures,
        wanted: wanted.iter().map(|record| record.test.clone()).collect(),
        unchanged: unchanged.into_iter().cloned().collect(),
    })
}

/// Remembers that all of a kit's tests pass with its files as they are.
pub(crate) fn remember(
    (cache, name): (&Cache, &str),
    files: &[KitFile],
    program: &Program,
) -> std::io::Result<()> {
    // A file that wasn't loaded already has its definitions kept. One that was may not, and one
    // that has no definitions at all leaves the next edit to it looking at the whole program.
    let rows = rows_by_module(program, files);
    for file in files {
        if let Some(rows) = rows.get(&file.module)
            && !cache.has_file_rows(file.text)
        {
            cache.store_file_rows(file.text, rows)?;
        }
    }
    cache.store_kit_state(name, BUILD, files)
}

/// Forgets what's remembered, since a test failed.
pub(crate) fn forget((cache, name): (&Cache, &str)) {
    cache.clear_kit_state(name, BUILD);
}
