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
mod tests_native;
mod types;

pub use lower::notes::Note;
pub use tests_native::{BenchRun, NativeTests, run_bench, run_tests};

use std::io::Write;

use fennec_check::Signatures;
use fennec_interp::Inputs;
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
    /// Boxed values' blocks no one released: none, when the run succeeds.
    pub leftover_boxes: usize,
    /// How many functions' machine code came from the cache rather than being compiled.
    pub cached: usize,
    /// What the run allocated and copied.
    pub counts: fennec_interp::embed::Counts,
}

/// Compiles `module` to native code and runs its `main`, writing what it prints to `out`.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<(), JitError> {
    run_with_report(module, out).map(|_| ())
}

/// Like `run`, handing `main` the command-line arguments and standard input in `inputs`.
pub fn run_with_inputs(module: &Module, out: &mut dyn Write, inputs: Inputs) -> Result<(), JitError> {
    report_with_inputs(module, None, out, inputs, None).map(|_| ())
}

/// Like `run_with_inputs`, reusing machine code from `cache` and storing what it compiles
/// there, so the next run skips code generation for every function that's unchanged.
/// `program` names the program, like its absolute path: each program's code is kept together.
/// `signatures` is what checking `module` gave, so a caller that checked it first saves
/// doing that again.
pub fn run_cached(
    module: &Module,
    signatures: Option<&Signatures>,
    out: &mut dyn Write,
    inputs: Inputs,
    cache: &fennec_build::Cache,
    program: &str,
) -> Result<Report, JitError> {
    report_with_inputs(module, signatures, out, inputs, Some((cache, program)))
}

/// Does everything `run_cached` does before `main` is called, and stops there, to measure how
/// long a program takes to start.
pub fn start_only(
    module: &Module,
    signatures: &Signatures,
    cache: &fennec_build::Cache,
    program: &str,
) -> Result<(), JitError> {
    let compile = || {
        let here = 0_u8;
        let stack_limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
        let prelude = fennec_syntax::prelude_modules().unwrap_or_default();
        let compiled = types::Program::from_module(module, prelude, signatures)
            .and_then(|program_| lower::compile(&program_, signatures, stack_limit, Some((cache, program))))
            .map_err(JitError::Unsupported)?;
        let (_entry, tables, _) = compiled;
        runtime::start_only(module, tables).map_err(JitError::Failed)
    };
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new().stack_size(STACK_BYTES).spawn_scoped(scope, compile);
        match worker.map(std::thread::ScopedJoinHandle::join) {
            Ok(Ok(started)) => started,
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(err) => Err(JitError::Unsupported(Diagnostic::new(
                format!("couldn't start the JIT's thread: {err}"),
                Span::default(),
            ))),
        }
    })
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
    report_with_inputs(module, None, out, Inputs::default(), None)
}

fn report_with_inputs(
    module: &Module,
    signatures: Option<&Signatures>,
    out: &mut dyn Write,
    inputs: Inputs,
    cache: Option<(&fennec_build::Cache, &str)>,
) -> Result<Report, JitError> {
    let ran = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || compile_and_run(module, signatures, inputs, cache))
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
fn compile_and_run(
    module: &Module,
    signatures: Option<&Signatures>,
    inputs: Inputs,
    cache: Option<(&fennec_build::Cache, &str)>,
) -> Result<(Vec<u8>, Result<Report, JitError>), JitError> {
    let here = 0_u8;
    let stack_limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
    let checked;
    let signatures = if let Some(signatures) = signatures {
        signatures
    } else {
        checked = fennec_check::check(module).map_err(|diagnostics| {
            JitError::Unsupported(
                diagnostics.into_iter().next().unwrap_or_else(|| Diagnostic::new("", Span::default())),
            )
        })?;
        &checked
    };
    let prelude = fennec_syntax::prelude_modules().unwrap_or_default();
    let program = types::Program::from_module(module, prelude, signatures).map_err(JitError::Unsupported)?;

    let (entry, tables, compilation) =
        lower::compile(&program, signatures, stack_limit, cache).map_err(JitError::Unsupported)?;
    let result = runtime::execute(&entry, module, tables, inputs).map_err(JitError::Failed)?;
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
            leftover_boxes: result.leftovers.boxes,
            counts: result.leftovers.counts,
            cached: compilation.cached,
        }),
    };
    Ok((result.output, report))
}

/// A program compiled to an object file for an executable, and how it compiled.
#[derive(Debug)]
pub struct Built {
    /// The object file. Linked against the runtime library, it's the program's executable.
    pub object: Vec<u8>,
    /// The functions and methods the interpreter in the runtime runs, which couldn't be
    /// compiled.
    pub interpreted: Vec<String>,
    /// How many operations the program's functions hand to the interpreter one at a time.
    pub handed_over: usize,
}

/// Compiles a program ahead of time, to an object file with a C `main` that runs it.
/// `sources` are the files it was read from, which the executable carries to show where
/// failures happen.
pub fn build(module: &Module, sources: &fennec_syntax::Sources) -> Result<Built, Diagnostic> {
    let signatures = fennec_check::check(module).map_err(|diagnostics| {
        diagnostics.into_iter().next().unwrap_or_else(|| Diagnostic::new("", Span::default()))
    })?;
    let prelude = fennec_syntax::prelude_modules().unwrap_or_default();
    let program = types::Program::from_module(module, prelude, &signatures)?;
    let image = lower::object::Written { module, sources };
    let (object, compilation) = lower::object::compile_object(&program, &signatures, &image)?;
    let mut interpreted = compilation.interpreted;
    interpreted.extend(program.excluded);
    interpreted.sort();
    Ok(Built { object, interpreted, handed_over: compilation.handed_over })
}

/// Every place a program's compiled code isn't native: whole functions the interpreter
/// runs, with why, and each operation handed to the interpreter or the runtime's objects.
/// In source order, the standard library's left out.
pub fn fallbacks(module: &Module) -> Result<Vec<Note>, Diagnostic> {
    let signatures = fennec_check::check(module).map_err(|diagnostics| {
        diagnostics.into_iter().next().unwrap_or_else(|| Diagnostic::new("", Span::default()))
    })?;
    let prelude = fennec_syntax::prelude_modules().unwrap_or_default();
    let program = types::Program::from_module(module, prelude, &signatures)?;
    let (_, _, compilation) = lower::compile(&program, &signatures, 0, None)?;
    let whole = |function: &str, what: String| Note {
        function: function.to_owned(),
        span: definition(module, function).unwrap_or_default(),
        what,
        loops: 0,
        compiled_in: function.to_owned(),
    };
    let mut notes: Vec<Note> = compilation
        .interpreted
        .iter()
        .zip(&compilation.reasons)
        .map(|(function, why)| {
            whole(function, format!("the interpreter runs the whole function: {}", why.message))
        })
        .chain(program.excluded.iter().map(|function| {
            whole(
                function,
                "the interpreter runs the whole function: it's a generator or holds a `def`".to_owned(),
            )
        }))
        .chain(compilation.notes)
        .filter(|note| !note.span.in_prelude())
        .collect();
    notes.sort_by_key(|note| (note.span.start, note.span.end));
    // A loop lowered twice notes the same places twice.
    notes.dedup_by(|a, b| a.span == b.span && a.what == b.what);
    Ok(notes)
}

/// Where a function or method is defined in a program, by its key.
fn definition(module: &Module, key: &str) -> Option<Span> {
    module.items.iter().find_map(|item| match item {
        fennec_syntax::Item::Function(def) if def.name == key => Some(def.span),
        fennec_syntax::Item::Struct(ty) | fennec_syntax::Item::Enum(ty) | fennec_syntax::Item::Error(ty) => {
            let (owner, method) = key.split_once('.')?;
            ty.methods.iter().find(|def| ty.name == owner && def.name == method).map(|def| def.span)
        }
        _ => None,
    })
}
