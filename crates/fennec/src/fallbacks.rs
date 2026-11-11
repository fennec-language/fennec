//! `fennec dev fallbacks [path]`: every function LLVM doesn't take yet in what `main` reaches,
//! which keeps the program from building. `--llvm` says the same, as it used to.

use std::process::ExitCode;

/// `fennec dev checks [path]`: each check native code keeps in what `main` reaches, where it is
/// and what it's for, then how many of each kind there are.
pub(crate) fn checks(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let reached = crate::aot::main_reaches(&program.module);
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

/// `fennec dev waits [path]`: which functions of what `main` reaches may wait, and so would be
/// compiled as state machines that can pause, as plain names and with blocking reads counted
/// too, and the longest ways down to a wait.
pub(crate) fn waits(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let reached = crate::aot::main_reaches(&program.module);
    let lowered = match crate::aot::lowered_for_native(&reached) {
        Ok(lowered) => lowered,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::FAILURE;
        }
    };
    for blocking in [false, true] {
        let waits = fennec_llvm::may_wait(&lowered, blocking);
        let (all, marked) = (waits.functions.len(), waits.marked.len());
        let indirect = waits.marked.values().filter(|why| **why == fennec_llvm::Why::Indirectly).count();
        let own = waits.marked.values().filter(|why| matches!(why, fennec_llvm::Why::Itself(_))).count();
        let counted = if blocking { "with blocking reads" } else { "network, locks, sleeps and parallel" };
        println!(
            "{counted}: {marked} of {all} functions may wait ({own} wait themselves, {indirect} only through a function value or a generator)"
        );
        let mut chains: Vec<Vec<String>> = waits.marked.keys().map(|name| waits.chain(name)).collect();
        chains.sort_by_key(|chain| std::cmp::Reverse(chain.len()));
        for chain in chains.iter().take(3) {
            let end = chain.last().and_then(|last| waits.marked.get(last));
            let why = match end {
                Some(fennec_llvm::Why::Itself(what)) => what.clone(),
                _ => "a function value".to_owned(),
            };
            println!("  {} deep: {} ({why})", chain.len(), chain.join(" -> "));
        }
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
    let reached = crate::aot::main_reaches(&program.module);
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
