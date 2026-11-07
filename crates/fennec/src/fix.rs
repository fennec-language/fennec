//! `fennec fix`: makes the changes the type checker and the linter suggest, like writing in the
//! return type a `def` gives back, in a file or across a kit. It checks again after each round of changes,
//! since one fix can show the next, and then reports whatever it couldn't fix.

use std::path::Path;
use std::process::ExitCode;

/// How many rounds of fixes to make before giving up on reaching a program with none left.
const ROUNDS: usize = 8;

pub(crate) fn fix(path: &str) -> ExitCode {
    if let Err(code) = parse_fixes(path) {
        return code;
    }
    if let Err(code) = moved_imports(path) {
        return code;
    }
    for _ in 0..ROUNDS {
        let Some(program) = crate::load(path, fennec_kit::Entry::Everything) else {
            return ExitCode::FAILURE;
        };
        // What's left once the program checks is the linter's. Some of its lints have no fix,
        // and those are for `fennec lint` to report.
        let Err(mut diagnostics) = fennec_check::check(&program.module) else {
            let mut lints = fennec_lint::lint(&program.module, &program.sources);
            lints.extend(fennec_lint::needless_copies(&program.module, &program.sources));
            lints.extend(fennec_lint::needless_conversions(&program.module, &program.sources));
            lints.extend(fennec_lint::double_lookups(&program.module, &program.sources));
            lints.extend(fennec_lint::self_compares(&program.module, &program.sources));
            let diagnostics: Vec<_> = lints.into_iter().map(|lint| lint.diagnostic).collect();
            let changed = program.sources.fixed(&diagnostics);
            if changed.is_empty() {
                return ExitCode::SUCCESS;
            }
            if let Err(code) = write_all(changed) {
                return code;
            }
            continue;
        };
        fennec_check::add_rename_fixes(&mut diagnostics, &program.module, &program.sources);
        fennec_check::add_pattern_fixes(&mut diagnostics, &program.sources);
        let changed = program.sources.fixed(&diagnostics);
        if changed.is_empty() {
            eprint!("{}", program.sources.render_all(&diagnostics));
            eprintln!("\nthese have no fix `fennec fix` can make");
            if diagnostics.iter().any(|diagnostic| diagnostic.hint.is_some()) {
                eprintln!("a `fix:` may change what the program does, so it's left to be made by hand");
            }
            return ExitCode::FAILURE;
        }
        if let Err(code) = write_all(changed) {
            return code;
        }
    }
    eprintln!("error: fixes kept showing more fixes; run `fennec fix` again to go on");
    ExitCode::FAILURE
}

/// Makes the fixes a file's parse error carries, like writing `ref` for each `mut` on a
/// parameter, before anything else, since a file that doesn't parse can't be checked. The
/// examples in its doc comments are parsed on their own, so they're asked too.
fn parse_fixes(path: &str) -> Result<(), ExitCode> {
    let mut files = Vec::new();
    if crate::fmt::collect(Path::new(path), true, &mut files).is_err() {
        // Loading says what's wrong with the path.
        return Ok(());
    }
    let mut changed = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        let mut text = source.clone();
        // Each round fixes everything up to the first mistake without a fix, so a round can
        // only show more when it changed something.
        for _ in 0..ROUNDS {
            let problem = match fennec_syntax::parse(&text) {
                Ok(_) => fennec_syntax::doc_examples(&text, 0, "").err(),
                Err(diagnostic) => Some(diagnostic),
            };
            let Some(diagnostic) = problem else { break };
            let Some(fixed) = fennec_syntax::apply_fixes(&text, 0, &[diagnostic]) else { break };
            text = fixed;
        }
        if text != source {
            changed.push((file.to_string_lossy().into_owned(), text));
        }
    }
    write_all(changed)
}

/// Rewrites each `from std.game import ...` under `path`, which can't load, to the module those
/// names moved to, with `IdMap` as `Table`. It comes first, since a module that's gone stops the
/// program loading before any other fix can be made. The checker's own fixes do the rest.
fn moved_imports(path: &str) -> Result<(), ExitCode> {
    let mut files = Vec::new();
    if crate::fmt::collect(Path::new(path), true, &mut files).is_err() {
        // Loading says what's wrong with the path.
        return Ok(());
    }
    let mut changed = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        let Ok(module) = fennec_syntax::parse(&source) else { continue };
        let mut text = source.clone();
        // From the end, so each import's place is still where it was.
        for item in module.items.iter().rev() {
            let fennec_syntax::Item::Import(import) = item else { continue };
            if import.module != ["std", "game"] || import.names.is_empty() {
                continue;
            }
            let names: Vec<String> = import
                .names
                .iter()
                .zip(&import.aliases)
                .map(|(name, alias)| {
                    let name = if name == "IdMap" { "Table" } else { name.as_str() };
                    alias.as_ref().map_or_else(|| name.to_owned(), |alias| format!("{name} as {alias}"))
                })
                .collect();
            let (start, end) = (import.span.start as usize, import.span.end as usize);
            let written = format!("from std.collections import {}", names.join(", "));
            if text.get(start..end).is_some_and(|old| old.starts_with("from")) {
                text.replace_range(start..end, &written);
            }
        }
        if text != source {
            changed.push((file.to_string_lossy().into_owned(), text));
        }
    }
    write_all(changed)
}

/// Writes each changed file, saying so.
fn write_all(changed: Vec<(String, String)>) -> Result<(), ExitCode> {
    for (file, text) in changed {
        let text = formatted_like_before(&file, text);
        if let Err(err) = std::fs::write(&file, text) {
            eprintln!("error: couldn't write {file}: {err}");
            return Err(ExitCode::FAILURE);
        }
        println!("fixed {file}");
    }
    Ok(())
}

/// A file's fixed text, formatted if the file was formatted before, since removing `-> None`
/// leaves a space behind.
fn formatted_like_before(file: &str, fixed: String) -> String {
    let was_formatted = std::fs::read_to_string(file)
        .is_ok_and(|before| fennec_fmt::format(&before).is_ok_and(|formatted| formatted == before));
    if !was_formatted {
        return fixed;
    }
    fennec_fmt::format(&fixed).unwrap_or(fixed)
}
