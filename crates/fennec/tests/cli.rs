//! The `fennec` command itself: `run`, `test`, `new`, `repl`, `--version`, and what it does
//! with bad input. These run the real binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn fennec(args: &[&str], stdin: &str) -> std::io::Result<Output> {
    fennec_in(&repo(), args, stdin)
}

fn fennec_in(dir: &Path, args: &[&str], stdin: &str) -> std::io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut input) = child.stdin.take() {
        input.write_all(stdin.as_bytes())?;
    }
    child.wait_with_output()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn run_prints_the_programs_output() {
    let output = fennec(&["run", "examples/hello.fen"], "").unwrap();
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), "Hello, Fennec\n");
}

#[test]
fn run_reports_errors_and_fails() {
    let output = fennec(&["run", "examples/errors/print.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: `print` doesn't exist"));
}

#[test]
fn run_reports_a_missing_file() {
    let output = fennec(&["run", "examples/no-such-program.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("couldn't read examples/no-such-program.fen"));
}

#[test]
fn test_lists_each_test_and_a_summary() {
    let output = fennec(&["test", "examples/testing.fen"], "").unwrap();
    assert!(output.status.success());
    let stdout = text(&output.stdout);
    assert!(stdout.contains("✓ reversing twice gives the original (100 inputs)"));
    assert!(stdout.ends_with("3 passed, 0 failed\n"));
}

#[test]
fn test_fails_when_a_test_fails() {
    let dir = std::env::temp_dir().join(format!("fennec-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("failing.fen");
    std::fs::write(&program, "test \"one is two\":\n    assert 1 == 2\n").unwrap();
    let output = fennec(&["test", program.to_str().unwrap()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).contains("✗ one is two"));
    assert!(text(&output.stderr).contains("the left side is 1 and the right side is 2"));
}

#[test]
fn run_reports_every_problem_the_checker_finds_before_running() {
    let output = fennec(&["run", "examples/errors/several.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert_eq!(stderr.matches("error: ").count(), 3, "{stderr}");
    assert!(stderr.contains("\n\nerror: can't mix int and float"), "{stderr}");
}

#[test]
fn test_checks_the_program_before_running_its_tests() {
    let output = fennec(&["test", "examples/errors/mut_self.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    assert!(text(&output.stderr).starts_with("error: `self` isn't mut"));
}

#[test]
fn repl_echoes_values_and_keeps_definitions() {
    let input = "1 + 2\ndef double(x: int):\n    x * 2\n\ndouble(21)\nprint(1)\nexit\n";
    let output = fennec(&["repl"], input).unwrap();
    assert!(output.status.success());
    let stdout = text(&output.stdout);
    assert!(stdout.contains(">>> 3\n"));
    assert!(stdout.contains("42\n"));
    assert!(text(&output.stderr).contains("`print` doesn't exist"));
}

#[test]
fn version_shows_the_fox() {
    let output = fennec(&["--version"], "").unwrap();
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), format!("🦊 fennec {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_commands_show_usage() {
    let output = fennec(&["fly"], "").unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).starts_with("usage:"));
}

/// A fresh empty folder for one test.
fn scratch(name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-cli-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[test]
fn run_with_no_path_runs_the_kit_around_the_current_folder() {
    let expected = std::fs::read_to_string(repo().join("examples/projects/arena.out")).unwrap();
    let kit = repo().join("examples/projects/arena");
    for dir in [kit.clone(), kit.join("src/combat")] {
        let output = fennec_in(&dir, &["run"], "").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(text(&output.stdout), expected);
    }
}

#[test]
fn run_takes_a_kits_folder_in_either_engine() {
    let expected = std::fs::read_to_string(repo().join("examples/projects/arena.out")).unwrap();
    for args in [&["run", "examples/projects/arena"][..], &["run", "--jit", "examples/projects/arena"]] {
        let output = fennec(args, "").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(text(&output.stdout), expected);
        assert_eq!(text(&output.stderr), "", "the JIT shouldn't need the interpreter for the whole program");
    }
}

#[test]
fn test_runs_the_tests_in_every_file_of_a_kit() {
    let output = fennec(&["test", "examples/projects/arena"], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("✓ a wave has as many enemies as asked"), "{stdout}");
    assert!(stdout.contains("✓ hitting takes health away"), "{stdout}");
    assert!(stdout.contains("✓ a boss survives a full hit"), "nothing imports balance.fen, but it has tests");
    assert!(stdout.ends_with("3 passed, 0 failed\n"), "{stdout}");
}

#[test]
fn run_points_at_the_import_and_names_the_file() {
    let output = fennec(&["run", "examples/projects/errors/cycle"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("error: these imports go in a circle: ships → ports → ships"), "{stderr}");
    assert!(stderr.contains("--> examples/projects/errors/cycle/src/ports.fen:1:1"), "{stderr}");
}

#[test]
fn errors_show_paths_the_way_the_kit_was_named() {
    let output = fennec(&["run", "examples/projects/errors/cycle/src"], "").unwrap();
    assert!(text(&output.stderr).contains("--> examples/projects/errors/cycle/src/ports.fen:1:1"));
    // From inside the kit, `.` has no name to trim, so the path is absolute.
    let inside = repo().join("examples/projects/errors/cycle/src");
    let output = fennec_in(&inside, &["run"], "").unwrap();
    let stderr = text(&output.stderr);
    assert!(stderr.contains("examples/projects/errors/cycle/src/ports.fen:1:1"), "{stderr}");
    assert!(!stderr.contains("--> src/ports.fen"), "{stderr}");
}

#[test]
fn a_kit_without_main_can_be_tested_but_not_run() {
    let dir = scratch("no-main").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"lib\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(
        dir.join("src/maths.fen"),
        "def twice(x: int) -> int:\n    x * 2\n\ntest \"twice\":\n    assert twice(2) == 4\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/notes.txt"), "not Fennec\n").unwrap();
    let run = fennec_in(&dir, &["run"], "").unwrap();
    let test = fennec_in(&dir, &["test"], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(run.status.code(), Some(1));
    assert!(text(&run.stderr).starts_with("error: src/main.fen doesn't exist"), "{}", text(&run.stderr));
    assert!(test.status.success(), "{}", text(&test.stderr));
    assert!(text(&test.stdout).ends_with("1 passed, 0 failed\n"));
}

#[test]
fn run_outside_a_kit_says_how_to_start_one() {
    let dir = scratch("no-kit").unwrap();
    let output = fennec_in(&dir, &["run"], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("error: there's no kit.toml in . or any folder above it"), "{stderr}");
    assert!(stderr.contains("fennec new"), "{stderr}");
}

#[test]
fn new_makes_a_kit_that_runs_and_passes_its_tests() {
    let dir = scratch("new").unwrap();
    let made = fennec_in(&dir, &["new", "star_fox"], "").unwrap();
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert!(text(&made.stdout).contains("star_fox"));
    let kit = dir.join("star_fox");
    let manifest = std::fs::read_to_string(kit.join("kit.toml")).unwrap();
    assert!(manifest.contains("name = \"star_fox\""), "{manifest}");
    assert!(manifest.contains(&format!("fennec = \"{}\"", env!("CARGO_PKG_VERSION"))), "{manifest}");

    let run = fennec_in(&kit, &["run"], "").unwrap();
    assert!(run.status.success(), "{}", text(&run.stderr));
    assert_eq!(text(&run.stdout), "Hello from star_fox!\n");
    let jit = fennec_in(&kit, &["run", "--jit"], "").unwrap();
    assert_eq!(text(&jit.stdout), "Hello from star_fox!\n");
    let test = fennec_in(&kit, &["test"], "").unwrap();
    assert!(test.status.success(), "{}", text(&test.stderr));
    assert!(text(&test.stdout).ends_with("1 passed, 0 failed\n"));

    let again = fennec_in(&dir, &["new", "star_fox"], "").unwrap();
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("already exists"));
    for bad_name in ["Star_Fox", "star-fox", "9lives", ""] {
        let bad = fennec_in(&dir, &["new", bad_name], "").unwrap();
        assert_eq!(bad.status.code(), Some(1), "{bad_name:?}");
        assert!(text(&bad.stderr).contains("can't name a kit"), "{bad_name:?}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
