//! `fennec dev fallbacks [path]`: every place a program's compiled code isn't native, with
//! why. Code inside loops comes first, deepest first, since that's where speed goes. With
//! `--llvm`, every function LLVM doesn't take yet, which would leave the whole program to the JIT.

use std::process::ExitCode;

pub(crate) fn fallbacks(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let mut notes = match fennec_jit::fallbacks(&program.module) {
        Ok(notes) => notes,
        Err(diagnostic) => {
            eprint!("{}", program.sources.render(&diagnostic));
            return ExitCode::FAILURE;
        }
    };
    if notes.is_empty() {
        println!("everything compiles to native code");
        return ExitCode::SUCCESS;
    }
    notes.sort_by_key(|note| std::cmp::Reverse(note.loops));
    for note in &notes {
        let place = program.sources.locate(note.span);
        let (path, line, column, text) = place.unwrap_or(("?", 0, 0, ""));
        let depth = match note.loops {
            0 => String::new(),
            1 => " (in a loop)".to_owned(),
            n => format!(" (in {n} loops)"),
        };
        println!("{path}:{line}:{column} in `{}`{depth}: {}", note.function, note.what);
        println!("    {}", text.trim());
    }
    println!("{} places aren't native", notes.len());
    ExitCode::SUCCESS
}

/// `fennec dev checks [path]`: each check native code keeps in what `main` reaches, where it is
/// and what it's for, then how many of each kind there are.
pub(crate) fn checks(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    let lowered = match crate::aot::lowered_for_native(&reached) {
        Ok(lowered) => lowered,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::FAILURE;
        }
    };
    let mut kinds: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for check in fennec_llvm::checks_left(&lowered) {
        let (path, line, column, text) = program.sources.locate(check.span).unwrap_or(("?", 0, 0, ""));
        let what = match &check.what {
            fennec_llvm::CheckKind::Builtin(name) => format!("Builtin {name}"),
            other => format!("{other:?}"),
        };
        println!("{path}:{line}:{column} in `{}`: {what}", check.function);
        println!("    {}", text.trim());
        *kinds.entry(what).or_default() += 1;
    }
    for (what, count) in &kinds {
        println!("{count} {what}");
    }
    ExitCode::SUCCESS
}

/// `fennec dev fallbacks --llvm [path]`: what LLVM doesn't take yet in what `main` reaches, a
/// line for each function, and how many there are.
pub(crate) fn not_lowered(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    match crate::aot::takes(&reached) {
        Ok(()) => println!("everything compiles to native code"),
        Err(why) => {
            let places: Vec<&str> = why.lines().filter(|line| !line.trim().is_empty()).collect();
            for place in &places {
                println!("{place}");
            }
            println!("{} places aren't native", places.len());
        }
    }
    ExitCode::SUCCESS
}
