//! The `fennec` command itself: `run`, `test`, `new`, `repl`, `dev`, `--version`, and what it does
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
        // Every run does all of its work, whatever earlier runs left in the real cache.
        .env("FENNEC_NO_CACHE", "1")
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
fn run_hands_main_its_arguments_and_input() {
    let program = "examples/args_and_stdin.fen";
    for engine in [&["run"][..], &["run", "--interp"]] {
        let args: Vec<&str> = engine.iter().copied().chain([program, "fire", "two words"]).collect();
        let output = fennec(&args, "one\ntwo\r\n").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(
            text(&output.stdout),
            "2 arguments: FIRE TWO WORDS\nthe first is fire\n2 lines of input\n> one\n> two\n"
        );
    }
    let output = fennec_on("run", "program_name", "def main(stdout: Stdout, args: Args):\n    stdout.print(args.program.endswith(\"program_name.fen\"))\n").unwrap();
    assert_eq!(text(&output.stdout), "True\n");
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
fn dev_tokens_and_dev_tree_dump_the_front_ends_work() {
    let tokens = fennec(&["dev", "tokens", "examples/hello.fen"], "").unwrap();
    assert!(tokens.status.success());
    assert!(text(&tokens.stdout).starts_with("0..3 keyword def\n4..8 name main\n"));
    let tree = fennec(&["dev", "tree", "examples/hello.fen"], "").unwrap();
    assert!(tree.status.success());
    assert!(text(&tree.stdout).starts_with("module\n  def main @0..24\n"));
}

#[test]
fn dev_reports_a_missing_file() {
    let output = fennec(&["dev", "tokens", "examples/no-such-program.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("couldn't read examples/no-such-program.fen"));
}

#[test]
fn dev_check_dumps_the_checkers_diagnostics() {
    let clean = fennec(&["dev", "check", "examples/hello.fen"], "").unwrap();
    assert!(clean.status.success());
    assert_eq!(text(&clean.stdout), "");
    let several = fennec(&["dev", "check", "examples/errors/several.fen"], "").unwrap();
    let dump = text(&several.stdout);
    assert!(dump.starts_with("error 82..94 `enemy` isn't mut\n"), "{dump}");
    assert_eq!(dump.lines().filter(|line| line.starts_with("error ")).count(), 3);
    let broken = fennec(&["dev", "check", "examples/errors/tabs.fen"], "").unwrap();
    assert!(text(&broken.stdout).starts_with("error 26..27 tabs aren't allowed\n"));
}

#[test]
fn test_prints_only_a_summary_when_everything_passes() {
    let output = fennec(&["test", "examples/testing.fen"], "").unwrap();
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), "3 passed, 0 failed\n");
}

#[test]
fn test_lists_each_test_when_asked() {
    for flags in [["--verbose", "examples/testing.fen"], ["examples/testing.fen", "-v"]] {
        let output = fennec(&["test", flags[0], flags[1]], "").unwrap();
        assert!(output.status.success());
        let stdout = text(&output.stdout);
        assert!(stdout.contains("✓ reversing twice gives the original (100 inputs)"));
        assert!(stdout.ends_with("3 passed, 0 failed\n"));
    }
}

#[test]
fn test_says_how_to_use_it_when_its_arguments_are_wrong() {
    for args in [&["test", "--verbos"][..], &["test", "--verbos", "a.fen"], &["test", "a.fen", "b.fen"]] {
        let output = fennec(args, "").unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(text(&output.stderr).starts_with("usage:"), "{args:?}");
    }
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
    let source = "def forever(n: int) -> int:\n    forever(n + 1)\n\ndef main(stdout: Stdout):\n    stdout.print(forever(0))\n";
    let output = fennec_on("run", "forever", source).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: calls nested too deeply"));
}

#[test]
fn test_fails_when_a_test_fails() {
    let source = "test \"one is one\":\n    assert 1 == 1\n\ntest \"one is two\":\n    assert 1 == 2\n";
    let output = fennec_on("test", "failing", source).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "  ✗ one is two\n1 passed, 1 failed\n");
    assert!(text(&output.stderr).contains("the left side is 1 and the right side is 2"));
}

#[test]
fn test_shrinks_and_replays_property_failures() {
    let output = fennec(&["test", "crates/fennec/tests/fixtures/failing_properties.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = text(&output.stdout);
    assert!(stdout.ends_with("1 passed, 15 failed\n") && !stdout.contains('✓'), "{stdout}");
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
fn tests_get_fakes_unless_asked_for_the_real_thing() {
    let dir = std::env::temp_dir().join(format!("fennec-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let written = dir.join("written.txt");
    let start = dir.join("start.txt");
    std::fs::write(&start, "from disk").unwrap();
    let source = format!(
        "test \"files\"(fs: Fs, out: Stdout, clock: Clock):\n    fs.make_dir({dir:?})?\n    fs.write_text({written:?}, \"hi\")?\n    assert fs.exists({written:?})\n    out.print(\"printed\", fs.exists({start:?}))\n    assert clock.now().nanos >= 0\n",
    );
    let path = dir.join("real.fen");
    std::fs::write(&path, source).unwrap();
    let path = path.to_str().unwrap();

    let faked = fennec(&["test", "--all", path], "").unwrap();
    assert!(faked.status.success(), "{}", text(&faked.stderr));
    assert!(!written.exists(), "the fake wrote to the disk");
    assert!(!text(&faked.stdout).contains("printed"));

    let real = fennec(&["test", "--real", path], "").unwrap();
    assert!(real.status.success(), "{}", text(&real.stderr));
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "hi");
    assert!(text(&real.stdout).contains("printed True\n"), "{}", text(&real.stdout));
    // Nothing a real run passes counts for the next one.
    let again = fennec(&["test", "--real", path], "").unwrap();
    assert!(text(&again.stdout).contains("1 passed, 0 failed\n"), "{}", text(&again.stdout));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_clock_is_fixed_unless_asked_for_the_real_one() {
    let dir = std::env::temp_dir().join(format!("fennec-clock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clock.fen");
    let source = "from std.time import Duration\n\ntest \"clock\"(clock: Clock):\n    start = clock.now()\n    clock.sleep(Duration.seconds(0.01))\n    assert (clock.now() - start).nanos > 10_000_000\n";
    std::fs::write(&path, source).unwrap();
    let path = path.to_str().unwrap();
    assert!(!fennec(&["test", "--all", path], "").unwrap().status.success());
    let real = fennec(&["test", "--real", path], "").unwrap();
    assert!(real.status.success(), "{}", text(&real.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
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
fn repl_sums_floats_without_the_checker() {
    let output = fennec(&["repl"], "sum([1.5, 2.5])\nsum([1, 2])\nsum([1], 10)\nexit\n").unwrap();
    let stdout = text(&output.stdout);
    assert!(stdout.contains(">>> 4.0\n>>> 3\n>>> 11\n"), "{stdout}");
}

#[test]
fn repl_says_which_module_to_import_from() {
    let output = fennec(&["repl"], "sqrt(2.0)\nloads(\"1\")\nexit\n").unwrap();
    let errors = text(&output.stderr);
    assert!(errors.contains("import it from std.math, like `from std.math import sqrt`"), "{errors}");
    assert!(
        errors.contains("import it from std.json or std.toml, like `from std.json import loads`"),
        "{errors}"
    );
}

#[test]
fn repl_says_how_to_write_ord_and_chr() {
    let input = "ord(\"A\")\nchr(65)\n\"A\".code()\nstr.lower(\"A\")\nlen([1])\nabs(1)\nexit\n";
    let output = fennec(&["repl"], input).unwrap();
    let errors = text(&output.stderr);
    // Only `from_code` is called on `str` itself.
    assert!(errors.contains("a function has no method `lower`"), "{errors}");
    assert!(errors.contains("a length is a method: write `x.len()`"), "{errors}");
    assert!(errors.contains("a size is a method: write `x.abs()`"), "{errors}");
    assert!(errors.contains("a character's code is a method: write `c.code()`"), "{errors}");
    assert!(errors.contains("make a character from its code with `str.from_code(n)`"), "{errors}");
    assert!(text(&output.stdout).contains("65\n"));
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
                 add(mut xs)\nxs\nsorted(mut xs)\nxs\nstdout.print(\"hello\")\nif True:\n    5\n\nbreak\n";
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
            "error: `sorted` doesn't change its arguments, so don't pass `mut`",
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
fn fs_makes_lists_and_removes_files_and_folders() {
    let dir = scratch("fs").unwrap();
    std::fs::write(dir.join("main.fen"), PROGRAM_WITH_FILES).unwrap();
    for engine in [&["run", "main.fen"][..], &["run", "--interp", "main.fen"]] {
        let output = fennec_in(&dir, engine, "").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(
            text(&output.stdout),
            "Ok() Ok() True\n\
             [\"saves/slots/a.txt\", \"saves/slots/b.txt\"] ay\n\
             3 True False False True\n\
             a folder has to be empty to remove\n\
             False [Path(text=\"./main.fen\")]\n"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fs_reads_and_writes_bytes() {
    let dir = scratch("fs-bytes").unwrap();
    let program = r#"
def main(stdout: Stdout, fs: Fs):
    fs.write_bytes("save.bin", bytes([0, 1, 254, 255]))?
    stdout.print(fs.read_bytes("save.bin")?, fs.read_text("save.bin") else "not text")
    fs.write_text("note.txt", "hé")?
    stdout.print(fs.read_bytes("note.txt")?, fs.read_bytes("missing.bin") else bytes())
"#;
    std::fs::write(dir.join("main.fen"), program).unwrap();
    for engine in [&["run", "main.fen"][..], &["run", "--interp", "main.fen"]] {
        let output = fennec_in(&dir, engine, "").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(
            text(&output.stdout),
            "bytes([0, 1, 254, 255]) not text\nbytes([104, 195, 169]) bytes([])\n"
        );
    }
    assert_eq!(std::fs::read(dir.join("save.bin")).unwrap(), [0, 1, 254, 255]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program that changes files, which runs in a folder of its own.
const PROGRAM_WITH_FILES: &str = r#"from std.path import Path

def main(stdout: Stdout, fs: Fs):
    slots = Path("saves") / "slots"
    stdout.print(fs.make_dir(slots), fs.make_dir(slots), fs.exists(slots))
    fs.write_text(slots / "b.txt", "bee")?
    fs.write_text("saves/slots/a.txt", "ay")?
    stdout.print([path.text for path in fs.list_dir(slots)?], fs.read_text(slots / "a.txt")?)
    found = fs.metadata(slots / "b.txt")?
    folder = fs.metadata(slots)?
    stdout.print(found.size, found.is_file, found.is_dir, folder.is_file, folder.is_dir)
    match fs.remove("saves"):
        case Ok(_): stdout.print("removed a folder that isn't empty")
        case IoError(_): stdout.print("a folder has to be empty to remove")
    for path in fs.list_dir(slots)?:
        fs.remove(path)?
    fs.remove(slots)?
    fs.remove("saves")?
    stdout.print(fs.exists("saves"), fs.list_dir(".")?)
"#;

#[test]
fn fmt_check_passes_on_the_examples() {
    let output = fennec(&["fmt", "--check", "examples"], "").unwrap();
    assert!(output.status.success(), "{}{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(text(&output.stdout), "");
    assert!(text(&output.stderr).contains("raise.fen: it doesn't parse (2:5: Fennec doesn't have `raise`)"));
    assert!(
        text(&output.stderr).contains("tab_inside_line.fen: it doesn't parse (2:12: tabs aren't allowed)")
    );
}

#[test]
fn fmt_rewrites_a_folder_and_check_lists_what_it_would_change() {
    let dir = scratch("fmt").unwrap();
    std::fs::create_dir_all(dir.join("src/.hidden")).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"demo\"\n").unwrap();
    std::fs::write(dir.join("src/main.fen"), "def main(stdout:Stdout):\n  stdout.print(1+2)\n").unwrap();
    std::fs::write(dir.join("src/done.fen"), "def done():\n    pass\n").unwrap();
    std::fs::write(dir.join("src/.hidden/skip.fen"), "def skip( ):\n  pass\n").unwrap();
    let folder = dir.to_str().unwrap();

    let check = fennec(&["fmt", "--check", folder], "").unwrap();
    assert_eq!(check.status.code(), Some(1));
    assert!(text(&check.stdout).trim_end().ends_with("main.fen"), "{}", text(&check.stdout));
    assert!(text(&check.stderr).contains("1 file isn't formatted"));

    let write = fennec(&["fmt", folder], "").unwrap();
    assert!(write.status.success());
    let main = std::fs::read_to_string(dir.join("src/main.fen")).unwrap();
    assert_eq!(main, "def main(stdout: Stdout):\n    stdout.print(1 + 2)\n");
    let hidden = std::fs::read_to_string(dir.join("src/.hidden/skip.fen")).unwrap();
    assert_eq!(hidden, "def skip( ):\n  pass\n");
    assert!(fennec(&["fmt", "--check", folder], "").unwrap().status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fmt_has_no_options() {
    let output = fennec(&["fmt", "--line-width=80", "examples"], "").unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("has no option `--line-width=80`"));
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
    for args in [&["run", "examples/projects/arena"][..], &["run", "--interp", "examples/projects/arena"]] {
        let output = fennec(args, "").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(text(&output.stdout), expected);
        assert_eq!(text(&output.stderr), "", "the JIT shouldn't need the interpreter for the whole program");
    }
}

#[test]
fn test_runs_the_tests_in_every_file_of_a_kit() {
    let output = fennec(&["test", "--verbose", "examples/projects/arena"], "").unwrap();
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

/// `fennec bench` on a program, with the cache in `home`, as on a person's machine.
fn bench_in(home: &Path, program: &Path) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("bench")
        .arg(program)
        .env("FENNEC_HOME", home)
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

const BENCHED: &str = "def double(x: int) -> int:\n    x * 2\n\n\
    bench \"doubling\":\n    assert double(21) == 42\n\n\
    test \"a test\":\n    assert double(1) == 2\n\n\
    def main(stdout: Stdout):\n    stdout.print(double(2))\n";

#[test]
fn bench_runs_blocks_natively_and_compares_with_the_last_run() {
    let dir = scratch("bench").unwrap();
    let (home, program) = (dir.join("home"), dir.join("game.fen"));
    std::fs::write(&program, BENCHED).unwrap();
    let first = bench_in(&home, &program).unwrap();
    assert!(first.status.success(), "{}", text(&first.stderr));
    let printed = text(&first.stdout);
    assert!(printed.starts_with("  doubling: ") && printed.contains(" ms"), "{printed}");
    assert!(!printed.contains("last run"), "{printed}");
    assert!(!printed.contains("a test"), "{printed}");

    let second = text(&bench_in(&home, &program).unwrap().stdout);
    assert!(second.contains("from the last run)"), "{second}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_bench_only_runs_for_fennec_bench() {
    let source = BENCHED.replace("double(21) == 42", "double(21) == 43");
    let tested = fennec_on("test", "failing_bench", &source).unwrap();
    assert!(tested.status.success(), "{}", text(&tested.stderr));
    assert_eq!(text(&tested.stdout), "1 passed, 0 failed\n");
    assert_eq!(text(&fennec_on("run", "failing_bench", &source).unwrap().stdout), "4\n");

    let benched = fennec_on("bench", "failing_bench", &source).unwrap();
    assert_eq!(benched.status.code(), Some(1));
    assert!(text(&benched.stderr).contains("the left side is 42"), "{}", text(&benched.stderr));
}

#[test]
fn a_bench_is_checked_like_a_test() {
    let source = BENCHED.replace("double(21) == 42", "double(\"no\") == 42");
    let tested = fennec_on("test", "wrong_bench", &source).unwrap();
    assert_eq!(tested.status.code(), Some(1));
    assert!(text(&tested.stderr).starts_with("error: "), "{}", text(&tested.stderr));
}

#[test]
fn a_bench_takes_no_inputs() {
    let source = "bench \"b\" for x: int:\n    assert x == x\n";
    assert_eq!(fennec_on("test", "bench_inputs", source).unwrap().status.code(), Some(1));
}

#[test]
fn bench_without_blocks_says_so() {
    let output =
        fennec_on("bench", "no_benches", "def main(stdout: Stdout):\n    stdout.print(1)\n").unwrap();
    assert!(output.status.success());
    assert!(text(&output.stdout).starts_with("no benches in "), "{}", text(&output.stdout));
}

#[test]
fn dev_copies_lists_what_may_cost_hot_code_first() {
    let source = "def unused() -> list[int]:\n    [1]\n\n\
        def main(stdout: Stdout):\n    mut xs = [1]\n    snapshot = xs\n    xs.append(2)\n    \
        for at in range(2):\n        stdout.print(f\"{at}\")\n    stdout.print(snapshot.len())\n";
    let dir = scratch("copies").unwrap();
    let program = dir.join("game.fen");
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["dev", "copies", &program.to_string_lossy()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    let lines: Vec<&str> = printed.lines().filter(|line| line.contains(" in `")).collect();
    assert!(lines[0].contains("(in a loop): this f-string may allocate"), "{printed}");
    assert!(printed.contains("may copy `xs`: snapshot still refers to it"), "{printed}");
    assert!(!printed.contains("unused"), "only what `main` reaches: {printed}");
    assert!(printed.ends_with("of them in loops\n"), "{printed}");
}

/// The last lines of `fennec dev copies --run` on a program that does nothing else.
fn counted_in_a_run(name: &str, body: &str) -> String {
    let dir = scratch(name).unwrap_or_default();
    let program = dir.join("game.fen");
    let source = format!("def main(stdout: Stdout):\n{body}");
    let path = program.to_string_lossy();
    let written = std::fs::write(&program, source);
    let output = fennec(&["dev", "copies", "--run", &path], "");
    let _ = std::fs::remove_dir_all(&dir);
    match (written, output) {
        (Ok(()), Ok(output)) => text(&output.stdout),
        (written, output) => format!("{written:?} {output:?}"),
    }
}

#[test]
fn a_run_counts_a_copy_only_when_a_shared_value_changes() {
    let shared = "    mut xs = [1, 2, 3]\n    snapshot = xs\n    xs.append(4)\n    \
                  stdout.print(snapshot.len() + xs.len())\n";
    let counted = counted_in_a_run("copies_shared", shared);
    assert!(counted.contains("in a run: 1 copy of 24 B"), "{counted}");

    let alone = "    mut xs = [1, 2, 3]\n    xs.append(4)\n    stdout.print(xs.len())\n";
    let counted = counted_in_a_run("copies_alone", alone);
    assert!(counted.contains("in a run: no copies, "), "{counted}");
}

#[test]
fn a_run_of_plain_numbers_allocates_nothing() {
    let numbers = "    mut total = 0\n    mut at = 0\n    while at < 1000:\n        total += at\n        \
                   at += 1\n    stdout.print(total)\n";
    let counted = counted_in_a_run("copies_numbers", numbers);
    assert!(counted.contains("in a run: no copies, 0 allocations of 0 B"), "{counted}");
}

#[test]
fn a_run_counts_what_a_hot_loop_allocates() {
    let looped = "    mut total = 0\n    for at in range(100):\n        xs = [at, at]\n        \
                  total += xs.len()\n    stdout.print(total)\n";
    let counted = counted_in_a_run("copies_loop", looped);
    let allocations = counted.split("no copies, ").nth(1).and_then(|rest| rest.split(' ').next());
    let allocations: u64 = allocations.and_then(|count| count.parse().ok()).unwrap_or(0);
    assert!(allocations >= 100, "{counted}");
}

#[test]
fn dev_copies_reports_a_mistake_instead_of_a_list() {
    let dir = scratch("copies_bad").unwrap();
    let program = dir.join("bad.fen");
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(1 + \"a\")\n").unwrap();
    let output = fennec(&["dev", "copies", &program.to_string_lossy()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: "), "{}", text(&output.stderr));
}

/// The language reference is written from the examples, when the website is built. Every
/// decision they name has to exist.
#[test]
fn the_language_reference_can_be_written() {
    let output = fennec(&["dev", "reference", "--check"], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[test]
fn the_reference_is_written_and_a_stale_decision_fails() {
    let dir = scratch("reference").unwrap();
    for folder in ["docs", "examples", "site/src/pages"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    std::fs::write(dir.join("docs/decisions.md"), "## Syntax\n\n**A rule.** Why.\n").unwrap();
    std::fs::write(dir.join("examples/one.fen"), "# decision: A rule.\nx = 1\n").unwrap();
    let page = dir.join("site/src/pages/reference.md");

    assert!(fennec_in(&dir, &["dev", "reference", "--check"], "").unwrap().status.success());
    assert!(!page.exists(), "checking writes nothing");
    assert!(fennec_in(&dir, &["dev", "reference"], "").unwrap().status.success());
    assert!(std::fs::read_to_string(&page).unwrap().contains("**A rule.**"));

    std::fs::write(dir.join("examples/one.fen"), "# decision: Another rule.\nx = 1\n").unwrap();
    for args in [&["dev", "reference"][..], &["dev", "reference", "--check"]] {
        let stale = fennec_in(&dir, args, "").unwrap();
        assert_eq!(stale.status.code(), Some(1));
        assert!(text(&stale.stderr).contains("names a decision"), "{}", text(&stale.stderr));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_no_test_with_no_path_runs_the_kit_here() {
    let dir = scratch("no_test_here").unwrap();
    assert!(fennec_in(&dir, &["new", "demo"], "").unwrap().status.success());
    let kit = dir.join("demo");
    std::fs::write(kit.join("src/other.fen"), "test \"fails\":\n    assert 1 == 2\n").unwrap();
    let output = fennec_in(&kit, &["run", "--no-test"], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "Hello from demo!\n");
}

#[test]
fn check_and_lint_say_the_same_in_json() {
    let dir = scratch("json").unwrap();
    let program = dir.join("game.fen");
    let path = program.to_string_lossy().into_owned();
    let json = |args: &[&str]| {
        let output = fennec(args, "").unwrap();
        let report = fennec_lsp::Json::parse(text(&output.stdout).trim()).unwrap_or(fennec_lsp::Json::Null);
        (output.status.code(), report)
    };
    let rules = |report: &fennec_lsp::Json| -> Vec<String> {
        let found = report.get("diagnostics").and_then(fennec_lsp::Json::as_array).unwrap_or_default();
        found.iter().filter_map(|item| item.get("rule")?.as_str().map(str::to_owned)).collect()
    };
    std::fs::write(
        &program,
        "def one() -> int:\n    return 1\n\ndef main(stdout: Stdout):\n    mut items = [1]\n    \
         held = items\n    items.append(2)\n    stdout.print(held.len(), one())\n",
    )
    .unwrap();
    let (code, report) = json(&["lint", "--json", &path]);
    assert_eq!(code, Some(1));
    assert_eq!(rules(&report), ["needless-return", "copies"]);
    let first = report.at(&["diagnostics"]).and_then(fennec_lsp::Json::as_array).and_then(<[_]>::first);
    let at = |key: &str| first.and_then(|item| item.get(key)).and_then(fennec_lsp::Json::as_usize);
    assert_eq!((at("line"), at("column")), (Some(2), Some(5)));
    assert_eq!(report.get("fixable").and_then(fennec_lsp::Json::as_usize), Some(1));
    assert_eq!(
        json(&["check", "--json", &path]),
        (Some(0), fennec_lsp::Json::parse(r#"{"diagnostics":[],"errors":0,"hints":0,"fixable":0}"#).unwrap())
    );

    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(1 + \"a\")\n").unwrap();
    let (code, report) = json(&["check", "--json", &path]);
    assert_eq!((code, rules(&report)), (Some(1), vec!["check".to_owned()]));
    assert_eq!(fennec(&["check", &path], "").unwrap().status.code(), Some(1));

    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print((\n").unwrap();
    let (code, report) = json(&["lint", "--json", &path]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!((code, rules(&report)), (Some(1), vec!["load".to_owned()]));
}

#[test]
fn lint_reports_and_fix_fixes_and_hints_dont_fail() {
    let dir = scratch("lint").unwrap();
    let program = dir.join("game.fen");
    let path = program.to_string_lossy().into_owned();
    let hinted = "def main(stdout: Stdout):\n    mut items = [1]\n    held = items\n    items.append(2)\n    \
                  stdout.print(held.len())\n";
    std::fs::write(&program, hinted).unwrap();
    let clean = fennec(&["lint", &path], "").unwrap();
    assert!(clean.status.success(), "{}", text(&clean.stderr));
    assert!(text(&clean.stdout).contains("hint: "), "{}", text(&clean.stdout));
    assert!(
        text(&clean.stdout).contains("may copy `items`: held still refers to it"),
        "{}",
        text(&clean.stdout)
    );

    let lints = "def one() -> int:\n    return 1\n\ndef main(stdout: Stdout):\n    stdout.print(one())\n";
    std::fs::write(&program, lints).unwrap();
    let flagged = fennec(&["lint", &path], "").unwrap();
    assert_eq!(flagged.status.code(), Some(1));
    assert!(text(&flagged.stderr).contains("needless-return:"), "{}", text(&flagged.stderr));
    assert!(text(&flagged.stderr).contains("1 lints, 1 of which"), "{}", text(&flagged.stderr));

    assert!(fennec(&["fix", &path], "").unwrap().status.success());
    let after = std::fs::read_to_string(&program).unwrap();
    assert!(after.contains("-> int:\n    1\n"), "{after}");
    assert!(fennec(&["lint", &path], "").unwrap().status.success());

    std::fs::write(&program, "x = 1 + \"a\"\n").unwrap();
    assert_eq!(fennec(&["lint", &path], "").unwrap().status.code(), Some(1));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Every command the guidance tells an agent to run has to exist.
#[test]
fn the_guidance_names_only_commands_there_are() {
    let usage = text(&fennec(&["no such command"], "").unwrap().stderr);
    let mut named = 0;
    for part in fennec_kit::GUIDANCE.split("`fennec ").skip(1) {
        let command = part.split([' ', '`']).next().unwrap();
        assert!(usage.contains(&format!("fennec {command}")), "`fennec {command}` isn't a command");
        named += 1;
    }
    assert!(named >= 4, "{named}");
}

/// The guidance's example is what `fennec fmt` writes, and it runs, tests and all.
#[test]
fn the_guidance_shows_a_formatted_program_that_works() {
    let source = fennec_kit::GUIDANCE.split("```fennec\n").nth(1).unwrap().split("```").next().unwrap();
    let dir = scratch("guidance").unwrap();
    let program = dir.join("guidance.fen");
    std::fs::write(&program, source).unwrap();
    let path = program.to_string_lossy();
    for args in [&["fmt", "--check", &path][..], &["run", &path], &["test", &path]] {
        let output = fennec(args, "").unwrap();
        assert!(output.status.success(), "{args:?}: {}", text(&output.stderr));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_in_a_new_folder_makes_a_kit_with_guidance() {
    let dir = scratch("init_new").unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let made = fennec_in(&dir, &["init"], "").unwrap();
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert!(text(&made.stdout).contains("added Fennec's guidance"), "{}", text(&made.stdout));
    assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), fennec_kit::GUIDANCE);
    let run = fennec_in(&dir, &["run"], "").unwrap();
    let test = fennec_in(&dir, &["test"], "").unwrap();
    assert!(run.status.success(), "{}", text(&run.stderr));
    assert!(test.status.success(), "{}", text(&test.stderr));

    // Now it's an existing project: nothing but the guidance is looked at again.
    let again = fennec_in(&dir, &["init"], "").unwrap();
    assert_eq!(text(&again.stdout), "Fennec's guidance in AGENTS.md is already current\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_in_an_existing_project_only_touches_the_block() {
    let dir = scratch("init_existing").unwrap();
    std::fs::write(dir.join("README.md"), "mine\n").unwrap();
    std::fs::write(dir.join("AGENTS.md"), "# Mine\n").unwrap();
    let added = fennec_in(&dir, &["init"], "").unwrap();
    assert!(added.status.success(), "{}", text(&added.stderr));
    assert_eq!(text(&added.stdout), "added Fennec's guidance to AGENTS.md\n");
    assert!(!dir.join("kit.toml").exists());
    let written = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert_eq!(written, format!("# Mine\n\n{}", fennec_kit::GUIDANCE));

    let stale = written.replace("## Fennec", "## Old");
    std::fs::write(dir.join("AGENTS.md"), stale).unwrap();
    let refreshed = fennec_in(&dir, &["init"], "").unwrap();
    assert_eq!(text(&refreshed.stdout), "refreshed Fennec's guidance in AGENTS.md\n");
    assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), written);

    std::fs::write(dir.join("AGENTS.md"), "<!-- fennec:start -->\nno end\n").unwrap();
    let broken = fennec_in(&dir, &["init"], "").unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert!(text(&broken.stderr).contains("left alone"), "{}", text(&broken.stderr));
    let kept = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert_eq!(kept, "<!-- fennec:start -->\nno end\n");
    std::fs::remove_dir_all(&dir).unwrap();
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
    let interpreted = fennec_in(&kit, &["run", "--interp"], "").unwrap();
    assert_eq!(text(&interpreted.stdout), "Hello from star_fox!\n");
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

#[test]
fn fmt_with_no_path_formats_the_kit_around_the_current_folder() {
    let dir = scratch("fmt-kit").unwrap();
    std::fs::create_dir_all(dir.join("kit/src/combat")).unwrap();
    std::fs::write(dir.join("kit/kit.toml"), "name = \"demo\"\n").unwrap();
    std::fs::write(dir.join("kit/src/main.fen"), "def main(stdout:Stdout):\n  pass\n").unwrap();
    std::fs::write(dir.join("kit/src/combat/hit.fen"), "def hit( ):\n  pass\n").unwrap();
    std::fs::write(dir.join("outside.fen"), "def outside( ):\n  pass\n").unwrap();

    let check = fennec_in(&dir.join("kit/src/combat"), &["fmt", "--check"], "").unwrap();
    assert_eq!(check.status.code(), Some(1));
    let listed = text(&check.stdout).replace('\\', "/");
    assert_eq!(listed, "../../src/combat/hit.fen\n../../src/main.fen\n");

    let write = fennec_in(&dir.join("kit/src/combat"), &["fmt"], "").unwrap();
    assert!(write.status.success());
    let hit = std::fs::read_to_string(dir.join("kit/src/combat/hit.fen")).unwrap();
    assert_eq!(hit, "def hit():\n    pass\n");
    let outside = std::fs::read_to_string(dir.join("outside.fen")).unwrap();
    assert_eq!(outside, "def outside( ):\n  pass\n");

    let plain = fennec_in(&dir, &["fmt", "--check"], "").unwrap();
    assert_eq!(text(&plain.stdout).replace('\\', "/"), "outside.fen\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Runs `fennec dev changes` in `dir`, keeping its snapshots under `home`.
fn changes_in(dir: &Path, home: &Path) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(["dev", "changes"])
        .current_dir(dir)
        .env("FENNEC_HOME", home)
        .env("FENNEC_NO_DAEMON", "1")
        .output()
}

#[test]
fn dev_changes_reports_what_changed_since_the_last_run() {
    let dir = scratch("changes").unwrap();
    let home = dir.join("home");
    let kit = dir.join("kit");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::write(kit.join("kit.toml"), "name = \"demo\"\n").unwrap();
    std::fs::write(kit.join("src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n").unwrap();
    std::fs::write(kit.join("src/other.fen"), "def other() -> int:\n    1\n").unwrap();

    let first = changes_in(&kit.join("src"), &home).unwrap();
    assert_eq!(text(&first.stdout), "first run: hashed 2 files and 2 definitions\n");
    let again = changes_in(&kit, &home).unwrap();
    assert_eq!(text(&again.stdout), "0 files parsed, 2 unchanged and not parsed again\nnothing changed\n");

    std::fs::write(kit.join("src/main.fen"), "# hi\ndef main(stdout: Stdout):\n    stdout.print( 1 )\n")
        .unwrap();
    std::fs::write(kit.join("src/other.fen"), "def other() -> int:\n    2\n\ndef more() -> int:\n    3\n")
        .unwrap();
    let edited = changes_in(&kit, &home).unwrap();
    assert_eq!(
        text(&edited.stdout),
        "2 files parsed, 0 unchanged and not parsed again\n\
         src/main.fen: only its layout or comments changed\n\
         src/other.fen\n  added    def more\n  changed  def other\n"
    );

    std::fs::write(kit.join("src/other.fen"), "def other( -> int:\n").unwrap();
    let broken = changes_in(&kit, &home).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert!(
        text(&broken.stderr).contains("src/other.fen doesn't parse (1:12: expected a parameter name"),
        "{}",
        text(&broken.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fix_writes_in_return_types_and_removes_arrows_to_none() {
    let dir = std::env::temp_dir().join(format!("fennec-cli-{}-fix", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("fix.fen");
    let source = "def square(n: int):\n    n * n\n\ndef greet(stdout: Stdout) -> None:\n    stdout.print(square(3))\n\ndef main(stdout: Stdout):\n    greet(stdout)\n";
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["fix", &program.to_string_lossy()], "").unwrap();
    let fixed = std::fs::read_to_string(&program).unwrap();
    let again = fennec(&["fix", &program.to_string_lossy()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).starts_with("fixed "));
    assert_eq!(
        fixed,
        "def square(n: int) -> int:\n    n * n\n\ndef greet(stdout: Stdout):\n    stdout.print(square(3))\n\ndef main(stdout: Stdout):\n    greet(stdout)\n"
    );
    assert!(again.status.success() && again.stdout.is_empty());
}

#[test]
fn fix_reports_what_it_cant_fix() {
    let output =
        fennec_on("fix", "unfixable", "def main(stdout: Stdout):\n    stdout.print(1 + True)\n").unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("can't use `+`") && stderr.contains("no fix"), "{stderr}");
}

/// Runs `fennec` in `dir` with its own Fennec folder, letting it use the daemon.
#[cfg(unix)]
fn with_daemon(dir: &Path, home: &Path, idle: &str, args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(args)
        .current_dir(dir)
        .env("FENNEC_HOME", home)
        .env("FENNEC_DAEMON_IDLE", idle)
        .env_remove("FENNEC_NO_DAEMON")
        .output()
}

/// Waits up to 5 seconds for the daemon to be gone.
#[cfg(unix)]
fn stops(dir: &Path, home: &Path) -> bool {
    (0..250).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        with_daemon(dir, home, "30", &["daemon", "status"])
            .is_ok_and(|output| text(&output.stdout) == "not running\n")
    })
}

#[cfg(unix)]
fn small_kit(name: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = scratch(name)?;
    std::fs::create_dir_all(dir.join("kit/src"))?;
    std::fs::write(dir.join("kit/kit.toml"), "name = \"demo\"\n")?;
    std::fs::write(dir.join("kit/src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n")?;
    Ok((dir.join("kit"), dir.join("home")))
}

#[cfg(unix)]
#[test]
fn the_daemon_starts_on_demand_and_keeps_runs_in_memory() {
    let (kit, home) = small_kit("daemon").unwrap();
    let status = || text(&with_daemon(&kit, &home, "30", &["daemon", "status"]).unwrap().stdout);
    assert_eq!(status(), "not running\n");

    let first = with_daemon(&kit, &home, "30", &["dev", "changes"]).unwrap();
    assert_eq!(text(&first.stdout), "first run: hashed 1 files and 1 definitions\n");
    assert!(
        status().starts_with("running: Fennec") && status().contains("1 request served\n"),
        "{}",
        status()
    );
    // The one snapshot is a few hundred bytes, under a tenth of a megabyte.
    assert!(status().ends_with("\ncache: 0.0 MB of 1073.7 MB\n"), "{}", status());

    let again = with_daemon(&kit, &home, "30", &["dev", "changes"]).unwrap();
    assert_eq!(text(&again.stdout), "0 files parsed, 1 unchanged and not parsed again\nnothing changed\n");
    assert!(status().contains("2 requests served\n"), "{}", status());

    let stop = with_daemon(&kit, &home, "30", &["daemon", "stop"]).unwrap();
    assert_eq!(text(&stop.stdout), "stopped\n");
    assert!(stops(&kit, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn an_idle_daemon_exits_by_itself() {
    let (kit, home) = small_kit("idle-daemon").unwrap();
    with_daemon(&kit, &home, "1", &["dev", "changes"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't exit after a second with nothing to do");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn the_daemon_keeps_the_cache_under_its_cap() {
    let (kit, home) = small_kit("cache-daemon").unwrap();
    let old = home.join("cache").join("0.0.0-old").join("functions").join("x");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, vec![0_u8; 10_000]).unwrap();
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(["dev", "changes"])
        .current_dir(&kit)
        .env("FENNEC_HOME", &home)
        .env("FENNEC_DAEMON_IDLE", "30")
        .env("FENNEC_CACHE_CAP", "5000")
        .env_remove("FENNEC_NO_DAEMON")
        .output()
        .unwrap();
    let cleaned = (0..250).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        !old.exists()
    });
    with_daemon(&kit, &home, "30", &["daemon", "stop"]).unwrap();
    assert!(cleaned, "the daemon should drop another version's entries once over its cap");
    assert!(stops(&kit, &home));
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[test]
fn dev_fallbacks_says_when_everything_is_native() {
    let output = fennec(&["dev", "fallbacks", "bench/kernels/fib.fen"], "").unwrap();
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), "everything compiles to native code\n");
}

#[test]
fn a_loop_that_only_indexes_a_list_and_takes_its_length_stays_native() {
    let dir = scratch("hoisted").unwrap();
    let program = dir.join("total.fen");
    std::fs::write(
        &program,
        "def total(xs: list[int]) -> int:\n    mut sum_of = 0\n    for i in range(xs.len()):\n        \
         sum_of += xs[i]\n    sum_of\n\ndef main(stdout: Stdout):\n    stdout.print(total([i for i in range(10)]))\n",
    )
    .unwrap();
    let output = fennec(&["dev", "fallbacks", program.to_str().unwrap()], "").unwrap();
    let printed = text(&output.stdout);
    assert!(!printed.contains("total.fen:4:"), "{printed}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn abs_is_native_on_numbers() {
    let dir = scratch("abs").unwrap();
    let program = dir.join("size.fen");
    let source = "def main(stdout: Stdout):\n    stdout.print((-4).abs(), (-2.5).abs())\n";
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["dev", "fallbacks", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "everything compiles to native code\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_dicts_length_is_native() {
    let dir = scratch("dict-length").unwrap();
    let program = dir.join("size.fen");
    std::fs::write(
        &program,
        "def size(table: dict[int, int]) -> int:\n    table.len()\n\ndef main(stdout: Stdout):\n    \
         mut table: dict[int, int] = {}\n    table[1] = 2\n    stdout.print(size(table))\n",
    )
    .unwrap();
    let output = fennec(&["dev", "fallbacks", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "everything compiles to native code\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_dicts_get_with_a_negative_default_is_native() {
    let dir = scratch("negative-default").unwrap();
    let program = dir.join("table.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    mut table: dict[int, int] = {}\n    table[1] = 2\n    \
         stdout.print(table.get(3) else -1)\n",
    )
    .unwrap();
    let output = fennec(&["dev", "fallbacks", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "everything compiles to native code\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dev_fallbacks_lists_what_isnt_native_hot_loops_first() {
    // A dict of lists isn't native yet.
    let dir = scratch("fallbacks").unwrap();
    let program = dir.join("groups.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    mut groups: dict[int, list[int]] = {}\n    for i in range(10):\n        \
         groups[i % 3] = (groups.get(i % 3) else []) + [i]\n    stdout.print(groups.len())\n",
    )
    .unwrap();
    let output = fennec(&["dev", "fallbacks", program.to_str().unwrap()], "").unwrap();
    assert!(output.status.success());
    let printed = text(&output.stdout);
    let first = printed.lines().next().unwrap();
    assert!(first.contains("groups.fen:4:"), "{printed}");
    assert!(first.contains("in `main` (in a loop)"), "{printed}");
    assert!(printed.contains("the interpreter calls the method `get`"), "{printed}");
    assert!(printed.contains("    groups[i % 3] = (groups.get(i % 3) else []) + [i]\n"), "{printed}");
    assert!(printed.ends_with("places aren't native\n"), "{printed}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn run_compiles_by_default_and_shows_output_before_it_reads_input() {
    let dir = scratch("default-run").unwrap();
    let program = dir.join("prompt.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout, stdin: Stdin):\n    stdout.print(\"name?\")\n    stdout.print(stdin.lines())\n",
    )
    .unwrap();
    for engine in [&["run"][..], &["run", "--interp"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fennec"))
            .args(engine)
            .arg(&program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // The prompt arrives while the program is still waiting for its input.
        let mut stdout = child.stdout.take().unwrap();
        let mut prompt = [0; 6];
        std::io::Read::read_exact(&mut stdout, &mut prompt).unwrap();
        assert_eq!(&prompt, b"name?\n");
        child.stdin.take().unwrap().write_all(b"fox\n").unwrap();
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut rest).unwrap();
        assert_eq!(rest, "[\"fox\"]\n");
        assert!(child.wait().unwrap().success());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// A program whose memory has to stay small: it's stopped, and fails, past `limit_kb`.
#[cfg(unix)]
fn peak_is_under(program: &Path, limit_kb: u64) -> Result<(), String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("run")
        .arg(program)
        .stdout(Stdio::null())
        .env("FENNEC_NO_CACHE", "1")
        .spawn()
        .map_err(|err| err.to_string())?;
    let pid = child.id().to_string();
    loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            return if status.success() { Ok(()) } else { Err(format!("it failed: {status}")) };
        }
        let rss =
            Command::new("ps").args(["-o", "rss=", "-p", &pid]).output().map_err(|err| err.to_string())?;
        let used: u64 = text(&rss.stdout).trim().parse().unwrap_or(0);
        if used > limit_kb {
            child.kill().ok();
            child.wait().ok();
            return Err(format!("it reached {used} KB"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// What a loop body's last statement leaves must go each time round. It used to pile up, and
/// each one kept a copy of the dict below alive: this program reached gigabytes.
#[cfg(unix)]
#[test]
fn a_loop_changing_a_dict_in_a_struct_stays_small() {
    let dir = scratch("loop-memory").unwrap();
    let program = dir.join("counter.fen");
    std::fs::write(
        &program,
        "struct Counter:\n    counts: dict[int, int]\n\n    def bump(mut self, key: int):\n        \
         self.counts[key] = (self.counts.get(key) else 0) + 1\n\ndef main(stdout: Stdout):\n    mut counter = \
         Counter(counts={})\n    for i in range(20_000):\n        counter.bump(i * 7919 % 10_007)\n    \
         stdout.print(counter.counts.len())\n",
    )
    .unwrap();
    peak_is_under(&program, 200_000).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
