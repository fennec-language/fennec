//! `fennec lint [--json] [path]`: what the checker and the linter find in a kit or a file, then
//! hints about what may copy or allocate. Hints flag correct code, so only errors fail it.
//!
//! `fennec check [--json] [path]`: only what the checker finds, which is quicker.
//!
//! `--json` writes the same findings as JSON on standard output, as `report` describes.

use std::collections::HashSet;
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
    rejected(&program, &diagnostics, json)
}

/// Every lint the linter finds in a program, in the order they're written.
fn lints_of(program: &fennec_kit::Program) -> Vec<fennec_lint::Lint> {
    let mut found = fennec_lint::lint(&program.module, &program.sources);
    found.extend(fennec_lint::needless_copies(&program.module, &program.sources));
    found.extend(fennec_lint::needless_conversions(&program.module, &program.sources));
    found.extend(fennec_lint::double_lookups(&program.module, &program.sources));
    found.extend(fennec_lint::self_compares(&program.module, &program.sources));
    if program.manifest.is_some() {
        found.extend(fennec_lint::kit_lints(&program.module));
    }
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
/// it again.
pub(crate) fn errors_remembered(path: &str) -> String {
    let Some(cache) = crate::run::cache() else { return errors_shown(path) };
    let Some(key) = files_key(path, &cache) else { return errors_shown(path) };
    let name = crate::run::passed_name(path);
    if let Some(kept) = cache.read("linted", &name)
        && let Some(shown) = kept.strip_prefix(key.as_bytes()).and_then(|rest| rest.strip_prefix(b"\n"))
    {
        return String::from_utf8_lossy(shown).into_owned();
    }
    let shown = errors_shown(path);
    // A cache that can't be written only means linting it again next time.
    let _ = cache.write("linted", &name, format!("{key}\n{shown}").as_bytes());
    shown
}

/// What the files of the program at `path` hold, as a hash: a kit's files by their sizes and when
/// they changed where those say they're as last seen, or a single file's text.
fn files_key(path: &str, cache: &fennec_build::Cache) -> Option<String> {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec lints 2026-11-07");
    hasher.update(env!("FENNEC_BUILD").as_bytes());
    match crate::image::files_now(path, cache) {
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
    let copies = fennec_lint::implicit_copies(&costs);
    if !copies.is_empty() {
        return rejected(&program, &copies, json);
    }
    let hints = fennec_lint::costs(&costs);
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
