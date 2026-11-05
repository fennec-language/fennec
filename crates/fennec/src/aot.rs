//! `fennec build`: compiles a program ahead of time to a standalone executable. The JIT's
//! compiler or LLVM writes object files, and the system's linker links them with Fennec's
//! runtime library, which sits next to the `fennec` command. On Windows, that's Visual Studio's
//! linker, and for now only LLVM's objects.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

/// The libraries Rust's standard library needs from the system, which the runtime
/// library uses, and the flag that drops the parts of the runtime a program never calls.
#[cfg(target_os = "macos")]
const SYSTEM_LIBRARIES: &[&str] = &["-lSystem", "-lc", "-lm", "-Wl,-dead_strip"];
/// What TLS checks certificates with, for a program that makes requests. Loading these costs
/// every run, so nothing else links them.
#[cfg(target_os = "macos")]
const TLS_LIBRARIES: &[&str] = &["-framework", "Security", "-framework", "CoreFoundation"];
#[cfg(all(not(target_os = "macos"), not(windows)))]
const TLS_LIBRARIES: &[&str] = &[];
#[cfg(all(not(target_os = "macos"), not(windows)))]
const SYSTEM_LIBRARIES: &[&str] =
    &["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc", "-Wl,--gc-sections"];

/// What Rust's standard library needs from Windows, which the runtime library uses.
#[cfg(windows)]
const WINDOWS_LIBRARIES: &[&str] = &[
    "kernel32.lib",
    "ntdll.lib",
    "userenv.lib",
    "crypt32.lib",
    "ncrypt.lib",
    "ws2_32.lib",
    "dbghelp.lib",
    "advapi32.lib",
    "bcrypt.lib",
    "synchronization.lib",
    "msvcrt.lib",
    "vcruntime.lib",
    "ucrt.lib",
    "legacy_stdio_definitions.lib",
];

/// Which runtime library a program links against, next to the `fennec` command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Runtime {
    /// The usual one, for programs that run on one thread.
    Plain,
    /// The one built with `threads`, whose pool runs `parallel`'s ranges on workers. Its
    /// counts are atomic, so only programs that use `parallel` pay for it.
    Threads,
    /// With no interpreter in it.
    Lean,
    /// With no interpreter in it, for `parallel`.
    LeanThreads,
}

impl Runtime {
    /// The library's file name.
    fn library(self) -> &'static str {
        match self {
            // Cargo names a static library this way on Windows.
            Self::Plain if cfg!(windows) => "fennec_runtime.lib",
            Self::Threads if cfg!(windows) => "fennec_runtime_threads.lib",
            Self::Lean if cfg!(windows) => "fennec_runtime_lean.lib",
            Self::LeanThreads if cfg!(windows) => "fennec_runtime_lean_threads.lib",
            Self::Plain => "libfennec_runtime.a",
            Self::Threads => "libfennec_runtime_threads.a",
            Self::Lean => "libfennec_runtime_lean.a",
            Self::LeanThreads => "libfennec_runtime_lean_threads.a",
        }
    }

    /// The runtime's own bitcode, which a release build puts in the program's unit when it's
    /// there.
    fn bitcode(self) -> &'static str {
        match self {
            Self::Plain => "libfennec_runtime.bc",
            Self::Threads => "libfennec_runtime_threads.bc",
            Self::Lean => "libfennec_runtime_lean.bc",
            Self::LeanThreads => "libfennec_runtime_lean_threads.bc",
        }
    }

    /// How to build the library, for saying when it's missing.
    fn how_to_build(self) -> &'static str {
        match self {
            Self::Plain => "`cargo build -p fennec-runtime`",
            Self::Threads => {
                "`cargo build -p fennec-runtime --features threads --target-dir target/threads`, copying it there"
            }
            Self::Lean => {
                "`cargo build -p fennec-runtime --features lean --target-dir target/lean`, copying it there"
            }
            Self::LeanThreads => {
                "`cargo build -p fennec-runtime --features lean,threads --target-dir target/lean-threads`, copying it there"
            }
        }
    }

    /// The runtime a lowered program needs: the threads one when it runs `parallel` and
    /// `FENNEC_THREADS_RUNTIME=1` asks for it. Until a range's copies of what it captures are
    /// its own, workers would count shared blocks plainly, so the plain one stays the default.
    /// `FENNEC_LEAN_RUNTIME=1` asks for one with no interpreter in it, which a program still
    /// needing the interpreter fails to link against.
    fn for_program(lowered: &fennec_ir::Program) -> Self {
        let on = |name: &str| std::env::var_os(name).is_some_and(|on| on == "1");
        let threads = on("FENNEC_THREADS_RUNTIME") && fennec_llvm::uses_parallel(lowered);
        match (on("FENNEC_LEAN_RUNTIME"), threads) {
            (false, false) => Self::Plain,
            (false, true) => Self::Threads,
            (true, false) => Self::Lean,
            (true, true) => Self::LeanThreads,
        }
    }
}

pub(crate) fn build(path: &str, output: Option<&str>) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check_to_build(&mut program) {
        return ExitCode::FAILURE;
    }
    // What's wrong with the program comes first, on every platform.
    if cfg!(windows) {
        eprintln!("error: `fennec build` can't link executables on Windows yet; use `fennec run`");
        return ExitCode::FAILURE;
    }
    let started = Instant::now();
    let built = match fennec_jit::build(&program.module, &program.sources) {
        Ok(built) => built,
        Err(diagnostic) => {
            eprint!("{}", program.sources.render(&diagnostic));
            return ExitCode::FAILURE;
        }
    };
    let compiled = started.elapsed();
    let output = output.map_or_else(|| default_output(path, &program), PathBuf::from);
    match link(&built.object, &output) {
        Ok(linked) => {
            let size = std::fs::metadata(&output).map_or(0, |metadata| metadata.len());
            println!(
                "🦊 built {} ({} KB) in {} ms: compiled in {} ms, linked in {} ms",
                output.display(),
                size / 1024,
                (compiled + linked).as_millis(),
                compiled.as_millis(),
                linked.as_millis(),
            );
            if !built.interpreted.is_empty() {
                println!("   run by the interpreter: {}", built.interpreted.join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Whether `--cpu baseline` asked for code any machine of its architecture runs.
pub(crate) static BASELINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `fennec build --llvm`: the same through LLVM, which says what it can't take yet rather than
/// falling back to anything.
pub(crate) fn build_llvm(path: &str, output: Option<&str>, release: bool) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check_to_build(&mut program) {
        return ExitCode::FAILURE;
    }
    let output = output.map_or_else(|| default_output(path, &program), PathBuf::from);
    match executable(&program, &output, release) {
        Ok(built) => {
            println!(
                "🦊 built {} with LLVM: compiled {} of {} functions in {} ms, linked in {} ms",
                output.display(),
                built.fresh,
                built.total,
                built.compiled.as_millis(),
                built.linked.as_millis(),
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Builds an executable that runs the functions `tests` of a module with everything else trimmed
/// away, through LLVM's cached dev build. Gives back the functions that have a flag saying they
/// ran, in the order the executable numbers them.
pub(crate) fn tests_executable(
    module: &fennec_syntax::Module,
    tests: &[String],
    output: &Path,
) -> Result<Vec<String>, String> {
    let emitted = emitted(module, true)?;
    let image_of = |image: &mut fennec_llvm::Image| {
        fennec_interp::embed::codec::Image {
            module: module.clone(),
            // `fennec test` renders a failure itself.
            sources: fennec_syntax::Sources::new(),
            tables: std::mem::take(&mut image.tables),
            literals: image.literals.clone(),
        }
        .to_bytes()
    };
    let Compiled { objects, runtime, tls, .. } =
        compile_emitted(&emitted, false, image_of, |lowered, shared, image, bytes| {
            fennec_llvm::tests_entry(lowered, shared, image, bytes, tests)
        })?;
    link_objects(&objects, output, runtime, false, tls)?;
    Ok(emitted.shared.ran.iter().cloned().collect())
}

/// Builds an executable of a program that says what each place in its source cost, for `fennec dev
/// trace`, through LLVM's cached dev build. Gives back what's at each place, and the lowered
/// program.
pub(crate) fn tracing_executable(
    program: &fennec_kit::Program,
    output: &Path,
) -> Result<(std::collections::BTreeMap<u32, String>, fennec_ir::Program), String> {
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    let emitted = emitted_with(&reached, fennec_llvm::emit_tracing_costs)?;
    let image_of = |image: &mut fennec_llvm::Image| {
        fennec_interp::embed::codec::Image {
            module: program.module.clone(),
            sources: program.sources.clone(),
            tables: std::mem::take(&mut image.tables),
            literals: image.literals.clone(),
        }
        .to_bytes()
    };
    let Compiled { objects, runtime, tls, .. } =
        compile_emitted(&emitted, false, image_of, |lowered, shared, image, bytes| {
            fennec_llvm::entry(lowered, shared, image, bytes)
        })?;
    link_objects(&objects, output, runtime, false, tls)?;
    Ok((emitted.shared.sites, emitted.lowered))
}

/// Whether LLVM has text for a trimmed program, or why not, without compiling any of it.
pub(crate) fn takes(module: &fennec_syntax::Module) -> Result<(), String> {
    emitted(module, false).map(|_| ())
}

/// What building an executable through LLVM did.
pub(crate) struct Built {
    /// How many functions the program has, and how many weren't in the cache.
    total: usize,
    fresh: usize,
    compiled: std::time::Duration,
    linked: std::time::Duration,
}

/// Whether there's a linker to find. Windows doesn't come with one: Visual Studio's Build Tools do.
fn linker_here() -> bool {
    #[cfg(windows)]
    {
        cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "link.exe").is_some()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Whether this machine can build through LLVM at all: its tools are there, and the runtime
/// library is beside `fennec`. Without them, nothing about the program is the reason.
pub(crate) fn llvm_ready() -> bool {
    linker_here() && fennec_llvm::Tools::find().is_ok() && runtime_library(Runtime::Plain).is_ok()
}

/// Builds a checked program through LLVM to `output`, or says why LLVM can't: a function it
/// doesn't take yet, or a tool or library that isn't there.
pub(crate) fn executable(
    program: &fennec_kit::Program,
    output: &Path,
    release: bool,
) -> Result<Built, String> {
    let started = Instant::now();
    let Compiled { objects, total, fresh, runtime, tls } = llvm_objects(program, release)?;
    let compiled = started.elapsed();
    // A release built with the runtime's bitcode has its Rust inside already.
    let runtime_in = release && runtime_bitcode(runtime).is_some();
    // A missing runtime with threads fails here, so `fennec run` says so rather than quietly
    // taking another way.
    let linked = link_objects(&objects, output, runtime, runtime_in, tls)?;
    Ok(Built { total, fresh, compiled, linked })
}

/// What compiling a program with LLVM gives.
struct Compiled {
    objects: Vec<Vec<u8>>,
    /// How many functions there are.
    total: usize,
    /// How many of them weren't already in the cache.
    fresh: usize,
    /// The runtime the objects link against.
    runtime: Runtime,
    /// Whether the program makes requests, so it links what TLS needs.
    tls: bool,
}

/// A trimmed program LLVM has text for.
struct Emitted {
    lowered: fennec_ir::Program,
    units: Vec<fennec_llvm::Unit>,
    shared: fennec_llvm::Shared,
    layouts: fennec_llvm::Layouts,
}

/// Lowers a trimmed program and writes its text, or says why LLVM can't.
fn emitted(reached: &fennec_syntax::Module, traced: bool) -> Result<Emitted, String> {
    emitted_with(reached, if traced { fennec_llvm::emit_traced } else { fennec_llvm::emit_for })
}

/// What LLVM emits for a program: each function's text, and what they share.
type Emits = Result<(Vec<fennec_llvm::Unit>, fennec_llvm::Shared, fennec_llvm::Layouts), String>;

/// A trimmed program's text from one of LLVM's ways of emitting it.
fn emitted_with(
    reached: &fennec_syntax::Module,
    emit: fn(&fennec_ir::Program, fennec_llvm::Cpu) -> Emits,
) -> Result<Emitted, String> {
    let checked =
        fennec_check::check(reached).map_err(|_| "the program didn't check once trimmed".to_owned())?;
    // `parallel` runs on the runtime's threads, its work lifted for each range of positions.
    let mut lowered = fennec_ir::lower_with(
        reached,
        &checked,
        fennec_ir::Options { outline_parallel: true, stop_on_failed_main: true },
    );
    // On threads, a value crossing the edge of a `shared` value's lock is copied.
    if fennec_llvm::uses_parallel(&lowered) {
        fennec_ir::guard_shared(&mut lowered);
    }
    if !lowered.not_lowered.is_empty() {
        let skipped: Vec<String> = lowered
            .not_lowered
            .iter()
            .map(|skipped| format!("`{}` isn't lowered yet: {}", skipped.function, skipped.why))
            .collect();
        return Err(skipped.join("\n"));
    }
    // A mistake in lowering is caught here in a dev build of `fennec`, whatever the linker takes:
    // macOS's links a function defined twice.
    if cfg!(debug_assertions)
        && let Err(problems) = fennec_ir::validate(&lowered)
    {
        return Err(format!("the IR isn't sound: {}", problems.join("; ")));
    }
    let main_at = reached.items.iter().find_map(|item| match item {
        fennec_syntax::Item::Function(def) if def.name == "main" => Some(def.span),
        _ => None,
    });
    fennec_llvm::wrap_main(&mut lowered, main_at.unwrap_or_default());
    let cpu = if BASELINE.load(std::sync::atomic::Ordering::Relaxed) {
        fennec_llvm::Cpu::Baseline
    } else {
        fennec_llvm::Cpu::Usual
    };
    let (units, shared, layouts) = emit(&lowered, cpu)?;
    Ok(Emitted { lowered, units, shared, layouts })
}

/// Lowers what the program reaches and compiles it.
fn llvm_objects(program: &fennec_kit::Program, release: bool) -> Result<Compiled, String> {
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    let emitted = emitted(&reached, false)?;
    let image_of = |image: &mut fennec_llvm::Image| {
        fennec_interp::embed::codec::Image {
            module: program.module.clone(),
            sources: program.sources.clone(),
            tables: std::mem::take(&mut image.tables),
            literals: image.literals.clone(),
        }
        .to_bytes()
    };
    compile_emitted(&emitted, release, image_of, |lowered, shared, image, bytes| {
        fennec_llvm::entry(lowered, shared, image, bytes)
    })
}

/// Compiles what LLVM has text for, with the module made fresh each build from `entry_of`.
fn compile_emitted(
    emitted: &Emitted,
    release: bool,
    image_of: impl Fn(&mut fennec_llvm::Image) -> Vec<u8>,
    entry_of: impl Fn(
        &fennec_ir::Program,
        &fennec_llvm::Shared,
        &fennec_llvm::Image,
        &[u8],
    ) -> Result<String, String>,
) -> Result<Compiled, String> {
    let Emitted { lowered, units, shared, layouts } = emitted;
    let runtime = Runtime::for_program(lowered);
    let tls = fennec_llvm::uses_tls(lowered);
    let tools = fennec_llvm::Tools::find()?;
    let mut image = fennec_llvm::Image::of(layouts, shared)?;
    let bytes = image_of(&mut image);
    let entry = entry_of(lowered, shared, &image, &bytes)?;
    let home = fennec_build::fennec_home();
    let cache = home
        .as_deref()
        .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
        .map(fennec_build::Cache::new);
    let work = std::env::temp_dir().join(format!("fennec-llvm-{}", std::process::id()));
    let built = if release {
        fennec_llvm::compile_whole(&tools, units, &entry, &work, runtime_bitcode(runtime).as_deref())
    } else {
        fennec_llvm::compile(&tools, units, &entry, cache.as_ref(), &work)
    };
    // `FENNEC_LLVM_KEEP` leaves the modules' text to look at.
    if std::env::var_os("FENNEC_LLVM_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    let built = built?;
    Ok(Compiled { objects: built.objects, total: units.len(), fresh: built.compiled, runtime, tls })
}

/// Links object files with the runtime library, giving back how long linking took. When the
/// runtime's Rust is in the objects already, only its C parts are linked, so nothing of it is
/// there twice.
fn link_objects(
    objects: &[Vec<u8>],
    output: &Path,
    runtime: Runtime,
    runtime_in: bool,
    tls: bool,
) -> Result<std::time::Duration, String> {
    let runtime = runtime_library(runtime)?;
    let work = std::env::temp_dir().join(format!("fennec-link-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let mut paths = Vec::with_capacity(objects.len());
    for (at, object) in objects.iter().enumerate() {
        let path = work.join(format!("{at}.o"));
        std::fs::write(&path, object).map_err(|err| format!("couldn't write an object file: {err}"))?;
        paths.push(path);
    }
    let started = Instant::now();
    let runtime = if runtime_in { c_parts(&runtime, &work)? } else { runtime };
    let linked = linker(&paths, &runtime, output, tls).output().map_err(|err| {
        let help = if cfg!(windows) { "; install Visual Studio's Build Tools" } else { "" };
        format!("couldn't run the linker: {err}{help}")
    })?;
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    if linked.status.success() { Ok(took) } else { Err(link_failure(&linked)) }
}

/// What a linker said when it failed: Visual Studio's says it on standard output, others on
/// standard error.
fn link_failure(linked: &std::process::Output) -> String {
    format!(
        "linking failed:\n{}{}",
        String::from_utf8_lossy(&linked.stderr),
        String::from_utf8_lossy(&linked.stdout)
    )
}

/// The system's linker, set to link objects with a runtime library into an executable.
#[cfg(not(windows))]
fn linker(objects: &[PathBuf], runtime: &Path, output: &Path, tls: bool) -> Command {
    let mut command = Command::new("cc");
    command
        .args(objects)
        .arg(runtime)
        .args(SYSTEM_LIBRARIES)
        .args(if tls { TLS_LIBRARIES } else { &[] })
        // `FENNEC_SANITIZE` links in its sanitizer's runtime.
        .args(fennec_llvm::sanitize_flag())
        .arg("-o")
        .arg(output);
    command
}

/// The system's linker, set to link objects with a runtime library into an executable.
#[cfg(windows)]
fn linker(objects: &[PathBuf], runtime: &Path, output: &Path, _tls: bool) -> Command {
    // Where Visual Studio's linker is, with what it needs to find the system's libraries, or
    // else one a developer prompt has set up.
    let found = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "link.exe");
    let mut command = found.map_or_else(|| Command::new("link.exe"), |tool| tool.to_command());
    command
        .args(["/NOLOGO", "/SUBSYSTEM:CONSOLE", "/OPT:REF", "/NODEFAULTLIB:libcmt"])
        .arg(format!("/OUT:{}", output.display()))
        .args(objects)
        .arg(runtime)
        .args(WINDOWS_LIBRARIES);
    command
}

/// Where an executable goes when no `-o` says: named after the kit, or the file, in the
/// current folder.
fn default_output(path: &str, program: &fennec_kit::Program) -> PathBuf {
    let name = program
        .manifest
        .as_ref()
        .map(|manifest| manifest.name.clone())
        .or_else(|| Path::new(path).file_stem().map(|stem| stem.to_string_lossy().into_owned()));
    PathBuf::from(name.filter(|name| !name.is_empty() && name != ".").unwrap_or_else(|| "main".to_owned()))
}

/// Links an object file with the runtime library into an executable at `output`, giving
/// back how long linking took.
fn link(object: &[u8], output: &Path) -> Result<std::time::Duration, String> {
    let runtime = runtime_library(Runtime::Plain)?;
    let work = std::env::temp_dir().join(format!("fennec-build-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let object_path = work.join("program.o");
    std::fs::write(&object_path, object).map_err(|err| format!("couldn't write the object file: {err}"))?;
    let started = Instant::now();
    let linked =
        linker(std::slice::from_ref(&object_path), &runtime, output, false).output().map_err(|err| {
            let help = if cfg!(windows) { "; install Visual Studio's Build Tools" } else { "" };
            format!("couldn't run the linker: {err}{help}")
        })?;
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    if linked.status.success() { Ok(took) } else { Err(link_failure(&linked)) }
}

/// The runtime's bitcode, next to the `fennec` command, if it was built: see
/// `Runtime::bitcode`.
fn runtime_bitcode(runtime: Runtime) -> Option<PathBuf> {
    let fennec = std::env::current_exe().ok()?;
    Some(fennec.with_file_name(runtime.bitcode())).filter(|path| path.is_file())
}

/// A copy of the runtime library with only its C objects: mimalloc and the compiler's helpers.
fn c_parts(runtime: &Path, work: &Path) -> Result<PathBuf, String> {
    let copy = work.join("runtime-c.a");
    std::fs::copy(runtime, &copy).map_err(|err| format!("couldn't copy the runtime library: {err}"))?;
    let ar = |args: &[&std::ffi::OsStr]| {
        Command::new("ar").args(args).output().map_err(|err| format!("couldn't run `ar`: {err}"))
    };
    let listed = ar(&["t".as_ref(), copy.as_os_str()])?;
    let rust: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|name| name.ends_with(".rcgu.o"))
        .map(str::to_owned)
        .collect();
    for names in rust.chunks(200) {
        let mut args: Vec<&std::ffi::OsStr> = vec!["d".as_ref(), copy.as_os_str()];
        args.extend(names.iter().map(std::ffi::OsStr::new));
        let done = ar(&args)?;
        if !done.status.success() {
            return Err(format!("`ar d` failed: {}", String::from_utf8_lossy(&done.stderr)));
        }
    }
    Ok(copy)
}

/// The runtime library, next to the `fennec` command.
fn runtime_library(runtime: Runtime) -> Result<PathBuf, String> {
    let fennec =
        std::env::current_exe().map_err(|err| format!("couldn't find the `fennec` command: {err}"))?;
    let path = fennec.with_file_name(runtime.library());
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "the runtime library `{}` should be next to `fennec`, at {}; build it with {}",
            runtime.library(),
            path.display(),
            runtime.how_to_build()
        ))
    }
}
