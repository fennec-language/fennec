//! Fennec's native engine for development builds: compiles a program to machine code with
//! Cranelift and runs it. Ints, floats and bools live in machine registers and every
//! operation on them is native. Other values live in the runtime, which native code calls
//! into; anything the JIT doesn't compile natively yet runs there as one small step, so
//! programs behave exactly as they do in the interpreter.

mod helpers;
mod layout;
mod lower;
mod reach;
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

/// How a program was compiled, and what its run left behind: for tests of the JIT itself.
#[derive(Debug, Default)]
pub struct Report {
    /// The functions and methods the JIT couldn't compile, which ran in the interpreter.
    pub interpreted: Vec<String>,
    /// How many operations were handed to the interpreter one at a time.
    pub handed_over: usize,
    /// How many compiled functions needed a frame in the runtime for their values.
    pub with_frames: usize,
    /// Temporaries left in the runtime at the end: none, when the run succeeds.
    pub leftover_temps: usize,
    /// Frames left in the runtime at the end: none, when the run succeeds.
    pub leftover_frames: usize,
}

/// Compiles `module` to native code and runs its `main`, writing what it prints to `out`.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<(), JitError> {
    run_with_report(module, out).map(|_| ())
}

/// The stack the program runs on. It's only reserved, not used, until calls nest deeply.
const STACK_BYTES: usize = 1 << 30;

/// How much of the stack native calls may use, leaving the rest for the runtime and the
/// interpreter, which native code calls into.
const NATIVE_STACK_BYTES: usize = 1 << 28;

/// Like `run`, also reporting how the program compiled and what the run left behind.
///
/// The program runs on a thread of its own with a big stack, so native code knows how deep
/// its calls may nest: it compares the stack pointer against a limit, which costs almost
/// nothing per call.
pub fn run_with_report(module: &Module, out: &mut dyn Write) -> Result<Report, JitError> {
    let ran = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || compile_and_run(module))
            .map(std::thread::ScopedJoinHandle::join)
    });
    let (output, report) = match ran {
        Ok(Ok(result)) => result?,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(err) => {
            return Err(JitError::Unsupported(Diagnostic::new(
                format!("couldn't start the JIT's thread: {err}"),
                Span::default(),
            )));
        }
    };
    out.write_all(&output).and_then(|()| out.flush()).map_err(|err| {
        JitError::Failed(Diagnostic::new(format!("couldn't write output: {err}"), Span::default()))
    })?;
    report
}

/// Compiles and runs a program on the current thread, giving back what it printed and how
/// it went.
fn compile_and_run(module: &Module) -> Result<(Vec<u8>, Result<Report, JitError>), JitError> {
    let here = 0_u8;
    let stack_limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
    let signatures = fennec_check::check(module).map_err(|diagnostics| {
        JitError::Unsupported(
            diagnostics.into_iter().next().unwrap_or_else(|| Diagnostic::new("", Span::default())),
        )
    })?;
    let prelude: Vec<Module> = fennec_syntax::PRELUDE
        .iter()
        .filter_map(|source| fennec_syntax::parse_prelude(source).ok())
        .collect();
    let program =
        types::Program::from_module(module, &prelude, &signatures).map_err(JitError::Unsupported)?;

    let (entry, tables, compilation) =
        lower::compile(&program, &signatures, stack_limit).map_err(JitError::Unsupported)?;
    let result = runtime::execute(&entry, module, tables).map_err(JitError::Failed)?;
    let report = match result.failure {
        Some(failure) => Err(JitError::Failed(failure)),
        None => Ok(Report {
            interpreted: {
                let mut interpreted = compilation.interpreted;
                interpreted.extend(program.excluded);
                interpreted.sort();
                interpreted
            },
            handed_over: compilation.handed_over,
            with_frames: compilation.with_frames,
            leftover_temps: result.leftovers.temps,
            leftover_frames: result.leftovers.frames,
        }),
    };
    Ok((result.output, report))
}
