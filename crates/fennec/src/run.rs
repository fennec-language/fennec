//! `fennec run`: checks a program, then runs its `main`, with the interpreter or, with
//! `--jit`, as native code.

use std::io::{BufReader, BufWriter, Write};
use std::process::ExitCode;

use fennec_interp::Inputs;

/// Runs the program at `path`, handing `main` the `arguments` and this process's input.
pub(crate) fn run(path: &str, jit: bool, arguments: &[String]) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    let signatures = match fennec_check::check_to_run(&mut program.module) {
        Ok(signatures) => signatures,
        Err(diagnostics) => return reject(&program, &diagnostics),
    };
    let module = &program.module;
    let mut out = BufWriter::new(std::io::stdout().lock());
    let inputs = || Inputs {
        program: path.to_owned(),
        arguments: arguments.to_vec(),
        stdin: Box::new(BufReader::new(std::io::stdin())),
    };
    let result = if jit {
        native(module, &signatures, path, &mut out, inputs)
    } else {
        interpret(module, &mut out, inputs())
    };
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
    signatures: &fennec_check::Signatures,
    path: &str,
    out: &mut dyn Write,
    inputs: impl Fn() -> Inputs,
) -> Result<(), fennec_syntax::Diagnostic> {
    // Machine code is reused from the cache in `~/.fennec`, unless `FENNEC_NO_CACHE` is set.
    let cache = fennec_build::fennec_home()
        .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
        .map(|home| fennec_build::Cache::new(&home));
    let ran = match &cache {
        Some(cache) => {
            // A program's machine code is kept together, under its absolute path.
            let program = std::fs::canonicalize(path)
                .map_or_else(|_| path.to_owned(), |path| path.display().to_string());
            fennec_jit::run_cached(module, Some(signatures), out, inputs(), cache, &program).map(|_| ())
        }
        None => fennec_jit::run_with_inputs(module, out, inputs()),
    };
    match ran {
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
            reject(program, &diagnostics);
            false
        }
    }
}

/// Prints the problems a check found.
fn reject(program: &fennec_kit::Program, diagnostics: &[fennec_syntax::Diagnostic]) -> ExitCode {
    eprint!("{}", program.sources.render_all(diagnostics));
    ExitCode::FAILURE
}
