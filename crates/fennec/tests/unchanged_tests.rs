//! `fennec test` runs only what a change affects, reporting the rest as unchanged.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SOURCE: &str = "def double(x: int) -> int:\n    x * 2\n\ndef triple(x: int) -> int:\n    x * 3\n\n\
    test \"doubling\":\n    assert double(2) == 4\n\ntest \"tripling\":\n    assert triple(2) == 6\n";

/// A folder for a program and for Fennec's own files, with `SOURCE` written in it.
fn scratch(name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-unchanged-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("game.fen"), SOURCE)?;
    Ok(dir)
}

/// Runs `fennec test` on the program, with the cache in `dir` and the environment of a
/// person's machine rather than CI's.
fn run_tests(dir: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("test")
        .args(args)
        .arg(dir.join("game.fen"))
        .env("FENNEC_HOME", dir.join("home"))
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

#[test]
fn only_the_tests_a_change_affects_run_again() {
    let dir = scratch("affected").unwrap();
    let first = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&first), "  ✓ doubling\n  ✓ tripling\n2 passed, 0 failed\n");

    let again = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&again), "2 tests: 0 run, 2 unchanged\n2 passed, 0 failed\n");

    let reformatted = format!("# A comment.\n\n{SOURCE}");
    std::fs::write(dir.join("game.fen"), reformatted).unwrap();
    let comment = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&comment), "2 tests: 0 run, 2 unchanged\n2 passed, 0 failed\n");

    std::fs::write(dir.join("game.fen"), SOURCE.replace("x * 2", "x + x")).unwrap();
    let edited = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&edited), "  ✓ doubling\n2 tests: 1 run, 1 unchanged\n2 passed, 0 failed\n");

    let all = run_tests(&dir, &["--all"]).unwrap();
    assert_eq!(stdout(&all), "  ✓ doubling\n  ✓ tripling\n2 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failing_test_runs_every_time_and_a_break_is_never_hidden() {
    let dir = scratch("failing").unwrap();
    run_tests(&dir, &[]).unwrap();

    std::fs::write(dir.join("game.fen"), SOURCE.replace("x * 2", "x * 5")).unwrap();
    let broken = run_tests(&dir, &[]).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert_eq!(stdout(&broken), "  ✗ doubling\n2 tests: 1 run, 1 unchanged\n1 passed, 1 failed\n");

    let still = run_tests(&dir, &[]).unwrap();
    assert_eq!(still.status.code(), Some(1));
    assert_eq!(stdout(&still), "  ✗ doubling\n2 tests: 1 run, 1 unchanged\n1 passed, 1 failed\n");

    // Going back to code that passed still has to run, since the failure replaced its result.
    std::fs::write(dir.join("game.fen"), SOURCE).unwrap();
    let fixed = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&fixed), "  ✓ doubling\n2 tests: 1 run, 1 unchanged\n2 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ci_runs_every_test() {
    let dir = scratch("ci").unwrap();
    run_tests(&dir, &[]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("test")
        .arg(dir.join("game.fen"))
        .env("FENNEC_HOME", dir.join("home"))
        .env("CI", "true")
        .env_remove("FENNEC_NO_CACHE")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), "  ✓ doubling\n  ✓ tripling\n2 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_change_in_another_file_of_a_kit_reaches_tests_through_aliases() {
    let dir = scratch("kit").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x * 2\n").unwrap();
    std::fs::write(dir.join("src/other.fen"), "def one() -> int:\n    1\n").unwrap();
    let tests = "from maths import double as twice\nimport maths as numbers\nfrom other import one\n\n\
        test \"by alias\":\n    assert twice(2) == 4\n\ntest \"by module\":\n    assert numbers.double(1) == 2\n\n\
        test \"elsewhere\":\n    assert one() == 1\n";
    std::fs::write(dir.join("src/main.fen"), tests).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_fennec"))
            .arg("test")
            .arg(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("CI")
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    assert!(stdout(&run()).ends_with("3 passed, 0 failed\n"));
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + x\n").unwrap();
    let edited = stdout(&run());
    assert_eq!(edited, "  ✓ by alias\n  ✓ by module\n3 tests: 2 run, 1 unchanged\n3 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}
