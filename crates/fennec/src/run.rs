//! `fennec run`: checks a program, then runs its `main` as native code, or with the
//! interpreter for `--interp` or for what can't be compiled yet.

use std::io::{BufReader, BufWriter, Write};
use std::process::ExitCode;

use fennec_interp::Inputs;

/// What runs a program.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    /// Native code, or the interpreter for what can't be compiled yet.
    Native,
    /// Native code, saying when the interpreter stands in. The benchmarks ask for this.
    NativeNoted,
    /// Native code, without running the tests first.
    Untested,
    Interpreter,
}

/// Runs the program at `path`, handing `main` the `arguments` and this process's input.
pub(crate) fn run(path: &str, engine: Engine, arguments: &[String]) -> ExitCode {
    if engine == Engine::Native
        && let Some(failed) = crate::test::before_a_run(path)
    {
        return failed;
    }
    let prepared = if engine == Engine::Interpreter { whole(path) } else { reached(path) };
    let (program, signatures) = match prepared {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let module = &program.module;
    // Not locked: native code writes to standard output itself as the program runs.
    let mut out = BufWriter::new(std::io::stdout());
    let inputs = || Inputs {
        program: path.to_owned(),
        arguments: arguments.to_vec(),
        stdin: Box::new(BufReader::new(std::io::stdin())),
        live: true,
    };
    let result = match engine {
        Engine::Interpreter => interpret(module, &mut out, inputs()),
        _ => native(module, &signatures, (path, engine == Engine::NativeNoted), &mut out, inputs),
    };
    let flushed = out.flush();
    match (result, flushed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(diagnostic), _) => {
            eprint!("{}", rendered(&program, path, &diagnostic));
            ExitCode::FAILURE
        }
        (Ok(()), Err(err)) => {
            eprintln!("error: couldn't write output: {err}");
            ExitCode::FAILURE
        }
    }
}

/// A failure against its source. A program started from a kept image doesn't have its source, so
/// this reads it, which only a failing run has to wait for.
fn rendered(program: &fennec_kit::Program, path: &str, diagnostic: &fennec_syntax::Diagnostic) -> String {
    if program.sources.files().next().is_some() {
        return program.sources.render(diagnostic);
    }
    match fennec_kit::load(std::path::Path::new(path), path, fennec_kit::Entry::Main) {
        Ok(whole) => whole.sources.render(diagnostic),
        Err(_) => format!("error: {}\n", diagnostic.message),
    }
}

/// `fennec dev startup <path>`: everything `fennec run --jit` does before `main`, for the
/// benchmark suite to tell how long a program takes to start from how long its work takes.
pub(crate) fn startup(path: &str) -> ExitCode {
    let (program, signatures) = match reached(path) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: couldn't find a home folder; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    let cache = fennec_build::Cache::new(&home);
    match fennec_jit::start_only(&program.module, &signatures, &cache, &absolute(path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(fennec_jit::JitError::Failed(diagnostic) | fennec_jit::JitError::Unsupported(diagnostic)) => {
            eprint!("{}", program.sources.render(&diagnostic));
            ExitCode::FAILURE
        }
    }
}

/// Loads and checks all of a program.
fn whole(path: &str) -> Result<(fennec_kit::Program, fennec_check::Signatures), ExitCode> {
    let mut program = crate::load(path, fennec_kit::Entry::Main).ok_or(ExitCode::FAILURE)?;
    match crate::copy_rule::check_to_run(&mut program.module) {
        Ok(signatures) => Ok((program, signatures)),
        Err(diagnostics) => Err(reject(&program, &diagnostics)),
    }
}

/// A program checked and cut down to what `main` reaches, so that starting it doesn't grow
/// with everything else in its kit. All of it is checked the first time, and again whenever
/// its text changes; the cache remembers the text that passed. The cut-down program is what
/// always runs, so its machine code is reused from the very first run.
fn reached(path: &str) -> Result<(fennec_kit::Program, fennec_check::Signatures), ExitCode> {
    let Some(cache) = cache() else { return whole(path) };
    if let Some(mut program) = crate::image::load(path, &cache)
        && let Ok(signatures) = fennec_check::check_to_run(&mut program.module)
    {
        return Ok((program, signatures));
    }
    let files = crate::image::files_now(path, &cache);
    let mut program = crate::load(path, fennec_kit::Entry::Main).ok_or(ExitCode::FAILURE)?;
    let name = passed_name(path);
    let text = fingerprint(program.sources.files());
    if cache.read("checked", &name).is_none_or(|passed| passed != text.as_bytes()) {
        if let Err(diagnostics) = crate::copy_rule::check_to_run(&mut program.module.clone()) {
            return Err(reject(&program, &diagnostics));
        }
        // A cache that can't be written only means checking all of it again next time.
        let _ = cache.write("checked", &name, text.as_bytes());
    }
    fennec_build::keep_what_main_reaches(&mut program.module);
    if let Some(files) = &files {
        crate::image::store(path, &cache, files, &program.module);
    }
    match fennec_check::check_to_run(&mut program.module) {
        Ok(signatures) => Ok((program, signatures)),
        // Cutting it down missed something the whole program has.
        Err(_) => whole(path),
    }
}

/// The cache, unless `FENNEC_NO_CACHE` is set.
pub(crate) fn cache() -> Option<fennec_build::Cache> {
    fennec_build::fennec_home()
        .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
        .map(|home| fennec_build::Cache::new(&home))
}

/// A program's name in the cache: a hash of its absolute path.
pub(crate) fn passed_name(path: &str) -> String {
    format!("{}.bin", fennec_blake3::hash(absolute(path).as_bytes()).to_hex())
}

/// A program's absolute path, which is what names it in the cache.
pub(crate) fn absolute(path: &str) -> String {
    std::fs::canonicalize(path).map_or_else(|_| path.to_owned(), |path| path.display().to_string())
}

/// A hash of every file of a program, by name and text, and of which build of Fennec read
/// them, since another build's checker may find what this one doesn't.
pub(crate) fn fingerprint<'t>(files: impl Iterator<Item = (&'t str, &'t str)>) -> fennec_blake3::Hash {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec build cache 2026-10-31 passed check");
    hasher.update(env!("FENNEC_BUILD").as_bytes());
    for (path, text) in files {
        for part in [path, text] {
            hasher.update(&part.len().to_le_bytes());
            hasher.update(part.as_bytes());
        }
    }
    hasher.finalize()
}

/// Runs a program as native code, or with the interpreter if it uses something the JIT
/// can't compile yet. Nothing runs before the JIT decides, so nothing runs twice.
fn native(
    module: &fennec_syntax::Module,
    signatures: &fennec_check::Signatures,
    (path, noted): (&str, bool),
    out: &mut dyn Write,
    inputs: impl Fn() -> Inputs,
) -> Result<(), fennec_syntax::Diagnostic> {
    // Machine code is reused from the cache in `~/.fennec`, unless `FENNEC_NO_CACHE` is set.
    let ran = match &cache() {
        // A program's machine code is kept together, under its absolute path.
        Some(cache) => {
            fennec_jit::run_cached(module, Some(signatures), out, inputs(), cache, &absolute(path))
                .map(|_| ())
        }
        None => fennec_jit::run_with_inputs(module, out, inputs()),
    };
    match ran {
        Ok(()) => Ok(()),
        Err(fennec_jit::JitError::Failed(diagnostic)) => Err(diagnostic),
        Err(fennec_jit::JitError::Unsupported(diagnostic)) => {
            if noted {
                eprintln!("note: {}; using the interpreter", diagnostic.message);
            }
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
