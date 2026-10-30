//! Fennec's native engine for development builds: compiles a program to machine code with
//! Cranelift and runs it. It handles the core of the language so far (ints, floats, bools,
//! functions, `if`, loops and printing) and reports anything else as unsupported, so the
//! caller can fall back to the interpreter.

mod lower;
mod runtime;
mod types;

use std::io::Write;

use fennec_syntax::{Diagnostic, Module};

/// Why a program didn't run natively.
#[derive(Debug)]
pub enum JitError {
    /// The program uses something the JIT can't compile yet; the interpreter can run it.
    Unsupported(Diagnostic),
    /// The program compiled and ran, and failed the way the interpreter would report it.
    Failed(Diagnostic),
}

/// Compiles `module` to native code and runs its `main`, writing what it prints to `out`.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<(), JitError> {
    let program = types::Program::from_module(module).map_err(JitError::Unsupported)?;
    let entry = lower::compile(&program).map_err(JitError::Unsupported)?;
    let result = runtime::execute(&entry);
    out.write_all(result.output.as_bytes()).and_then(|()| out.flush()).map_err(|err| {
        JitError::Failed(Diagnostic::new(format!("couldn't write output: {err}"), program.main_span))
    })?;
    match result.failure {
        Some(failure) => Err(JitError::Failed(failure)),
        None => Ok(()),
    }
}
