//! `fennec run` checks all of a program the first time and only what `main` reaches while its
//! text stays the same, which must never change what a program does or what's rejected.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SOURCE: &str = "def used() -> int:\n    41 + 1\n\ndef unused() -> int:\n    2\n\n\
    def main(stdout: Stdout):\n    stdout.print(used())\n";

/// A folder for a program and for Fennec's own files, with `SOURCE` written in it.
fn scratch(name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-reached-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("game.fen"), SOURCE)?;
    Ok(dir)
}

/// Runs a program with the cache in `home` and the environment of a person's machine.
fn run(home: &Path, program: &Path) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("run")
        .arg(program)
        .env("FENNEC_HOME", home)
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

#[test]
fn a_mistake_main_doesnt_reach_stops_the_run_even_after_a_run_that_passed() {
    let dir = scratch("mistake").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    for _ in 0..2 {
        assert_eq!(stdout(&run(&home, &game).unwrap()), "42\n");
    }
    std::fs::write(&game, SOURCE.replace("    2\n", "    \"two\"\n")).unwrap();
    for _ in 0..2 {
        let rejected = run(&home, &game).unwrap();
        assert!(!rejected.status.success());
        assert_eq!(stdout(&rejected), "");
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("error"));
    }
    std::fs::write(&game, SOURCE).unwrap();
    assert_eq!(stdout(&run(&home, &game).unwrap()), "42\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_runs_is_what_the_whole_program_does_in_every_example() {
    let dir = scratch("examples").unwrap();
    let projects = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/projects");
    let mut compared = 0;
    for entry in std::fs::read_dir(&projects).unwrap() {
        let kit = entry.unwrap().path();
        if !kit.join("kit.toml").is_file() {
            continue;
        }
        let expected = std::fs::read_to_string(kit.with_extension("out"))
            .or_else(|_| std::fs::read_to_string(kit.join("expected.out")))
            .unwrap()
            .replace("\r\n", "\n");
        for _ in 0..2 {
            assert_eq!(stdout(&run(&dir.join("home"), &kit).unwrap()), expected, "{}", kit.display());
        }
        compared += 1;
    }
    assert!(compared > 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

const TESTED: &str = "def double(x: int) -> int:\n    x * 2\n\ntest \"doubling\":\n    assert double(2) == 4\n\n\
    def main(stdout: Stdout):\n    stdout.print(double(21))\n";

#[test]
fn a_failing_test_stops_the_run_until_its_fixed_or_skipped() {
    let dir = scratch("tested").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    std::fs::write(&game, TESTED).unwrap();
    let passing = run(&home, &game).unwrap();
    assert_eq!(stdout(&passing), "42\n", "the tests print nothing when they pass");
    assert_eq!(String::from_utf8_lossy(&passing.stderr), "");

    std::fs::write(&game, TESTED.replace("x * 2", "x * 3")).unwrap();
    for _ in 0..2 {
        let stopped = run(&home, &game).unwrap();
        assert_eq!(stopped.status.code(), Some(1));
        assert_eq!(stdout(&stopped), "", "the program doesn't run");
        let said = String::from_utf8_lossy(&stopped.stderr).into_owned();
        assert!(said.contains("✗ doubling") && said.contains("--no-test"), "{said}");
    }

    let skipped = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(["run", "--no-test"])
        .arg(&game)
        .env("FENNEC_HOME", &home)
        .env_remove("FENNEC_NO_CACHE")
        .output()
        .unwrap();
    assert_eq!(stdout(&skipped), "63\n");

    std::fs::write(&game, TESTED).unwrap();
    assert_eq!(stdout(&run(&home, &game).unwrap()), "42\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_kits_tests_run_first_even_in_files_main_doesnt_import() {
    let dir = scratch("kit_tested").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n").unwrap();
    std::fs::write(dir.join("src/other.fen"), "test \"other\":\n    assert 1 == 1\n").unwrap();
    let home = dir.join("home");
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "1\n");
    std::fs::write(dir.join("src/other.fen"), "test \"other\":\n    assert 1 == 2\n").unwrap();
    let stopped = run(&home, &dir).unwrap();
    assert_eq!(stopped.status.code(), Some(1));
    assert_eq!(stdout(&stopped), "");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_interpreter_and_the_benchmarks_flag_dont_run_tests() {
    let dir = scratch("flags").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    std::fs::write(&game, TESTED.replace("x * 2", "x * 3")).unwrap();
    for flag in ["--interp", "--jit"] {
        let ran = Command::new(env!("CARGO_BIN_EXE_fennec"))
            .args(["run", flag])
            .arg(&game)
            .env("FENNEC_HOME", &home)
            .output()
            .unwrap();
        assert_eq!(stdout(&ran), "63\n", "{flag}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_kit_started_from_what_it_kept_behaves_the_same() {
    let dir = scratch("image").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest).unwrap();
    let lib = |value: &str| format!("def value() -> int:\n    {value}\n");
    std::fs::write(dir.join("src/lib.fen"), lib("1")).unwrap();
    let main = "from lib import value\n\ndef main(stdout: Stdout):\n    stdout.print(value())\n    stdout.print([1][value()])\n";
    std::fs::write(dir.join("src/main.fen"), main).unwrap();
    let home = dir.join("home");
    let shown = |output: &Output| String::from_utf8_lossy(&output.stderr).into_owned();
    let first = run(&home, &dir).unwrap();
    assert_eq!(first.status.code(), Some(1));
    assert!(shown(&first).contains("src/main.fen"), "{}", shown(&first));
    for _ in 0..2 {
        let again = run(&home, &dir).unwrap();
        assert_eq!(stdout(&again), stdout(&first));
        assert_eq!(shown(&again), shown(&first), "a failure points at the source just the same");
    }
    // Long enough that a file's time can be told from when it was noted.
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    assert_eq!(stdout(&run(&home, &dir).unwrap()), stdout(&first));
    let noted = run(&home, &dir).unwrap();
    assert_eq!(shown(&noted), shown(&first), "started without reading anything");

    std::fs::write(dir.join("src/lib.fen"), lib("0")).unwrap();
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "0\n1\n", "an edit is seen");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A parameter named like a constant is the parameter inside its function, in every engine. Native
/// code writes constants out where they're used, which mustn't reach a name that only looks like
/// one. The linter's `naming-case` keeps examples from doing this, so it's checked here.
#[test]
fn a_parameter_named_like_a_constant_is_the_parameter() {
    let source = "LIMIT = 10\n\ndef shadow(LIMIT: int) -> int:\n    LIMIT + 1\n\n\
                  def main(stdout: Stdout):\n    stdout.print(shadow(1), LIMIT)\n";
    let dir = scratch("shadowed_constant").unwrap();
    let game = dir.join("game.fen");
    std::fs::write(&game, source).unwrap();
    for engine in ["--no-test", "--interp"] {
        let ran = Command::new(env!("CARGO_BIN_EXE_fennec"))
            .args(["run", engine])
            .arg(&game)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap();
        assert_eq!(stdout(&ran), "2 10\n", "{engine}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
