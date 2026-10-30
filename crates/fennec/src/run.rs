//! `fennec run`: checks a program, then runs its `main`.

use std::io::{BufWriter, Write};
use std::process::ExitCode;

pub(crate) fn run(path: &str) -> ExitCode {
    let Some((source, module)) = crate::load(path) else { return ExitCode::FAILURE };
    if !check(path, &source, &module) {
        return ExitCode::FAILURE;
    }
    let mut out = BufWriter::new(std::io::stdout().lock());
    let result = fennec_interp::run(&module, &mut out);
    let flushed = out.flush();
    match (result, flushed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(diagnostic), _) => {
            eprint!("{}", diagnostic.render(path, &source));
            ExitCode::FAILURE
        }
        (Ok(()), Err(err)) => {
            eprintln!("error: couldn't write output: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Type-checks a program before it runs, printing every problem found. Returns whether it
/// passed.
pub(crate) fn check(path: &str, source: &str, module: &fennec_syntax::Module) -> bool {
    match fennec_check::check(module) {
        Ok(_) => true,
        Err(diagnostics) => {
            let rendered: Vec<String> =
                diagnostics.iter().map(|diagnostic| diagnostic.render(path, source)).collect();
            eprint!("{}", rendered.join("\n"));
            false
        }
    }
}
