//! `fennec build`: every example compiles to a standalone executable that prints exactly
//! its `.out` file, and every example that fails while running fails the same way as an
//! executable, with the interpreter's message.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The runtime library executables link against, which `fennec build` expects next to
/// itself. `cargo test` doesn't build it, so this does, every time: an old one would be
/// missing what the interpreter has learned since. Cargo does nothing when it's current.
fn ensure_runtime() -> std::io::Result<bool> {
    let profile = if cfg!(debug_assertions) { None } else { Some("--release") };
    let built = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "-p", "fennec-runtime"])
        .args(profile)
        .status()?;
    Ok(built.success())
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

/// Builds a program into `work`, giving back the executable, or `None` when it doesn't
/// build because checking it fails.
fn build(path: &Path, work: &Path) -> std::io::Result<Option<PathBuf>> {
    let executable = work.join(path.file_stem().unwrap_or_default());
    let built = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("build")
        .arg(path)
        .arg("-o")
        .arg(&executable)
        .output()?;
    Ok(built.status.success().then_some(executable))
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

#[test]
#[cfg_attr(windows, ignore = "`fennec build` doesn't link executables on Windows yet")]
fn examples_build_to_executables_that_print_their_expected_output() {
    if under_mutants() {
        return;
    }
    assert!(ensure_runtime().unwrap(), "couldn't build the runtime library");
    let work = work_dir("examples").unwrap();
    for path in programs(&examples_dir()).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let executable = build(&path, &work).unwrap().unwrap_or_else(|| panic!("{name} didn't build"));
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
    assert!(ensure_runtime().unwrap(), "couldn't build the runtime library");
    let work = work_dir("errors").unwrap();
    let mut built = 0;
    for path in programs(&examples_dir().join("errors")).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // Most error examples are mistakes the checker catches, which don't build.
        let Some(executable) = build(&path, &work).unwrap() else { continue };
        built += 1;
        let ran = run(&executable).unwrap();
        let interpreted = Command::new(env!("CARGO_BIN_EXE_fennec")).arg("run").arg(&path).output().unwrap();
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
