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
    if diagnostics.is_empty()
        && let Some((run, diagnostics)) = what_a_run_rejects(path, &program)
    {
        return rejected(&run, &diagnostics, json);
    }
    rejected(&program, &diagnostics, json)
}

fn linted(path: &str, json: bool) -> ExitCode {
    let Some(program) = loaded(path, json) else { return ExitCode::FAILURE };
    // Lowering takes the module with what checking works out written in, and the linter the
    // module as it's written, so checking writes into a copy.
    let mut resolved = program.module.clone();
    let signatures = match fennec_check::check_and_resolve(&mut resolved) {
        Ok(signatures) => signatures,
        Err(diagnostics) => return rejected(&program, &diagnostics, json),
    };
    if let Some((run, diagnostics)) = what_a_run_rejects(path, &program) {
        return rejected(&run, &diagnostics, json);
    }
    let mut found = fennec_lint::lint(&program.module, &program.sources);
    found.extend(fennec_lint::needless_copies(&program.module, &program.sources));
    found.extend(fennec_lint::needless_conversions(&program.module, &program.sources));
    found.extend(fennec_lint::double_lookups(&program.module, &program.sources));
    found.extend(fennec_lint::self_compares(&program.module, &program.sources));
    if program.manifest.is_some() {
        found.extend(fennec_lint::kit_lints(&program.module));
    }
    found.sort_by_key(|lint| (lint.diagnostic.span.start, lint.rule));
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
