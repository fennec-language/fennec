//! `fennec run`: checks a program, then runs its `main` as native code. That's the cached dev
//! build LLVM makes, as `fennec build` does, compiled a function at a time so an edit
//! recompiles only what it touched. Where LLVM can't build the program yet, it says why.

use std::process::ExitCode;

/// How a program runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    /// The LLVM dev build, after the tests a change affects.
    Native,
    /// As `Native`, without running the tests first.
    Untested,
}

/// A run recorded into a log, or replayed from one (see `fennec_rt::replay`).
pub(crate) enum Logged {
    Recording(std::path::PathBuf),
    Replaying(std::path::PathBuf),
}

impl Logged {
    /// What tells an executable this run starts to record or replay.
    fn variable(&self) -> (&'static str, &std::path::Path) {
        match self {
            Logged::Recording(log) => ("FENNEC_RECORD", log),
            Logged::Replaying(log) => ("FENNEC_REPLAY", log),
        }
    }
}

/// Runs the program at `path`, handing `main` the `arguments` and this process's input.
pub(crate) fn run(path: &str, engine: Engine, arguments: &[String]) -> ExitCode {
    run_logged(path, engine, arguments, None)
}

/// `run`, recording the program's outside inputs, or replaying them. A replay runs no tests: its
/// program's run is the one that was recorded, which ran them first.
pub(crate) fn run_logged(
    path: &str,
    engine: Engine,
    arguments: &[String],
    logged: Option<&Logged>,
) -> ExitCode {
    let replaying = matches!(logged, Some(Logged::Replaying(_)));
    let tested = engine == Engine::Native && !replaying;
    let executable = crate::own_temp("run").with_extension(std::env::consts::EXE_EXTENSION);
    // Made by the build daemon when there's one to be had, as `fennec build` is, and started here,
    // so the program has this process's input, output, signals and exit.
    let made =
        crate::daemon::run(path, &executable, tested).unwrap_or_else(|| prepared(path, tested, &executable));
    if let Err(code) = made {
        return code;
    }
    match started(&executable, path, arguments, logged) {
        Ok(code) => code,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Makes the program at `path` at `executable`, ready to start, after the tests a change affects
/// if `tested`, and shows the lint errors it has. Or else how the command ends, once it's said why:
/// a test failed, or the program doesn't check or can't be built.
pub(crate) fn prepared(path: &str, tested: bool, executable: &std::path::Path) -> Result<(), ExitCode> {
    if tested && let Some(failed) = crate::test::before_a_run(path) {
        return Err(failed);
    }
    // What the linter finds doesn't stop a run, so iteration stays fast, but it's shown first, on
    // standard error. It's found while the program is built, which takes as long or longer.
    crate::lint::find_errors_beside(path);
    let (program, checked, kept) = reached(path).inspect_err(|_| crate::lint::show_errors_found())?;
    // What LLVM can't take yet, or a tool or library that isn't there, is said as `fennec build`
    // says it.
    match built((program, checked, kept.as_deref()), path, executable) {
        Ok(None) => {
            crate::lint::show_errors_found();
            Ok(())
        }
        Ok(Some(stopped)) => Err(stopped),
        Err(why) => {
            crate::lint::show_errors_found();
            eprintln!("error: {why}");
            Err(ExitCode::FAILURE)
        }
    }
}

/// What a run knows of its program's check.
enum Checked {
    /// Checked and resolved for a build, with what the check found, when that's known.
    Resolved(Option<Box<fennec_check::Signatures>>),
    /// A kept image, the program as it was checked when it was kept: its files are the ones it was
    /// kept from, so it checks the same. It's checked again, which resolves it, only if it has to
    /// be built.
    Kept,
}

/// Builds the program to `executable` through LLVM's cached dev build. Some code when it's to stop
/// instead, having said why.
fn built(
    (mut program, checked, kept): (fennec_kit::Program, Checked, Option<&str>),
    path: &str,
    executable: &std::path::Path,
) -> Result<Option<ExitCode>, String> {
    crate::kept::say_analyses_notice();
    // A program the same as a run before had, kept as an image, starts from what that run built,
    // with nothing lowered or compiled to find it again: never one built before the build daemon
    // last found an analysis it kept wrong (`kept::analyses_found_wrong`).
    let started_from = kept.zip(cache()).map(|(kept, cache)| {
        let name = format!(
            "{}.root",
            fennec_blake3::hash(
                format!(
                    "{kept} {} {} {}",
                    env!("FENNEC_BUILD"),
                    crate::aot::dev_build_settings(),
                    crate::kept::analyses_found_wrong()
                )
                .as_bytes()
            )
            .to_hex()
        );
        (cache, name)
    });
    // A count of analyses found wrong that can't be read names nothing that can be trusted.
    let trusted = crate::kept::analyses_found_wrong() != u64::MAX;
    let ready = trusted
        && started_from.as_ref().is_some_and(|(cache, name)| {
            cache
                .read(RUN_ROOTS, name)
                .and_then(|root| String::from_utf8(root).ok())
                .is_some_and(|root| crate::aot::stored_executable(cache, &root, executable).is_some())
        });
    if !ready {
        let checked = match checked {
            Checked::Resolved(signatures) => signatures.map(|signatures| *signatures),
            Checked::Kept => match fennec_check::check_to_build(&mut program.module) {
                Ok(signatures) => signatures,
                // A kept image that doesn't check as it did is passed over for the files.
                Err(_) => match cache().map(|cache| loaded(path, &cache)) {
                    Some(Ok((loaded, Checked::Resolved(signatures), _))) => {
                        program = loaded;
                        signatures.map(|signatures| *signatures)
                    }
                    Some(Err(code)) => return Ok(Some(code)),
                    _ => return Err("its kept image couldn't be checked again".to_owned()),
                },
            },
        };
        let built = build_for_run((&program, checked), path, executable)?;
        if let (Some((cache, name)), Some(root)) = (&started_from, &built.root) {
            // A store that can't be written only means building it again next time.
            fennec_build::kept(
                "keep which program a run built",
                cache.write(RUN_ROOTS, name, root.as_bytes()),
            );
        }
    }
    Ok(None)
}

/// Where the root of what a kept image built is kept, by the image and what else the build read.
const RUN_ROOTS: &str = "run-roots";

/// Builds the program to `executable` through LLVM's cached dev build.
/// What the run's check worked out, when it's what a build of the program would check again.
fn build_for_run(
    (program, checked): (&fennec_kit::Program, Option<fennec_check::Signatures>),
    path: &str,
    executable: &std::path::Path,
) -> Result<crate::aot::Built, String> {
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
            dependency_files: std::collections::HashSet::new(),
        };
        &whole
    } else {
        program
    };
    crate::aot::executable_checked(program, executable, false, checked)
}

/// Runs what was built at `executable` as the program at `path`, then lets go of it.
fn started(
    executable: &std::path::Path,
    path: &str,
    arguments: &[String],
    logged: Option<&Logged>,
) -> Result<ExitCode, String> {
    let mut command = std::process::Command::new(executable);
    // `args.program` is the program it ran, not where it was built to for this run.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::arg0(&mut command, path);
    #[cfg(not(unix))]
    command.env("FENNEC_RUN_AS", format!("{}|{path}", executable.display()));
    if let Some(logged) = logged {
        let (variable, log) = logged.variable();
        command.env(variable, log);
    }
    command.args(arguments);
    // On Unix it becomes the program, so a signal sent to `fennec run`, like a supervisor's
    // SIGTERM, reaches the program itself, which can stop cleanly, and its exit, a signal's
    // included, is `fennec run`'s. What was built is swept up by a later run instead.
    // `FENNEC_RUN_WAITS` has it start the program and wait for it instead, for a tool that counts
    // what a run does: Valgrind loses what a process did before it became another.
    #[cfg(unix)]
    if std::env::var_os("FENNEC_RUN_WAITS").is_none() {
        use std::io::Write as _;
        use std::os::unix::process::CommandExt as _;
        sweep_now_and_then(executable);
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let failed = command.exec();
        let _ = std::fs::remove_file(executable);
        return Err(format!("the program it built didn't start: {failed}"));
    }
    // On Windows the program shares the console, so Ctrl-C reaches it as it reaches this.
    // This lets it go by, so it waits for the program to stop, and its exit code is the
    // program's, not a Ctrl-C's.
    #[cfg(windows)]
    let _ = ctrlc::set_handler(|| {});
    let ran = command.status();
    let _ = std::fs::remove_file(executable);
    let status = ran.map_err(|err| format!("the program it built didn't start: {err}"))?;
    Ok(exit_code_of(status))
}

/// The exit code `fennec run` passes on for the program it waited for: the program's own, when
/// it's one a process can give back, 0 to 255. Any other status means the program didn't end by
/// giving one back, and it's said on standard error, as its system names it, with a code of 1.
///
/// A crash on Windows ends a process with an exception's code, which no exit code holds, and
/// Windows itself prints nothing; a signal ends one on Unix with no code at all. Passed on as a
/// plain 1 with nothing said, a crash read as the program failing without a word.
fn exit_code_of(status: std::process::ExitStatus) -> ExitCode {
    if let Some(code) = status.code().and_then(|code| u8::try_from(code).ok()) {
        return ExitCode::from(code);
    }
    eprintln!("error: {}", ended_how(status));
    ExitCode::FAILURE
}

/// How a program that gave back no exit code a process can pass on ended, in words.
fn ended_how(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            let dumped = if status.core_dumped() { ", and dumped its core" } else { "" };
            return format!(
                "the program it built was ended by signal {signal}{}{dumped}",
                signal_name(signal)
            );
        }
    }
    match status.code() {
        // An exception's code on Windows, an NTSTATUS, written as Windows writes them.
        Some(code) => {
            let code = code.cast_unsigned();
            format!("the program it built stopped with exception {code:#010X}{}", exception_name(code))
        }
        None => format!("the program it built ended with no exit code: {status}"),
    }
}

/// The name of a signal that ends a program, after its number, for the ones a crash or a stop
/// sends: nothing for the rest.
#[cfg(unix)]
fn signal_name(signal: i32) -> &'static str {
    // SIGBUS's number is the one that differs between the two.
    let bus = if cfg!(target_os = "macos") { 10 } else { 7 };
    match signal {
        2 => " (SIGINT)",
        4 => " (SIGILL)",
        6 => " (SIGABRT)",
        8 => " (SIGFPE)",
        9 => " (SIGKILL)",
        11 => " (SIGSEGV)",
        13 => " (SIGPIPE)",
        15 => " (SIGTERM)",
        _ if signal == bus => " (SIGBUS)",
        _ => "",
    }
}

/// What a Windows exception that ends a program is, after its code, for the ones a crash ends one
/// with: nothing for the rest.
fn exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => ", an access violation",
        // What Rust's `abort` raises on Windows.
        0xC000_0409 => ", a fast fail, as an abort raises",
        0xC000_00FD => ", a stack overflow",
        0xC000_0374 => ", a corrupted heap",
        0xC000_001D => ", an illegal instruction",
        0xC000_0094 => ", an integer divided by zero",
        0xC000_013A => ", a Ctrl-C",
        0x8000_0003 => ", a breakpoint",
        _ => "",
    }
}

/// How old what an earlier `fennec run` built has to be before a run sweeps it up: far longer than
/// a run takes from building its program to starting it, so another run's is never taken from it
/// in between. One still running can go: on Unix a running program keeps its file until it ends.
#[cfg(unix)]
const SWEPT_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// How long after one sweep the next waits: a sweep looks at every file beside the program, which
/// costs each run about 50 thousand instructions a file, and an agent running a program in a loop,
/// or a machine running many tests, leaves a thousand there. Each run then only looks at when the
/// last sweep was, and a file is never left there much past `SWEPT_AFTER`.
#[cfg(unix)]
const SWEPT_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// The file whose time says when the last sweep was, beside the programs runs built. It isn't a
/// `fennec-run-` file, so no sweep takes it.
#[cfg(unix)]
const LAST_SWEPT: &str = "fennec-swept";

/// Sweeps what earlier runs built, beside `executable`, unless that was done in the last
/// `SWEPT_EVERY`.
#[cfg(unix)]
fn sweep_now_and_then(executable: &std::path::Path) {
    let Some(folder) = executable.parent() else { return };
    let marker = folder.join(LAST_SWEPT);
    let recent = std::fs::metadata(&marker)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|swept| swept.elapsed().ok())
        .is_some_and(|since| since < SWEPT_EVERY);
    if recent {
        return;
    }
    // Noted before sweeping, so runs that start meanwhile leave it to this one. A marker that
    // can't be written only means the next run sweeps too.
    let _ = std::fs::write(&marker, b"");
    sweep_old_runs(executable);
}

/// Removes what earlier runs built and started, beside `executable`, once they're old enough.
#[cfg(unix)]
fn sweep_old_runs(executable: &std::path::Path) {
    let Some(folder) = executable.parent() else { return };
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(fennec_build::RUN_PREFIX));
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > SWEPT_AFTER);
        if ours && old && path != executable {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Loads and checks all of a program.
fn whole(path: &str) -> Result<(fennec_kit::Program, fennec_check::Signatures), ExitCode> {
    let mut program = crate::load(path, fennec_kit::Entry::Main).ok_or(ExitCode::FAILURE)?;
    let files = crate::verdict::kit_files(path, None);
    refused_as_a_whole(path, None, files.as_deref(), &program)?;
    match crate::copy_rule::check_to_run(&mut program.module, &program.sources) {
        Ok(signatures) => Ok((program, signatures)),
        Err(diagnostics) => Err(reject(&program, &diagnostics)),
    }
}

/// A program checked and cut down to what `main` reaches, so that starting it doesn't grow
/// with everything else in its kit. All of it is checked the first time, and again whenever
/// its text changes; the cache remembers the text that passed. The cut-down program is what
/// always runs, so its machine code is reused from the very first run.
/// With the program, the signatures a build of it would work out, when its check gave them.
fn reached(path: &str) -> Result<(fennec_kit::Program, Checked, Option<String>), ExitCode> {
    let Some(cache) = cache() else {
        return whole(path).map(|(program, _)| (program, Checked::Resolved(None), None));
    };
    // A kept image is checked again only when the run has to build it: a program the store has,
    // by a key that holds the image's own, needs nothing more from it.
    if let Some((program, kept)) = crate::image::load(path, &cache) {
        crate::lint::lend_loaded(None, path);
        return Ok((program, Checked::Kept, Some(kept)));
    }
    loaded(path, &cache)
}

/// The program at `path` loaded from its files, checked when its text is new, cut down to what
/// `main` reaches and kept as an image.
fn loaded(
    path: &str,
    cache: &fennec_build::Cache,
) -> Result<(fennec_kit::Program, Checked, Option<String>), ExitCode> {
    let cache = cache.clone();
    let files = crate::image::files_now(path, &cache);
    let loaded = crate::load(path, fennec_kit::Entry::Main).map(std::sync::Arc::new);
    // The lint errors are found from this load, when it has every file they'd look at.
    crate::lint::lend_loaded(loaded.as_ref(), path);
    let program = loaded.ok_or(ExitCode::FAILURE)?;
    let name = passed_name(path);
    let embedded = program.embedded_texts();
    let embedded = embedded.iter().map(|(path, text)| (path.as_str(), fennec_blake3::hash(text.as_bytes())));
    // Every file of a kit counts, loaded or not, since one `main` doesn't import can still make a
    // name or a case mean something else to a whole load.
    let kit_files = files.iter().flatten().map(|file| (file.module.as_str(), file.text));
    let text = fingerprint(program.sources.hashed_files().chain(embedded).chain(kit_files));
    if cache.read("checked", &name).is_none_or(|passed| passed != text.as_bytes()) {
        // An edit that only changed functions' bodies is checked on its own, which is all a
        // change there can break, rather than checking all of a big kit again.
        let quickly = files.as_deref().and_then(|files| crate::verdict::checked_quickly(path, &cache, files));
        let facts = match quickly {
            Some(Err((partial, diagnostics))) => return Err(reject(&partial, &diagnostics)),
            Some(Ok(facts)) => facts,
            None => {
                let kit = refused_as_a_whole(path, Some(&cache), files.as_deref(), &program)?;
                match crate::copy_rule::check_rules(&mut program.module.clone(), &program.sources) {
                    Ok(signatures) => crate::verdict::KitFacts::of(kit, &signatures),
                    Err(diagnostics) => return Err(reject(&program, &diagnostics)),
                }
            }
        };
        if let Some(files) = &files {
            crate::verdict::remember_checked(path, &cache, (files, &program), &facts);
        }
        // A cache that can't be written only means checking all of it again next time.
        fennec_build::kept("keep the program's check", cache.write("checked", &name, text.as_bytes()));
    }
    // What `main` reaches is copied out, rather than the rest cut away, since the lint errors may
    // still be being found from all of it. The whole program is let go of beside the run.
    let loaded = program;
    let mut program = loaded.with_module(fennec_build::what_main_reaches(&loaded.module));
    std::thread::spawn(move || drop(loaded));
    // The image keeps the constants as worked out while compiling, so a cached run skips that.
    if let Err(diagnostics) = crate::copy_rule::bake(&mut program.module) {
        return Err(reject(&program, &diagnostics));
    }
    // The hash the next run finds the image by, so what this run builds from it is noted for it.
    let kept = if let Some(files) = &files {
        crate::image::store(path, &cache, files, &program)
    } else if let Some(files) = crate::verdict::loaded_files(&program) {
        crate::image::store(path, &cache, &files, &program)
    } else {
        None
    };
    match fennec_check::check_to_build(&mut program.module) {
        Ok(signatures) => Ok((program, Checked::Resolved(signatures.map(Box::new)), kept)),
        // Cutting it down missed something the whole program has.
        Err(_) => whole(path).map(|(program, _)| (program, Checked::Resolved(None), None)),
    }
}

/// What the kit at `path` defines, once a run that loaded only `main` and what it imports is
/// held to what a whole load refuses. It may read a body differently from a whole load, through
/// a name another file defines or a case two enums share, and then the whole kit is checked, so
/// a run accepts only what `fennec check` does.
fn refused_as_a_whole(
    path: &str,
    cache: Option<&fennec_build::Cache>,
    files: Option<&[fennec_build::KitFile]>,
    program: &fennec_kit::Program,
) -> Result<crate::verdict::KitNames, ExitCode> {
    let Some(files) = files.filter(|files| !crate::verdict::has_every_file(path, program, files)) else {
        return Ok(crate::verdict::KitNames::of(&program.module.items));
    };
    let kit = crate::verdict::KitNames::of_kit(path, cache, files, program);
    let items = crate::verdict::kit_items(program, files);
    if kit.as_ref().is_none_or(|kit| kit.may_differ(&items, program)) {
        let everything = crate::load(path, fennec_kit::Entry::Everything).ok_or(ExitCode::FAILURE)?;
        if let Err(diagnostics) =
            crate::copy_rule::check_rules(&mut everything.module.clone(), &everything.sources)
        {
            return Err(reject(&everything, &diagnostics));
        }
    }
    Ok(kit.unwrap_or_else(|| crate::verdict::KitNames::of(&program.module.items)))
}

/// The cache, unless `FENNEC_NO_CACHE` is set.
pub(crate) fn cache() -> Option<fennec_build::Cache> {
    fennec_build::fennec_home().filter(|_| !crate::no_cache()).map(|home| fennec_build::Cache::new(&home))
}

/// A program's name in the cache: a hash of its absolute path.
pub(crate) fn passed_name(path: &str) -> String {
    format!("{}.bin", fennec_blake3::hash(absolute(path).as_bytes()).to_hex())
}

/// A program's absolute path, which is what names it in the cache.
pub(crate) fn absolute(path: &str) -> String {
    std::fs::canonicalize(path).map_or_else(|_| path.to_owned(), |path| path.display().to_string())
}

/// A hash of every file of a program, by name and the hash of its text, and of which build of
/// Fennec read them, since another build's checker may find what this one doesn't. A loaded
/// program's texts were hashed as they were parsed, so they aren't read through again here.
pub(crate) fn fingerprint<'t>(
    files: impl Iterator<Item = (&'t str, fennec_blake3::Hash)>,
) -> fennec_blake3::Hash {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec build cache 2026-11-10 passed check");
    hasher.update(env!("FENNEC_BUILD").as_bytes());
    for (path, text) in files {
        hasher.update(&path.len().to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(text.as_bytes());
    }
    hasher.finalize()
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

/// Checks a program to build, refusing what `fennec check` and a run refuse, as they check it:
/// all of the program, and of the standard library what it reaches, since the library's own tests
/// check the rest. A build works its constants out when the program starts. Returns whether it
/// passed. Tools that report on a program use `check`, since what they report may be why.
pub(crate) fn check_to_build(program: &mut fennec_kit::Program) -> bool {
    checked_to_build(program).is_ok()
}

/// `check_to_build`, giving back what the check found when it passed, if that's what checking
/// the program as it's left would find: the build then lowers it without checking it again.
pub(crate) fn checked_to_build(
    program: &mut fennec_kit::Program,
) -> Result<Option<fennec_check::Signatures>, ExitCode> {
    crate::copy_rule::check_rules_to_build(&mut program.module, &program.sources)
        .map_err(|diagnostics| reject(program, &diagnostics))
}

/// Prints the problems a check found.
pub(crate) fn reject(program: &fennec_kit::Program, diagnostics: &[fennec_syntax::Diagnostic]) -> ExitCode {
    // Lint errors being found for a run come before what stops it.
    crate::lint::show_errors_found();
    let told: Vec<_> = diagnostics.iter().map(|diagnostic| with_extension(program, diagnostic)).collect();
    eprint!("{}", program.sources.render_all(&told));
    ExitCode::FAILURE
}

/// A problem, saying which file has a missing method when a file the run didn't load has it. A run
/// loads `main` and what it imports, so nothing else brings in a file that only extends a type.
/// One in a dependency offers no fix: no tool changes another kit.
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
    let mut told = match found {
        Some(help) => diagnostic.clone().with_help(help),
        None => diagnostic.clone(),
    };
    program.no_fixes_in_dependencies(std::slice::from_mut(&mut told));
    told
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, SystemTime};

    /// A run whose program is the store's, kept hours ago, has it put out with this run's time,
    /// so another run's sweep, which goes by that time, leaves it while it waits to start it. What
    /// an earlier run started long ago still goes.
    #[test]
    fn a_program_about_to_start_outlasts_another_runs_sweep() {
        let dir =
            fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-swept-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let cache = fennec_build::Cache::new(&dir.join("home"));
        let hours_ago = SystemTime::now() - Duration::from_hours(4);
        // A first run built it and kept it, hours ago; its own file was swept up since.
        let first = work.join("fennec-run-1-a-0");
        std::fs::write(&first, b"a program").unwrap();
        cache.keep_file("programs", "root", &first).unwrap();
        std::fs::remove_file(&first).unwrap();
        let kept = cache.kept_program("programs", "root").unwrap();
        std::fs::File::open(&kept).unwrap().set_modified(hours_ago).unwrap();
        // A second run puts it out, and is about to start it, when a third, about to start its
        // own, sweeps.
        let second = work.join("fennec-run-2-b-0");
        assert!(cache.link_out("programs", "root", &second).unwrap());
        super::sweep_old_runs(&work.join("fennec-run-3-c-0"));
        assert!(second.exists(), "the sweep took a program about to start");
        // One an earlier run started long ago goes.
        let old = work.join("fennec-run-4-d-0");
        std::fs::write(&old, b"another program").unwrap();
        std::fs::File::open(&old).unwrap().set_modified(hours_ago).unwrap();
        super::sweep_old_runs(&work.join("fennec-run-3-c-0"));
        assert!(!old.exists() && second.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A run sweeps only when the last sweep was a while ago, so with a thousand programs left
    /// beside it, it looks at one file, not a thousand; once it's a while, it sweeps them all.
    #[test]
    fn a_run_sweeps_only_now_and_then() {
        let dir =
            fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-sweeps-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let hours_ago = SystemTime::now() - Duration::from_hours(4);
        let old = |name: &str| {
            let path = work.join(name);
            std::fs::write(&path, b"a program").unwrap();
            std::fs::File::open(&path).unwrap().set_modified(hours_ago).unwrap();
            path
        };
        let first = old("fennec-run-1-a-0");
        super::sweep_now_and_then(&work.join("fennec-run-9-z-0"));
        assert!(!first.exists(), "the first run sweeps");
        let second = old("fennec-run-2-b-0");
        super::sweep_now_and_then(&work.join("fennec-run-9-z-0"));
        assert!(second.exists(), "a run just after one that swept doesn't");
        let marker = work.join(super::LAST_SWEPT);
        let a_while_ago = SystemTime::now() - super::SWEPT_EVERY - Duration::from_secs(1);
        std::fs::File::options().write(true).open(&marker).unwrap().set_modified(a_while_ago).unwrap();
        super::sweep_now_and_then(&work.join("fennec-run-9-z-0"));
        assert!(!second.exists() && marker.exists(), "a run a while after one that swept does");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod exits {
    /// What a program gave back as an exit code a process can give is passed on, and nothing's
    /// said.
    #[cfg(unix)]
    #[test]
    fn an_exit_code_a_process_can_give_is_the_programs_own() {
        use std::os::unix::process::ExitStatusExt as _;
        for code in [0, 2, 255] {
            let status = std::process::ExitStatus::from_raw(code << 8);
            assert_eq!(
                super::exit_code_of(status),
                std::process::ExitCode::from(u8::try_from(code).unwrap())
            );
        }
    }

    /// A program a signal ended is said to be, by the signal's number and name, and whether it
    /// dumped its core.
    #[cfg(unix)]
    #[test]
    fn a_program_a_signal_ended_is_said_to_be() {
        use std::os::unix::process::ExitStatusExt as _;
        let said = |raw| super::ended_how(std::process::ExitStatus::from_raw(raw));
        assert_eq!(said(9), "the program it built was ended by signal 9 (SIGKILL)");
        assert_eq!(
            said(0x0B | 0x80),
            "the program it built was ended by signal 11 (SIGSEGV), and dumped its core"
        );
        assert_eq!(said(31), "the program it built was ended by signal 31");
        assert_eq!(
            super::exit_code_of(std::process::ExitStatus::from_raw(9)),
            std::process::ExitCode::FAILURE
        );
    }

    /// A program a crash ended on Windows is said to have stopped with the exception's code, as
    /// Windows writes it, and what it is.
    #[cfg(windows)]
    #[test]
    fn a_program_a_crash_ended_is_said_to_have_stopped_with_its_exception() {
        use std::os::windows::process::ExitStatusExt as _;
        let said = |raw| super::ended_how(std::process::ExitStatus::from_raw(raw));
        assert_eq!(
            said(0xC000_0005),
            "the program it built stopped with exception 0xC0000005, an access violation"
        );
        assert_eq!(
            said(0xC000_0409),
            "the program it built stopped with exception 0xC0000409, a fast fail, as an abort raises"
        );
        assert_eq!(said(0xE06D_7363), "the program it built stopped with exception 0xE06D7363");
        let crashed = std::process::ExitStatus::from_raw(0xC000_0005);
        assert_eq!(super::exit_code_of(crashed), std::process::ExitCode::FAILURE);
        assert_eq!(
            super::exit_code_of(std::process::ExitStatus::from_raw(3)),
            std::process::ExitCode::from(3)
        );
    }
}
