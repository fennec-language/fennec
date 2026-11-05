//! `fennec fix`: makes the changes the type checker and the linter suggest, like writing in the
//! return type a `def` gives back, in a file or across a kit. It checks again after each round of changes,
//! since one fix can show the next, and then reports whatever it couldn't fix.

use std::path::Path;
use std::process::ExitCode;

/// How many rounds of fixes to make before giving up on reaching a program with none left.
const ROUNDS: usize = 8;

pub(crate) fn fix(path: &str) -> ExitCode {
    if let Err(code) = moved_imports(path) {
        return code;
    }
    for _ in 0..ROUNDS {
        let Some(program) = crate::load(path, fennec_kit::Entry::Everything) else {
            return ExitCode::FAILURE;
        };
        // What's left once the program checks is the linter's. Some of its lints have no fix,
        // and those are for `fennec lint` to report.
        let Err(diagnostics) = fennec_check::check(&program.module) else {
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
        let changed = program.sources.fixed(&diagnostics);
        if changed.is_empty() {
            eprint!("{}", program.sources.render_all(&diagnostics));
            eprintln!("\nthese have no fix `fennec fix` can make");
            return ExitCode::FAILURE;
        }
        if let Err(code) = write_all(changed) {
            return code;
        }
    }
    eprintln!("error: fixes kept showing more fixes; run `fennec fix` again to go on");
    ExitCode::FAILURE
}

/// `fennec fix --mut-to-ref`: writes `ref` where the language has moved from `mut`, in each file or
/// folder named, or the current folder. It reads each file on its own and doesn't check it, so it
/// runs on anything that can be read as tokens. It asks for by name, so `fennec fix` never renames
/// on its own, and what it can't decide, a person reads.
pub(crate) fn mut_to_ref(paths: &[&str]) -> ExitCode {
    let mut files = Vec::new();
    for path in if paths.is_empty() { &["."] } else { paths } {
        if let Err(err) = crate::fmt::collect(Path::new(path), true, &mut files) {
            eprintln!("error: couldn't read {path}: {err}");
            return ExitCode::FAILURE;
        }
    }
    let mut failed = false;
    for file in &files {
        let shown = file.strip_prefix(".").unwrap_or(file).display();
        let source = match std::fs::read_to_string(file) {
            Ok(source) => source,
            Err(err) => {
                eprintln!("error: couldn't read {shown}: {err}");
                failed = true;
                continue;
            }
        };
        let renamed = match fennec_syntax::mut_to_ref(&source) {
            Ok(renamed) => renamed,
            Err(diagnostic) => {
                let (line, column) = crate::fmt::position(&source, diagnostic.span.start);
                eprintln!("skipped {shown}: it can't be read ({line}:{column}: {})", diagnostic.message);
                continue;
            }
        };
        if renamed.text != source {
            if let Err(err) = std::fs::write(file, &renamed.text) {
                eprintln!("error: couldn't write {shown}: {err}");
                failed = true;
                continue;
            }
            println!("fixed {shown}");
        }
        for span in renamed.left {
            let (line, _) = crate::fmt::position(&source, span.start);
            eprintln!("{shown}:{line}: this still says `mut`, so read it");
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
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
