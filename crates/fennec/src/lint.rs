//! `fennec lint [path]`: what the linter finds in a kit or a file, then hints about what may copy
//! or allocate. Hints flag correct code, so only lints fail the command.

use std::process::ExitCode;

use fennec_ir::Cost;

pub(crate) fn lint(path: &str) -> ExitCode {
    let Some(program) = crate::load(path, fennec_kit::Entry::Everything) else { return ExitCode::FAILURE };
    let signatures = match fennec_check::check(&program.module) {
        Ok(signatures) => signatures,
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            return ExitCode::FAILURE;
        }
    };
    let found = fennec_lint::lint(&program.module, &program.sources);
    let diagnostics: Vec<_> = found.iter().map(|lint| lint.diagnostic.clone()).collect();
    eprint!("{}", program.sources.render_all(&diagnostics));
    let lowered = fennec_ir::lower(&program.module, &signatures);
    for finding in fennec_ir::costs(&lowered) {
        // What may copy is always worth knowing, and what allocates when it runs often.
        if matches!(finding.cost, Cost::Copy { .. }) || finding.is_hot() {
            let (file, line, column, _) = program.sources.locate(finding.span).unwrap_or(("?", 0, 0, ""));
            println!("hint: {file}:{line}:{column}: {}", finding.message());
        }
    }
    if found.is_empty() {
        return ExitCode::SUCCESS;
    }
    let fixable = found.iter().filter(|lint| !lint.diagnostic.fixes.is_empty()).count();
    eprintln!("{} lints, {fixable} of which `fennec fix` can fix", found.len());
    ExitCode::FAILURE
}
