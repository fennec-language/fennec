//! `fennec build`: every example compiles to a standalone executable that prints exactly
//! its `.out` file, and every example that fails while running fails the same way as an
//! executable, with the interpreter's message.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// A `fennec` to start, with the runtime library built first. `fennec run` links against that
/// library, and one left from before a rebase links without a word and gets things wrong: a
/// dict's layout that no longer matches, say, printing zeroes. So nothing starts `fennec` until
/// the library is current. Mutation testing and Windows don't link programs, and a test that
/// needs the library says so itself when it couldn't be built.
pub(crate) fn fennec_command() -> Command {
    if !cfg!(windows) && !under_mutants() {
        runtime_built();
    }
    Command::new(env!("CARGO_BIN_EXE_fennec"))
}

/// The runtime library executables link against, which `fennec build` expects next to
/// itself. `cargo test` doesn't build it, so this does, once a run: an old one would be
/// missing what the interpreter has learned since. Only once, because even when it's
/// current Cargo puts the library in place again, removing it first, so a test linking
/// while another builds it could find it missing. `llvm.rs` uses this too.
pub(crate) fn runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    *BUILT.get_or_init(|| {
        let profile = if cfg!(debug_assertions) { None } else { Some("--release") };
        Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "fennec-runtime"])
            .args(profile)
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// The runtime library with `threads`, which `fennec build` takes for a program that uses
/// `parallel` when `FENNEC_THREADS_RUNTIME=1` asks, next to `fennec` as
/// `libfennec_runtime_threads.a`. It's built in a folder of its own, since Cargo puts the same
/// file name in place for both, once a run as `runtime_built` does.
pub(crate) fn threads_runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    *BUILT.get_or_init(|| {
        let Some(folder) = Path::new(env!("CARGO_BIN_EXE_fennec")).parent() else { return false };
        // `target/threads`, beside the plain build's `target/debug`, as CI builds it.
        let Some(threads) = folder.parent().map(|target| target.join("threads")) else { return false };
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        let built = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "fennec-runtime", "--features", "threads", "--target-dir"])
            .arg(&threads)
            .args((profile == "release").then_some("--release"))
            .status()
            .is_ok_and(|status| status.success());
        let library = threads.join(profile).join("libfennec_runtime.a");
        built && fs::copy(library, folder.join("libfennec_runtime_threads.a")).is_ok()
    })
}

/// Whether mutation testing is running the tests (CI's mutation jobs set
/// `FENNEC_UNDER_MUTANTS`). Building the runtime library and linking every example takes
/// longer than its time limit for a whole run, so these tests stand aside there. Every
/// other run of the tests, CI's test jobs included, runs them.
fn under_mutants() -> bool {
    std::env::var_os("FENNEC_UNDER_MUTANTS").is_some()
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn programs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Builds a program into `work`, giving back the executable, or what `fennec build` said
/// when it doesn't build.
fn build(path: &Path, work: &Path) -> std::io::Result<Result<PathBuf, Output>> {
    let executable = work.join(path.file_stem().unwrap_or_default());
    let built = fennec_command().arg("build").arg(path).arg("-o").arg(&executable).output()?;
    Ok(if built.status.success() { Ok(executable) } else { Err(built) })
}

fn run(executable: &Path) -> std::io::Result<Output> {
    Command::new(executable).output()
}

/// A fresh folder for this test's executables.
fn work_dir(test: &str) -> std::io::Result<PathBuf> {
    let work = std::env::temp_dir().join(format!("fennec-executables-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    Ok(work)
}

/// Examples that can't be built to executables yet, because the JIT doesn't take a type they
/// use: `shared` types, for now. Each must still fail to build, so it comes off when it builds.
const NOT_BUILT: &[&str] = &["shared_accounts.fen"];

#[test]
#[cfg_attr(windows, ignore = "`fennec build` doesn't link executables on Windows yet")]
fn examples_build_to_executables_that_print_their_expected_output() {
    if under_mutants() {
        return;
    }
    assert!(runtime_built(), "couldn't build the runtime library");
    let work = work_dir("examples").unwrap();
    for path in programs(&examples_dir()).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if NOT_BUILT.contains(&name.as_str()) {
            assert!(build(&path, &work).unwrap().is_err(), "{name} builds now: take it off `NOT_BUILT`");
            continue;
        }
        let executable = build(&path, &work).unwrap().unwrap_or_else(|built| {
            panic!("{name} didn't build ({}):\n{}", built.status, String::from_utf8_lossy(&built.stderr))
        });
        let ran = run(&executable).unwrap();
        let expected = fs::read_to_string(path.with_extension("out")).unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), expected, "{name} printed something different");
        assert!(ran.status.success(), "{name} failed:\n{}", String::from_utf8_lossy(&ran.stderr));
    }
    let _ = fs::remove_dir_all(&work);
}

#[test]
#[cfg_attr(windows, ignore = "`fennec build` doesn't link executables on Windows yet")]
fn failing_examples_fail_the_same_way_as_executables() {
    if under_mutants() {
        return;
    }
    assert!(runtime_built(), "couldn't build the runtime library");
    let work = work_dir("errors").unwrap();
    let mut built = 0;
    for path in programs(&examples_dir().join("errors")).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // Most error examples are mistakes the checker catches, which don't build.
        let Ok(executable) = build(&path, &work).unwrap() else { continue };
        built += 1;
        let ran = run(&executable).unwrap();
        // `fennec run` runs an executable itself where LLVM builds the program, so the
        // interpreter is what an executable's failure is held to.
        let interpreted = fennec_command().args(["run", "--interp"]).arg(&path).output().unwrap();
        assert!(!ran.status.success(), "{name} should fail");
        assert_eq!(
            String::from_utf8_lossy(&ran.stderr),
            String::from_utf8_lossy(&interpreted.stderr),
            "{name} failed differently"
        );
        assert_eq!(ran.stdout, interpreted.stdout, "{name} printed something different before failing");
    }
    assert!(built > 0, "no error example built");
    let _ = fs::remove_dir_all(&work);
}
