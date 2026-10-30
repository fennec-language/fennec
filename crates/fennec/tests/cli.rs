//! The `fennec` command itself: `run`, `test`, `repl`, `--version`, and what it does with
//! bad input. These run the real binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn fennec(args: &[&str], stdin: &str) -> std::io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(args)
        .current_dir(repo())
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

/// Runs `fennec <command>` on a program written to a temporary file of its own.
fn fennec_on(command: &str, name: &str, source: &str) -> std::io::Result<Output> {
    let dir = std::env::temp_dir().join(format!("fennec-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let output = fennec(&[command, &program.to_string_lossy()], "");
    std::fs::remove_dir_all(&dir)?;
    output
}

#[test]
fn run_stops_runaway_recursion_with_an_error() {
    // The interpreter runs on a big stack so that the depth limit, not a stack overflow,
    // is what stops a program that recurses forever.
    let source = "def forever(n: int):\n    forever(n + 1)\n\ndef main(stdout: Stdout):\n    forever(0)\n";
    let output = fennec_on("run", "forever", source).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: calls nested more than 1000 deep"));
}

#[test]
fn test_fails_when_a_test_fails() {
    let source = "test \"one is one\":\n    assert 1 == 1\n\ntest \"one is two\":\n    assert 1 == 2\n";
    let output = fennec_on("test", "failing", source).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "  ✓ one is one\n  ✗ one is two\n1 passed, 1 failed\n");
    assert!(text(&output.stderr).contains("the left side is 1 and the right side is 2"));
}

#[test]
fn test_shrinks_and_replays_property_failures() {
    let output = fennec(&["test", "crates/fennec/tests/fixtures/failing_properties.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).ends_with("  ✓ this one passes\n1 passed, 16 failed\n"));
    let stderr = text(&output.stderr);
    let reported: Vec<&str> = stderr
        .lines()
        .filter_map(|line| {
            line.strip_prefix("error: ").or_else(|| line.trim_start().strip_prefix("= help: "))
        })
        .filter(|line| *line != "this assertion failed")
        .collect();
    assert_eq!(
        reported,
        [
            "it failed for n = 10; the left side is 10 and the right side is 10",
            "it failed for x = 0.125; the left side is 0.125 and the right side is 0.0",
            "it failed for x = 51.0; the left side is 51.0 and the right side is 2.0",
            "it failed for word = \"znk\"; the left side is 3 and the right side is 3",
            "it failed for items = [-656, -806]; the left side is 2 and the right side is 2",
            "it failed for grid = [[\"ldu\", \"tdxoqz\", \"fwvlmdz\", \"guzmwk\"], [\"qpov\", \"\", \"upomgfx\", \
             \"hrcdawk\", \"ka\", \"xer\", \"\"], []]; the left side is 3 and the right side is 3",
            "it failed for deep = [[[[], [], [], [], []], [[], []], [[], [], [], [], [], []]], [[[], []], [[], [], [], \
             [], [], [], []], [[], [], [], []], [[], [], [], [], [], []], [[], [], [], [], [], []], [], [[], [], [], \
             [], [], [], []]]]; the left side is 2 and the right side is 2",
            "it failed for deep = ((([], 777), -171), -278); the left side is 777 and the right side is 100",
            "it failed for outer = Outer(inner=Inner(rows=[[], [], []])); the left side is 3 and the right side is 2",
            "it failed for pair = (False, 993)",
            "it failed for pair = (0, -106); the left side is 0 and the right side is None",
            "it failed for box = Box(label=\"box\", size=436); the left side is 436 and the right side is 3",
            "it failed for coin = Coin.Tails(times=533); the left side is Coin.Tails(times=533) and the right \
             side is Coin.Heads",
            "it failed for n = 160",
            "the test failed with Oops(reason=\"broken\")",
            "property tests can't generate a `Widget` yet",
            "use int, float, bool, str, list, tuples, or your own structs and enums",
        ]
    );
}

#[test]
fn test_says_when_there_are_no_tests() {
    let output = fennec(&["test", "examples/hello.fen"], "").unwrap();
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), "no tests in examples/hello.fen\n");
}

#[test]
fn test_reports_a_program_that_cant_load() {
    let output = fennec_on("test", "bad_import", "from std.nothing import thing\n").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: there's no module called `std.nothing`"));
}

#[test]
fn test_reports_a_program_that_cant_parse() {
    let output = fennec(&["test", "examples/errors/raise.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: Fennec doesn't have `raise`"));
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
fn repl_continues_open_brackets_past_blank_lines() {
    let output = fennec(&["repl"], "[1,\n\n2]\n\nexit\n").unwrap();
    assert!(output.status.success());
    assert!(text(&output.stdout).contains(">>> ... ... ... [1, 2]\n"));
}

#[test]
fn repl_keeps_going_after_errors() {
    let input = "mut xs = [1]\ndef add(mut items: list[int], item: int):\n    items.append(item)\n\n\
                 add(mut xs)\nxs\nlen(mut xs)\nxs\nstdout.print(\"hello\")\nif True:\n    5\n\nbreak\n";
    let output = fennec(&["repl"], input).unwrap();
    assert!(output.status.success());
    // A failed call puts back what was passed with `mut`, and only expressions are echoed.
    let stdout = text(&output.stdout);
    let after_banner = stdout.split_once('\n').unwrap().1;
    assert_eq!(after_banner, ">>> >>> ... ... >>> >>> [1]\n>>> >>> [1]\n>>> hello\n>>> ... ... >>> >>> \n");
    let stderr = text(&output.stderr);
    let errors: Vec<&str> = stderr.lines().filter(|line| line.starts_with("error")).collect();
    assert_eq!(
        errors,
        [
            "error: `add` is missing its `item` argument",
            "error: `len` doesn't change its arguments, so don't pass `mut`",
            "error: there's no function or loop here to leave",
        ]
    );
}

#[test]
fn repl_leaves_on_quit() {
    let output = fennec(&["repl"], "quit\n1 + 41\n").unwrap();
    assert!(output.status.success());
    assert!(!text(&output.stdout).contains("42"));
    assert_eq!(text(&output.stderr), "");
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
