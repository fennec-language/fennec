//! `fennec run`: checks a program, then runs its `main` as native code. That's the cached dev
//! build LLVM makes, as `fennec build --llvm` does, compiled a function at a time so an edit
//! recompiles only what it touched. Where LLVM can't build the program yet it says why, in
//! one line, and the JIT runs it, or the interpreter what the JIT can't. `--jit` and
//! `--interp` ask for those directly.

use std::io::{BufReader, BufWriter, Write};
use std::process::ExitCode;

use fennec_interp::Inputs;

/// What runs a program.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    /// The LLVM dev build, or the JIT where LLVM can't build it, or the interpreter for what
    /// neither can compile yet.
    Native,
    /// The JIT, saying when the interpreter stands in. The benchmarks ask for this.
    NativeNoted,
    /// As `Native`, without running the tests first.
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
    // Where LLVM's tools or the runtime library aren't there, the JIT runs it without a word:
    // that's this machine, not the program. Only LLVM turning the program down is said.
    if matches!(engine, Engine::Native | Engine::Untested) && crate::aot::llvm_ready() {
        match built_and_run(&program, path, arguments) {
            Ok(code) => return code,
            Err(why) => eprintln!("note: running this with the JIT, since LLVM can't build it: {why}"),
        }
    }
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

/// Builds the program through LLVM's cached dev build and runs it, handing it the arguments,
/// this process's input and output, and its exit code. A machine without LLVM's tools, the
/// runtime library or a linker can't, so it doesn't get here.
fn built_and_run(
    program: &fennec_kit::Program,
    path: &str,
    arguments: &[String],
) -> Result<ExitCode, String> {
    // A program started from a kept image doesn't have its source, which an executable keeps
    // to point a failure at. Its files haven't changed since, so reading them again matches.
    let whole;
    let program = if program.sources.files().next().is_none() {
        let read = fennec_kit::load(std::path::Path::new(path), path, fennec_kit::Entry::Main);
        let sources = read.map_err(|_| "its source couldn't be read again".to_owned())?.sources;
        whole = fennec_kit::Program {
            module: program.module.clone(),
            sources,
            manifest: None,
            embedded: Vec::new(),
            kit_src: None,
        };
        &whole
    } else {
        program
    };
    let executable = std::env::temp_dir().join(format!(
        "fennec-run-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    crate::aot::executable(program, &executable, false)?;
    let mut command = std::process::Command::new(&executable);
    // `args.program` is the program it ran, not where it was built to for this run.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::arg0(&mut command, path);
    #[cfg(not(unix))]
    command.env("FENNEC_RUN_AS", format!("{}|{path}", executable.display()));
    let ran = command.args(arguments).status();
    let _ = std::fs::remove_file(&executable);
    let status = ran.map_err(|err| format!("the program it built didn't start: {err}"))?;
    // A program stopped by a signal has no code of its own.
    Ok(status.code().and_then(|code| u8::try_from(code).ok()).map_or(ExitCode::FAILURE, ExitCode::from))
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
    let embedded = program.embedded_texts();
    let text = fingerprint(
        program.sources.files().chain(embedded.iter().map(|(path, text)| (path.as_str(), text.as_str()))),
    );
    if cache.read("checked", &name).is_none_or(|passed| passed != text.as_bytes()) {
        if let Err(diagnostics) = crate::copy_rule::check_rules(&mut program.module.clone()) {
            return Err(reject(&program, &diagnostics));
        }
        // A cache that can't be written only means checking all of it again next time.
        let _ = cache.write("checked", &name, text.as_bytes());
    }
    fennec_build::keep_what_main_reaches(&mut program.module);
    // The image keeps the constants as worked out while compiling, so a cached run skips that.
    if let Err(diagnostics) = crate::copy_rule::bake(&mut program.module) {
        return Err(reject(&program, &diagnostics));
    }
    if let Some(files) = &files {
        crate::image::store(path, &cache, files, &program);
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
pub(crate) fn reject(program: &fennec_kit::Program, diagnostics: &[fennec_syntax::Diagnostic]) -> ExitCode {
    let told: Vec<_> = diagnostics.iter().map(|diagnostic| with_extension(program, diagnostic)).collect();
    eprint!("{}", program.sources.render_all(&told));
    ExitCode::FAILURE
}

/// A problem, saying which file has a missing method when a file the run didn't load has it. A run
/// loads `main` and what it imports, so nothing else brings in a file that only extends a type.
pub(crate) fn with_extension(
    program: &fennec_kit::Program,
    diagnostic: &fennec_syntax::Diagnostic,
) -> fennec_syntax::Diagnostic {
    let found = fennec_check::missing_method(diagnostic).and_then(|(who, method)| {
        let (shown, module) = program.extension_elsewhere(who, method)?;
        Some(format!(
            "`{method}` is in an `extend` in {shown}, which this program doesn't import: add `import {module}`"
        ))
    });
    match found {
        Some(help) => diagnostic.clone().with_help(help),
        None => diagnostic.clone(),
    }
}
