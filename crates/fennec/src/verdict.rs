//! Getting from an edit to a verdict without looking at the rest of the kit. When an edit only
//! changes the bodies of functions, nothing outside those bodies can have broken, so it's enough
//! to load the changed files, the tests that ran those functions, and what they import.
//!
//! This only works from a kit whose tests all passed, and it gives up on anything else, which
//! leaves the whole program to load and check: an edit that reaches further, a program that
//! isn't a kit, or a mistake, which the whole program reports best.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use fennec_build::{Cache, KitFile, KitState, Row, TestRecord};
use fennec_kit::Program;
use fennec_syntax::Item;

const BUILD: &str = env!("FENNEC_BUILD");

/// What a run has to do: the program to run it on, the tests to run, and the rest.
pub(crate) struct Plan {
    pub(crate) program: Program,
    pub(crate) wanted: Vec<String>,
    pub(crate) unchanged: Vec<TestRecord>,
    /// What the whole program's check found, which the next quick check holds an edit against.
    pub(crate) facts: KitFacts,
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

/// The files a single file's program read when it was last kept, each as it is now: the file,
/// what it imports, and what `embed` read. None if one can't be read.
pub(crate) fn noted_files(state: &KitState) -> Option<Vec<KitFile>> {
    state
        .files
        .keys()
        .map(|module| {
            kit_file(module.clone(), Path::new(module.trim_start_matches(fennec_kit::EMBEDDED)), Some(state))
        })
        .collect()
}

/// The files a single file's program was just loaded from, each by its full path, with its size
/// and changed time now and the hash of the text that was parsed. None if one can't be read, or
/// isn't the text that was parsed any more, which leaves nothing to keep.
pub(crate) fn loaded_files(program: &Program) -> Option<Vec<KitFile>> {
    program
        .sources
        .hashed_files()
        .map(|(path, parsed)| {
            let file = kit_file(crate::run::absolute(path), Path::new(path), None)?;
            (file.text == parsed).then_some(file)
        })
        .collect()
}

/// Each of a kit's files as they are now, or None if this isn't a kit.
pub(crate) fn kit_files(path: &str, state: Option<&KitState>) -> Option<Vec<KitFile>> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return None;
    }
    let paths = fennec_kit::kit_paths(dir)?;
    let mut files: Vec<KitFile> =
        paths.into_iter().map(|(module, path)| kit_file(module, &path, state)).collect::<Option<_>>()?;
    // Which files `embed` reads is only known from reading the kit, so it's what was last noted.
    // Any edit that changes that changes a module, which is seen anyway.
    let listed: HashSet<String> = files.iter().map(|file| file.module.clone()).collect();
    let noted = state.into_iter().flat_map(|state| state.files.keys());
    for module in noted.filter(|module| module.starts_with(fennec_kit::EMBEDDED) && !listed.contains(*module))
    {
        let path = Path::new(module.trim_start_matches(fennec_kit::EMBEDDED));
        files.push(kit_file(module.clone(), path, state).unwrap_or_else(|| KitFile {
            module: module.clone(),
            text: fennec_blake3::hash(b"a file that embed read, which is gone"),
            size: 0,
            changed_at: 0,
        }));
    }
    Some(files)
}

/// A kit's files, with the files a program loaded from it has just read through `embed`. Each is
/// noted with the hash of what was read, so an edit made since is seen next time, however close.
pub(crate) fn with_embedded(files: &[KitFile], program: &Program) -> Vec<KitFile> {
    let mut all = files.to_vec();
    for file in &program.embedded {
        let module = fennec_kit::embedded_name(&file.path);
        all.retain(|known| known.module != module);
        let metadata = std::fs::metadata(&file.path).ok();
        let changed_at = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        let size = metadata.map_or(0, |metadata| metadata.len());
        all.push(KitFile { module, text: file.data.hash(), size, changed_at });
    }
    all
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

/// The definitions of each module a loaded program has.
fn rows_by_module(program: &Program, files: &[KitFile]) -> HashMap<String, Vec<Row>> {
    rows_of_modules(program, files, &|_| true)
}

/// Like `rows_by_module`, for just the modules `wanted` says, since working out every
/// definition's hash in a big kit takes a while.
fn rows_of_modules(
    program: &Program,
    files: &[KitFile],
    wanted: &dyn Fn(&str) -> bool,
) -> HashMap<String, Vec<Row>> {
    items_of_modules(program, files, wanted)
        .into_iter()
        .map(|(module, items)| (module, fennec_build::rows(items.into_iter())))
        .collect()
}

/// A loaded program's items, by the module of the kit each is written in, for the modules
/// `wanted` says.
fn items_of_modules<'p>(
    program: &'p Program,
    files: &[KitFile],
    wanted: &dyn Fn(&str) -> bool,
) -> HashMap<String, Vec<&'p Item>> {
    let modules = Modules::of(files);
    let mut items: HashMap<&str, (Option<&str>, Vec<&Item>)> = HashMap::new();
    for item in &program.module.items {
        let Some((shown, ..)) = program.sources.file_at(fennec_build::item_span(item)) else { continue };
        items.entry(shown).or_insert_with(|| (modules.of_file(shown), Vec::new())).1.push(item);
    }
    items
        .into_values()
        .filter_map(|(module, of_module)| {
            Some((module.filter(|module| wanted(module))?.to_owned(), of_module))
        })
        .collect()
}

/// What a run needs, or None to look at the whole program. Like `checked_quickly`, it checks a
/// changed function's lock verdict against the last whole check's.
pub(crate) fn plan(
    path: &str,
    (cache, name): (&Cache, &str),
    (state, files): (&KitState, &[KitFile]),
    passed: &HashMap<String, TestRecord>,
) -> Option<Plan> {
    let facts = stored_facts(cache, name, state)?;
    if state.files.len() != files.len() {
        return None;
    }
    let mut changed: Vec<String> = Vec::new();
    for file in files {
        if state.files.get(&file.module)?.text != file.text {
            changed.push(file.module.clone());
        }
    }
    if changed.iter().any(|module| module == "kit.toml" || module.starts_with(fennec_kit::EMBEDDED)) {
        return None;
    }
    let dir = Path::new(path);
    let loaded = fennec_kit::load_modules_to_test(dir, path, &changed).ok()?;
    let rows = rows_by_module(&loaded, files);
    let mut bodies: HashSet<String> = HashSet::new();
    for module in &changed {
        let old = cache.file_rows(state.files.get(module)?.text)?;
        // A file with nothing left in it has no items to find.
        let new = rows.get(module).map_or(&[][..], Vec::as_slice);
        bodies.extend(fennec_build::changed_bodies(&old, new)?);
    }
    // Everything below goes in one order every run, never a hash map's: the test program built
    // from it has to be the same for the same edit, byte for byte.
    let mut changed_functions: Vec<String> = bodies.iter().cloned().collect();
    changed_functions.sort();
    let changed_modules = changed.clone();
    let affected = |record: &TestRecord| {
        bodies.contains(&format!("test \"{}\"", record.test))
            || bodies
                .iter()
                .any(|key| key.strip_prefix("def ").is_some_and(|function| record.depends_on(function)))
    };
    let (wanted, unchanged, needed) = in_order(passed, changed, affected);
    let mut program = fennec_kit::load_modules_to_test(dir, path, &needed).ok()?;
    // What runs a module needs it checked and resolved.
    let edited = changed_items(&program, files, &changed_modules);
    if facts.may_differ(&edited, &program) {
        return None;
    }
    let signatures = fennec_check::check_to_run(&mut program.module).ok()?;
    if !facts.same_lock_verdicts(&changed_functions, signatures.lock_verdicts()) {
        return None;
    }
    Some(Plan {
        program,
        wanted: wanted.iter().map(|record| record.test.clone()).collect(),
        unchanged: unchanged.into_iter().cloned().collect(),
        facts,
    })
}

/// The tests an edit affects and those it doesn't, and the modules a quick run loads: the changed
/// ones and the affected tests' own. All in one order every run, never `passed`'s, a hash map's,
/// which changes from process to process: the test program built from them has to be the same for
/// the same edit, byte for byte.
fn in_order(
    passed: &HashMap<String, TestRecord>,
    changed: Vec<String>,
    affected: impl Fn(&TestRecord) -> bool,
) -> (Vec<&TestRecord>, Vec<&TestRecord>, Vec<String>) {
    let mut records: Vec<&TestRecord> = passed.values().collect();
    records.sort_by(|first, second| (&first.module, &first.test).cmp(&(&second.module, &second.test)));
    let (wanted, unchanged): (Vec<&TestRecord>, Vec<&TestRecord>) =
        records.into_iter().partition(|record| affected(record));
    let mut needed = changed;
    for record in &wanted {
        if !needed.contains(&record.module) {
            needed.push(record.module.clone());
        }
    }
    needed.sort();
    (wanted, unchanged, needed)
}

/// What each function waits on or locks, by its key, as the checker judges a `lock` that calls it.
pub(crate) type LockVerdicts = BTreeMap<String, String>;

/// What a kit's files define, as a whole load reads them: every top-level name, enum cases
/// included, and every case name more than one enum has. A load of part of the kit can read a
/// body differently only through these.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KitNames {
    names: BTreeSet<String>,
    shared_cases: BTreeSet<String>,
}

/// What one file defines: its top-level names, enum cases included, and each enum's cases.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileNames {
    names: BTreeSet<String>,
    enum_cases: Vec<(String, String)>,
}

impl FileNames {
    fn of<'i>(items: impl IntoIterator<Item = &'i Item> + Clone) -> FileNames {
        let names = fennec_build::Defined::of(items.clone()).names;
        let mut enum_cases = Vec::new();
        for item in items {
            if let Item::Enum(def) = item {
                enum_cases.extend(def.cases.iter().map(|case| (def.name.clone(), case.name.clone())));
            }
        }
        FileNames { names, enum_cases }
    }

    /// `n` and a name, or `e` and an enum and one of its cases, apart by a nul, a line each.
    fn text(&self) -> String {
        let mut text = String::new();
        for name in &self.names {
            text.push_str("n\0");
            text.push_str(name);
            text.push('\n');
        }
        for (owner, case) in &self.enum_cases {
            text.push_str("e\0");
            text.push_str(owner);
            text.push('\0');
            text.push_str(case);
            text.push('\n');
        }
        text
    }

    fn from_text(text: &str) -> Option<FileNames> {
        let mut names = FileNames::default();
        for line in text.lines() {
            match line.split('\0').collect::<Vec<&str>>().as_slice() {
                ["n", name] => {
                    names.names.insert((*name).to_owned());
                }
                ["e", owner, case] => names.enum_cases.push(((*owner).to_owned(), (*case).to_owned())),
                _ => return None,
            }
        }
        Some(names)
    }
}

/// The cache's kind for what one file defines, by its text.
const FILE_NAMES: &str = "file-names";

impl KitNames {
    /// What `items`, all of a kit, define.
    pub(crate) fn of(items: &[Item]) -> KitNames {
        KitNames::joined(std::iter::once(FileNames::of(items)))
    }

    fn joined(files: impl Iterator<Item = FileNames>) -> KitNames {
        let mut names = BTreeSet::new();
        let mut owners: HashMap<String, BTreeSet<String>> = HashMap::new();
        for file in files {
            names.extend(file.names);
            for (owner, case) in file.enum_cases {
                owners.entry(case).or_default().insert(owner);
            }
        }
        let shared_cases =
            owners.into_iter().filter(|(_, owners)| owners.len() > 1).map(|(case, _)| case).collect();
        KitNames { names, shared_cases }
    }

    /// What the whole kit at `path`, with `files` as they are, defines, from `program` for the
    /// files it loaded and from each other file's text, kept in `cache` by its text after the
    /// first time. None when a file `program` didn't load doesn't parse, which a whole load
    /// refuses.
    pub(crate) fn of_kit(
        path: &str,
        cache: Option<&Cache>,
        files: &[KitFile],
        program: &Program,
    ) -> Option<KitNames> {
        let loaded = items_of_modules(program, files, &|_| true);
        let paths = own_sources(path)?;
        let mut each = Vec::new();
        for file in files.iter().filter(|file| paths.contains_key(&file.module)) {
            if let Some(items) = loaded.get(&file.module) {
                each.push(FileNames::of(items.iter().copied()));
                continue;
            }
            let key = format!("{}.names", file.text.to_hex());
            let kept = cache
                .and_then(|cache| cache.read(FILE_NAMES, &key))
                .and_then(|bytes| FileNames::from_text(&String::from_utf8(bytes).ok()?));
            if let Some(names) = kept {
                each.push(names);
                continue;
            }
            let text = std::fs::read_to_string(paths.get(&file.module)?).ok()?;
            let module = fennec_syntax::parse(&text).ok()?;
            let names = FileNames::of(&module.items);
            if let Some(cache) = cache {
                // A cache that can't be written only means reading the file again next time.
                let _ = cache.write(FILE_NAMES, &key, names.text().as_bytes());
            }
            each.push(names);
        }
        Some(KitNames::joined(each.into_iter()))
    }

    /// Whether `items`, as `partial` loaded them, may not be what a whole load makes of them.
    /// They may use a name the kit defines in a file `partial` didn't load, which a whole load
    /// refuses, as the file doesn't import it, and which `partial` may read as a builtin's. Or
    /// they may keep a leading-dot case another enum in the kit has too, which a whole load
    /// writes with its enum where it can tell which, and refuses where it can't.
    pub(crate) fn may_differ(&self, items: &[Item], partial: &Program) -> bool {
        let loaded = fennec_build::Defined::of(&partial.module.items).names;
        let elsewhere = |name: &String| self.names.contains(name) && !loaded.contains(name);
        if fennec_build::names_used(items).iter().any(elsewhere) {
            return true;
        }
        let module = fennec_syntax::Module { items: items.to_vec(), ..fennec_syntax::Module::default() };
        let mut shared = false;
        fennec_syntax::read::exprs(&module, &mut |expr| {
            if let fennec_syntax::ExprKind::Variant(case) = &expr.kind {
                shared |= self.shared_cases.contains(case);
            }
        });
        shared
    }
}

/// What a quick check holds the changed files against, kept from the last whole check, since an
/// edit to bodies changes none of it: each function's lock verdict, and what the kit defines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KitFacts {
    lock_verdicts: LockVerdicts,
    kit: KitNames,
}

impl KitFacts {
    /// What a whole check found, with what the kit defines.
    pub(crate) fn of(kit: KitNames, signatures: &fennec_check::Signatures) -> KitFacts {
        KitFacts { lock_verdicts: signatures.lock_verdicts().clone(), kit }
    }

    fn may_differ(&self, items: &[Item], partial: &Program) -> bool {
        self.kit.may_differ(items, partial)
    }

    /// Whether each function whose body changed has the lock verdict it had: everything a changed
    /// function calls is loaded, so its verdict in a partial check is the whole program's, and
    /// when it's the same, no `lock` that calls it, anywhere, can have changed.
    fn same_lock_verdicts(&self, bodies: &[String], now: &LockVerdicts) -> bool {
        let mut functions = bodies.iter().filter_map(|key| key.strip_prefix("def "));
        functions.all(|key| now.get(key) == self.lock_verdicts.get(key))
    }

    /// The facts as lines of text: `v` and a function's key and verdict, `n` and a name the kit
    /// defines, or `c` and a case name more than one enum has, apart by a nul.
    fn text(&self) -> String {
        let mut text = String::new();
        for (key, verdict) in &self.lock_verdicts {
            for part in ["v", key, verdict] {
                text.push_str(part);
                text.push('\0');
            }
            text.push('\n');
        }
        for (kind, words) in [("n", &self.kit.names), ("c", &self.kit.shared_cases)] {
            for word in words {
                text.push_str(kind);
                text.push('\0');
                text.push_str(word);
                text.push('\0');
                text.push('\n');
            }
        }
        text
    }

    fn from_text(text: &str) -> Option<KitFacts> {
        let mut facts = KitFacts::default();
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            match parts.as_slice() {
                ["v", key, verdict, ""] => {
                    facts.lock_verdicts.insert((*key).to_owned(), (*verdict).to_owned());
                }
                ["n", name, ""] => {
                    facts.kit.names.insert((*name).to_owned());
                }
                ["c", case, ""] => {
                    facts.kit.shared_cases.insert((*case).to_owned());
                }
                _ => return None,
            }
        }
        Some(facts)
    }
}

/// Every item of `program` that a kit's own file holds, not a dependency's or std's.
pub(crate) fn kit_items(program: &Program, files: &[KitFile]) -> Vec<Item> {
    items_of_modules(program, files, &|_| true).into_values().flatten().cloned().collect()
}

/// Whether `program` holds every one of the source files of the kit at `path`.
pub(crate) fn has_every_file(path: &str, program: &Program, files: &[KitFile]) -> bool {
    let Some(sources) = own_sources(path) else { return false };
    let loaded = items_of_modules(program, files, &|_| true);
    sources.keys().all(|module| loaded.contains_key(module))
}

/// The kit at `path`'s own source files, under its `src`, by module: not its manifest, its lock
/// or a dependency's.
fn own_sources(path: &str) -> Option<HashMap<String, std::path::PathBuf>> {
    let paths = fennec_kit::kit_paths(Path::new(path))?;
    let root = paths.iter().find(|(module, _)| module == "kit.toml")?.1.parent()?.to_path_buf();
    let src = root.join("src");
    let is_source = |file: &Path| file.starts_with(&src) && file.extension().is_some_and(|ext| ext == "fen");
    Some(paths.into_iter().filter(|(_, file)| is_source(file)).collect())
}

/// The changed modules' items, as `program` loaded them.
fn changed_items(program: &Program, files: &[KitFile], changed: &[String]) -> Vec<Item> {
    let items = items_of_modules(program, files, &|module| changed.iter().any(|name| name == module));
    items.into_values().flatten().cloned().collect()
}

/// The cache's kind for what a kit's whole check found, each kept under the state it was found for.
const KIT_FACTS: &str = "kit-facts";

/// What names a kit's facts in the cache: a hash of the name its state is kept under, since a run
/// and a test keep theirs apart, and each file's module and text.
fn state_key(name: &str, state: &KitState) -> String {
    let mut files: Vec<(&String, &KitFile)> = state.files.iter().collect();
    files.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec kit facts 2026-11-11");
    hasher.update(name.as_bytes());
    hasher.update(&[2]);
    for (module, file) in files {
        hasher.update(module.as_bytes());
        hasher.update(&[0]);
        hasher.update(file.text.to_hex().as_bytes());
        hasher.update(&[1]);
    }
    hasher.finalize().to_hex()
}

/// The facts kept for a kit's state, if they were.
fn stored_facts(cache: &Cache, name: &str, state: &KitState) -> Option<KitFacts> {
    let bytes = cache.read(KIT_FACTS, &state_key(name, state))?;
    KitFacts::from_text(&String::from_utf8(bytes).ok()?)
}

/// Keeps a kit's facts under its state, then the state itself, so a state is never kept without
/// them.
fn store_with_facts(cache: &Cache, name: &str, files: &[KitFile], facts: &KitFacts) -> std::io::Result<()> {
    let state = KitState {
        files: files.iter().map(|file| (file.module.clone(), file.clone())).collect(),
        noted_at: 0,
    };
    cache.write(KIT_FACTS, &state_key(name, &state), facts.text().as_bytes())?;
    cache.store_kit_state(name, BUILD, files)
}

/// What names the last time all of a kit `main` reaches passed the checker, for `fennec run`.
fn checked_name(path: &str) -> String {
    format!("{}#checked", crate::run::absolute(path))
}

/// Whether the kit at `path`, with `files` as they are now, passes the checker, when that can be
/// told without checking all of it: the last check passed, and every file changed since only in the
/// bodies of its functions. Then nothing outside those bodies can have broken, so the changed files
/// and what they import are checked, and what that finds is the answer, with the program it was
/// found in. `None` means the whole program has to be checked.
///
/// A body can still reach a `lock` in a file it doesn't import: one that calls it, which a `lock`
/// refuses if it now waits or locks, however many calls down. So each changed function's lock
/// verdict has to be what the last full check found. A body can also read differently in a whole
/// load than in the partial one: through a name another file defines, or a leading-dot case
/// another file's enum has too. Either means a whole check. A pass gives back the facts it held
/// the edit against, which still hold.
pub(crate) fn checked_quickly(
    path: &str,
    cache: &Cache,
    files: &[KitFile],
) -> Option<Result<KitFacts, (Program, Vec<fennec_syntax::Diagnostic>)>> {
    let state = cache.kit_state(&checked_name(path), BUILD)?;
    let facts = stored_facts(cache, &checked_name(path), &state)?;
    if state.files.len() != files.len() {
        return None;
    }
    let mut changed: Vec<String> = Vec::new();
    for file in files {
        if state.files.get(&file.module)?.text != file.text {
            changed.push(file.module.clone());
        }
    }
    if changed.is_empty() {
        return Some(Ok(facts));
    }
    if changed.iter().any(|module| module == "kit.toml" || module.starts_with(fennec_kit::EMBEDDED)) {
        return None;
    }
    let overlay = fennec_kit::Overlay::new();
    let loaded = fennec_kit::load_modules_with(Path::new(path), path, &changed, &overlay).ok()?;
    let rows = rows_by_module(&loaded, files);
    let mut bodies = Vec::new();
    for module in &changed {
        let old = cache.file_rows(state.files.get(module)?.text)?;
        let new = rows.get(module).map_or(&[][..], Vec::as_slice);
        bodies.extend(fennec_build::changed_bodies(&old, new)?);
    }
    if facts.may_differ(&changed_items(&loaded, files, &changed), &loaded) {
        return None;
    }
    match crate::copy_rule::check_rules(&mut loaded.module.clone(), &loaded.sources) {
        // Everything a changed function calls is loaded, so its verdict here is the whole
        // program's: the same as before, and no `lock` that calls it can have changed.
        Ok(signatures) => facts.same_lock_verdicts(&bodies, signatures.lock_verdicts()).then_some(Ok(facts)),
        Err(diagnostics) => Some(Err((loaded, diagnostics))),
    }
}

/// Remembers that all of the kit at `path` that `main` reaches, `program`, passes the checker
/// with its files as they are, with each file's definitions and its lock verdicts, for
/// `checked_quickly`.
pub(crate) fn remember_checked(
    path: &str,
    cache: &Cache,
    (files, program): (&[KitFile], &Program),
    facts: &KitFacts,
) {
    // Only a file whose text is new needs its definitions worked out: after an edit, that's the
    // file edited.
    let new: HashSet<&str> = files
        .iter()
        .filter(|file| !cache.has_file_rows(file.text))
        .map(|file| file.module.as_str())
        .collect();
    let rows = rows_of_modules(program, files, &|module| new.contains(module));
    for file in files {
        if let Some(rows) = rows.get(&file.module) {
            fennec_build::kept("keep a file's definitions", cache.store_file_rows(file.text, rows));
        }
    }
    // A cache that can't be written only means checking all of it again next time.
    let stored = store_with_facts(cache, &checked_name(path), &with_embedded(files, program), facts);
    fennec_build::kept("keep the program's check", stored);
}

/// Remembers that all of a kit's tests pass with its files as they are, with its lock verdicts.
pub(crate) fn remember(
    (cache, name): (&Cache, &str),
    (files, program): (&[KitFile], &Program),
    facts: &KitFacts,
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
    store_with_facts(cache, name, &with_embedded(files, program), facts)
}

/// Forgets what's remembered, since a test failed.
pub(crate) fn forget((cache, name): (&Cache, &str)) {
    cache.clear_kit_state(name, BUILD);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fennec_build::TestRecord;

    /// The same records in two hash maps, which iterate in two orders, plan the same: the same
    /// tests wanted and left, in the same order, and the same modules loaded.
    #[test]
    fn a_quick_runs_plan_is_the_same_whatever_order_a_hash_map_gives() {
        let record = |module: &str, test: &str| TestRecord {
            test: test.to_owned(),
            module: module.to_owned(),
            key: fennec_blake3::hash(test.as_bytes()),
            ran: None,
            reach: Vec::new(),
        };
        let records: Vec<TestRecord> =
            (0..40).map(|at| record(&format!("module{}", at % 7), &format!("test {at}"))).collect();
        let map = || -> HashMap<String, TestRecord> {
            records.iter().map(|record| (record.test.clone(), record.clone())).collect()
        };
        let affected = |record: &TestRecord| record.test.ends_with('3') || record.module == "module2";
        let planned = |passed: &HashMap<String, TestRecord>| {
            let (wanted, unchanged, needed) = super::in_order(passed, vec!["module6".to_owned()], affected);
            let names = |records: Vec<&TestRecord>| {
                records.iter().map(|record| record.test.clone()).collect::<Vec<_>>()
            };
            (names(wanted), names(unchanged), needed)
        };
        let (first, second) = (map(), map());
        // Each map has a hasher of its own, so the two go in two orders, or the test shows nothing.
        let order = |passed: &HashMap<String, TestRecord>| passed.keys().cloned().collect::<Vec<_>>();
        let maps: Vec<HashMap<String, TestRecord>> = (0..8).map(|_| map()).collect();
        assert!(maps.iter().any(|other| order(other) != order(&first)), "every map iterated in one order");
        let want = planned(&first);
        assert_eq!(planned(&second), want);
        for other in &maps {
            assert_eq!(planned(other), want);
        }
        // The changed module, and the modules of the tests it affects: `module2`'s, and tests 3, 13,
        // 23 and 33, in modules 3, 6, 2 and 5. In one order, sorted.
        assert_eq!(want.2, ["module2", "module3", "module5", "module6"]);
    }
}
