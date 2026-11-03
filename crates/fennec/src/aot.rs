//! `fennec build`: compiles a program ahead of time to a standalone executable. The JIT's
//! compiler writes an object file, and the system's C compiler links it with Fennec's
//! runtime library, which sits next to the `fennec` command.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

/// The libraries Rust's standard library needs from the system, which the runtime
/// library uses, and the flag that drops the parts of the runtime a program never calls.
#[cfg(target_os = "macos")]
const SYSTEM_LIBRARIES: &[&str] = &["-lSystem", "-lc", "-lm", "-Wl,-dead_strip"];
#[cfg(not(target_os = "macos"))]
const SYSTEM_LIBRARIES: &[&str] =
    &["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc", "-Wl,--gc-sections"];

/// Which runtime library a program links against, next to the `fennec` command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Runtime {
    /// The usual one, for programs that run on one thread.
    Plain,
    /// The one built with `threads`, whose pool runs `parallel`'s ranges on workers. Its
    /// counts are atomic, so only programs that use `parallel` pay for it.
    Threads,
}

impl Runtime {
    /// The library's file name.
    fn library(self) -> &'static str {
        match self {
            Self::Plain => "libfennec_runtime.a",
            Self::Threads => "libfennec_runtime_threads.a",
        }
    }

    /// The runtime's own bitcode, which a release build puts in the program's unit when it's
    /// there.
    fn bitcode(self) -> &'static str {
        match self {
            Self::Plain => "libfennec_runtime.bc",
            Self::Threads => "libfennec_runtime_threads.bc",
        }
    }

    /// How to build the library, for saying when it's missing.
    fn how_to_build(self) -> &'static str {
        match self {
            Self::Plain => "`cargo build -p fennec-runtime`",
            Self::Threads => {
                "`cargo build -p fennec-runtime --features threads --target-dir target/threads`, copying its `libfennec_runtime.a` next to `fennec` as `libfennec_runtime_threads.a`"
            }
        }
    }

    /// The runtime a lowered program needs: the threads one when it runs `parallel` and
    /// `FENNEC_THREADS_RUNTIME=1` asks for it. Until a range's copies of what it captures are
    /// its own, workers would count shared blocks plainly, so the plain one stays the default.
    fn for_program(lowered: &fennec_ir::Program) -> Self {
        let asked = std::env::var_os("FENNEC_THREADS_RUNTIME").is_some_and(|on| on == "1");
        if asked && uses_parallel(lowered) { Self::Threads } else { Self::Plain }
    }
}

/// Whether any function of a lowered program hands work to the runtime's `parallel` calls.
fn uses_parallel(lowered: &fennec_ir::Program) -> bool {
    lowered.functions.iter().chain(&lowered.constants).flat_map(|function| &function.blocks).any(|block| {
        block.insts.iter().any(|inst| {
            matches!(&inst.kind, fennec_ir::InstKind::Builtin { name, .. } if name.starts_with("parallel."))
        })
    })
}

pub(crate) fn build(path: &str, output: Option<&str>) -> ExitCode {
    if cfg!(windows) {
        eprintln!("error: `fennec build` can't link executables on Windows yet; use `fennec run`");
        return ExitCode::FAILURE;
    }
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
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
    if cfg!(windows) {
        eprintln!("error: `fennec build` can't link executables on Windows yet; use `fennec run`");
        return ExitCode::FAILURE;
    }
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let output = output.map_or_else(|| default_output(path, &program), PathBuf::from);
    let started = Instant::now();
    match llvm_objects(&program, release) {
        Ok(Objects { objects, total, fresh, runtime }) => {
            let compiled = started.elapsed();
            // A release built with the runtime's bitcode has its Rust inside already.
            let runtime_in = release && runtime_bitcode(runtime).is_some();
            match link_objects(&objects, &output, runtime, runtime_in) {
                Ok(linked) => {
                    println!(
                        "🦊 built {} with LLVM: compiled {fresh} of {total} functions in {} ms, linked in {} ms",
                        output.display(),
                        compiled.as_millis(),
                        linked.as_millis(),
                    );
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// What compiling a program with LLVM gives.
struct Objects {
    objects: Vec<Vec<u8>>,
    /// How many functions there are.
    total: usize,
    /// How many of them weren't already in the cache.
    fresh: usize,
    /// The runtime the objects link against.
    runtime: Runtime,
}

/// Lowers what the program reaches and compiles it.
fn llvm_objects(program: &fennec_kit::Program, release: bool) -> Result<Objects, String> {
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    let checked =
        fennec_check::check(&reached).map_err(|_| "the program didn't check once trimmed".to_owned())?;
    // `parallel` runs on the runtime's threads, its work lifted for each range of positions.
    let lowered = fennec_ir::lower_with(&reached, &checked, fennec_ir::Options { outline_parallel: true });
    if let Some(skipped) = lowered.not_lowered.first() {
        return Err(format!("`{}` isn't lowered yet: {}", skipped.function, skipped.why));
    }
    let runtime = Runtime::for_program(&lowered);
    let cpu = if BASELINE.load(std::sync::atomic::Ordering::Relaxed) {
        fennec_llvm::Cpu::Baseline
    } else {
        fennec_llvm::Cpu::Usual
    };
    let (units, shared, layouts) = fennec_llvm::emit_for(&lowered, cpu)?;
    let tools = fennec_llvm::Tools::find()?;
    let mut image = fennec_llvm::Image::of(&layouts, &shared)?;
    let bytes = fennec_interp::embed::codec::Image {
        module: program.module.clone(),
        sources: program.sources.clone(),
        tables: std::mem::take(&mut image.tables),
        literals: image.literals.clone(),
    }
    .to_bytes();
    let entry = fennec_llvm::entry(&lowered, &shared, &image, &bytes)?;
    let home = fennec_build::fennec_home();
    let cache = home
        .as_deref()
        .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
        .map(fennec_build::Cache::new);
    let work = std::env::temp_dir().join(format!("fennec-llvm-{}", std::process::id()));
    let built = if release {
        fennec_llvm::compile_whole(&tools, &units, &entry, &work, runtime_bitcode(runtime).as_deref())
    } else {
        fennec_llvm::compile(&tools, &units, &entry, cache.as_ref(), &work)
    };
    // `FENNEC_LLVM_KEEP` leaves the modules' text to look at.
    if std::env::var_os("FENNEC_LLVM_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    let built = built?;
    Ok(Objects { objects: built.objects, total: units.len(), fresh: built.compiled, runtime })
}

/// Links object files with the runtime library, giving back how long linking took. When the
/// runtime's Rust is in the objects already, only its C parts are linked, so nothing of it is
/// there twice.
fn link_objects(
    objects: &[Vec<u8>],
    output: &Path,
    runtime: Runtime,
    runtime_in: bool,
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
    let linked = Command::new("cc")
        .args(&paths)
        .arg(&runtime)
        .args(SYSTEM_LIBRARIES)
        // `FENNEC_SANITIZE=address` links in AddressSanitizer's runtime.
        .args(fennec_llvm::sanitizing().then_some("-fsanitize=address"))
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|err| format!("couldn't run the system's C compiler, `cc`, to link: {err}"))?;
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    if linked.status.success() {
        Ok(took)
    } else {
        Err(format!("linking failed:\n{}", String::from_utf8_lossy(&linked.stderr)))
    }
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
    let linked = Command::new("cc")
        .arg(&object_path)
        .arg(&runtime)
        .args(SYSTEM_LIBRARIES)
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|err| format!("couldn't run the system's C compiler, `cc`, to link: {err}"))?;
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    if linked.status.success() {
        Ok(took)
    } else {
        Err(format!("linking failed:\n{}", String::from_utf8_lossy(&linked.stderr)))
    }
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
