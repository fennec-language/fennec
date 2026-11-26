//! `fennec build`: compiles a program ahead of time to a native program through LLVM. LLVM writes
//! object files, and the system's linker links them with Fennec's runtime library, which sits
//! next to the `fennec` command. On Windows, that's LLD with MinGW-w64's libraries, which come
//! with LLVM's tools in the `llvm` folder beside `fennec`.

use std::collections::HashMap;
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
    /// The library's file name. A program built for a heap audit, under `FENNEC_AUDIT=1`, links
    /// the one built with the audit in it, which no release has: so it's refused there, as any
    /// library that's missing is, rather than run with nothing checked.
    fn library(self) -> &'static str {
        match (self, fennec_llvm::audited()) {
            (Self::Plain, false) => "libfennec_runtime.a",
            (Self::Threads, false) => "libfennec_runtime_threads.a",
            (Self::Plain, true) => "libfennec_runtime_audit.a",
            (Self::Threads, true) => "libfennec_runtime_threads_audit.a",
        }
    }

    /// The runtime's own bitcode, which a release build puts in the program's unit when it's
    /// there.
    fn bitcode(self) -> &'static str {
        match (self, fennec_llvm::audited()) {
            (Self::Plain, false) => "libfennec_runtime.bc",
            (Self::Threads, false) => "libfennec_runtime_threads.bc",
            (Self::Plain, true) => "libfennec_runtime_audit.bc",
            (Self::Threads, true) => "libfennec_runtime_threads_audit.bc",
        }
    }

    /// How to build the library, for saying when it's missing.
    fn how_to_build(self) -> &'static str {
        match (self, fennec_llvm::audited()) {
            (Self::Plain, false) => "`cargo build -p fennec-runtime`",
            (Self::Threads, false) => {
                "`cargo build -p fennec-runtime --features threads --target-dir target/threads`, copying it there"
            }
            (Self::Plain, true) => {
                "`cargo build -p fennec-runtime --features audit --target-dir target/audit`, copying it there"
            }
            (Self::Threads, true) => {
                "`cargo build -p fennec-runtime --features threads,audit --target-dir target/threads-audit`, copying \
                 it there"
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

/// What `main` reaches in `module`, copied out, rather than copying it all and cutting the rest
/// away. A dev build of `fennec` checks it's what the cutting leaves.
pub(crate) fn main_reaches(module: &fennec_syntax::Module) -> fennec_syntax::Module {
    let reached = fennec_build::what_main_reaches(module);
    if cfg!(debug_assertions) {
        let mut cut = module.clone();
        fennec_build::keep_what_main_reaches(&mut cut);
        assert!(cut == reached, "what `main` reaches, copied out, isn't what cutting the rest away leaves");
    }
    reached
}

/// Whether `--cpu baseline` asked for code any machine of its architecture runs.
pub(crate) static BASELINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the daemon's last build loaded, for its next build to use again: each file's parse, and
/// its rewriting of names across files (`fennec_kit::load_keeping`). Only the daemon keeps one,
/// from when it starts: a command building in its own process loads once.
pub(crate) static KEPT_LOAD: std::sync::Mutex<Option<fennec_kit::KeptLoad>> = std::sync::Mutex::new(None);

/// Loads a program to build, as `crate::load` does, using what the daemon's last build kept.
/// A dev build compares one kept load in 8 with loading it all again.
fn load_to_build(path: &str) -> Option<fennec_kit::Program> {
    let mut kept = KEPT_LOAD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(kept) = kept.as_mut() else { return crate::load(path, fennec_kit::Entry::Main) };
    let compare = fennec_kit::Compare::InDevBuilds;
    match fennec_kit::load_keeping(Path::new(path), path, fennec_kit::Entry::Main, kept, compare) {
        Ok(program) => Some(program),
        Err(problems) => {
            eprint!("{}", problems.render());
            None
        }
    }
}

/// `fennec build`: a native program through LLVM, which says what it can't take yet rather than
/// falling back to anything. `--release` optimizes the whole program as one.
pub(crate) fn build(path: &str, output: Option<&str>, release: bool) -> ExitCode {
    build_kept(path, output, release).map_or_else(|code| code, |_| ExitCode::SUCCESS)
}

/// `build`, giving back the program it loaded and what it built from it, for the store to note.
pub(crate) fn build_kept(
    path: &str,
    output: Option<&str>,
    release: bool,
) -> Result<(fennec_kit::Program, Built), ExitCode> {
    let Some(mut program) = load_to_build(path) else { return Err(ExitCode::FAILURE) };
    // A build the daemon runs for a command that has gone stops between its steps.
    if fennec_build::cancelled() {
        return Err(ExitCode::FAILURE);
    }
    let checked = crate::run::checked_to_build(&mut program)?;
    if fennec_build::cancelled() {
        return Err(ExitCode::FAILURE);
    }
    let output = output.map_or_else(|| default_output(path, &program), PathBuf::from);
    match executable_checked(&program, &output, release, checked) {
        Ok(built) => {
            said_built(&output, &built);
            said_packed(&program.sources);
            Ok((program, built))
        }
        Err(message) => {
            eprintln!("error: {message}");
            Err(ExitCode::FAILURE)
        }
    }
}

/// What a build says when a program has more files than there are slots for, so the spans of
/// those past them move when an earlier one's length changes, and what's kept from one build to
/// the next can't be kept for them (#2114).
fn said_packed(sources: &fennec_syntax::Sources) {
    if let Some(note) = packed_note(sources) {
        eprintln!("{note}");
    }
}

/// The note `said_packed` gives, when there is one.
fn packed_note(sources: &fennec_syntax::Sources) -> Option<String> {
    let slotted = sources.packed_past()?;
    let packed = sources.files().count().saturating_sub(slotted);
    Some(format!(
        "note: the first {slotted} files each start at a slot of their own, and the {packed} after them are packed one after another, so a change to one moves the spans of those after it, and a build keeps nothing for them from the last (#2114)"
    ))
}

/// What a build says it made.
pub(crate) fn said_built(output: &Path, built: &Built) {
    // How many functions' text was written, when the build used any again without writing it.
    let written = if built.reused > 0 {
        format!(" (wrote {} of {})", built.total.saturating_sub(built.reused), built.total)
    } else {
        String::new()
    };
    println!(
        "🦊 built {} with LLVM: compiled {} of {} functions in {} ms, linked in {} ms{written}",
        output.display(),
        built.fresh,
        built.total,
        built.compiled.as_millis(),
        built.linked.as_millis(),
    );
}

/// Builds an executable that runs the functions `tests` of a module with everything else trimmed
/// away, through LLVM's cached dev build. Gives back the functions that have a flag saying they
/// ran, in the order the executable numbers them.
pub(crate) fn tests_executable(
    module: &fennec_syntax::Module,
    tests: &[String],
    output: &Path,
) -> Result<Vec<String>, String> {
    tests_executable_from(TestsText(emitted(module, true)?), tests, output)
}

/// [`tests_executable`] from the text [`tests_take`] wrote for the same trimmed program, so it isn't
/// written twice. Writing it twice cost more than the text: each time is a build to the daemon's
/// kept analyses, and two test builds between a `fennec run`'s programs pushed out every analysis
/// of the program's own functions, which then were all worked out again.
pub(crate) fn tests_executable_from(
    TestsText(emitted): TestsText,
    tests: &[String],
    output: &Path,
) -> Result<Vec<String>, String> {
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
    finish(&compiled, output, false, &emitted.units)?;
    Ok(emitted.shared.ran.iter().cloned().collect())
}

/// Builds an executable of a program that says what each place in its source cost, for `fennec dev
/// trace`, through LLVM's cached dev build. Gives back what's at each place, and the lowered
/// program.
pub(crate) fn tracing_executable(
    program: &fennec_kit::Program,
    output: &Path,
) -> Result<(std::collections::BTreeMap<u32, String>, fennec_ir::Program), String> {
    let reached = main_reaches(&program.module);
    let emitted = emitted_with(&reached, &fennec_llvm::emit_tracing_costs, false, None)?;
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
    finish(&compiled, output, false, &emitted.units)?;
    Ok((emitted.shared.sites, emitted.lowered))
}

/// Whether LLVM has text for a trimmed program, or why not, without compiling any of it.
pub(crate) fn takes(module: &fennec_syntax::Module) -> Result<(), String> {
    emitted(module, false).map(|_| ())
}

/// LLVM's text for a trimmed program of tests, as `tests_executable` writes it, or why it has none:
/// [`tests_executable_from`] builds the executable from it.
pub(crate) fn tests_take(module: &fennec_syntax::Module) -> Result<TestsText, String> {
    emitted(module, true).map(TestsText)
}

/// What [`tests_take`] wrote for a trimmed program of tests, to build its executable from.
pub(crate) struct TestsText(Emitted);

/// The IR a trimmed program's native code is written from, as a build lowers it.
pub(crate) fn lowered_for_native(module: &fennec_syntax::Module) -> Result<fennec_ir::Program, String> {
    emitted(module, false).map(|emitted| emitted.lowered)
}

/// What building an executable through LLVM did.
pub(crate) struct Built {
    /// How many functions the program has, and how many weren't in the cache.
    pub(crate) total: usize,
    pub(crate) fresh: usize,
    /// How many were used again as the last build kept them, without writing their text.
    pub(crate) reused: usize,
    pub(crate) compiled: std::time::Duration,
    pub(crate) linked: std::time::Duration,
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
    executable_checked(program, output, release, None)
}

/// Like `executable`, for a program whose module was checked already, cut down to what `main`
/// reaches: `checked` is what that check found, as checking it to emit would, when it's known.
pub(crate) fn executable_checked(
    program: &fennec_kit::Program,
    output: &Path,
    release: bool,
    checked: Option<fennec_check::Signatures>,
) -> Result<Built, String> {
    if crate::child::wanted() {
        match crate::child::built(program, output, release, checked.as_ref()) {
            Ok(built) => return built,
            Err(no_answer) => crate::child::said_no_answer(&no_answer, output),
        }
    }
    executable_checked_here(program, output, release, checked)
}

/// [`executable_checked`], in this process.
pub(crate) fn executable_checked_here(
    program: &fennec_kit::Program,
    output: &Path,
    release: bool,
    checked: Option<fennec_check::Signatures>,
) -> Result<Built, String> {
    match built_and_linked(program, output, release, (checked.clone(), true)) {
        // A unit used again has no text to compile again from, so when its object goes from the
        // cache before the link, the build is made again writing every unit.
        Err(why) if why == fennec_llvm::USED_AGAIN_OBJECT_GONE => {
            eprintln!(
                "note: a cleanup took the compiled object of a function this build used again, so it wrote every function again"
            );
            built_and_linked(program, output, release, (checked, false))
        }
        other => other,
    }
}

/// [`executable_checked`]'s build, using units again when `reusing` (a dev build only).
fn built_and_linked(
    program: &fennec_kit::Program,
    output: &Path,
    release: bool,
    (checked, reusing): (Option<fennec_check::Signatures>, bool),
) -> Result<Built, String> {
    let started = Instant::now();
    let (built, units, known) = llvm_objects(program, release, (checked, reusing))?;
    let compiled = started.elapsed();
    if fennec_build::cancelled() {
        return Err(fennec_llvm::STOPPED.to_owned());
    }
    // A release built with the runtime's bitcode has its Rust inside already.
    let runtime_in = built.bitcode.is_some();
    // A missing runtime with threads fails here, so `fennec run` says so rather than quietly
    // taking another way.
    let linked = finish(&built, output, runtime_in, &units)?;
    // Every object it linked is in the cache, for the daemon's next build to know without looking.
    fennec_llvm::objects_known(known, &units);
    let root = built.stored.as_ref().map(|stored| stored.root.clone());
    Ok(Built { total: built.total, fresh: built.fresh, reused: built.reused, compiled, linked, root })
}

/// Puts the finished program in the store under `root` at `output`, if it's there.
/// For a run, into Fennec's own folder, the file is the store's own, linked out rather than
/// copied: macOS holds the first launch of each new file for seconds while it checks it, and this
/// one it has launched before. Anywhere else, as for `fennec build -o`, it's a copy, since a file
/// someone else may write into must never be the one the store keeps.
pub(crate) fn stored_executable(cache: &fennec_build::Cache, root: &str, output: &Path) -> Option<()> {
    if fennec_owns(output) {
        return cache.link_out(PROGRAMS, root, output).ok()?.then_some(());
    }
    let bytes = cache.read(PROGRAMS, root)?;
    put_program(output, &bytes).ok()
}

/// Writes a program to `output` as a new file, put in place of what's there by a rename, never
/// written into the file already there. macOS remembers the code signature of a file it has
/// launched, and a program written over one it launched before is killed when it starts.
fn put_program(output: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = output.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let writing = output.with_file_name(format!(".{name}.{}.writing", std::process::id()));
    let put = || {
        std::fs::write(&writing, bytes)
            .map_err(|err| format!("couldn't write {}: {err}", output.display()))?;
        executable_mode(&writing)?;
        std::fs::rename(&writing, output).map_err(|err| format!("couldn't write {}: {err}", output.display()))
    };
    put().inspect_err(|_| {
        let _ = std::fs::remove_file(&writing);
    })?;
    made_new(output);
    Ok(())
}

/// Whether a program at `path` is kept as one file with the store's: on macOS, which holds a new
/// file's first launch, and in Fennec's own folder for what it makes and throws away, where
/// nothing but Fennec writes, and only once. Elsewhere a copy costs nothing a link would save.
fn fennec_owns(path: &Path) -> bool {
    cfg!(target_os = "macos") && path.starts_with(fennec_build::work_folder())
}

/// What a dev build depends on besides the program: which runtime it links, as far as anything
/// outside the program chooses that, whether it's linked on its own, what `--cpu` asked for, and
/// every output switch.
pub(crate) fn dev_build_settings() -> String {
    let threads = std::env::var_os("FENNEC_THREADS_RUNTIME").map(|on| on.to_string_lossy().into_owned());
    let found = runtime_folder(false).is_ok_and(|folder| folder.join(Runtime::Threads.library()).is_file());
    let baseline = BASELINE.load(std::sync::atomic::Ordering::Relaxed);
    format!(
        "threads={threads:?} found={found} baseline={baseline} static={} {}",
        links_statically(),
        fennec_llvm::output_switches()
    )
}

/// Which of the runtime's bitcode a release build would put in its unit, for the key of a build
/// of unchanged files (see `built`): the plain runtime's and the one with threads, either of
/// which a program may link. Bitcode from another build is refused, as building refuses it, so
/// any found is this build's.
pub(crate) fn release_bitcode() -> Result<String, String> {
    let statically = links_statically();
    let plain = runtime_bitcode(Runtime::Plain, statically)?.is_some();
    let threads = runtime_bitcode(Runtime::Threads, statically)?.is_some();
    Ok(format!("bitcode plain={plain} threads={threads}"))
}

/// Puts the finished program at `output`: the one in the store under its root hash, or else the
/// objects linked, which then go in the store under it, with the list of its functions' hashes
/// beside them. Gives back how long linking took, which is nothing for a stored one.
fn finish(
    compiled: &Compiled,
    output: &Path,
    runtime_in: bool,
    units: &[fennec_llvm::Unit],
) -> Result<std::time::Duration, String> {
    if let Some(Stored { cache, root, program: Some(bytes), .. }) = &compiled.stored {
        if stored_executable(cache, root, output).is_none() {
            put_program(output, bytes)?;
        }
        return Ok(std::time::Duration::ZERO);
    }
    // The linker writes a new file where none is, rather than into one a run may have launched.
    let _ = std::fs::remove_file(output);
    // `FENNEC_LOSE_KEPT_OBJECTS=1`, for a test: every kept object goes before the link, as a
    // cleanup another command runs may take them.
    if std::env::var("FENNEC_LOSE_KEPT_OBJECTS").as_deref() == Ok("1") {
        for object in &compiled.objects {
            if let fennec_llvm::Object::Kept(path) = object {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    // Each kept object compiled again because it went, by its place among the objects.
    let mut again = HashMap::new();
    let mut tries = 0;
    let linked = loop {
        tries += 1;
        compiled_again(&compiled.objects, &mut again, units)?;
        let objects: Vec<&fennec_llvm::Object> = compiled
            .objects
            .iter()
            .enumerate()
            .map(|(at, object)| again.get(&at).unwrap_or(object))
            .collect();
        let how = (compiled.statically, compiled.release);
        match link_objects(&objects, output, compiled.runtime, runtime_in, compiled.tls, how) {
            Ok(linked) => break linked,
            // An object taken while the linker read the others: compiled again, and linked again.
            Err(_) if tries < LINK_TRIES && lost(&compiled.objects, &again).next().is_some() => {}
            Err(failed) => return Err(failed),
        }
    };
    if let Some(Stored { cache, root, functions, .. }) = &compiled.stored {
        // A store that can't be written only means linking it again next time. A run's program
        // is kept as the file just linked, the one about to be launched, so a later run of the
        // same program launches a file macOS has checked already.
        fennec_build::kept(
            "keep a program's list of functions",
            cache.write(PROGRAMS, &format!("{root}.functions"), functions.as_bytes()),
        );
        if fennec_owns(output) {
            fennec_build::kept("keep the program it linked", cache.keep_file(PROGRAMS, root, output));
        } else if let Ok(bytes) = std::fs::read(output) {
            fennec_build::kept("keep the program it linked", cache.write(PROGRAMS, root, &bytes));
        }
    }
    Ok(linked)
}

/// How many times a build links while kept objects go from under it, compiling each again.
const LINK_TRIES: usize = 3;

/// The places of the kept objects that are gone from the cache and not compiled again yet.
fn lost<'o>(
    objects: &'o [fennec_llvm::Object],
    again: &'o HashMap<usize, fennec_llvm::Object>,
) -> impl Iterator<Item = usize> + 'o {
    objects.iter().enumerate().filter_map(move |(at, object)| match object {
        fennec_llvm::Object::Kept(path) if !again.contains_key(&at) && !path.is_file() => Some(at),
        _ => None,
    })
}

/// Compiles again each of a dev build's kept objects that's gone from the cache since the build
/// found it, as a cleanup another command started can take it, for the link to use its bytes. So
/// a cache file going missing can never fail a build: it only costs compiling that function again.
/// `objects` are the units' in order, and the entry's after them, which is never kept.
fn compiled_again(
    objects: &[fennec_llvm::Object],
    again: &mut HashMap<usize, fennec_llvm::Object>,
    units: &[fennec_llvm::Unit],
) -> Result<(), String> {
    let gone: Vec<usize> = lost(objects, again).collect();
    if gone.is_empty() {
        return Ok(());
    }
    let tools = fennec_llvm::Tools::find()?;
    let work = crate::own_temp("llvm-again");
    let made = gone.into_iter().try_for_each(|at| {
        let unit = units.get(at).ok_or("a kept object that's gone is no function's")?;
        again.insert(at, fennec_llvm::Object::Bytes(fennec_llvm::compile_again(&tools, unit, &work)?));
        Ok::<(), String>(())
    });
    let _ = std::fs::remove_dir_all(&work);
    made
}

/// Lets a program written to `path` run.
#[cfg_attr(
    not(unix),
    expect(clippy::unnecessary_wraps, reason = "only Unix has a mode to set, which can fail")
)]
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
/// the list of its functions' hashes to keep beside a program it links, with each function's
/// hash as it is, which also names its compiled object, so no function's text is hashed twice.
struct Stored {
    cache: fennec_build::Cache,
    root: String,
    functions: String,
    program: Option<Vec<u8>>,
}

/// A build's root hash: one hash of everything its program depends on. That's each function's
/// text and the entry's, as LLVM reads them, which hold everything from the program's sources and
/// the standard library that it reaches; the build of `fennec`; the version of LLVM that compiles
/// them, so a program built before LLVM's tools changed isn't handed back after; the target and
/// the CPU it's built for; whether it's a release build and a static one; every switch that
/// changes what's built, as `fennec_llvm::output_switches` names them; and the runtime library
/// linked in, and the runtime's bitcode a release puts in its unit, by the build each is stamped
/// with, or for a library from before stamps, its size and when it was built, since hashing it
/// whole would cost every run. See `docs/store.md`.
fn root_hash(
    units: &[fennec_llvm::Unit],
    (entry, llvm): (&str, &str),
    (runtime, bitcode): (Runtime, Option<&Path>),
    (release, statically): (bool, bool),
) -> (String, String) {
    // Each unit's hash was taken as its text was written, or kept with a unit used again.
    let functions: Vec<String> = units.iter().map(|unit| unit.hash.to_hex()).collect();
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec build root 2026-11-07");
    let baseline = BASELINE.load(std::sync::atomic::Ordering::Relaxed);
    // Every switch that changes what's built, so a program built one way is never handed back for
    // another: the sanitizer, checked proofs and stripped facts.
    let target = format!(
        "{}\n{}\n{}\n{}\n{release}\n{statically}\n{baseline}\n{}\n{llvm}\n",
        env!("FENNEC_BUILD"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::env::consts::OS,
        fennec_llvm::output_switches()
    );
    hasher.update(target.as_bytes());
    let library = runtime_library(runtime, statically).ok().into_iter().chain(bitcode.map(Path::to_path_buf));
    for path in library {
        // Which library, by its name, and the build of Fennec it's from, as its stamp says, so the
        // same library keys the same program on every machine and in every folder. The name tells
        // the runtime with threads from the one without, which one build of Fennec makes both of.
        // One from before stamps goes by where it is, how big and when it changed.
        if let Some(build) = stamp_of(&path) {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            hasher.update(format!("library {name} built from {build}\n").as_bytes());
            continue;
        }
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
    objects: Vec<fennec_llvm::Object>,
    /// How many functions there are.
    total: usize,
    /// How many of them weren't already in the cache.
    fresh: usize,
    /// How many were used again as kept, without writing their text (see [`emitted_reusing`]).
    reused: usize,
    /// The runtime the objects link against.
    runtime: Runtime,
    /// Whether the program makes requests, so it links what TLS needs.
    tls: bool,
    /// Whether it links on its own, against the runtime built for musl, which its runtime was
    /// chosen for, so it's linked that way too.
    statically: bool,
    /// Whether it's a release build, which is linked without the runtime's debug information.
    release: bool,
    /// The runtime's bitcode a release build put in its unit, when it's there: see
    /// [`runtime_bitcode`].
    bitcode: Option<PathBuf>,
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
    emitted_with(
        reached,
        if traced { &fennec_llvm::emit_traced } else { &fennec_llvm::emit_for },
        traced,
        None,
    )
}

/// What LLVM emits for a program: each function's text, and what they share.
type Emits = Result<(Vec<fennec_llvm::Unit>, fennec_llvm::Shared, fennec_llvm::Layouts), String>;

/// A trimmed program's text from one of LLVM's ways of emitting it. `tests` says it's a program of
/// tests, whose network is a fake.
fn emitted_with(
    reached: &fennec_syntax::Module,
    emit: &dyn Fn(&fennec_ir::Program, fennec_llvm::Cpu) -> Emits,
    tests: bool,
    checked: Option<fennec_check::Signatures>,
) -> Result<Emitted, String> {
    // Reading and writing JSON and TOML go through std's code in Fennec, where LLVM can take it.
    // So does sorting what has its own `<`. Finding that out checks the program, which is all
    // the checking it needs when nothing changes.
    let mut routed = reached.clone();
    match fennec_check::route_natively(&mut routed) {
        fennec_check::Routed::Changed => {
            if let Ok(emitted) = emitted_as_written(&routed, emit, tests, None) {
                return Ok(emitted);
            }
            emitted_as_written(reached, emit, tests, checked)
        }
        fennec_check::Routed::Unchanged(found) => {
            // Routing's own check of the cut module is exact, so it goes before what the build's
            // check handed on.
            emitted_as_written(reached, emit, tests, found.map(|found| *found).or(checked))
        }
    }
}

/// What LLVM emits for a trimmed program, with its calls as they are.
fn emitted_as_written(
    reached: &fennec_syntax::Module,
    emit: &dyn Fn(&fennec_ir::Program, fennec_llvm::Cpu) -> Emits,
    tests: bool,
    checked: Option<fennec_check::Signatures>,
) -> Result<Emitted, String> {
    let checked = match checked {
        // What the run's check found, which checking it again here would find: a dev build of
        // `fennec` checks it again anyway, to say so if it ever isn't.
        Some(checked) => {
            if cfg!(debug_assertions) && !fennec_check::leaves_out_a_kept_rule() {
                let again = fennec_check::check_to_emit(reached).ok();
                assert!(
                    again.as_ref().is_some_and(|again| again.agree_within(&checked)),
                    "a run's check found other signatures than a build's"
                );
            }
            checked
        }
        None => fennec_check::check_to_emit(reached)
            .map_err(|problems| format!("the program didn't check once trimmed: {problems:?}"))?,
    };
    // `parallel` runs on the runtime's threads, its work lifted for each range of positions.
    let mut lowered = fennec_ir::lower_with(
        reached,
        &checked,
        // A test's network is a fake, which answers a server's connections in order.
        fennec_ir::Options { outline_parallel: true, stop_on_failed_main: true, serve_in_order: tests },
    );
    if fennec_build::cancelled() {
        return Err(fennec_llvm::STOPPED.to_owned());
    }
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

/// The sources a release build carries, to show a failure in, named the same wherever the program
/// was built, so two release builds of one program are the same bytes, as a signed release has to
/// be to check by building it again. A dev build keeps the names as they were loaded, so a failure
/// in a program run by its whole path shows that whole path, as a person running it wants. A name written as a path from where
/// the command ran, as `examples/game.fen` or `src/main.fen`, is the same on every machine, and
/// stays. One written from the root of the disk, as a kit built by its whole path or a dependency
/// in the store is, says where the builder keeps things, and is named instead: a file of the kit
/// from the kit's folder, as `src/main.fen`; a stored dependency from the store's `kits/`, which
/// names each by its hash; any other file by its own name. Only names change, in the same order,
/// so every place in the program still points where it did.
fn portable_sources(program: &fennec_kit::Program) -> fennec_syntax::Sources {
    let root = program
        .kit_src
        .as_ref()
        .and_then(|(src, _)| src.parent())
        .and_then(|root| std::fs::canonicalize(root).ok());
    let mut portable = fennec_syntax::Sources::new();
    for (shown, text) in program.sources.files() {
        portable.add(portable_name(shown, root.as_deref()), text);
    }
    portable
}

/// What [`portable_sources`] names one file shown as `shown`.
fn portable_name(shown: &str, root: Option<&Path>) -> String {
    let path = Path::new(shown);
    if !path.is_absolute() {
        return shown.to_owned();
    }
    let Ok(real) = std::fs::canonicalize(path) else { return shown.to_owned() };
    let slashed = |parts: &[std::path::Component<'_>]| {
        parts.iter().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
    };
    if let Some(inside) = root.and_then(|root| real.strip_prefix(root).ok()) {
        return slashed(&inside.components().collect::<Vec<_>>());
    }
    let parts: Vec<std::path::Component<'_>> = real.components().collect();
    let stored = parts.windows(2).position(|pair| {
        let [kits, hash] = pair else { return false };
        let hash = hash.as_os_str().to_string_lossy();
        kits.as_os_str() == "kits" && hash.len() == 64 && hash.bytes().all(|digit| digit.is_ascii_hexdigit())
    });
    if let Some(from) = stored.and_then(|at| parts.get(at..)) {
        return slashed(from);
    }
    // A kit named by a path from this one, as `tools = { path = "../tools" }`, from the kit's
    // folder, as its `kit.toml` names it: `../tools/src/lib.fen`.
    if let Some(root) = root {
        let theirs: Vec<std::path::Component<'_>> = root.components().collect();
        let shared = parts.iter().zip(&theirs).take_while(|(ours, theirs)| ours == theirs).count();
        if shared > 1 {
            let up = theirs.len() - shared;
            let rest = slashed(parts.get(shared..).unwrap_or_default());
            return format!("{}{rest}", "../".repeat(up));
        }
    }
    real.file_name().map_or_else(|| shown.to_owned(), |name| name.to_string_lossy().into_owned())
}

/// The signatures' keys of what cutting `whole` down to `kept` let go: every key `whole` defines
/// that `kept` doesn't. That's each function's, with the functions written inside it, each type's,
/// with its methods, and each extension's methods.
fn cut_keys(
    whole: &fennec_syntax::Module,
    kept: &fennec_syntax::Module,
) -> std::collections::BTreeSet<String> {
    let still = defined_keys(kept);
    defined_keys(whole).into_iter().filter(|key| !still.contains(key)).collect()
}

/// The keys of what a module defines, as the checker keys their signatures: a function by its name,
/// a type by its name and a method as `Type.method`, an extension's method as a type's own, and a
/// function written inside one as `owner.inner`.
fn defined_keys(module: &fennec_syntax::Module) -> std::collections::BTreeSet<String> {
    use fennec_syntax::{Expr, ExprKind, FnDef, Item, Stmt, StmtKind};
    fn nested(def: &FnDef, owner: &str, keys: &mut std::collections::BTreeSet<String>) {
        let mut defs = Vec::new();
        collect_defs(&def.body, &mut defs);
        for inner in defs {
            let key = format!("{owner}.{}", inner.name);
            nested(inner, &key, keys);
            keys.insert(key);
        }
    }
    /// The arms of a `match` written as a value, which can hold a `def` of their own.
    fn arms<'a>(value: &'a Expr, defs: &mut Vec<&'a FnDef>) {
        if let ExprKind::Match { arms, .. } = &value.kind {
            for arm in arms {
                collect_defs(&arm.body, defs);
            }
        }
    }
    fn collect_defs<'a>(body: &'a [Stmt], defs: &mut Vec<&'a FnDef>) {
        for stmt in body {
            match &stmt.kind {
                StmtKind::Def(def) => defs.push(def),
                StmtKind::If { branches, otherwise } => {
                    for (_, block) in branches {
                        collect_defs(block, defs);
                    }
                    if let Some(block) = otherwise {
                        collect_defs(block, defs);
                    }
                }
                StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                    collect_defs(body, defs);
                }
                StmtKind::OnFailure { cleanup, body, .. } => {
                    collect_defs(cleanup, defs);
                    collect_defs(body, defs);
                }
                StmtKind::Lend { value, otherwise, .. } => {
                    arms(value, defs);
                    collect_defs(otherwise, defs);
                }
                StmtKind::Expr(value) | StmtKind::Assign { value, .. } | StmtKind::Return(Some(value)) => {
                    arms(value, defs);
                }
                _ => {}
            }
        }
    }
    fn methods(owner: &str, defs: &[FnDef], keys: &mut std::collections::BTreeSet<String>) {
        for method in defs {
            let key = format!("{owner}.{}", method.name);
            nested(method, &key, keys);
            keys.insert(key);
        }
    }
    let mut keys = std::collections::BTreeSet::new();
    for item in &module.items {
        match item {
            Item::Function(def) => {
                keys.insert(def.name.clone());
                nested(def, &def.name, &mut keys);
            }
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                keys.insert(def.name.clone());
                methods(&def.name, &def.methods, &mut keys);
            }
            Item::Extend(extension) => methods(&extension.name, &extension.methods, &mut keys),
            _ => {}
        }
    }
    keys
}

/// Lowers what the program reaches and compiles it.
fn llvm_objects(
    program: &fennec_kit::Program,
    release: bool,
    (checked, reusing): (Option<fennec_check::Signatures>, bool),
) -> Result<(Compiled, Vec<fennec_llvm::Unit>, Option<fennec_llvm::KnownUnder>), String> {
    // A release build leaves native blocks unwatched, paying nothing for it; a dev build tells
    // the runtime of each, for it to stop if it's ever given one.
    fennec_llvm::trust_native_blocks(release);
    // Nor does a release build use an analysis a build before it kept: it's the build that ships.
    fennec_llvm::use_kept_analyses(!release);
    let reached = main_reaches(&program.module);
    // Cutting it down only lets items go, so what was checked holds for what's left once what was
    // cut is let go of too.
    let checked = checked.map(|checked| checked.cut_to(&cut_keys(&program.module, &reached)));
    let (emitted, known) = if !release && reusing {
        emitted_reusing(program, &reached, checked)?
    } else {
        (emitted_with(&reached, &fennec_llvm::emit_for, false, checked)?, None)
    };
    // Asked to check what's kept, a build says how much of it it used, each checked against
    // working it out again: none, for a release build.
    if std::env::var("FENNEC_CHECK_WHOLE").as_deref() == Ok("1") {
        let (used, of) = fennec_llvm::analyses_used_again();
        eprintln!(
            "note: used {used} of {of} functions' analyses again, each checked against analyzing it again"
        );
    }
    let image_of = |image: &mut fennec_llvm::Image| {
        fennec_rt::embed::codec::Image {
            sources: if release { portable_sources(program) } else { program.sources.clone() },
            tables: std::mem::take(&mut image.tables),
            literals: image.literals.clone(),
        }
        .to_bytes()
    };
    let compiled = compile_emitted(&emitted, release, image_of, |lowered, shared, image, bytes| {
        fennec_llvm::entry(lowered, shared, image, bytes)
    })?;
    // Kept for compiling again any kept object a cleanup takes before the link, with what the
    // objects it found are known under.
    Ok((compiled, emitted.units, known))
}

/// A dev build's text, using again each unit the last build of this program kept whose function,
/// and everything it read, are as they were, so it isn't written or compiled again (edit builds,
/// stage 1). What's kept is one cache entry for the program, named by where its first file is,
/// written after every build. `FENNEC_CACHE_AUDIT=1` writes every unit used again anyway and
/// checks it comes out as it was kept. Without a cache, every unit is written, as always.
fn emitted_reusing(
    program: &fennec_kit::Program,
    reached: &fennec_syntax::Module,
    checked: Option<fennec_check::Signatures>,
) -> Result<(Emitted, Option<fennec_llvm::KnownUnder>), String> {
    let cache = fennec_build::fennec_home()
        .filter(|_| !crate::no_cache())
        .map(|home| fennec_build::Cache::new(&home));
    let first = program.sources.files().next().map(|(shown, _)| shown.to_owned());
    let named = first.and_then(|shown| std::fs::canonicalize(shown).ok());
    let (Some(cache), Some(named)) = (cache, named) else {
        return emitted_with(reached, &fennec_llvm::emit_for, false, checked).map(|emitted| (emitted, None));
    };
    let key = units_key(program, &named);
    let tools = fennec_llvm::Tools::find()?;
    // Every object the program held last time is marked used at once, by marking its list: the
    // units used again are found without marking each (`fennec_llvm::has_object`).
    cache.mark(fennec_build::HOLDS, &key);
    let (has_object, known) = fennec_llvm::has_object(&cache, &tools)?;
    later::say_failure();
    // An index the daemon hasn't written yet is the newest there is.
    let kept = later::pending(&key).map(|index| index.as_ref().clone()).or_else(|| cache.read(UNITS, &key));
    let reuse = std::cell::RefCell::new(fennec_llvm::Reuse {
        kept,
        has_object: &has_object,
        audit: std::env::var("FENNEC_CACHE_AUDIT").as_deref() == Ok("1"),
        keep: None,
    });
    let emit = |lowered: &fennec_ir::Program, cpu: fennec_llvm::Cpu| {
        fennec_llvm::emit_reusing(lowered, cpu, &mut reuse.borrow_mut())
    };
    let emitted = emitted_with(reached, &emit, false, checked)?;
    if let Some(index) = reuse.borrow_mut().keep.take() {
        // Every unit's object, as the next build may use any of them again: those compiled this
        // build are new, and so used now, till this list is marked.
        let holding = fennec_llvm::holding(&tools, &emitted.units);
        if fennec_llvm::keeps_between_builds()
            && let Ok(holding) = holding
        {
            later::write(&cache, &key, index, holding);
        } else {
            fennec_build::kept("keep what the build's units read", cache.write(UNITS, &key, &index));
            let held = holding.and_then(|holding| holding.hold(&cache, &key).map_err(|err| err.to_string()));
            fennec_build::kept("keep the program's list of objects", held.map_err(std::io::Error::other));
        }
    }
    Ok((emitted, known))
}

/// What names a program's index of kept units, and its list of objects, in the cache. For a kit,
/// its name and the entry's path inside its `src`, so every checkout and worktree of the kit,
/// wherever it is, starts from the units the last build of any of them kept; for a single file,
/// its absolute path. A kept unit is used again only when its function's IR and every read it made
/// are as they were, so an index another checkout kept is only ever somewhere to start, never
/// wrong: what differs between the two is written again.
fn units_key(program: &fennec_kit::Program, entry: &Path) -> String {
    let in_kit = program.manifest.as_ref().zip(program.kit_src.as_ref()).and_then(|(manifest, (src, _))| {
        let src = std::fs::canonicalize(src).ok()?;
        let inside = entry.strip_prefix(src).ok()?;
        // By its parts, so it's the same wherever `/` isn't what parts a path.
        let parts: Vec<String> =
            inside.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
        Some(format!("kit {}\n{}", manifest.name, parts.join("/")))
    });
    let named = in_kit.unwrap_or_else(|| format!("file {}", entry.display()));
    fennec_blake3::hash(named.as_bytes()).to_hex()
}

/// What an edit build keeps for the next, its index of units and its list of objects, written
/// once the build is made, on a thread of the daemon's own: an edit doesn't wait for 35 MB to be
/// written at 200,000 lines. A build that comes before the write is done uses the index waiting
/// to be written, which is the newest. A write a later one has overtaken isn't made, so the cache
/// ends with the last. One that's lost, as when the daemon is killed, costs the cache only being a
/// build behind, which costs reuse, never a wrong program: every unit's record says when it can be
/// used again, and each use is checked against it. A command without the daemon writes them
/// itself, before it ends.
pub(crate) mod later {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    /// One write: a program's index and its list of objects.
    struct Job {
        cache: fennec_build::Cache,
        key: String,
        index: Arc<Vec<u8>>,
        holding: fennec_llvm::Holding,
    }

    /// Each program's index waiting to be written, by its name in the cache.
    static PENDING: Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = Mutex::new(None);

    /// Where writes go, to the thread that makes them, started by the first.
    static QUEUE: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

    /// How many writes are waiting or being made.
    static WAITING: AtomicUsize = AtomicUsize::new(0);

    /// A write that failed, to be said by the next build: the thread can't say it itself, since
    /// what's printed while a build runs is that build's.
    static FAILED: Mutex<Option<(&'static str, std::io::Error)>> = Mutex::new(None);

    /// The index of the program the cache names `key` waiting to be written, if there's one.
    pub(crate) fn pending(key: &str) -> Option<Arc<Vec<u8>>> {
        PENDING.lock().ok()?.as_ref()?.get(key).cloned()
    }

    /// Says, in this build, a write that failed since the last.
    pub(crate) fn say_failure() {
        if let Some((what, err)) = FAILED.lock().ok().and_then(|mut failed| failed.take()) {
            fennec_build::kept(what, Err::<(), _>(err));
        }
    }

    /// Writes `index` and the list `holding` makes as the program `key`'s, on the writing thread.
    pub(crate) fn write(
        cache: &fennec_build::Cache,
        key: &str,
        index: Vec<u8>,
        holding: fennec_llvm::Holding,
    ) {
        let index = Arc::new(index);
        if let Ok(mut pending) = PENDING.lock() {
            pending.get_or_insert_with(HashMap::new).insert(key.to_owned(), Arc::clone(&index));
        }
        let job = Job { cache: cache.clone(), key: key.to_owned(), index, holding };
        WAITING.fetch_add(1, Ordering::SeqCst);
        let Ok(mut queue) = QUEUE.lock() else { return };
        let sender = queue.get_or_insert_with(|| {
            let (sender, jobs) = mpsc::channel::<Job>();
            std::thread::spawn(move || {
                for job in jobs {
                    made(job);
                    WAITING.fetch_sub(1, Ordering::SeqCst);
                }
            });
            sender
        });
        if let Err(mpsc::SendError(job)) = sender.send(job) {
            made(job);
            WAITING.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Makes one write, unless a later one for the same program has taken its place.
    fn made(job: Job) {
        let latest = |pending: &HashMap<String, Arc<Vec<u8>>>| {
            pending.get(&job.key).is_some_and(|newest| Arc::ptr_eq(newest, &job.index))
        };
        if !PENDING.lock().ok().and_then(|pending| pending.as_ref().map(latest)).unwrap_or(false) {
            return;
        }
        if let Err(err) = job.cache.write(super::UNITS, &job.key, &job.index) {
            failed("keep what the build's units read", err);
        } else if let Err(err) = job.holding.hold(&job.cache, &job.key) {
            failed("keep the program's list of objects", err);
        }
        if let Ok(mut pending) = PENDING.lock()
            && let Some(pending) = pending.as_mut()
            && latest(pending)
        {
            pending.remove(&job.key);
        }
    }

    fn failed(what: &'static str, err: std::io::Error) {
        if let Ok(mut failed) = FAILED.lock() {
            *failed = Some((what, err));
        }
    }

    /// Waits up to `most` for every write to be made, as the daemon does before it goes.
    #[cfg(unix)]
    pub(crate) fn finish(most: Duration) {
        let started = Instant::now();
        while WAITING.load(Ordering::SeqCst) > 0 && started.elapsed() < most {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The store's kind for each program's index of kept units (see [`emitted_reusing`]).
const UNITS: &str = "units";

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
    // A function too big to optimize in reasonable time is compiled as written, which keeps its
    // checks but runs slower, so the build says which.
    for name in fennec_llvm::huge_functions(units) {
        eprintln!(
            "hint: `{name}` is too big to optimize, so it's compiled as written and runs slower; split it into smaller functions to have it optimized"
        );
    }
    let mut image = fennec_llvm::Image::of(layouts, shared)?;
    let bytes = image_of(&mut image);
    let entry = entry_of(lowered, shared, &image, &bytes)?;
    let statically = links_statically();
    let runtime = Runtime::for_program(lowered, statically)?;
    let home = fennec_build::fennec_home();
    let cache = home.as_deref().filter(|_| !crate::no_cache()).map(fennec_build::Cache::new);
    // Bitcode from another build is refused before the store is looked in, so a program built
    // with other bitcode is never handed back, nor one built with it.
    let bitcode = if release { runtime_bitcode(runtime, statically)? } else { None };
    // A build that's been made before is in the store whole, under its root hash.
    let llvm = tools.version()?;
    let stored = cache.clone().map(|cache| {
        let (root, functions) =
            root_hash(units, (&entry, &llvm), (runtime, bitcode.as_deref()), (release, statically));
        let program = cache.read(PROGRAMS, &root);
        Stored { cache, root, functions, program }
    });
    if stored.as_ref().is_some_and(|stored| stored.program.is_some()) {
        return Ok(Compiled {
            objects: Vec::new(),
            total: units.len(),
            fresh: 0,
            reused: units.iter().filter(|unit| unit.text.is_empty()).count(),
            runtime,
            tls,
            statically,
            release,
            bitcode,
            stored,
        });
    }
    let work = crate::own_temp("llvm");
    let built = if release {
        fennec_llvm::compile_whole(&tools, units, &entry, &work, bitcode.as_deref())
    } else {
        fennec_llvm::compile(&tools, units, &entry, stored.as_ref().map(|stored| &stored.cache), &work)
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
        reused: units.iter().filter(|unit| unit.text.is_empty()).count(),
        runtime,
        tls,
        statically,
        release,
        bitcode,
        stored,
    })
}

/// Links object files with the runtime library, giving back how long linking took: all of it,
/// writing out the objects that aren't files already and clearing them away after, not only the
/// linker. Every object written and deleted again for each build was once most of a build after
/// a one-line edit on a Mac, and showed nowhere. A dev build's functions are the cache's own
/// files, read where they lie. When the runtime's Rust is in the objects already, only its C parts
/// are linked, so nothing of it is there twice.
fn link_objects(
    objects: &[&fennec_llvm::Object],
    output: &Path,
    runtime: Runtime,
    runtime_in: bool,
    tls: bool,
    (statically, release): (bool, bool),
) -> Result<std::time::Duration, String> {
    let runtime = runtime_library(runtime, statically)?;
    let started = Instant::now();
    let work = crate::own_temp("link");
    std::fs::create_dir_all(&work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let mut paths = Vec::with_capacity(objects.len());
    for (at, object) in objects.iter().enumerate() {
        match *object {
            fennec_llvm::Object::Kept(path) => paths.push(path.clone()),
            fennec_llvm::Object::Bytes(bytes) => {
                let path = work.join(format!("{at}.o"));
                std::fs::write(&path, bytes)
                    .map_err(|err| format!("couldn't write an object file: {err}"))?;
                paths.push(path);
            }
        }
    }
    // A program linked on its own holds the notices musl's license asks to go with it.
    if statically {
        paths.push(crate::notices::object(&work)?);
    }
    let runtime = if runtime_in { c_parts(&runtime, &work)? } else { runtime };
    // Linked under one name always, then moved where it goes: macOS's linker makes a program's
    // UUID, and the signature over it, from the name it's written to, so the same program built
    // to two names, as a recorded run and its replay are, came out 48 bytes apart.
    let linked_at = work.join(format!("program{}", std::env::consts::EXE_SUFFIX));
    let mut command = if statically {
        static_linker(&paths, &runtime, &linked_at, release)
    } else {
        let mut command = linker(&paths, &runtime, &linked_at, tls);
        if release {
            command.arg(NO_DEBUG_INFO);
        }
        command
    };
    // `FENNEC_LINK_KEEP` keeps the objects linked, in the folder it names, for a test to link them
    // another way and compare.
    let kept = std::env::var_os("FENNEC_LINK_KEEP").map(PathBuf::from);
    if kept.is_some() && cfg!(target_os = "macos") {
        // And told to keep every piece of every object, where it is, so each section is whole.
        let args: Vec<std::ffi::OsString> = command
            .get_args()
            .filter(|arg| *arg != "-Wl,-dead_strip")
            .map(std::ffi::OsStr::to_os_string)
            .collect();
        command = Command::new(command.get_program());
        // The runtime's code no program calls is kept then too, and it needs these.
        command.args(args).args(["-framework", "Security", "-framework", "CoreFoundation"]);
    }
    let linked = command.output();
    if let Some(kept) = kept {
        keep_objects(&paths, &kept)?;
    }
    let moved = if linked.as_ref().is_ok_and(|linked| linked.status.success()) {
        moved_into_place(&linked_at, output)
    } else {
        Ok(())
    };
    let _ = std::fs::remove_dir_all(&work);
    let took = started.elapsed();
    moved.map_err(|err| format!("couldn't put the program at {}: {err}", output.display()))?;
    let linked = linked.map_err(|err| {
        if cfg!(target_os = "macos") && err.kind() == std::io::ErrorKind::NotFound {
            NO_XCODE_TOOLS.to_owned()
        } else {
            format!("couldn't run the linker: {err}")
        }
    })?;
    if !linked.status.success() {
        return Err(link_failure(&linked));
    }
    made_new(output);
    Ok(took)
}

/// Moves a program just linked to where it goes: renamed, or copied where it's on another disk,
/// with its permissions, as a copy keeps them.
fn moved_into_place(linked: &Path, output: &Path) -> std::io::Result<()> {
    if std::fs::rename(linked, output).is_ok() {
        return Ok(());
    }
    std::fs::copy(linked, output).map(|_| ())
}

/// Adds `path` to the file `FENNEC_NEW_EXECUTABLES` names, a line each, when it's set. It's called
/// for every executable made as a new file, linked or written from the store's bytes. One linked
/// out of the store is the same file as one launched before, so it isn't counted. macOS can hold a
/// new file's first launch while it's assessed, so the gate counts how many it makes.
fn made_new(path: &Path) {
    use std::io::Write as _;
    let Some(log) = std::env::var_os("FENNEC_NEW_EXECUTABLES") else { return };
    // One short line, appended, so lines from programs built at once never mix.
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(file, "{}", path.display());
    }
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

/// Copies the objects a link used into `folder`, by their place in the link, as `0.o` and on.
fn keep_objects(paths: &[PathBuf], folder: &Path) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|err| format!("couldn't make {}: {err}", folder.display()))?;
    // Named by where each is in the link, as the cache's files aren't.
    for (at, path) in paths.iter().enumerate() {
        // A copy of a kept object is read-only as the object is, so one an earlier build left
        // here goes first.
        let kept = folder.join(format!("{at}.o"));
        let _ = std::fs::remove_file(&kept);
        std::fs::copy(path, &kept).map_err(|err| format!("couldn't keep {}: {err}", path.display()))?;
    }
    Ok(())
}

/// What has the linker leave the runtime's debug information out of a release program: on Linux and
/// Windows it's most of a small program's file, 5 MB of a hello world's 6.3. Nothing a release
/// program says needs it: a failure, and the calls it came through, are named from the program's own
/// table of where in its source each call is. A dev build keeps it, for a debugger. On a Mac the
/// linker leaves DWARF in the object files anyway; this leaves out the table that points to them.
#[cfg(target_os = "macos")]
const NO_DEBUG_INFO: &str = "-Wl,-S";
#[cfg(all(not(target_os = "macos"), not(windows)))]
const NO_DEBUG_INFO: &str = "-Wl,--strip-debug";
#[cfg(windows)]
const NO_DEBUG_INFO: &str = "--strip-debug";

/// What a Mac's `cc` is told to link for: the architecture this `fennec` builds programs for. On
/// its own, `cc` links for the one it runs as, and since macOS 27.2.1, whose Xcode has `clang`
/// and `ld` for arm64 only, that's arm64 even when an x86-64 `fennec` starts it under Rosetta.
#[cfg(target_os = "macos")]
const LINKS_FOR: &[&str] = &["-arch", if cfg!(target_arch = "x86_64") { "x86_64" } else { "arm64" }];

/// Elsewhere, `cc` links for the machine it runs on, which is the one `fennec` builds for.
#[cfg(all(not(target_os = "macos"), not(windows)))]
const LINKS_FOR: &[&str] = &[];

/// The system's linker, set to link objects with a runtime library into an executable. On a Mac
/// that's `cc` from Xcode's command line tools, which a Mac program links against.
#[cfg(not(windows))]
fn linker(objects: &[PathBuf], runtime: &Path, output: &Path, tls: bool) -> Command {
    let mut command = Command::new("cc");
    command.args(LINKS_FOR);
    name_objects(&mut command, objects, output);
    command
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
fn static_linker(objects: &[PathBuf], runtime: &Path, output: &Path, release: bool) -> Command {
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
    command.arg(musl.join("crt1.o")).arg(musl.join("crti.o")).arg(musl.join("crtbegin.o"));
    name_objects(&mut command, objects, output);
    command
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
    // As `NO_DEBUG_INFO` says.
    if release {
        command.arg(format!("{pass}--strip-debug"));
    }
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
        // A PE header holds the time it was linked unless told not to, so the same program linked
        // a second later would be different bytes.
        .arg("--no-insert-timestamp")
        .arg("-L")
        .arg(&libraries)
        .arg(libraries.join("crt2.o"));
    name_objects(&mut command, objects, output);
    command.arg(runtime).args(WINDOWS_LIBRARIES).arg("-o").arg(output);
    command
}

/// Names the objects a linker links, in a file it reads with `@`. A command line holds only so
/// much: 32,767 characters on Windows, and on macOS and Linux as much as `ARG_MAX` allows, about a
/// megabyte. A program built in many parts names more objects than that, as a kit's tests do that
/// reach 14,000 functions. `cc` reads the file on macOS and Linux, and hands its linker one of its
/// own when the line it runs it with would be too long. LLD reads it everywhere. Where the file
/// can't be written, they go on the command line as before.
fn name_objects(command: &mut Command, objects: &[PathBuf], output: &Path) {
    match output.parent().and_then(|folder| response_file(objects, folder)) {
        Some(file) => command.arg(format!("@{}", file.display())),
        None => command.args(objects),
    };
}

/// A file naming each object on a line of its own, quoted, in `folder`, where the link writes its
/// program: `None` when there are none, or it couldn't be written. The objects may be the cache's
/// own files, so it's never written beside them. LLD and `cc` read a backslash in one as an escape:
/// on Windows each becomes `/`, which it takes in a path as well, and elsewhere, where one can
/// be part of a name, it's escaped, as a quote is.
fn response_file(objects: &[PathBuf], folder: &Path) -> Option<PathBuf> {
    use std::fmt::Write as _;
    objects.first()?;
    let file = folder.join("objects.rsp");
    let mut named = String::new();
    for object in objects {
        let path = object.display().to_string();
        let path = if cfg!(windows) { path.replace('\\', "/") } else { path.replace('\\', "\\\\") };
        let _ = writeln!(named, "\"{}\"", path.replace('"', "\\\""));
    }
    std::fs::write(&file, named).ok()?;
    Some(file)
}

/// Where an executable goes when no `-o` says: named after the kit, or the file, in the
/// current folder.
pub(crate) fn default_output(path: &str, program: &fennec_kit::Program) -> PathBuf {
    let name = program
        .manifest
        .as_ref()
        .map(|manifest| manifest.name.clone())
        .or_else(|| Path::new(path).file_stem().map(|stem| stem.to_string_lossy().into_owned()));
    PathBuf::from(name.filter(|name| !name.is_empty() && name != ".").unwrap_or_else(|| "main".to_owned()))
}

/// The runtime's bitcode, next to the `fennec` command, if it was built: see
/// `Runtime::bitcode`. Bitcode that isn't stamped as from this build of Fennec is refused, as a
/// runtime library from another build is. Unlike a library's, its stamp is never missing: every
/// build of it has had one, and bitcode from other source links without a word and gives wrong
/// answers.
fn runtime_bitcode(runtime: Runtime, statically: bool) -> Result<Option<PathBuf>, String> {
    let path = runtime_folder(statically)?.join(runtime.bitcode());
    if !path.is_file() {
        return Ok(None);
    }
    match stamp_of(&path) {
        Some(build) if build == env!("FENNEC_BUILD") => Ok(Some(path)),
        stamp => Err(format!(
            "the runtime's bitcode `{}` is {} this `fennec`: make it again from the same checkout, as `.github/workflows/release.yml` does, or take it away to link the runtime library without it",
            path.display(),
            if stamp.is_some() {
                "from another build of Fennec than"
            } else {
                "stamped with no build of Fennec, so it may not match"
            }
        )),
    }
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

/// The build a runtime library's bytes are stamped with: the 64 hex digits after its marker, in
/// an archive, where the stamp's value is as written; or in bitcode, which packs that into bits,
/// after the name of the static holding it, as the runtime's `build.rs` writes it.
fn stamp_in(bytes: &[u8]) -> Option<String> {
    let after = |marker: &[u8]| {
        let at = bytes.windows(marker.len()).position(|window| window == marker)? + marker.len();
        let hex = std::str::from_utf8(bytes.get(at..at + 64)?).ok()?;
        hex.bytes().all(|digit| digit.is_ascii_hexdigit()).then(|| hex.to_ascii_lowercase())
    };
    // Each marker is written in two, so this command's own file never holds one.
    after(["fennec-runtime-", "build:"].concat().as_bytes())
        .or_else(|| after(["RUNTIME_BUILD_", "STAMP_"].concat().as_bytes()))
}

/// The build of Fennec a runtime library is from, as its stamp says, or `None` when it has none,
/// like one from before stamps. Reading a library whole takes a while, so what it says is kept in the
/// store by the file's path, size and when it changed, and in the toolchain's store too, which every
/// home reads (see `toolchain`).
fn stamp_of(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let changed = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let name =
        fennec_blake3::hash(format!("{}\n{}\n{changed}", path.display(), metadata.len()).as_bytes()).to_hex();
    let cache = crate::run::cache();
    let kept = cache
        .as_ref()
        .and_then(|cache| cache.read(RUNTIME_STAMPS, &name))
        .or_else(|| crate::toolchain::read(RUNTIME_STAMPS, &name));
    if let Some(kept) = kept {
        return String::from_utf8(kept).ok().filter(|build| !build.is_empty());
    }
    let build = stamp_in(&std::fs::read(path).ok()?);
    let stamp = build.as_deref().unwrap_or("").as_bytes();
    if let Some(cache) = &cache {
        // A store that can't be written only means reading the library again next time.
        fennec_build::kept(
            "keep which build a runtime library is",
            cache.write(RUNTIME_STAMPS, &name, stamp),
        );
    }
    crate::toolchain::keep(RUNTIME_STAMPS, &name, stamp);
    build
}

/// Where which build each runtime library is from is kept, by its path, size and changed time.
const RUNTIME_STAMPS: &str = "runtime-stamps";

#[cfg(test)]
mod tests {
    /// A build's root hash changes with the version of LLVM that compiles its program, so a
    /// program kept before LLVM's tools changed is never handed back after, and with nothing
    /// else here: the same build with the same LLVM has the same root.
    #[test]
    fn a_root_hash_changes_with_the_llvm_that_compiles_it() {
        let units = [fennec_llvm::Unit::written("f".to_owned(), "define i64 @f() {}".to_owned())];
        let root = |llvm: &str| {
            super::root_hash(&units, ("entry", llvm), (super::Runtime::Plain, None), (false, false)).0
        };
        assert_eq!(root("LLVM version 21.1.0"), root("LLVM version 21.1.0"));
        assert_ne!(root("LLVM version 21.1.0"), root("LLVM version 21.1.1"));
    }

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
        // Bitcode, where only the name of the static is as written, next to other names.
        let bitcode = [
            b"BC\xc0\xde\x00_RNvCs329Xtag7HA8_14fennec_runtime84RUNTIME_BUILD_STAMP_".as_slice(),
            build.to_ascii_uppercase().as_bytes(),
            b"fennec_fail",
        ]
        .concat();
        assert_eq!(super::stamp_in(&bitcode).as_deref(), Some(build));
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

    /// A program in more parts than a command line can name links, as a kit's tests that reach
    /// 14,000 functions do: objects whose paths come to twice the `ARG_MAX` this system allows,
    /// a megabyte or two on macOS and Linux. They go to `cc` in a file. Named on the command line,
    /// running the linker failed with "Argument list too long".
    #[test]
    #[cfg(not(windows))]
    fn a_program_of_more_objects_than_a_command_line_holds_links() {
        use std::process::Command;
        let work = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-many-objects-{}", std::process::id())),
        );
        // A quote and a backslash, which the file escapes, and spaces, which it quotes.
        let parts =
            work.join(format!("{}\"quoted\" and back\\slashed", "a folder with a long name, ".repeat(4)));
        std::fs::create_dir_all(&parts).unwrap();
        let compile = |name: &str, text: &str| {
            let (source, object) = (work.join(format!("{name}.c")), work.join(format!("{name}.o")));
            std::fs::write(&source, text).unwrap();
            let built = Command::new("cc").arg("-c").arg(&source).arg("-o").arg(&object).output().unwrap();
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            object
        };
        let main = compile("main", "int main(void) { return 7; }\n");
        let empty = compile("empty", "/* Nothing: a part with no code. */\n");
        // Twice what this system lets a command's arguments be, however long the temporary
        // folder's name: Linux's `/tmp` is short, so a fixed count fell short there.
        let most = Command::new("getconf")
            .arg("ARG_MAX")
            .output()
            .ok()
            .and_then(|asked| String::from_utf8_lossy(&asked.stdout).trim().parse::<usize>().ok())
            .unwrap_or(2 << 20);
        let (mut objects, mut named) = (Vec::new(), 0);
        while named <= 2 * most {
            let object = parts.join(format!("part {}.o", objects.len()));
            std::fs::hard_link(&empty, &object).unwrap();
            named += object.as_os_str().len() + 1;
            objects.push(object);
        }
        assert!(named > 2 * most, "{named} bytes of names, more than the {most} a command line holds");
        let output = work.join("linked");
        let mut linking = super::linker(&objects, &main, &output, false);
        let line: usize = linking.get_args().map(|arg| arg.len() + 1).sum();
        assert!(line < 64 << 10, "{line} bytes on the command line");
        let linked = linking.output().unwrap();
        assert!(linked.status.success(), "{}", String::from_utf8_lossy(&linked.stderr));
        assert_eq!(Command::new(&output).status().unwrap().code(), Some(7));
        std::fs::remove_dir_all(&work).unwrap();
    }

    /// The objects a linker is given go in a file, each quoted on a line of its own, so a
    /// program in many parts doesn't make a command line longer than the system takes. The file
    /// is in the link's own folder, never beside the objects, which may be the cache's.
    #[test]
    fn the_objects_to_link_go_in_a_file_each_on_a_line() {
        let work = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-response-file-{}", std::process::id())),
        );
        std::fs::create_dir_all(&work).unwrap();
        let elsewhere = work.join("cache");
        let objects: Vec<_> = (0..2_000).map(|at| elsewhere.join(format!("{at}.o"))).collect();
        let file = super::response_file(&objects, &work).unwrap();
        assert_eq!(file, work.join("objects.rsp"));
        let named = std::fs::read_to_string(&file).unwrap();
        assert_eq!(named.lines().count(), 2_000);
        let first = objects[0].display().to_string();
        let first = if cfg!(windows) { first.replace('\\', "/") } else { first.replace('\\', "\\\\") };
        assert_eq!(named.lines().next(), Some(format!("\"{first}\"").as_str()));
        if cfg!(windows) {
            assert!(!named.contains('\\'), "a backslash would be read as an escape");
        }
        assert!(named.len() > 32_767, "a file this size wouldn't fit on a command line");
        assert_eq!(super::response_file(&[], &work), None);
        std::fs::remove_dir_all(&work).unwrap();
    }
}

#[cfg(test)]
mod packed {
    /// A build of a program with more files than there are slots says so, naming how many have
    /// a slot and how many are packed; one with fewer says nothing.
    #[test]
    fn a_build_says_when_files_are_packed_past_the_slots() {
        let mut sources = fennec_syntax::Sources::new();
        for at in 0..6144 {
            sources.add(format!("f{at}.fen"), "x\n");
        }
        assert_eq!(super::packed_note(&sources), None);
        sources.add("over.fen", "x\n");
        sources.add("after.fen", "x\n");
        let note = super::packed_note(&sources).unwrap_or_default();
        assert!(
            note.contains("the first 6144 files each start at a slot of their own, and the 2 after them"),
            "{note}"
        );
    }
}
