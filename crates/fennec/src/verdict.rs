//! Getting from an edit to a verdict without looking at the rest of the kit. When an edit only
//! changes the bodies of functions, nothing outside those bodies can have broken, so it's enough
//! to load the changed files, the tests that ran those functions, and what they import.
//!
//! This only works from a kit whose tests all passed, and it gives up on anything else, which
//! leaves the whole program to load and check: an edit that reaches further, a program that
//! isn't a kit, or a mistake, which the whole program reports best.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use fennec_blake3::Hash;
use fennec_build::{Cache, Row, TestRecord};
use fennec_check::Signatures;
use fennec_kit::Program;
use fennec_syntax::{Item, Sources, Span};

const BUILD: &str = env!("FENNEC_BUILD");

/// What a run has to do: the program to run it on, the tests to run, and the rest.
pub(crate) struct Plan {
    pub(crate) program: Program,
    pub(crate) signatures: Signatures,
    pub(crate) wanted: Vec<String>,
    pub(crate) unchanged: Vec<TestRecord>,
}

/// Each of a kit's files, by module name, with a hash of its text.
pub(crate) fn kit_files(path: &str) -> Option<Vec<(String, Hash)>> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return None;
    }
    let texts = fennec_kit::all_texts(dir)?;
    Some(texts.into_iter().map(|(module, text)| (module, fennec_blake3::hash(text.as_bytes()))).collect())
}

/// The module a file shown like this holds, among a kit's.
fn module_of<'f>(shown: &str, files: &'f [(String, Hash)]) -> Option<&'f str> {
    let found = files.iter().map(|(module, _)| module.as_str()).find(|module| {
        shown
            .strip_suffix(".fen")
            .is_some_and(|path| path.ends_with(&format!("src/{}", module.replace('.', "/"))))
    });
    found.or_else(|| files.iter().map(|(module, _)| module.as_str()).find(|module| *module == shown))
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

/// The module an item of a loaded program came from.
pub(crate) fn module_at<'f>(sources: &Sources, span: Span, files: &'f [(String, Hash)]) -> Option<&'f str> {
    module_of(sources.file_at(span)?.0, files)
}

/// The definitions of each module of a loaded program.
fn rows_by_module(program: &Program, files: &[(String, Hash)]) -> HashMap<String, Vec<Row>> {
    let mut items: HashMap<&str, Vec<&Item>> = HashMap::new();
    for item in &program.module.items {
        if let Some(module) = module_at(&program.sources, span_of(item), files) {
            items.entry(module).or_default().push(item);
        }
    }
    let mut found: HashMap<String, Vec<Row>> = HashMap::new();
    for (module, _) in files {
        let of_module = items.get(module.as_str()).map_or(&[][..], Vec::as_slice);
        found.insert(module.clone(), fennec_build::rows(of_module.iter().copied()));
    }
    found
}

/// What a run needs, or None to look at the whole program.
pub(crate) fn plan(
    path: &str,
    (cache, name): (&Cache, &str),
    files: &[(String, Hash)],
    passed: &HashMap<String, TestRecord>,
) -> Option<Plan> {
    let state = cache.kit_state(name, BUILD)?;
    if state.len() != files.len() {
        return None;
    }
    let mut changed: Vec<String> = Vec::new();
    for (module, text) in files {
        if *state.get(module)? != *text {
            changed.push(module.clone());
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
        let old = cache.file_rows(*state.get(module)?)?;
        bodies.extend(fennec_build::changed_bodies(&old, rows.get(module)?)?);
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
    files: &[(String, Hash)],
    program: &Program,
) -> std::io::Result<()> {
    let mut rows = rows_by_module(program, files);
    for (module, text) in files {
        // A file that wasn't loaded, and so has no definitions here, already has them kept.
        if !cache.has_file_rows(*text)
            && let Some(rows) = rows.remove(module)
        {
            cache.store_file_rows(*text, &rows)?;
        }
    }
    cache.store_kit_state(name, BUILD, files)
}

/// Forgets what's remembered, since a test failed.
pub(crate) fn forget((cache, name): (&Cache, &str)) {
    cache.clear_kit_state(name, BUILD);
}
