//! `fennec lint [--json] [path]`: what the checker and the linter find in a kit or a file, then
//! hints about what may copy or allocate. Hints flag correct code, so only errors fail it.
//!
//! `fennec check [--json] [path]`: only what the checker finds, which is quicker.
//!
//! `--json` writes the same findings as JSON on standard output, as `report` describes.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use crate::report::{self, Finding};

pub(crate) fn lint(path: &str) -> ExitCode {
    linted(path, false)
}

pub(crate) fn lint_json(path: &str) -> ExitCode {
    linted(path, true)
}

pub(crate) fn check(path: &str) -> ExitCode {
    checked(path, false)
}

pub(crate) fn check_json(path: &str) -> ExitCode {
    checked(path, true)
}

/// The program at `path`, or, when it can't be loaded, `None` once that's shown as asked.
fn loaded(path: &str, json: bool) -> Option<fennec_kit::Program> {
    let overlay = fennec_kit::Overlay::new();
    match fennec_kit::load_with(std::path::Path::new(path), path, fennec_kit::Entry::Everything, &overlay) {
        Ok(program) => Some(program),
        Err(problems) if json => {
            print!("{}", report::unloaded(&problems));
            None
        }
        Err(problems) => {
            eprint!("{}", problems.render());
            None
        }
    }
}

/// The checker's errors, shown as asked, failing if there are any.
fn rejected(
    program: &fennec_kit::Program,
    diagnostics: &[fennec_syntax::Diagnostic],
    json: bool,
) -> ExitCode {
    let diagnostics: Vec<_> =
        diagnostics.iter().map(|diagnostic| crate::run::with_extension(program, diagnostic)).collect();
    if json {
        let found: Vec<Finding<'_>> = diagnostics.iter().map(Finding::checked).collect();
        print!("{}", report::json(&program.sources, &found));
    } else {
        eprint!("{}", program.sources.render_all(&diagnostics));
    }
    if diagnostics.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// What a run would reject that loading all of a kit hides. All of a kit is checked, but a run loads
/// `main` and what it imports, so a method from a file only some other file imports is there for
/// one and not the other. Only an `extend` can do that, so a kit with none left out is checked once.
fn what_a_run_rejects(
    path: &str,
    whole: &fennec_kit::Program,
) -> Option<(fennec_kit::Program, Vec<fennec_syntax::Diagnostic>)> {
    whole.manifest.as_ref()?;
    // A kit with no `main` has no run to compare with.
    let run = fennec_kit::load(std::path::Path::new(path), path, fennec_kit::Entry::Main).ok()?;
    let loaded: HashSet<&str> = run.sources.files().map(|(file, _)| file).collect();
    let left_out = whole.module.items.iter().any(|item| match item {
        fennec_syntax::Item::Extend(extension) => {
            whole.sources.file_at(extension.span).is_some_and(|(file, ..)| !loaded.contains(file))
        }
        _ => false,
    });
    if !left_out {
        return None;
    }
    let diagnostics = crate::copy_rule::check_to_run(&mut run.module.clone(), &run.sources).err()?;
    Some((run, diagnostics))
}

fn checked(path: &str, json: bool) -> ExitCode {
    let Some(program) = loaded(path, json) else { return ExitCode::FAILURE };
    // As a run checks it: all of the program, and of the standard library just what it reaches,
    // since the library's own tests check the rest.
    let mut diagnostics = crate::copy_rule::check_to_run(&mut program.module.clone(), &program.sources)
        .err()
        .unwrap_or_default();
    fennec_check::add_rename_fixes(&mut diagnostics, &program.module, &program.sources);
    fennec_check::add_pattern_fixes(&mut diagnostics, &program.sources);
    if diagnostics.is_empty()
        && let Some((run, diagnostics)) = what_a_run_rejects(path, &program)
    {
        return rejected(&run, &diagnostics, json);
    }
    if diagnostics.is_empty() && !json {
        for problem in locked_layout_problems(&program) {
            eprintln!("warning: {problem}");
        }
    }
    rejected(&program, &diagnostics, json)
}

/// Why a type the kit saves in `std.pack` can't load a layout `pack.lock` holds for it: data
/// saved that way before would fail to load now. A warning, since leaving that data behind can
/// be what's meant.
fn locked_layout_problems(program: &fennec_kit::Program) -> Vec<String> {
    let locked =
        program.module.items.iter().any(
            |item| matches!(item, fennec_syntax::Item::Const { name, .. } if name == fennec_kit::PACK_LOCK),
        );
    if !locked {
        return Vec::new();
    }
    let mut module = program.module.clone();
    let Ok(signatures) = fennec_check::check_and_resolve(&mut module) else { return Vec::new() };
    fennec_ir::lower(&module, &signatures).layout_problems
}

/// Every lint the linter finds in a program, in the order they're written.
fn lints_of(program: &fennec_kit::Program) -> Vec<fennec_lint::Lint> {
    let mut found = fennec_lint::lint(&program.module, &program.sources);
    found.extend(fennec_lint::needless_conversions(&program.module, &program.sources));
    found.extend(fennec_lint::stale_copies(&program.module, &program.sources));
    found.extend(fennec_lint::double_lookups(&program.module, &program.sources));
    found.extend(fennec_lint::self_compares(&program.module, &program.sources));
    if program.manifest.is_some() {
        found.extend(fennec_lint::kit_lints(&program.module));
    }
    found.retain(|lint| program.is_own(lint.diagnostic.span));
    found.sort_by_key(|lint| (lint.diagnostic.span.start, lint.rule));
    found
}

/// The lint errors in all of the program at `path`, rendered as `fennec lint` shows them, with a
/// line counting them: nothing when there are none, or when it doesn't load or check, which
/// whatever's running reports itself.
pub(crate) fn errors_shown(path: &str) -> String {
    let overlay = fennec_kit::Overlay::new();
    let Ok(program) =
        fennec_kit::load_with(std::path::Path::new(path), path, fennec_kit::Entry::Everything, &overlay)
    else {
        return String::new();
    };
    let errors: Vec<fennec_lint::Lint> =
        lints_of(&program).into_iter().filter(|lint| lint.severity == fennec_lint::Severity::Error).collect();
    if errors.is_empty() {
        return String::new();
    }
    let diagnostics: Vec<_> = errors.iter().map(|lint| lint.diagnostic.clone()).collect();
    let fixable = errors.iter().filter(|lint| !lint.diagnostic.fixes.is_empty()).count();
    format!(
        "{}{} lints, {fixable} of which `fennec fix` can fix\n",
        program.sources.render_all(&diagnostics),
        errors.len()
    )
}

/// The lint errors in the program at `path`, as `errors_shown` gives them, from the cache when
/// its files haven't changed since they were last linted, so a run of unchanged code doesn't lint
/// it again. In a kit, only the files that changed are linted again, as `of_kit` says.
/// The lint errors of a run's program, being found on a thread of their own while it's built.
static FINDING: std::sync::Mutex<Option<std::thread::JoinHandle<String>>> = std::sync::Mutex::new(None);

/// Where a run hands the lint errors being found the program it loaded, so they're found from the
/// one load rather than a second: one parse, and one program both see, shared rather than copied.
static LENT: std::sync::Mutex<Option<std::sync::mpsc::Sender<std::sync::Arc<fennec_kit::Program>>>> =
    std::sync::Mutex::new(None);

/// Starts finding the lint errors of the program at `path` beside whatever comes next, for
/// `show_errors_found` to show.
pub(crate) fn find_errors_beside(path: &str) {
    let owned = path.to_owned();
    let (sender, receiver) = std::sync::mpsc::channel();
    *LENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
    // It parses and walks the program as a run does, so it has a run's stack.
    let thread = std::thread::Builder::new().stack_size(crate::STACK_BYTES);
    if let Ok(finding) = thread.spawn(move || errors_remembered_with(&owned, &receiver)) {
        *FINDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(finding);
    } else {
        lend_loaded(None, path);
        eprint!("{}", errors_remembered(path));
    }
}

/// Hands the lint errors being found the program the run loaded, when it's what they'd load
/// themselves: one with every file of its kit, before anything changes it. With `None`, or one
/// that isn't, they load it themselves. Either way only the first call does anything.
pub(crate) fn lend_loaded(program: Option<&std::sync::Arc<fennec_kit::Program>>, path: &str) {
    let Some(lend) = LENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else { return };
    let whole =
        program.filter(|program| fennec_kit::holds_every_file(program, std::path::Path::new(path), path));
    if let Some(program) = whole {
        // Nobody waiting only means the errors were found without it.
        let _ = lend.send(std::sync::Arc::clone(program));
    }
}

/// Shows the lint errors `find_errors_beside` found, waiting for them if they aren't ready, the
/// first time it's called. A run calls it before anything else it says, so they come first.
pub(crate) fn show_errors_found() {
    // A run that ends before it loaded anything lets them load it themselves.
    lend_loaded(None, "");
    let finding = FINDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    if let Some(found) = finding.and_then(|finding| finding.join().ok()) {
        eprint!("{found}");
    }
}

pub(crate) fn errors_remembered(path: &str) -> String {
    errors_remembered_with(path, &std::sync::mpsc::channel().1)
}

/// `errors_remembered`, linting the program `lent` gives, if it gives one, rather than loading it.
fn errors_remembered_with(
    path: &str,
    lent: &std::sync::mpsc::Receiver<std::sync::Arc<fennec_kit::Program>>,
) -> String {
    let Some(cache) = crate::run::cache() else { return errors_shown(path) };
    let files = crate::image::files_now(path, &cache);
    let Some(key) = files_key(path, files.as_deref()) else { return errors_shown(path) };
    let name = crate::run::passed_name(path);
    if let Some(kept) = cache.read("linted", &name)
        && let Some(shown) = kept.strip_prefix(key.as_bytes()).and_then(|rest| rest.strip_prefix(b"\n"))
    {
        return String::from_utf8_lossy(shown).into_owned();
    }
    let shown = match &files {
        Some(files) => of_kit(path, &cache, files, lent),
        None => errors_shown(path),
    };
    // A cache that can't be written only means linting it again next time.
    let _ = cache.write("linted", &name, format!("{key}\n{shown}").as_bytes());
    shown
}

/// What the files of the program at `path` hold, as a hash: a kit's files by their sizes and when
/// they changed where those say they're as last seen, or a single file's text.
fn files_key(path: &str, files: Option<&[fennec_build::KitFile]>) -> Option<String> {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec lints 2026-11-07");
    hasher.update(env!("FENNEC_BUILD").as_bytes());
    match files {
        Some(files) => {
            for file in files {
                hasher.update(file.module.as_bytes());
                hasher.update(file.text.to_hex().as_bytes());
            }
        }
        None => {
            hasher.update(&std::fs::read(path).ok()?);
        }
    }
    Some(hasher.finalize().to_hex())
}

/// A lint error as shown, kept with the file it's in.
#[derive(Clone)]
struct Shown {
    /// Where it starts, from the start of its file.
    at: u32,
    rule: String,
    fixable: bool,
    text: String,
}

/// What each of a kit's files was last linted as, by the lints that look at one file at a time.
struct Linted {
    /// What `fennec_lint::Facts` were then, as a hash.
    facts: String,
    /// Each file's module, the hash of its text, and its lint errors.
    files: Vec<(String, String, Vec<Shown>)>,
}

impl Linted {
    fn read(cache: &fennec_build::Cache, name: &str) -> Option<Linted> {
        use fennec_rt::embed::codec::{Decode, Reader};
        let bytes = cache.read("linted-files", name)?;
        let mut reader = Reader::new(&bytes);
        if String::decode(&mut reader)? != env!("FENNEC_BUILD") {
            return None;
        }
        let facts = String::decode(&mut reader)?;
        let mut files = Vec::new();
        for _ in 0..usize::decode(&mut reader)? {
            let (module, text) = (String::decode(&mut reader)?, String::decode(&mut reader)?);
            let mut shown = Vec::new();
            for _ in 0..usize::decode(&mut reader)? {
                shown.push(Shown {
                    at: u32::decode(&mut reader)?,
                    rule: String::decode(&mut reader)?,
                    fixable: bool::decode(&mut reader)?,
                    text: String::decode(&mut reader)?,
                });
            }
            files.push((module, text, shown));
        }
        Some(Linted { facts, files })
    }

    fn write(&self, cache: &fennec_build::Cache, name: &str) {
        use fennec_rt::embed::codec::{Encode, Writer};
        let mut writer = Writer::default();
        env!("FENNEC_BUILD").encode(&mut writer);
        self.facts.encode(&mut writer);
        self.files.len().encode(&mut writer);
        for (module, text, shown) in &self.files {
            module.encode(&mut writer);
            text.encode(&mut writer);
            shown.len().encode(&mut writer);
            for one in shown {
                one.at.encode(&mut writer);
                one.rule.encode(&mut writer);
                one.fixable.encode(&mut writer);
                one.text.encode(&mut writer);
            }
        }
        // A cache that can't be written only means linting every file again next time.
        let _ = cache.write("linted-files", name, &writer.into_bytes());
    }
}

/// The lint errors in the kit at `path`, as `errors_shown` gives them, linting again with the
/// lints of one file at a time only the files whose text changed since they were last linted.
/// What those find in a file depends only on its text and on `fennec_lint::Facts`, so when the
/// facts are as they were, the rest are as they were too. The lints that look across files, and
/// those that check the program, look at all of it each time.
fn of_kit(
    path: &str,
    cache: &fennec_build::Cache,
    files: &[fennec_build::KitFile],
    lent: &std::sync::mpsc::Receiver<std::sync::Arc<fennec_kit::Program>>,
) -> String {
    let overlay = fennec_kit::Overlay::new();
    let loaded =
        || fennec_kit::load_with(std::path::Path::new(path), path, fennec_kit::Entry::Everything, &overlay);
    let Some(program) = lent.recv().ok().or_else(|| loaded().ok().map(std::sync::Arc::new)) else {
        return String::new();
    };
    let (module, sources) = (&program.module, &program.sources);
    let loaded = loaded_files(sources, files);
    let facts = fennec_lint::Facts::of(module, sources);
    let facts = fennec_blake3::hash(format!("{facts:?}").as_bytes()).to_hex();
    let name = crate::run::passed_name(path);
    let last = Linted::read(cache, &name).filter(|last| last.facts == facts);
    let mut kept: HashMap<&str, &Vec<Shown>> = HashMap::new();
    for (module, text, shown) in last.iter().flat_map(|last| &last.files) {
        let unchanged = loaded.iter().find(|(_, now)| {
            now.as_ref().is_some_and(|(module_now, text_now)| module_now == module && text_now == text)
        });
        if let Some((file, _)) = unchanged {
            kept.insert(file, shown);
        }
    }
    let Some(mut by_file) = within(&program, &|file| !kept.contains_key(file)) else {
        // A lint with no file to keep it with is found again each time, so nothing's kept.
        return shown_errors(errors_of(&program, &within(&program, &|_| true).unwrap_or_default()));
    };
    by_file.extend(kept.into_iter().map(|(file, shown)| (file.to_owned(), shown.clone())));
    let errors = errors_of(&program, &by_file);
    let files = loaded
        .into_iter()
        .filter_map(|(file, now)| {
            let (module, text) = now?;
            Some((module.to_owned(), text, by_file.remove(file).unwrap_or_default()))
        })
        .collect();
    Linted { facts, files }.write(cache, &name);
    shown_errors(errors)
}

/// Each file of a loaded kit, by how it's shown, with its module and its text's hash, in the order
/// it was loaded. A file that isn't one of the kit's modules has neither.
fn loaded_files<'p>(
    sources: &'p fennec_syntax::Sources,
    files: &'p [fennec_build::KitFile],
) -> Vec<(&'p str, Option<(&'p str, String)>)> {
    let modules = crate::verdict::Modules::of(files);
    let texts: HashMap<&str, String> =
        files.iter().map(|file| (file.module.as_str(), file.text.to_hex())).collect();
    sources
        .files()
        .map(|(shown, _)| {
            let module = modules.of_file(shown);
            (shown, module.and_then(|module| Some((module, texts.get(module)?.clone()))))
        })
        .collect()
}

/// The errors the lints of one file at a time find in the files `wanted` takes, by file, or None if
/// one of them is in no file.
fn within(
    program: &fennec_kit::Program,
    wanted: &dyn Fn(&str) -> bool,
) -> Option<HashMap<String, Vec<Shown>>> {
    let (module, sources) = (&program.module, &program.sources);
    let mut by_file: HashMap<String, Vec<Shown>> = HashMap::new();
    let found = fennec_lint::within_files(module, sources, program.manifest.is_some(), wanted);
    let found = found.into_iter().filter(|lint| program.is_own(lint.diagnostic.span));
    for lint in found.filter(|lint| lint.severity == fennec_lint::Severity::Error) {
        let (file, _, base) = sources.file_at(lint.diagnostic.span)?;
        by_file.entry(file.to_owned()).or_default().push(Shown {
            at: lint.diagnostic.span.start.saturating_sub(base),
            rule: lint.rule.to_owned(),
            fixable: !lint.diagnostic.fixes.is_empty(),
            text: sources.render(&lint.diagnostic),
        });
    }
    Some(by_file)
}

/// Every error in a kit, with what the lints of one file at a time found in each file: where it
/// starts in all of the program, its rule, whether it has a fix, and how it's shown.
fn errors_of(
    program: &fennec_kit::Program,
    by_file: &HashMap<String, Vec<Shown>>,
) -> Vec<(u32, String, bool, String)> {
    let (module, sources) = (&program.module, &program.sources);
    let mut errors = Vec::new();
    for (file, found) in by_file {
        let base = sources.by_path(file).map_or(0, |(_, base)| base);
        errors
            .extend(found.iter().map(|one| (base + one.at, one.rule.clone(), one.fixable, one.text.clone())));
    }
    let mut whole = fennec_lint::across_files(module, sources);
    whole.extend(fennec_lint::needless_conversions(module, sources));
    whole.extend(fennec_lint::stale_copies(module, sources));
    whole.extend(fennec_lint::double_lookups(module, sources));
    whole.extend(fennec_lint::self_compares(module, sources));
    whole.retain(|lint| program.is_own(lint.diagnostic.span));
    for lint in whole.into_iter().filter(|lint| lint.severity == fennec_lint::Severity::Error) {
        let fixable = !lint.diagnostic.fixes.is_empty();
        errors.push((
            lint.diagnostic.span.start,
            lint.rule.to_owned(),
            fixable,
            sources.render(&lint.diagnostic),
        ));
    }
    errors
}

/// Errors shown as `errors_shown` shows them: in the order they're written, with a line counting
/// them, or nothing when there are none.
fn shown_errors(mut errors: Vec<(u32, String, bool, String)>) -> String {
    errors.sort_by(|one, other| (one.0, &one.1).cmp(&(other.0, &other.1)));
    if errors.is_empty() {
        return String::new();
    }
    let fixable = errors.iter().filter(|error| error.2).count();
    let shown: Vec<String> = errors.into_iter().map(|error| error.3).collect();
    format!("{}{} lints, {fixable} of which `fennec fix` can fix\n", shown.join("\n"), shown.len())
}

fn linted(path: &str, json: bool) -> ExitCode {
    let Some(program) = loaded(path, json) else { return ExitCode::FAILURE };
    // Lowering takes the module with what checking works out written in, and the linter the
    // module as it's written, so checking writes into a copy. Of the standard library, only what
    // the program reaches is checked, with every type's fields for lowering: checking all of it
    // would take most of the time a lint has.
    let mut resolved = program.module.clone();
    let signatures = match fennec_check::check_to_lower(&mut resolved) {
        Ok(signatures) => signatures,
        Err(diagnostics) => return rejected(&program, &diagnostics, json),
    };
    if let Some((run, diagnostics)) = what_a_run_rejects(path, &program) {
        return rejected(&run, &diagnostics, json);
    }
    let found = lints_of(&program);
    let costs = fennec_ir::costs(&fennec_ir::lower(&resolved, &signatures));
    // A lint that's a hint flags correct code, so it's shown with the hints and fails nothing.
    let (found, mut hints): (Vec<_>, Vec<_>) =
        found.into_iter().partition(|lint| lint.severity == fennec_lint::Severity::Error);
    hints.extend(fennec_lint::costs(&costs));
    if json {
        let shown: Vec<Finding<'_>> = found.iter().chain(&hints).map(Finding::linted).collect();
        print!("{}", report::json(&program.sources, &shown));
    } else {
        let diagnostics: Vec<_> = found.iter().map(|lint| lint.diagnostic.clone()).collect();
        eprint!("{}", program.sources.render_all(&diagnostics));
        for hint in &hints {
            println!("{}", report::hint_line(&program.sources, &Finding::linted(hint)));
        }
        if !found.is_empty() {
            let fixable = found.iter().filter(|lint| !lint.diagnostic.fixes.is_empty()).count();
            eprintln!("{} lints, {fixable} of which `fennec fix` can fix", found.len());
        }
    }
    if found.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Every kit in the repository: the projects among the examples, and the self-hosted compiler.
    fn kits() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut found = vec![root.join("selfhost"), root.join("examples/proof")];
        for folder in ["examples/projects", "examples/projects/errors"] {
            let Ok(entries) = std::fs::read_dir(root.join(folder)) else { continue };
            found.extend(
                entries.flatten().map(|entry| entry.path()).filter(|path| path.join("kit.toml").is_file()),
            );
        }
        found.sort();
        found
    }

    /// What a lint says and where, to compare lints found two ways.
    fn said(lints: &[fennec_lint::Lint]) -> Vec<(u32, u32, &'static str, String)> {
        let mut said: Vec<_> = lints
            .iter()
            .map(|lint| {
                (
                    lint.diagnostic.span.start,
                    lint.diagnostic.span.end,
                    lint.rule,
                    lint.diagnostic.message.clone(),
                )
            })
            .collect();
        said.sort();
        said
    }

    /// Whether linting each file on its own, and then across them, finds what linting all of a
    /// program finds.
    fn alone_as_together(module: &fennec_syntax::Module, sources: &fennec_syntax::Sources) -> usize {
        let mut whole = fennec_lint::lint(module, sources);
        whole.extend(fennec_lint::kit_lints(module));
        let mut parts = fennec_lint::across_files(module, sources);
        for (file, _) in sources.files() {
            parts.extend(fennec_lint::within_files(module, sources, true, &|wanted| wanted == file));
        }
        assert_eq!(said(&parts), said(&whole));
        whole.len()
    }

    /// Every example in the repository as a file of one program: many files of every kind of code.
    /// The lint tests have the lints, each beside another file.
    #[test]
    fn each_example_linted_on_its_own_finds_what_linting_them_together_finds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "fen"))
            .collect();
        paths.sort();
        let mut sources = fennec_syntax::Sources::new();
        let mut module = fennec_syntax::Module::default();
        for path in &paths {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let base = sources.add(path.display().to_string(), text.as_str());
            if let Ok(parsed) = fennec_syntax::parse_at(&text, base) {
                module.items.extend(parsed.items);
            }
        }
        alone_as_together(&module, &sources);
        assert!(paths.len() > 100, "only {} examples", paths.len());
    }

    #[test]
    fn each_file_linted_on_its_own_finds_what_linting_all_of_a_kit_finds_there() {
        let mut linted = 0;
        for kit in kits() {
            let shown = kit.display().to_string();
            let overlay = fennec_kit::Overlay::new();
            let Ok(program) = fennec_kit::load_with(&kit, &shown, fennec_kit::Entry::Everything, &overlay)
            else {
                continue;
            };
            alone_as_together(&program.module, &program.sources);
            linted += 1;
        }
        assert!(linted > 10, "only {linted} kits loaded");
    }
}
