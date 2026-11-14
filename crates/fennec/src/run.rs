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
    if engine == Engine::Native
        && !replaying
        && let Some(failed) = crate::test::before_a_run(path)
    {
        return failed;
    }
    // What the linter finds doesn't stop a run, so iteration stays fast, but it's shown first, on
    // standard error. It's found while the program is built, which takes as long or longer.
    crate::lint::find_errors_beside(path);
    let (program, checked, kept) = match reached(path) {
        Ok(prepared) => prepared,
        Err(code) => {
            crate::lint::show_errors_found();
            return code;
        }
    };
    // What LLVM can't take yet, or a tool or library that isn't there, is said as `fennec build`
    // says it.
    match built_and_run((&program, checked, kept.as_deref()), path, arguments, logged) {
        Ok(code) => code,
        Err(why) => {
            crate::lint::show_errors_found();
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Builds the program through LLVM's cached dev build and runs it, handing it the arguments,
/// this process's input and output, and its exit code.
fn built_and_run(
    (program, checked, kept): (&fennec_kit::Program, Option<fennec_check::Signatures>, Option<&str>),
    path: &str,
    arguments: &[String],
    logged: Option<&Logged>,
) -> Result<ExitCode, String> {
    let executable = crate::own_temp("run").with_extension(std::env::consts::EXE_EXTENSION);
    // A program the same as a run before had, kept as an image, starts from what that run built,
    // with nothing lowered or compiled to find it again.
    let started_from = kept.zip(cache()).map(|(kept, cache)| {
        let name = format!(
            "{}.root",
            fennec_blake3::hash(
                format!("{kept} {} {}", env!("FENNEC_BUILD"), crate::aot::dev_build_settings()).as_bytes()
            )
            .to_hex()
        );
        (cache, name)
    });
    let ready = started_from.as_ref().is_some_and(|(cache, name)| {
        cache
            .read(RUN_ROOTS, name)
            .and_then(|root| String::from_utf8(root).ok())
            .is_some_and(|root| crate::aot::stored_executable(cache, &root, &executable).is_some())
    });
    if !ready {
        let built = build_for_run((program, checked), path, &executable)?;
        if let (Some((cache, name)), Some(root)) = (&started_from, &built.root) {
            // A store that can't be written only means building it again next time.
            fennec_build::kept(
                "keep which program a run built",
                cache.write(RUN_ROOTS, name, root.as_bytes()),
            );
        }
    }
    crate::lint::show_errors_found();
    started(&executable, path, arguments, logged)
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
        sweep_old_runs(executable);
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
    Ok(status.code().and_then(|code| u8::try_from(code).ok()).map_or(ExitCode::FAILURE, ExitCode::from))
}

/// How old what an earlier `fennec run` built has to be before a run sweeps it up: far longer than
/// a run takes from building its program to starting it, so another run's is never taken from it
/// in between. One still running can go: on Unix a running program keeps its file until it ends.
#[cfg(unix)]
const SWEPT_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

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
            .is_some_and(|name| name.starts_with("fennec-run-"));
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
fn reached(
    path: &str,
) -> Result<(fennec_kit::Program, Option<fennec_check::Signatures>, Option<String>), ExitCode> {
    let Some(cache) = cache() else {
        return whole(path).map(|(program, _)| (program, None, None));
    };
    if let Some((mut program, kept)) = crate::image::load(path, &cache)
        && let Ok(signatures) = fennec_check::check_to_build(&mut program.module)
    {
        crate::lint::lend_loaded(None, path);
        return Ok((program, signatures, Some(kept)));
    }
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
    if let Some(files) = &files {
        crate::image::store(path, &cache, files, &program);
    }
    match fennec_check::check_to_build(&mut program.module) {
        Ok(signatures) => Ok((program, signatures, None)),
        // Cutting it down missed something the whole program has.
        Err(_) => whole(path).map(|(program, _)| (program, None, None)),
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
    match crate::copy_rule::check_rules(&mut program.module, &program.sources) {
        Ok(_) => true,
        Err(diagnostics) => {
            reject(program, &diagnostics);
            false
        }
    }
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
        let kept = cache.path("programs", "root");
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
}
