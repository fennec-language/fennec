//! `fennec dev copies [path]`: every place a program may copy a large value or allocate, with
//! why. Code that runs in loops comes first, since that's where it costs the most.

use std::process::ExitCode;

pub(crate) fn copies(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if let Err(diagnostics) = fennec_check::check(&program.module) {
        eprint!("{}", program.sources.render_all(&diagnostics));
        return ExitCode::FAILURE;
    }
    // Only what runs matters. It checked whole, so what's left does too.
    fennec_build::keep_what_main_reaches(&mut program.module);
    let checked = match fennec_check::check(&program.module) {
        Ok(checked) => checked,
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            return ExitCode::FAILURE;
        }
    };
    let lowered = fennec_ir::lower(&program.module, &checked);
    let findings = fennec_ir::costs(&lowered);
    for finding in &findings {
        let (file, line, column, text) = program.sources.locate(finding.span).unwrap_or(("?", 0, 0, ""));
        let hot = match (finding.loops, &finding.called_from_loop) {
            (0, None) => String::new(),
            (0, Some(caller)) => format!(" (called from a loop in `{caller}`)"),
            (1, _) => " (in a loop)".to_owned(),
            (loops, _) => format!(" (in {loops} loops)"),
        };
        println!("{file}:{line}:{column} in `{}`{hot}: {}", finding.function, finding.message());
        println!("    {}", text.trim());
    }
    for skipped in &lowered.not_lowered {
        println!("not looked at: `{}`, since {}", skipped.function, skipped.why);
    }
    let hot = findings.iter().filter(|finding| finding.is_hot()).count();
    println!("{} places may copy or allocate, {hot} of them in loops", findings.len());
    ExitCode::SUCCESS
}
