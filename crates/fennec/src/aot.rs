//! `fennec build`: compiles a program ahead of time to a native program through LLVM. LLVM writes
//! object files, and the system's linker links them with Fennec's runtime library, which sits
//! next to the `fennec` command. On Windows, that's LLD with MinGW-w64's libraries, which come
//! with LLVM's tools in the `llvm` folder beside `fennec`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

/// The libraries Rust's standard library needs from the system, which the runtime
/// library uses, and the flag that drops the parts of the runtime a program never calls. On
/// macOS the main thread also gets the runtime's `MAIN_STACK_BYTES` of stack, which the runtime
/// counts on to run the program there, with no thread of its own to start.
#[cfg(target_os = "macos")]
const SYSTEM_LIBRARIES: &[&str] =
    &["-lSystem", "-lc", "-lm", "-Wl,-dead_strip", "-Wl,-stack_size,0x20000000"];
/// What TLS checks certificates with, for a program that makes requests. Loading these costs
/// every run, so nothing else links them.
#[cfg(target_os = "macos")]
const TLS_LIBRARIES: &[&str] = &["-framework", "Security", "-framework", "CoreFoundation"];
#[cfg(all(not(target_os = "macos"), not(windows)))]
const TLS_LIBRARIES: &[&str] = &[];
#[cfg(all(not(target_os = "macos"), not(windows)))]
const SYSTEM_LIBRARIES: &[&str] =
    &["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc", "-Wl,--gc-sections"];

// LLD links programs against MinGW-w64's libraries, so that's what the runtime library has to be
// built for, and nothing here looks for Visual Studio.
#[cfg(all(windows, target_env = "msvc"))]
compile_error!("Fennec builds with Rust's gnullvm toolchain on Windows: see CONTRIBUTING.md");

/// What Rust's standard library needs from Windows and MinGW-w64, which the runtime library uses,
/// in the order LLD reads them.
#[cfg(windows)]
const WINDOWS_LIBRARIES: &[&str] = &[
    "-lkernel32",
    "-lntdll",
    "-luserenv",
    "-lcrypt32",
    "-lncrypt",
    "-lws2_32",
    "-ldbghelp",
    "-lbcrypt",
    "-ladvapi32",
    "-lsynchronization",
    "-lunwind",
    "-lmingw32",
    "-lmingwex",
    "-lucrt",
    "-lclang_rt.builtins-x86_64",
    "-lmingwex",
    "-luser32",
    "-lkernel32",
];

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
                "`cargo build -p fennec-runtime --features threads --target-dir target/threads`, copying it there"
            }
        }
    }

    /// The runtime a lowered program needs: the threads one when it runs `parallel` and that
    /// runtime is there, so its loops run on every core and a server's connections each get a
    /// task. `FENNEC_THREADS_RUNTIME=0` asks for the plain one anyway, which runs them in order,
    /// and `=1` for the threads one, which fails if it isn't there. A threads runtime from another
    /// build is refused as a stale plain one is, never passed over for the plain one, which would
    /// quietly run a server's connections one at a time.
    fn for_program(lowered: &fennec_ir::Program, statically: bool) -> Result<Self, String> {
        if !fennec_llvm::uses_parallel(lowered) {
            return Ok(Self::Plain);
        }
        let set = |name: &str| std::env::var_os(name).filter(|on| !on.is_empty()).map(|on| on == "1");
        // `parallel` runs in order on the plain runtime, but a launched task can't run there at
        // all, so a program that launches one is refused here rather than failing when it's run.
        let launches = fennec_llvm::uses_launch(lowered);
        match set("FENNEC_THREADS_RUNTIME") {
            Some(true) => Ok(Self::Threads),
            Some(false) if launches => Err(
                "`launch` runs its task on the runtime's threads, and FENNEC_THREADS_RUNTIME=0 asks for the runtime without them".to_owned(),
            ),
            None if launches || runtime_folder(statically)?.join(Self::Threads.library()).is_file() => {
                runtime_library(Self::Threads, statically)
                    .map(|_| Self::Threads)
                    .map_err(|why| if launches { format!("`launch` runs its task on the runtime's threads: {why}") } else { why })
            }
            Some(false) | None => Ok(Self::Plain),
        }
    }
}

/// Whether `--cpu baseline` asked for code any machine of its architecture runs.
pub(crate) static BASELINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `fennec build`: a native program through LLVM, which says what it can't take yet rather than
/// falling back to anything. `--release` optimizes the whole program as one.
pub(crate) fn build(path: &str, output: Option<&str>, release: bool) -> ExitCode {
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
        fennec_rt::embed::codec::Image {
            // `fennec test` renders a failure itself.
            sources: fennec_syntax::Sources::new(),
            tables: std::mem::take(&mut image.tables),
            literals: image.literals.clone(),
        }
        .to_bytes()
    };
    let compiled = compile_emitted(&emitted, false, image_of, |lowered, shared, image, bytes| {
        fennec_llvm::tests_entry(lowered, shared, image, bytes, tests)
    })?;
    finish(&compiled, output, false)?;
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
    let emitted = emitted_with(&reached, fennec_llvm::emit_tracing_costs, false)?;
    let image_of = |image: &mut fennec_llvm::Image| {
        fennec_rt::embed::codec::Image {
            sources: program.sources.clone(),
            tables: std::mem::take(&mut image.tables),
            literals: image.literals.clone(),
        }
        .to_bytes()
    };
    let compiled = compile_emitted(&emitted, false, image_of, |lowered, shared, image, bytes| {
        fennec_llvm::entry(lowered, shared, image, bytes)
    })?;
    finish(&compiled, output, false)?;
    Ok((emitted.shared.sites, emitted.lowered))
}

/// Whether LLVM has text for a trimmed program, or why not, without compiling any of it.
pub(crate) fn takes(module: &fennec_syntax::Module) -> Result<(), String> {
    emitted(module, false).map(|_| ())
}

/// Whether LLVM has text for a trimmed program of tests, as `tests_executable` writes one.
pub(crate) fn tests_take(module: &fennec_syntax::Module) -> Result<(), String> {
    emitted(module, true).map(|_| ())
}

/// The IR a trimmed program's native code is written from, as a build lowers it.
pub(crate) fn lowered_for_native(module: &fennec_syntax::Module) -> Result<fennec_ir::Program, String> {
    emitted(module, false).map(|emitted| emitted.lowered)
}

/// What building an executable through LLVM did.
pub(crate) struct Built {
    /// How many functions the program has, and how many weren't in the cache.
    total: usize,
    fresh: usize,
    compiled: std::time::Duration,
    linked: std::time::Duration,
    /// Its place in the store, when there's one: what a run of the same program can start from.
    pub(crate) root: Option<String>,
}

/// Whether there's a linker to find. Windows doesn't come with one, so LLD and the libraries it
/// links come with LLVM's tools.
fn linker_here() -> bool {
    #[cfg(windows)]
    {
        fennec_llvm::Tools::find().is_ok_and(|tools| {
            let (lld, libraries) = windows_linker(tools.folder());
            lld.is_file() && libraries.join("crt2.o").is_file()
        })
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Why this machine can't build through LLVM at all: a tool, the linker or the runtime library
/// beside `fennec` isn't there, or the library is from another build. `None` when it can, and
/// then nothing but the program is the reason a build fails.
pub(crate) fn not_ready() -> Option<String> {
    if let Err(why) = fennec_llvm::Tools::find() {
        return Some(why);
    }
    if !linker_here() {
        return Some("there's no linker: LLD and the libraries it links come with LLVM's tools".to_owned());
    }
    runtime_library(Runtime::Plain, false).err()
}

/// Builds a checked program through LLVM to `output`, or says why LLVM can't: a function it
/// doesn't take yet, or a tool or library that isn't there.
pub(crate) fn executable(
    program: &fennec_kit::Program,
    output: &Path,
    release: bool,
) -> Result<Built, String> {
    let started = Instant::now();
    let built = llvm_objects(program, release)?;
    let compiled = started.elapsed();
    // A release built with the runtime's bitcode has its Rust inside already.
    let runtime_in = release && runtime_bitcode(built.runtime, built.statically).is_some();
    // A missing runtime with threads fails here, so `fennec run` says so rather than quietly
    // taking another way.
    let linked = finish(&built, output, runtime_in)?;
    let root = built.stored.as_ref().map(|stored| stored.root.clone());
    Ok(Built { total: built.total, fresh: built.fresh, compiled, linked, root })
}

/// Puts the finished program in the store under `root` at `output`, if it's there.
pub(crate) fn stored_executable(cache: &fennec_build::Cache, root: &str, output: &Path) -> Option<()> {
    let bytes = cache.read(PROGRAMS, root)?;
    std::fs::write(output, bytes).ok()?;
    executable_mode(output).ok()
}

/// What a dev build depends on besides the program: which runtime it links, as far as anything
/// outside the program chooses that, and what `--cpu` asked for.
pub(crate) fn dev_build_settings() -> String {
    let threads = std::env::var_os("FENNEC_THREADS_RUNTIME").map(|on| on.to_string_lossy().into_owned());
    let found = runtime_folder(false).is_ok_and(|folder| folder.join(Runtime::Threads.library()).is_file());
    let baseline = BASELINE.load(std::sync::atomic::Ordering::Relaxed);
    format!("threads={threads:?} found={found} baseline={baseline}")
}

/// Puts the finished program at `output`: the one in the store under its root hash, or else the
/// objects linked, which then go in the store under it, with the list of its functions' hashes
/// beside them. Gives back how long linking took, which is nothing for a stored one.
fn finish(compiled: &Compiled, output: &Path, runtime_in: bool) -> Result<std::time::Duration, String> {
    let written = |bytes: &[u8]| -> Result<(), String> {
        std::fs::write(output, bytes).map_err(|err| format!("couldn't write {}: {err}", output.display()))?;
        executable_mode(output)
    };
    if let Some(Stored { program: Some(bytes), .. }) = &compiled.stored {
        written(bytes)?;
        return Ok(std::time::Duration::ZERO);
    }
    let linked = link_objects(
        &compiled.objects,
        output,
        compiled.runtime,
        runtime_in,
        compiled.tls,
        compiled.statically,
    )?;
    if let Some(Stored { cache, root, functions, .. }) = &compiled.stored
        && let Ok(bytes) = std::fs::read(output)
    {
        // A store that can't be written only means linking it again next time.
        let _ = cache.write(PROGRAMS, &format!("{root}.functions"), functions.as_bytes());
        let _ = cache.write(PROGRAMS, root, &bytes);
    }
    Ok(linked)
}

/// Lets a program written to `path` run.
fn executable_mode(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|err| format!("couldn't make {} runnable: {err}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

/// The store's kind for finished programs, each named by its build's root hash.
const PROGRAMS: &str = "programs";

/// A build's place in the store: its root hash, the program kept under it when there is one, and
/// the list of its functions' hashes to keep beside a program it links.
struct Stored {
    cache: fennec_build::Cache,
    root: String,
    functions: String,
    program: Option<Vec<u8>>,
}

/// A build's root hash: one hash of everything its program depends on. That's each function's
/// text and the entry's, as LLVM reads them, which hold everything from the program's sources and
/// the standard library that it reaches; the build of `fennec`; the target and the CPU it's built
/// for; whether it's a release build and a static one; and the runtime library linked in, by its
/// size and when it was built, since hashing it whole would cost every run. See `docs/store.md`.
fn root_hash(
    units: &[fennec_llvm::Unit],
    entry: &str,
    runtime: Runtime,
    (release, statically): (bool, bool),
) -> (String, String) {
    let functions: Vec<String> =
        units.iter().map(|unit| fennec_blake3::hash(unit.text.as_bytes()).to_hex()).collect();
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec build root 2026-11-07");
    let baseline = BASELINE.load(std::sync::atomic::Ordering::Relaxed);
    let target = format!(
        "{}\n{}\n{}\n{}\n{release}\n{statically}\n{baseline}\n",
        env!("FENNEC_BUILD"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    hasher.update(target.as_bytes());
    let library = runtime_library(runtime, statically)
        .ok()
        .into_iter()
        .chain(release.then(|| runtime_bitcode(runtime, statically)).flatten());
    for path in library {
        let metadata = std::fs::metadata(&path).ok();
        let changed = metadata
            .as_ref()
            .and_then(|found| found.modified().ok())
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok());
        let size = metadata.map_or(0, |found| found.len());
        hasher.update(format!("{}\n{size}\n{changed:?}\n", path.display()).as_bytes());
    }
    for function in &functions {
        hasher.update(function.as_bytes());
    }
    hasher.update(fennec_blake3::hash(entry.as_bytes()).to_hex().as_bytes());
    (hasher.finalize().to_hex(), functions.join("\n"))
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
    /// Whether it links on its own, against the runtime built for musl, which its runtime was
    /// chosen for, so it's linked that way too.
    statically: bool,
    /// Its place in the store, when there's one to use.
    stored: Option<Stored>,
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
    emitted_with(reached, if traced { fennec_llvm::emit_traced } else { fennec_llvm::emit_for }, traced)
}

/// What LLVM emits for a program: each function's text, and what they share.
type Emits = Result<(Vec<fennec_llvm::Unit>, fennec_llvm::Shared, fennec_llvm::Layouts), String>;

/// A trimmed program's text from one of LLVM's ways of emitting it. `tests` says it's a program of
/// tests, whose network is a fake.
fn emitted_with(
    reached: &fennec_syntax::Module,
    emit: fn(&fennec_ir::Program, fennec_llvm::Cpu) -> Emits,
    tests: bool,
) -> Result<Emitted, String> {
    // Reading and writing JSON and TOML go through std's code in Fennec, where LLVM can take it.
    let mut routed = reached.clone();
    // So does sorting what has its own `<`, before formats, which resolve what they change.
    let sorts = fennec_check::route_sorts_natively(&mut routed);
    let formats = fennec_check::route_formats_natively(&mut routed);
    if (formats || sorts)
        && let Ok(emitted) = emitted_as_written(&routed, emit, tests)
    {
        return Ok(emitted);
    }
    emitted_as_written(reached, emit, tests)
}

/// What LLVM emits for a trimmed program, with its calls as they are.
fn emitted_as_written(
    reached: &fennec_syntax::Module,
    emit: fn(&fennec_ir::Program, fennec_llvm::Cpu) -> Emits,
    tests: bool,
) -> Result<Emitted, String> {
    let checked = fennec_check::check_to_emit(reached)
        .map_err(|problems| format!("the program didn't check once trimmed: {problems:?}"))?;
    // `parallel` runs on the runtime's threads, its work lifted for each range of positions.
    let mut lowered = fennec_ir::lower_with(
        reached,
        &checked,
        // A test's network is a fake, which answers a server's connections in order.
        fennec_ir::Options { outline_parallel: true, stop_on_failed_main: true, serve_in_order: tests },
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
    // A release build leaves native blocks unwatched, paying nothing for it; a dev build tells
    // the runtime of each, for it to stop if it's ever given one.
    fennec_llvm::trust_native_blocks(release);
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    let emitted = emitted(&reached, false)?;
    let image_of = |image: &mut fennec_llvm::Image| {
        fennec_rt::embed::codec::Image {
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
    let tls = fennec_llvm::uses_tls(lowered);
    let tools = fennec_llvm::Tools::find()?;
    let mut image = fennec_llvm::Image::of(layouts, shared)?;
    let bytes = image_of(&mut image);
    let entry = entry_of(lowered, shared, &image, &bytes)?;
    let statically = links_statically();
    let runtime = Runtime::for_program(lowered, statically)?;
    let home = fennec_build::fennec_home();
    let cache = home
        .as_deref()
        .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
        .map(fennec_build::Cache::new);
    // A build that's been made before is in the store whole, under its root hash.
    let stored = cache.clone().map(|cache| {
        let (root, functions) = root_hash(units, &entry, runtime, (release, statically));
        let program = cache.read(PROGRAMS, &root);
        Stored { cache, root, functions, program }
    });
    if stored.as_ref().is_some_and(|stored| stored.program.is_some()) {
        return Ok(Compiled {
            objects: Vec::new(),
            total: units.len(),
            fresh: 0,
            runtime,
            tls,
            statically,
            stored,
        });
    }
    let work = crate::own_temp("llvm");
    let built = if release {
        fennec_llvm::compile_whole(
            &tools,
            units,
            &entry,
            &work,
            runtime_bitcode(runtime, statically).as_deref(),
        )
    } else {
        fennec_llvm::compile(&tools, units, &entry, cache.as_ref(), &work)
    };
    // `FENNEC_LLVM_KEEP` leaves the modules' text to look at.
    if std::env::var_os("FENNEC_LLVM_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    let built = built?;
    Ok(Compiled {
        objects: built.objects,
        total: units.len(),
        fresh: built.compiled,
        runtime,
        tls,
        statically,
        stored,
    })
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
    statically: bool,
) -> Result<std::time::Duration, String> {
    let runtime = runtime_library(runtime, statically)?;
    let work = crate::own_temp("link");
    std::fs::create_dir_all(&work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let mut paths = Vec::with_capacity(objects.len());
    for (at, object) in objects.iter().enumerate() {
        let path = work.join(format!("{at}.o"));
        std::fs::write(&path, object).map_err(|err| format!("couldn't write an object file: {err}"))?;
        paths.push(path);
    }
    // A program linked on its own holds the notices musl's license asks to go with it.
    if statically {
        paths.push(crate::notices::object(&work)?);
    }
    let started = Instant::now();
    let runtime = if runtime_in { c_parts(&runtime, &work)? } else { runtime };
    let linked = if statically {
        static_linker(&paths, &runtime, output)
    } else {
        linker(&paths, &runtime, output, tls)
    }
    .output();
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    let linked = linked.map_err(|err| {
        if cfg!(target_os = "macos") && err.kind() == std::io::ErrorKind::NotFound {
            NO_XCODE_TOOLS.to_owned()
        } else {
            format!("couldn't run the linker: {err}")
        }
    })?;
    if linked.status.success() { Ok(took) } else { Err(link_failure(&linked)) }
}

/// What a Mac without Xcode's command line tools is told, as it links a program through them.
const NO_XCODE_TOOLS: &str = "linking a program on a Mac needs Xcode's command line tools, which aren't installed: run `xcode-select --install`";

/// What a linker said when it failed, on standard error or standard output.
fn link_failure(linked: &std::process::Output) -> String {
    // A Mac's `cc` is a stand-in that runs Xcode's, and says so through `xcrun` when there's none.
    if cfg!(target_os = "macos") && String::from_utf8_lossy(&linked.stderr).contains("xcrun: error") {
        return format!("{NO_XCODE_TOOLS}\n{}", String::from_utf8_lossy(&linked.stderr).trim_end());
    }
    format!(
        "linking failed:\n{}{}",
        String::from_utf8_lossy(&linked.stderr),
        String::from_utf8_lossy(&linked.stdout)
    )
}

/// The system's linker, set to link objects with a runtime library into an executable. On a Mac
/// that's `cc` from Xcode's command line tools, which a Mac program links against.
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

/// Whether a build links its program on its own, with musl's C library in it: on Linux, when the
/// `musl` folder beside `fennec` has the runtime built for musl and musl's own start-up files, as
/// a release's does. Then every build does, so building needs nothing from the machine, not even a
/// C compiler. `FENNEC_STATIC=0` asks for a program linked against the system's C library anyway,
/// and so does a sanitizer, whose runtime is the system compiler's.
fn links_statically() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("FENNEC_STATIC").is_none_or(|on| on != "0")
        && fennec_llvm::sanitize_flag().is_none()
        && musl_folder().is_some_and(|musl| musl.join("self-contained/crt1.o").is_file())
}

/// The folder beside `fennec` with what a program linked on its own needs.
fn musl_folder() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.with_file_name("musl"))
}

/// The linker for a program linked on its own, with a runtime library built for musl and with
/// musl's own start-up files and C library, into an executable that needs nothing from the machine
/// it runs on: no C library, and no loader. It's LLD, beside LLVM's other tools, so building needs
/// no C compiler either; where there's none, as in a checkout whose Rust has no `rust-lld`, it's the
/// system's compiler driving its own linker.
fn static_linker(objects: &[PathBuf], runtime: &Path, output: &Path) -> Command {
    let musl = musl_folder().unwrap_or_default().join("self-contained");
    let lld = fennec_llvm::Tools::find()
        .ok()
        .map(|tools| tools.folder().join("rust-lld"))
        .filter(|lld| lld.is_file());
    // musl's C library takes the compiler's helpers, like `__multf3` for a `long double` on arm64,
    // from the runtime, or, when only the runtime's C parts are linked, from the helpers taken out
    // of it for that, or the system compiler's `libgcc`.
    let helpers = musl.join("libcompiler_builtins.a");
    let (mut command, pass) = if let Some(lld) = &lld {
        let mut command = Command::new(lld);
        command.args(["-flavor", "gnu", "-static", "--no-pie"]);
        (command, "")
    } else {
        let mut command = Command::new("cc");
        command.args(["-static", "-no-pie", "-nostdlib", "-nostartfiles"]);
        (command, "-Wl,")
    };
    command
        .arg(musl.join("crt1.o"))
        .arg(musl.join("crti.o"))
        .arg(musl.join("crtbegin.o"))
        .args(objects)
        .arg(format!("{pass}--start-group"))
        .arg(runtime)
        .arg(musl.join("libunwind.a"))
        .arg(musl.join("libc.a"));
    if helpers.is_file() {
        command.arg(&helpers);
    } else if lld.is_none() {
        command.arg("-lgcc");
    }
    command
        .arg(format!("{pass}--end-group"))
        .arg(musl.join("crtend.o"))
        .arg(musl.join("crtn.o"))
        .args([format!("{pass}--gc-sections"), format!("{pass}--eh-frame-hdr")])
        // Nothing reads the notices, so dropping what's unused would drop them too.
        .arg(format!("{pass}--undefined={}", crate::notices::SYMBOL))
        .arg("-o")
        .arg(output);
    command
}

/// LLD and the folder of libraries it links, in the `llvm` folder beside `fennec`.
#[cfg(windows)]
fn windows_linker(tools: &Path) -> (PathBuf, PathBuf) {
    (tools.join("rust-lld.exe"), tools.with_file_name("lib").join("self-contained"))
}

/// LLD, set to link objects with a runtime library into an executable.
#[cfg(windows)]
fn linker(objects: &[PathBuf], runtime: &Path, output: &Path, _tls: bool) -> Command {
    let tools = fennec_llvm::Tools::find().map(|tools| tools.folder().to_path_buf()).unwrap_or_default();
    let (lld, libraries) = windows_linker(&tools);
    let mut command = Command::new(lld);
    command
        .args(["-flavor", "gnu", "-m", "i386pep", "--dynamicbase", "--high-entropy-va", "--nxcompat"])
        .args(["--gc-sections", "--subsystem", "console"])
        .arg("-L")
        .arg(&libraries)
        .arg(libraries.join("crt2.o"));
    // Windows takes a command line of 32,767 characters at most, and a program built in many
    // parts names more objects than that holds, so they go in a file LLD reads with `@`.
    match response_file(objects) {
        Some(file) => command.arg(format!("@{}", file.display())),
        None => command.args(objects),
    };
    command.arg(runtime).args(WINDOWS_LIBRARIES).arg("-o").arg(output);
    command
}

/// A file naming each object on a line of its own, quoted and with `/` for each backslash, as LLD
/// reads a backslash in one as an escape. It's beside the first of them: `None` when there are none, or it
/// couldn't be written.
#[cfg(any(windows, test))]
fn response_file(objects: &[PathBuf]) -> Option<PathBuf> {
    use std::fmt::Write as _;
    let file = objects.first()?.parent()?.join("objects.rsp");
    let mut named = String::new();
    for object in objects {
        let _ = writeln!(named, "\"{}\"", object.display().to_string().replace('\\', "/"));
    }
    std::fs::write(&file, named).ok()?;
    Some(file)
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

/// The runtime's bitcode, next to the `fennec` command, if it was built: see
/// `Runtime::bitcode`.
fn runtime_bitcode(runtime: Runtime, statically: bool) -> Option<PathBuf> {
    Some(runtime_folder(statically).ok()?.join(runtime.bitcode())).filter(|path| path.is_file())
}

/// A copy of the runtime library with only its C objects: mimalloc and the compiler's helpers.
fn c_parts(runtime: &Path, work: &Path) -> Result<PathBuf, String> {
    let copy = work.join("runtime-c.a");
    std::fs::copy(runtime, &copy).map_err(|err| format!("couldn't copy the runtime library: {err}"))?;
    let ar = |args: &[&std::ffi::OsStr]| {
        archiver().args(args).output().map_err(|err| format!("couldn't run `ar`: {err}"))
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

/// What edits an archive: LLVM's, beside its other tools, as a release has it, so building needs
/// nothing from the machine; or the system's `ar`.
fn archiver() -> Command {
    let shipped = fennec_llvm::Tools::find()
        .ok()
        .map(|tools| tools.folder().join(format!("llvm-ar{}", std::env::consts::EXE_SUFFIX)))
        .filter(|ar| ar.is_file());
    Command::new(shipped.unwrap_or_else(|| PathBuf::from("ar")))
}

/// Where the runtime libraries are: next to the `fennec` command, or in the `musl` folder there
/// for a program linked on its own.
fn runtime_folder(statically: bool) -> Result<PathBuf, String> {
    let fennec =
        std::env::current_exe().map_err(|err| format!("couldn't find the `fennec` command: {err}"))?;
    let beside = fennec.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(if statically { beside.join("musl") } else { beside })
}

/// The runtime library, next to the `fennec` command, or in its `musl` folder. One built from
/// other source than this `fennec` is refused, saying how to build it again, since linking a
/// program with it fails in ways that say nothing about why.
fn runtime_library(runtime: Runtime, statically: bool) -> Result<PathBuf, String> {
    let path = runtime_folder(statically)?.join(runtime.library());
    if path.is_file() {
        match stamp_of(&path) {
            Some(build) if build != env!("FENNEC_BUILD") => Err(format!(
                "the runtime library `{}` is from another build of Fennec than this `fennec`: build it again with {}, or install both again with `.github/scripts/install.sh` from the Fennec checkout",
                path.display(),
                runtime.how_to_build()
            )),
            _ => Ok(path),
        }
    } else {
        Err(format!(
            "the runtime library `{}` should be at {}; build it with {}",
            runtime.library(),
            path.display(),
            runtime.how_to_build()
        ))
    }
}

/// The build a runtime library's bytes are stamped with: the 64 hex digits after its marker.
fn stamp_in(bytes: &[u8]) -> Option<String> {
    let marker = ["fennec-runtime-", "build:"].concat().into_bytes();
    let at = bytes.windows(marker.len()).position(|window| window == marker.as_slice())? + marker.len();
    let hex = std::str::from_utf8(bytes.get(at..at + 64)?).ok()?;
    hex.bytes().all(|digit| digit.is_ascii_hexdigit()).then(|| hex.to_owned())
}

/// The build of Fennec a runtime library is from, as its stamp says, or `None` when it has none,
/// like one from before stamps. Reading a library whole takes a while, so what it says is kept in the
/// store by the file's path, size and when it changed.
fn stamp_of(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let changed = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let name =
        fennec_blake3::hash(format!("{}\n{}\n{changed}", path.display(), metadata.len()).as_bytes()).to_hex();
    let cache = crate::run::cache();
    if let Some(kept) = cache.as_ref().and_then(|cache| cache.read("runtime-stamps", &name)) {
        return String::from_utf8(kept).ok().filter(|build| !build.is_empty());
    }
    let build = stamp_in(&std::fs::read(path).ok()?);
    if let Some(cache) = &cache {
        // A store that can't be written only means reading the library again next time.
        let _ = cache.write("runtime-stamps", &name, build.as_deref().unwrap_or("").as_bytes());
    }
    build
}

#[cfg(test)]
mod tests {
    /// A runtime library's stamp is read from among its bytes, and one with none, or with a
    /// marker that isn't followed by a build's hash, has no stamp.
    #[test]
    fn a_runtime_librarys_stamp_is_read_from_its_bytes() {
        let build = "7e45e88e148429f54d5c3f2e10f914c2c9be0b18e8026e3c3290c9a9f7371d37";
        let library =
            [b"\x00archive\x7f".as_slice(), b"fennec-runtime-build:", build.as_bytes(), b"fennec_fail"]
                .concat();
        assert_eq!(super::stamp_in(&library).as_deref(), Some(build));
        assert_eq!(super::stamp_in(b"an old library with no stamp"), None);
        assert_eq!(super::stamp_in(b"fennec-runtime-build:short"), None);
        assert_eq!(super::stamp_in(&[b"fennec-runtime-build:".as_slice(), &[b'z'; 64]].concat()), None);
    }

    /// A Mac whose `cc` says through `xcrun` that Xcode's command line tools aren't there is told
    /// how to install them, and what `cc` said goes along.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_mac_without_xcodes_tools_is_told_how_to_get_them() {
        use std::os::unix::process::ExitStatusExt;
        let said = "xcrun: error: invalid active developer path (/Library/Developer/CommandLineTools)\n";
        let linked = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: said.as_bytes().to_vec(),
        };
        let told = super::link_failure(&linked);
        assert!(told.starts_with(super::NO_XCODE_TOOLS), "{told}");
        assert!(told.contains("invalid active developer path"), "{told}");
    }

    /// The main thread's stack an executable is linked with on macOS is the one the runtime
    /// counts on to run the program there.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_main_stack_linked_is_the_one_the_runtime_counts_on() {
        let flag = format!("-Wl,-stack_size,{:#x}", fennec_rt::embed::aot::MAIN_STACK_BYTES);
        assert!(super::SYSTEM_LIBRARIES.contains(&flag.as_str()), "{flag}");
    }

    /// The objects LLD is given on Windows go in a file, each quoted on a line of its own, so a
    /// program in many parts doesn't make a command line longer than Windows takes.
    #[test]
    fn the_objects_to_link_go_in_a_file_each_on_a_line() {
        let work = std::env::temp_dir().join(format!("fennec-response-file-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let objects: Vec<_> = (0..2_000).map(|at| work.join(format!("{at}.o"))).collect();
        let file = super::response_file(&objects).unwrap();
        assert_eq!(file, work.join("objects.rsp"));
        let named = std::fs::read_to_string(&file).unwrap();
        assert_eq!(named.lines().count(), 2_000);
        let first = objects[0].display().to_string().replace('\\', "/");
        assert_eq!(named.lines().next(), Some(format!("\"{first}\"").as_str()));
        assert!(!named.contains('\\'), "a backslash would be read as an escape");
        assert!(named.len() > 32_767, "a file this size wouldn't fit on a command line");
        assert_eq!(super::response_file(&[]), None);
        std::fs::remove_dir_all(&work).unwrap();
    }
}
