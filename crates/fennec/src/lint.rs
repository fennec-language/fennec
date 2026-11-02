//! `fennec lint [--json] [path]`: what the checker and the linter find in a kit or a file, then
//! hints about what may copy or allocate. Hints flag correct code, so only errors fail it.
//!
//! `fennec check [--json] [path]`: only what the checker finds, which is quicker.
//!
//! `--json` writes the same findings as JSON on standard output, as `report` describes.

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
    match fennec_kit::load(std::path::Path::new(path), path, fennec_kit::Entry::Everything) {
        Ok(program) => Some(program),
        Err(rendered) if json => {
            print!("{}", report::unloaded(&rendered));
            None
        }
        Err(rendered) => {
            eprint!("{rendered}");
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
    if json {
        let found: Vec<Finding<'_>> = diagnostics.iter().map(Finding::checked).collect();
        print!("{}", report::json(&program.sources, &found));
    } else {
        eprint!("{}", program.sources.render_all(diagnostics));
    }
    if diagnostics.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn checked(path: &str, json: bool) -> ExitCode {
    let Some(program) = loaded(path, json) else { return ExitCode::FAILURE };
    let diagnostics = fennec_check::check(&program.module).err().unwrap_or_default();
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
    let found = fennec_lint::lint(&program.module, &program.sources);
    let hints = fennec_lint::costs(&fennec_ir::lower(&resolved, &signatures));
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
