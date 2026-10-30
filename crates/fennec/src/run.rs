//! `fennec run`: checks a program, then runs its `main`, with the interpreter or, with
//! `--jit`, as native code.

use std::io::{BufWriter, Write};
use std::process::ExitCode;

pub(crate) fn run(path: &str) -> ExitCode {
    execute(path, false)
}

pub(crate) fn run_jit(path: &str) -> ExitCode {
    execute(path, true)
}

fn execute(path: &str, jit: bool) -> ExitCode {
    let Some((source, module)) = crate::load(path) else { return ExitCode::FAILURE };
    if !check(path, &source, &module) {
        return ExitCode::FAILURE;
    }
    let mut out = BufWriter::new(std::io::stdout().lock());
    let result = if jit { native(&module, &mut out) } else { fennec_interp::run(&module, &mut out) };
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

/// Runs a program as native code, or with the interpreter if it uses something the JIT
/// can't compile yet. Nothing runs before the JIT decides, so nothing runs twice.
fn native(module: &fennec_syntax::Module, out: &mut dyn Write) -> Result<(), fennec_syntax::Diagnostic> {
    match fennec_jit::run(module, out) {
        Ok(()) => Ok(()),
        Err(fennec_jit::JitError::Failed(diagnostic)) => Err(diagnostic),
        Err(fennec_jit::JitError::Unsupported(diagnostic)) => {
            eprintln!("note: {}; using the interpreter", diagnostic.message);
            fennec_interp::run(module, out)
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
