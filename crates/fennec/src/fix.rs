//! `fennec fix`: makes the changes the type checker and the linter suggest, like writing in the
//! return type a `def` gives back, in a file or across a kit. It checks again after each round of changes,
//! since one fix can show the next, and then reports whatever it couldn't fix.

use std::path::Path;
use std::process::ExitCode;

/// How many rounds of fixes to make before giving up on reaching a program with none left.
const ROUNDS: usize = 8;

/// Whether to say nothing of what's fixed, for `--rust-strings`, which migrates a program for
/// each literal through one scratch file.
static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `fennec fix --migrate` on one file, saying nothing.
pub(crate) fn migrate_quietly(path: &str) {
    QUIET.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = migrate_forms(path);
}

fn quiet() -> bool {
    QUIET.load(std::sync::atomic::Ordering::Relaxed)
}

/// With `migrate`, it makes only the rewrites to the forms of 2026-11-11, which the migration
/// runs over every file at once.
pub(crate) fn fix(path: &str, migrate: bool) -> ExitCode {
    if migrate {
        return migrate_forms(path);
    }
    if let Err(code) = parse_fixes(path) {
        return code;
    }
    if let Err(code) = moved_imports(path) {
        return code;
    }
    if let Err(code) = loops_over_copies(path) {
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
            lints.extend(fennec_lint::needless_conversions(&program.module, &program.sources));
            lints.extend(fennec_lint::double_lookups(&program.module, &program.sources));
            lints.extend(fennec_lint::self_compares(&program.module, &program.sources));
            let mut diagnostics: Vec<_> = lints.into_iter().map(|lint| lint.diagnostic).collect();
            diagnostics.extend(fennec_check::result_fixes(&program.module, &program.sources));
            // `x.len()` as `len(x)`, the one rewrite of 2026-11-11 a program that checks can still use.
            diagnostics.extend(fennec_check::migration_fixes(&program.module, &program.sources));
            program.no_fixes_in_dependencies(&mut diagnostics);
            let changed = program.own_changes(program.sources.fixed(&diagnostics));
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
        fennec_check::add_capability_fixes(&mut diagnostics, &program.module, &program.sources);
        diagnostics.extend(fennec_check::result_fixes(&program.module, &program.sources));
        // A form of before 2026-11-11 is refused: its rewrite is the fix.
        diagnostics.extend(fennec_check::migration_fixes(&program.module, &program.sources));
        // A tool changes only the kit it's run in: a problem in a dependency has no fix here.
        program.no_fixes_in_dependencies(&mut diagnostics);
        let changed = program.own_changes(program.sources.fixed(&diagnostics));
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

/// `fennec fix --migrate`: the rewrites to the forms of 2026-11-11 and nothing else, so a
/// lint's fix never rides along. A program that doesn't check gets the ones that need no types. When a round's rewrites leave
/// a problem with a fix, like a name a file now needs to import, that fix is made next.
fn migrate_forms(path: &str) -> ExitCode {
    let mut first = true;
    for _ in 0..ROUNDS {
        let loaded = fennec_kit::load_with(
            Path::new(path),
            path,
            fennec_kit::Entry::Everything,
            &fennec_kit::Overlay::new(),
        );
        let program = match loaded {
            Ok(program) => program,
            // A rewrite can leave a file naming what it doesn't import, which loading finds.
            Err(_) if first => return migrate_syntax(path),
            Err(problems) => {
                let changed = problems.sources.fixed(&problems.diagnostics);
                if changed.is_empty() {
                    eprint!("{}", problems.render());
                    return ExitCode::FAILURE;
                }
                if let Err(code) = write_rewritten(changed) {
                    return code;
                }
                continue;
            }
        };
        // Typed as written, old forms and all, which `check` refuses.
        let (checks, mut diagnostics) = match fennec_check::check_typed(&program.module) {
            Ok(_) => (true, fennec_check::migration_fixes(&program.module, &program.sources)),
            Err(_) if first => return migrate_syntax(path),
            Err(problems) => (false, problems),
        };
        first = false;
        program.no_fixes_in_dependencies(&mut diagnostics);
        let changed = program.own_changes(program.sources.fixed(&diagnostics));
        if changed.is_empty() {
            if checks {
                return ExitCode::SUCCESS;
            }
            eprint!("{}", program.sources.render_all(&diagnostics));
            eprintln!("\nthe rewrites left these, which have no fix");
            return ExitCode::FAILURE;
        }
        if let Err(code) = write_rewritten(changed) {
            return code;
        }
    }
    eprintln!("error: rewrites kept showing more; run `fennec fix --migrate` again to go on");
    ExitCode::FAILURE
}

/// The rewrites that need no types, file by file, for a program that doesn't check, like an
/// example of a mistake. A file that doesn't parse is left as it is.
fn migrate_syntax(path: &str) -> ExitCode {
    let mut files = Vec::new();
    if let Err(err) = crate::fmt::collect(Path::new(path), true, &mut files) {
        eprintln!("error: couldn't read {path}: {err}");
        return ExitCode::FAILURE;
    }
    for file in files {
        let shown = file.to_string_lossy().into_owned();
        for _ in 0..ROUNDS {
            let Ok(text) = std::fs::read_to_string(&file) else { break };
            let Ok(module) = fennec_syntax::parse(&text) else { break };
            let mut sources = fennec_syntax::Sources::new();
            let base = sources.add(shown.clone(), text.clone());
            let rewrites = fennec_check::syntax_migration_fixes(&module, &sources);
            let Some(fixed) =
                fennec_syntax::apply_fixes(&text, base, &rewrites).filter(|fixed| *fixed != text)
            else {
                break;
            };
            if let Err(code) = write_rewritten(vec![(shown.clone(), fixed)]) {
                return code;
            }
        }
    }
    if !quiet() {
        eprintln!("note: {path} doesn't check, so only the rewrites that need no types were made");
    }
    ExitCode::SUCCESS
}

/// `fennec fix --migrate --std`, in the Fennec checkout: the standard library's own code, written
/// back to `crates/syntax/src/stdlib`. It reads the library this `fennec` was built with, so
/// after a round that changed something, build it again and run it again.
pub(crate) fn migrate_std() -> ExitCode {
    let Ok(module) = fennec_syntax::parse("def main(stdout: Stdout):\n    pass\n") else {
        return ExitCode::FAILURE;
    };
    let mut by_file: std::collections::BTreeMap<&str, (&str, u32, Vec<fennec_syntax::Diagnostic>)> =
        std::collections::BTreeMap::new();
    for fix in fennec_check::std_migration_fixes(&module) {
        let Some(at) = fix.fixes.first().map(|edit| edit.span) else { continue };
        let Some((name, text, base)) = fennec_syntax::prelude_file_at(at) else { continue };
        by_file.entry(name).or_insert_with(|| (text, base, Vec::new())).2.push(fix);
    }
    for (name, (text, base, fixes)) in by_file {
        let file = format!("crates/syntax/src/stdlib/{name}.fen");
        // What this `fennec` was built with has to be what's in the checkout, or the rewrites
        // land in the wrong places.
        if std::fs::read_to_string(&file).ok().as_deref() != Some(text) {
            eprintln!(
                "error: {file} isn't what this `fennec` was built with: build it again from this checkout"
            );
            return ExitCode::FAILURE;
        }
        let Some(fixed) = fennec_syntax::apply_fixes(text, base, &fixes) else { continue };
        if let Err(code) = write_rewritten(vec![(file, fixed)]) {
            return code;
        }
    }
    ExitCode::SUCCESS
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

/// Rewrites each loop that goes over `copy(xs)` to go over a name made the line before,
/// `xs_before = xs`, as the loop rule says to. It comes before the checker's own fix for a plain
/// `copy(...)`, which takes the call off: a loop over `xs` itself couldn't change `xs`, which is
/// what the copy was there for.
fn loops_over_copies(path: &str) -> Result<(), ExitCode> {
    let mut files = Vec::new();
    if crate::fmt::collect(Path::new(path), true, &mut files).is_err() {
        // Loading says what's wrong with the path.
        return Ok(());
    }
    let mut changed = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        let mut lines: Vec<String> = Vec::new();
        for line in source.split_inclusive('\n') {
            match loop_over_copy(line, &source) {
                Some((named, looped)) => {
                    lines.push(named);
                    lines.push(looped);
                }
                None => lines.push(line.to_owned()),
            }
        }
        let text = lines.concat();
        if text != source {
            changed.push((file.to_string_lossy().into_owned(), text));
        }
    }
    write_all(changed)
}

/// A line `for ... in ...copy(xs)...:` as the line that names `xs` first and the loop over that
/// name, or None for any other line, or a `copy` with fields to change, which stays.
fn loop_over_copy(line: &str, source: &str) -> Option<(String, String)> {
    let indent = line.get(..line.len() - line.trim_start().len())?;
    let rest = line.trim_start().strip_prefix("for ")?;
    let head = rest.find(" in ")? + " in ".len();
    let opened = rest.get(head..)?.find("copy(")? + head;
    let inside = opened + "copy(".len();
    let mut depth = 0_usize;
    let mut closed = None;
    for (at, ch) in rest.get(inside..)?.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => {
                closed = Some(inside + at);
                break;
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' | '=' if depth == 0 => return None,
            _ => {}
        }
    }
    let closed = closed?;
    let copied = rest.get(inside..closed)?.trim();
    let last = copied.rsplit(['.', '[']).next()?.trim_end_matches(']');
    let stem: String = last.chars().filter(|ch| ch.is_alphanumeric() || *ch == '_').collect();
    if stem.is_empty() {
        return None;
    }
    // A name the file doesn't use already.
    let word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let taken = |name: &str| {
        source.match_indices(name).any(|(at, _)| {
            let before = source.get(..at).and_then(|text| text.chars().next_back());
            let after = source.get(at + name.len()..).and_then(|text| text.chars().next());
            !before.is_some_and(word) && !after.is_some_and(word)
        })
    };
    let mut name = format!("{stem}_before");
    let mut count = 2;
    while taken(&name) {
        name = format!("{stem}_before_{count}");
        count += 1;
    }
    let named = format!("{indent}{name} = {copied}\n");
    let looped = format!("{indent}for {}{name}{}", rest.get(..opened)?, rest.get(closed + 1..)?);
    Some((named, looped))
}

/// Writes each changed file, saying so, formatted when it was before.
fn write_all(changed: Vec<(String, String)>) -> Result<(), ExitCode> {
    let changed = changed.into_iter().map(|(file, text)| {
        let text = formatted_like_before(&file, text);
        (file, text)
    });
    write_rewritten(changed.collect())
}

/// Writes each changed file as it was rewritten, saying so: the migration changes only the forms
/// it moves, and never formats a line it didn't.
fn write_rewritten(changed: Vec<(String, String)>) -> Result<(), ExitCode> {
    for (file, text) in changed {
        if let Err(err) = std::fs::write(&file, text) {
            eprintln!("error: couldn't write {file}: {err}");
            return Err(ExitCode::FAILURE);
        }
        if !quiet() {
            println!("fixed {file}");
        }
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
