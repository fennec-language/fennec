//! Fennec's native engine for development builds: compiles a program to machine code with
//! Cranelift and runs it. Ints, floats and bools live in machine registers and every
//! operation on them is native. Other values live in the runtime, which native code calls
//! into; anything the JIT doesn't compile natively yet runs there as one small step, so
//! programs behave exactly as they do in the interpreter.

mod helpers;
mod lower;
mod runtime;
mod types;

use std::io::Write;

use fennec_syntax::{Diagnostic, Module, Span};

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
    let signatures = fennec_check::check(module).map_err(|diagnostics| {
        JitError::Unsupported(
            diagnostics.into_iter().next().unwrap_or_else(|| Diagnostic::new("", Span::default())),
        )
    })?;
    let program = types::Program::from_module(module, &signatures).map_err(JitError::Unsupported)?;
    let (entry, tables) = lower::compile(&program).map_err(JitError::Unsupported)?;
    let result = runtime::execute(&entry, module, tables).map_err(JitError::Failed)?;
    out.write_all(&result.output).and_then(|()| out.flush()).map_err(|err| {
        JitError::Failed(Diagnostic::new(format!("couldn't write output: {err}"), program.main_span))
    })?;
    match result.failure {
        Some(failure) => Err(JitError::Failed(failure)),
        None => Ok(()),
    }
}
