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
    assert_eq!(stdout(&first), "2 passed, 0 failed\n");

    let again = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&again), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    let reformatted = format!("# A comment.\n\n{SOURCE}");
    std::fs::write(dir.join("game.fen"), reformatted).unwrap();
    let comment = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&comment), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(dir.join("game.fen"), SOURCE.replace("x * 2", "x + x")).unwrap();
    let edited = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&edited), "2 passed, 0 failed (1 run, 1 unchanged)\n");

    let all = run_tests(&dir, &["--all"]).unwrap();
    assert_eq!(stdout(&all), "2 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failing_test_runs_every_time_and_a_break_is_never_hidden() {
    let dir = scratch("failing").unwrap();
    run_tests(&dir, &[]).unwrap();

    std::fs::write(dir.join("game.fen"), SOURCE.replace("x * 2", "x * 5")).unwrap();
    let broken = run_tests(&dir, &[]).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert_eq!(stdout(&broken), "  ✗ doubling\n1 passed, 1 failed (1 run, 1 unchanged)\n");

    let still = run_tests(&dir, &[]).unwrap();
    assert_eq!(still.status.code(), Some(1));
    assert_eq!(stdout(&still), "  ✗ doubling\n1 passed, 1 failed (1 run, 1 unchanged)\n");

    // Going back to code that passed still has to run, since the failure replaced its result.
    std::fs::write(dir.join("game.fen"), SOURCE).unwrap();
    let fixed = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&fixed), "2 passed, 0 failed (1 run, 1 unchanged)\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ci_reuses_what_passed_like_anywhere_else() {
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
    assert_eq!(stdout(&output), "2 passed, 0 failed (0 run, 2 unchanged)\n");
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
    assert_eq!(edited, "3 passed, 0 failed (2 run, 1 unchanged)\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program whose test takes one branch of a function, written to `dir`.
fn with_branches(dir: &Path, taken: &str, untaken: &str, interface: &str) -> std::io::Result<()> {
    let source = format!(
        "def taken() -> int:\n    {taken}\n\ndef untaken() -> int:\n    {untaken}\n\n\
         def choose(x: int) -> int:\n    if x > 0:\n        return taken()\n    untaken()\n\n\
         interface Sized:\n    def size(self) -> int\n\n\
         struct Box:\n    n: int\n\n    def size(self) -> int:\n        {interface}\n\n\
         def size_of[T: Sized](s: T) -> int:\n    s.size()\n\n\
         test \"branch\":\n    assert choose(1) == 1\n\n\
         test \"through an interface\":\n    assert size_of(Box(n=1)) == 1\n"
    );
    std::fs::write(dir.join("game.fen"), source)
}

#[test]
fn only_what_a_test_ran_can_make_it_run_again() {
    let dir = scratch("traced").unwrap();
    with_branches(&dir, "1", "2", "self.n").unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "2 passed, 0 failed\n");

    with_branches(&dir, "1", "3", "self.n").unwrap();
    let branch = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&branch), "2 passed, 0 failed (0 run, 2 unchanged)\n", "a branch it never takes");

    with_branches(&dir, "1 + 0", "3", "self.n").unwrap();
    let taken = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&taken), "2 passed, 0 failed (1 run, 1 unchanged)\n", "a function it calls");

    with_branches(&dir, "1 + 0", "3", "self.n + 0").unwrap();
    let method = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&method), "2 passed, 0 failed (1 run, 1 unchanged)\n", "a method it finds at run time");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_test_of_inlined_code_still_knows_what_it_ran() {
    let dir = scratch("inlined").unwrap();
    let source = |body: &str| {
        format!(
            "def small(x: int) -> int:\n    x + {body}\n\ndef twice(x: int) -> int:\n    small(x) + small(x)\n\n\
             test \"t\":\n    assert twice(1) > 0\n"
        )
    };
    std::fs::write(dir.join("game.fen"), source("1")).unwrap();
    run_tests(&dir, &[]).unwrap();
    std::fs::write(dir.join("game.fen"), source("2")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "1 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_property_test_depends_on_all_it_can_reach() {
    let dir = scratch("property").unwrap();
    let source = |untaken: &str| {
        format!(
            "def untaken() -> int:\n    {untaken}\n\ndef choose(x: int) -> int:\n    if x > 1000000:\n        return untaken()\n    x\n\n\
             test \"any\" for x: int:\n    assert choose(0) == 0\n"
        )
    };
    std::fs::write(dir.join("game.fen"), source("1")).unwrap();
    run_tests(&dir, &[]).unwrap();
    assert!(stdout(&run_tests(&dir, &[]).unwrap()).contains("(0 run, 1 unchanged)"));
    std::fs::write(dir.join("game.fen"), source("2")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "1 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit in a folder of its own, with a function called from a test in another file and a test
/// that doesn't touch it.
fn kit_with_tests(name: &str) -> std::io::Result<PathBuf> {
    let dir = scratch(name)?;
    std::fs::create_dir_all(dir.join("src"))?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest)?;
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x * 2\n")?;
    std::fs::write(
        dir.join("src/uses.fen"),
        "from maths import double\n\ndef twice(x: int) -> int:\n    double(x)\n\n\
         test \"twice\":\n    assert twice(2) == 4\n",
    )?;
    std::fs::write(dir.join("src/other.fen"), "test \"other\":\n    assert 1 == 1\n")?;
    Ok(dir)
}

fn test_kit(dir: &Path) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("test")
        .arg(dir)
        .env("FENNEC_HOME", dir.join("home"))
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

#[test]
fn an_edit_to_a_body_runs_only_what_ran_it_across_files() {
    let dir = kit_with_tests("kit_body").unwrap();
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed\n");
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + x\n").unwrap();
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed (1 run, 1 unchanged)\n");
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x * 3\n").unwrap();
    let broken = test_kit(&dir).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert_eq!(stdout(&broken), "  ✗ twice\n1 passed, 1 failed (1 run, 1 unchanged)\n");

    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + x\n").unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_edit_that_breaks_a_caller_in_another_file_is_still_found() {
    for (edit, what) in [
        ("def double(x: float) -> float:\n    x * 2.0\n", "a changed signature"),
        (
            "error Bad:\n    n: int\n\ndef double(x: int) -> int:\n    if x > 9: fail Bad(n=x)\n    x * 2\n",
            "a body that can now fail",
        ),
    ] {
        let dir = kit_with_tests("kit_caller").unwrap();
        assert!(test_kit(&dir).unwrap().status.success());
        std::fs::write(dir.join("src/maths.fen"), edit).unwrap();
        let broken = test_kit(&dir).unwrap();
        assert_eq!(broken.status.code(), Some(1), "{what}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_mistake_in_an_edited_body_is_reported() {
    let dir = kit_with_tests("kit_mistake").unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + \"a\"\n").unwrap();
    let broken = test_kit(&dir).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&broken.stderr).starts_with("error"), "{broken:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_new_file_and_a_new_test_are_noticed() {
    let dir = kit_with_tests("kit_new").unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    std::fs::write(dir.join("src/third.fen"), "test \"third\":\n    assert 1 == 2\n").unwrap();
    assert_eq!(test_kit(&dir).unwrap().status.code(), Some(1));
    std::fs::remove_file(dir.join("src/third.fen")).unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    let added = "test \"other\":\n    assert 1 == 1\n\ntest \"new\":\n    assert 1 == 2\n";
    std::fs::write(dir.join("src/other.fen"), added).unwrap();
    assert_eq!(test_kit(&dir).unwrap().status.code(), Some(1));
    std::fs::remove_dir_all(&dir).unwrap();
}
