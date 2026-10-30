//! `fennec run`: checks a program, then runs its `main`, with the interpreter or, with
//! `--jit`, as native code.

use std::io::{BufReader, BufWriter, Write};
use std::process::ExitCode;

use fennec_interp::Inputs;

/// Runs the program at `path`, handing `main` the `arguments` and this process's input.
pub(crate) fn run(path: &str, jit: bool, arguments: &[String]) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !check(&mut program) {
        return ExitCode::FAILURE;
    }
    let module = &program.module;
    let mut out = BufWriter::new(std::io::stdout().lock());
    let inputs = || Inputs {
        program: path.to_owned(),
        arguments: arguments.to_vec(),
        stdin: Box::new(BufReader::new(std::io::stdin())),
    };
    let result = if jit { native(module, &mut out, inputs) } else { interpret(module, &mut out, inputs()) };
    let flushed = out.flush();
    match (result, flushed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(diagnostic), _) => {
            eprint!("{}", program.sources.render(&diagnostic));
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
fn native(
    module: &fennec_syntax::Module,
    out: &mut dyn Write,
    inputs: impl Fn() -> Inputs,
) -> Result<(), fennec_syntax::Diagnostic> {
    match fennec_jit::run_with_inputs(module, out, inputs()) {
        Ok(()) => Ok(()),
        Err(fennec_jit::JitError::Failed(diagnostic)) => Err(diagnostic),
        Err(fennec_jit::JitError::Unsupported(diagnostic)) => {
            eprintln!("note: {}; using the interpreter", diagnostic.message);
            interpret(module, out, inputs())
        }
    }
}

fn interpret(
    module: &fennec_syntax::Module,
    out: &mut dyn Write,
    inputs: Inputs,
) -> Result<(), fennec_syntax::Diagnostic> {
    let mut interp = fennec_interp::Interp::new(out)?;
    interp.give(inputs);
    interp.run_program(module)
}

/// Type-checks a program before it runs, printing every problem found, and names the enum
/// of each leading-dot case its expected type decided. Returns whether it passed.
pub(crate) fn check(program: &mut fennec_kit::Program) -> bool {
    match fennec_check::check_and_resolve(&mut program.module) {
        Ok(_) => true,
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            false
        }
    }
}
