//! `fennec test`: checks a program, then runs its `test` blocks and reports each one.

use std::process::ExitCode;

pub(crate) fn test(path: &str) -> ExitCode {
    let Some((source, module)) = crate::load(path) else { return ExitCode::FAILURE };
    if !crate::run::check(path, &source, &module) {
        return ExitCode::FAILURE;
    }
    let mut out = std::io::sink();
    let mut interp = match fennec_interp::Interp::new(&mut out) {
        Ok(interp) => interp,
        Err(diagnostic) => {
            eprint!("{}", diagnostic.render(path, &source));
            return ExitCode::FAILURE;
        }
    };
    if let Err(diagnostic) = interp.load(&module) {
        eprint!("{}", diagnostic.render(path, &source));
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
                eprint!("{}", diagnostic.render(path, &source));
            }
        }
    }
    let passed = outcomes.len() - failed;
    println!("{passed} passed, {failed} failed");
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
