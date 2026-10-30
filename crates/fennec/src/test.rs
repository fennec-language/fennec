//! `fennec test`: checks a program, then runs its `test` blocks and reports each one. For a
//! kit, that's the tests in every file under `src`.

use std::process::ExitCode;

pub(crate) fn test(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Everything) else {
        return ExitCode::FAILURE;
    };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let render = |diagnostic: &fennec_syntax::Diagnostic| program.sources.render(diagnostic);
    let mut out = std::io::sink();
    let mut interp = match fennec_interp::Interp::new(&mut out) {
        Ok(interp) => interp,
        Err(diagnostic) => {
            eprint!("{}", render(&diagnostic));
            return ExitCode::FAILURE;
        }
    };
    if let Err(diagnostic) = interp.load(&program.module) {
        eprint!("{}", render(&diagnostic));
        return ExitCode::FAILURE;
    }
    let outcomes = interp.run_tests();
    if outcomes.is_empty() {
        println!("no tests in {path}");
        return ExitCode::SUCCESS;
    }
    let mut failed = 0;
    for outcome in &outcomes {
        let cases = if outcome.cases > 1 { format!(" ({} inputs)", outcome.cases) } else { String::new() };
        match &outcome.failure {
            None => println!("  ✓ {}{cases}", outcome.name),
            Some(diagnostic) => {
                failed += 1;
                println!("  ✗ {}", outcome.name);
                eprint!("{}", render(diagnostic));
            }
        }
    }
    let passed = outcomes.len() - failed;
    println!("{passed} passed, {failed} failed");
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
