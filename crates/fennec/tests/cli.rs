//! The `fennec` command itself: `run`, `test`, `new`, `dev`, `--version`, and what it does
//! with bad input. These run the real binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

fn fennec(args: &[&str], stdin: &str) -> std::io::Result<Output> {
    fennec_in(&repo(), args, stdin)
}

fn fennec_in(dir: &Path, args: &[&str], stdin: &str) -> std::io::Result<Output> {
    let mut child = crate::executables::fennec_command()
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

/// What the linter finds doesn't stop a run, and it's on standard error, so what the program prints
/// is all of standard output. A run of the same code again shows it again, from the cache.
#[test]
fn run_shows_lints_on_standard_error_and_still_runs() {
    let dir = scratch("run_lints").unwrap();
    std::fs::write(
        dir.join("game.fen"),
        "def main(stdout: Stdout):\n    mut speed = 4\n    stdout.print(speed)\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    for time in ["first", "again"] {
        let output = run(&["run", "game.fen"]);
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(text(&output.stdout), "4\n", "{time}");
        let said = text(&output.stderr);
        assert!(
            said.contains("needless-mut") && said.contains("1 lints, 1 of which `fennec fix` can fix"),
            "{time}: {said}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A run lints again only the files of a kit that changed, and keeps the rest from the last run,
/// so what it shows after each edit has to be what linting all of the kit shows: an edit that adds
/// a lint to a body, one that takes it out, a new function nothing uses, which another file's lint
/// looks across files to find, and a nested `expect`, which changes what every file is linted by.
#[test]
fn a_run_after_an_edit_shows_the_lints_all_of_the_kit_has() {
    let dir = scratch("edits_linted").unwrap();
    assert!(fennec_in(&dir, &["new", "demo"], "").unwrap().status.success());
    let kit = dir.join("demo");
    let maths = "## Twice the number.\nexport def twice(number: int) -> int:\n    number * 2\n";
    let main = "from maths import twice\n\ndef main(stdout: Stdout):\n    mut doubled = twice(21)\n    stdout.print(doubled)\n";
    std::fs::write(kit.join("src/maths.fen"), maths).unwrap();
    std::fs::write(kit.join("src/main.fen"), main).unwrap();
    let run = |cached: bool| {
        let mut command = crate::executables::fennec_command();
        command.args(["run", "--no-test"]).current_dir(&kit).env("FENNEC_HOME", dir.join("home"));
        if cached {
            command.env_remove("FENNEC_NO_CACHE");
        } else {
            command.env("FENNEC_NO_CACHE", "1");
        }
        command.output().unwrap()
    };
    let edits = [
        maths.to_owned(),
        maths.replace("    number * 2", "    mut twice = number * 2\n    twice"),
        maths.to_owned(),
        format!("{maths}\ndef unused(number: int) -> int:\n    number\n"),
        maths.replace("    number * 2", "    def expect(x: int) -> int:\n        x\n    number * 2"),
        maths.to_owned(),
    ];
    for edit in edits {
        std::fs::write(kit.join("src/maths.fen"), &edit).unwrap();
        let (cached, whole) = (run(true), run(false));
        assert_eq!(text(&cached.stdout), text(&whole.stdout), "after {edit:?}");
        assert_eq!(text(&cached.stderr), text(&whole.stderr), "after {edit:?}");
        assert!(text(&cached.stderr).contains("needless-mut"), "{}", text(&cached.stderr));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A run lints from the program it loaded when `main` reaches every file of the kit, and loads
/// all of it again when it doesn't, so a file nothing imports is linted too: after an edit, what
/// a run shows is what `fennec lint` shows, either way.
#[test]
fn a_run_lints_the_files_main_doesnt_import_too() {
    let dir = scratch("unimported_linted").unwrap();
    assert!(fennec_in(&dir, &["new", "demo"], "").unwrap().status.success());
    let kit = dir.join("demo");
    let main = "def main(stdout: Stdout):\n    mut doubled = 42\n    stdout.print(doubled)\n";
    std::fs::write(kit.join("src/main.fen"), main).unwrap();
    let spare = "## Half the number.\nexport def half(number: int) -> int:\n    mut halved = number / 2\n    halved\n";
    std::fs::write(kit.join("src/spare.fen"), spare).unwrap();
    let fennec = |args: &[&str]| {
        let mut command = crate::executables::fennec_command();
        command
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE");
        command.output().unwrap()
    };
    for edit in ["42", "43"] {
        std::fs::write(kit.join("src/main.fen"), main.replace("42", edit)).unwrap();
        let ran = fennec(&["run", "--no-test"]);
        assert!(ran.status.success(), "{}", text(&ran.stderr));
        let said = text(&ran.stderr);
        assert!(said.contains("src/spare.fen") && said.contains("src/main.fen"), "after {edit}: {said}");
        assert!(said.contains("2 lints, 2 of which `fennec fix` can fix"), "after {edit}: {said}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The README's sample is the first Fennec code most people see, so it has to run as it's
/// shown, formatted and with nothing for the linter to say.
#[test]
fn the_readme_sample_runs() {
    let readme = std::fs::read_to_string(repo().join("README.md")).unwrap();
    let taste = readme.split("## A taste").nth(1).expect("the README has a section `## A taste`");
    let sample = taste
        .split_once("```python\n")
        .and_then(|(_, rest)| rest.split_once("```"))
        .map(|(code, _)| code)
        .expect("`## A taste` starts with a ```python block");
    let dir = scratch("readme").unwrap();
    std::fs::write(dir.join("taste.fen"), sample).unwrap();
    for command in [&["run", "taste.fen"][..], &["fmt", "--check", "taste.fen"], &["lint", "taste.fen"]] {
        let output = fennec_in(&dir, command, "").unwrap();
        assert!(output.status.success(), "`fennec {}` failed:\n{}", command.join(" "), text(&output.stderr));
        if command[0] == "run" {
            assert_eq!(text(&output.stdout), "95\n");
        }
    }
}

/// Skirmish, the proof program in `examples/proof`, is the biggest Fennec program there is, so
/// what breaks it has to fail here: it checks, passes its tests, and prints its `.out` file in
/// both engines.
#[test]
fn the_proof_program_checks_passes_its_tests_and_runs() {
    let proof = repo().join("examples/proof");
    let expected = std::fs::read_to_string(proof.join("skirmish.out")).unwrap();
    for command in [&["check"][..], &["test", "--all"]] {
        let output = fennec_in(&proof, command, "").unwrap();
        assert!(output.status.success(), "`fennec {}` failed:\n{}", command.join(" "), text(&output.stderr));
    }
    let output = fennec_in(&proof, &["run", "--no-test"], "").unwrap();
    assert!(output.status.success(), "`fennec run --no-test` failed:\n{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), expected, "`fennec run --no-test` printed something different");
}

/// When a `with` block fails and its source's `else:` cleanup fails too, the cleanup's failure
/// rides along as a note on the block's, which is the one that passes up, and `main` prints it
/// under its failure. The cleanup runs once, where a whole line that's a `?` fails the block.
#[test]
fn a_failing_cleanup_is_a_note_on_the_blocks_failure() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-cleanup-notes", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("notes.fen");
    std::fs::write(
        &program,
        "error DbError:\n    why: str\n\nstruct Log:\n    lines = list[str]()\n\n\
         def transaction(ref log: Log) -> with int fails DbError:\n    lend 1 else:\n        \
         log.lines.append(\"rolled back\")\n        fail DbError(why=\"lost the connection\")\n    pass\n\n\
         def insert(n: int) fails DbError:\n    if n > 1: fail DbError(why=\"duplicate row\")\n\n\
         def main(stdout: Stdout) fails DbError:\n    mut log = Log()\n    \
         with transaction(ref log)? as tx:\n        insert(tx + 4)?\n    stdout.print(log.lines)\n",
    )
    .unwrap();
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["run", "--no-test", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let printed = text(&ran.stderr);
    assert!(!ran.status.success());
    assert!(printed.contains("`main` failed: DbError(why=\"duplicate row\")"), "{printed}");
    assert_eq!(printed.matches("note: DbError(why=\"lost the connection\")").count(), 1, "{printed}");
}

#[test]
fn run_hands_main_its_arguments_and_input() {
    let program = "examples/args_and_stdin.fen";
    let output = fennec(&["run", program, "fire", "two words"], "one\ntwo\r\n").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "2 arguments: FIRE TWO WORDS\nthe first is fire\n2 lines of input\n> one\n> two\n"
    );
    let output = fennec_on("run", "program_name", "def main(stdout: Stdout, args: Args):\n    stdout.print(args.program.endswith(\"program_name.fen\"))\n").unwrap();
    assert_eq!(text(&output.stdout), "True\n");
}

/// `stdin.read_line()` and `stdin.read_chunk(size)` read standard input a piece at a time, through
/// the one buffer `lines()` reads too, so nothing's lost between them: a line without its ending,
/// `\r\n` too, then bytes, then the lines left, and `None` and empty bytes at the end.
#[test]
fn stdin_reads_a_line_or_a_chunk_at_a_time() -> std::io::Result<()> {
    let dir = scratch("stdin-pieces")?;
    let program = dir.join("pieces.fen");
    std::fs::write(
        &program,
        "def main(stdin: Stdin, stdout: Stdout) fails IoError:\n    stdout.print(stdin.read_line()?)\n    \
         stdout.print(stdin.read_line()?)\n    stdout.print(stdin.read_chunk(5)?.decode() else \"?\")\n    \
         stdout.print(stdin.lines())\n    stdout.print(stdin.read_line()?, stdin.read_chunk(5)?.len())\n",
    )?;
    let output =
        fennec_in(&dir, &["run", "--no-test", &program.to_string_lossy()], "one\ntwo\r\nthree four\nlast\n")?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "one\ntwo\nthree\n[\" four\", \"last\"]\nNone 0\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// `stdin.reader()` makes standard input a `Reader`, from `std.io`, so code written for any
/// stream reads it: here a line, then the lines left, through a function that takes any reader.
#[test]
fn stdin_is_a_reader_through_reader() -> std::io::Result<()> {
    let dir = scratch("stdin-reader")?;
    let program = dir.join("reader.fen");
    std::fs::write(
        &program,
        "from std.io import Reader\n\n\
         def counted[R: Reader](ref src: R) -> int fails IoError:\n    \
         mut count = 0\n    for _ in src.lines()?:\n        count += 1\n    count\n\n\
         def main(stdin: Stdin, stdout: Stdout) fails IoError:\n    mut src = stdin.reader()\n    \
         stdout.print(src.read_line()?)\n    stdout.print(counted(ref src)?)\n",
    )?;
    let output =
        fennec_in(&dir, &["run", "--no-test", &program.to_string_lossy()], "first\nsecond\r\nthird\n")?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "first\n2\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A signal sent to `fennec run`, like a supervisor's SIGTERM, reaches the program, which can stop
/// cleanly, and `fennec run` exits as the program does. It once ran the program as a child it
/// didn't pass signals to, so the SIGTERM ended `fennec run` and left the program running.
#[cfg(unix)]
#[test]
fn a_signal_sent_to_run_reaches_the_program() -> std::io::Result<()> {
    use std::io::BufRead;
    let dir = scratch("run-signal")?;
    let program = dir.join("serve.fen");
    std::fs::write(
        &program,
        "def main(signals: Signals, clock: Clock, stdout: Stdout):\n    stdout.print(\"ready\")\n    \
         while not signals.stopping():\n        clock.sleep(10ms)\n    stdout.print(\"stopped cleanly\")\n",
    )?;
    let mut run = crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .arg(&program)
        .env("FENNEC_HOME", &dir)
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = run.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
    let (says, said) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = says.send(line);
        }
    });
    let line = || said.recv_timeout(crate::executables::WALL_DEADLINE);
    assert_eq!(line().as_deref(), Ok("ready"));
    let sent = std::process::Command::new("kill").args(["-TERM", &run.id().to_string()]).status()?;
    assert!(sent.success(), "couldn't send SIGTERM");
    assert_eq!(line().as_deref(), Ok("stopped cleanly"));
    assert_eq!(run.wait()?.code(), Some(0));
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn run_reports_errors_and_fails() {
    let output = fennec(&["run", "examples/errors/print.fen"], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: there's no global `print`"));
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
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-cli-{}-{name}", std::process::id())));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let output = fennec(&[command, &program.to_string_lossy()], "");
    std::fs::remove_dir_all(&dir)?;
    output
}

#[test]
fn run_stops_runaway_recursion_with_an_error() {
    // The depth limit, not a stack overflow, is what stops a program that recurses forever.
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

/// `assert value fails Pattern` holds when the value fails with what the pattern matches, and a
/// failed one says what really happened: what it failed with instead, or what it gave.
#[test]
fn an_assert_that_something_fails_says_what_really_happened() {
    let output = fennec(&["test", "crates/fennec/tests/fixtures/assert_fails.fen"], "").unwrap();
    assert_eq!(text(&output.stdout), "  ✗ fails some other way\n  ✗ gives a value\n1 passed, 2 failed\n");
    let stderr = text(&output.stderr);
    assert!(stderr.contains("= help: it failed with Oops(line=4, why=\"worse\")"), "{stderr}");
    assert!(stderr.contains("= help: it gave Config(size=4)"), "{stderr}");
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
            "it failed for x = 115.375; the left side is 115.375 and the right side is 115.0",
            "it failed for x = 9.0; the left side is 9.0 and the right side is 2.0",
            "it failed for word = \"aik\"; the left side is 3 and the right side is 3",
            "it failed for items = [1, 320]; the left side is 2 and the right side is 2",
            "it failed for grid = [[\"\", \"pff\", \"rm\", \"clih\", \"\", \"feoiln\"], [\"kuys\", \"eyl\"], [\"gssnvg\", \
             \"zevmv\", \"enshyp\"]]; the left side is 3 and the right side is 3",
            "it failed for deep = [[[[], [], []], [[], []], [[], [], []]], [[[], [], [], [], []], [[], [], []], [[], [], \
             [], [], []]]]; the left side is 2 and the right side is 2",
            "it failed for deep = ((([], 373), 1), -117); the left side is 373 and the right side is 100",
            "it failed for outer = Outer(inner=Inner(rows=[[], []])); the left side is 2 and the right side is 2",
            "it failed for pair = (False, 740)",
            "it failed for pair = (-936, 676); the left side is -936 and the right side is None",
            "it failed for box = Box(size=725, label=\"box\"); the left side is 725 and the right side is 3",
            "it failed for coin = Coin.Tails(times=1); the left side is Coin.Tails(times=1) and the right \
             side is Coin.Heads",
            "it failed for n = 942",
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
    assert!(text(&output.stderr).starts_with("error: `self` isn't `ref`"));
}

#[test]
fn tests_get_fakes_unless_asked_for_the_real_thing() {
    let dir = fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-real-{}", std::process::id())));
    std::fs::create_dir_all(&dir).unwrap();
    let written = dir.join("written.txt");
    let start = dir.join("start.txt");
    std::fs::write(&start, "from disk").unwrap();
    let source = format!(
        "test \"files\"(fs: Fs, out: Stdout, clock: Clock):\n    fs.make_dir({dir:?})?\n    fs.write_text({written:?}, \"hi\")?\n    assert fs.exists({written:?})?\n    out.print(\"printed\", fs.exists({start:?})?)\n    assert clock.now().nanos >= 0\n",
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

/// `--real` runs tests through LLVM too, with the real capabilities: what they print comes out in
/// order, around a real file written and read, and a failure says which assert failed.
#[test]
fn real_tests_run_through_llvm() {
    if !llvm_ready_for_run() {
        return;
    }
    let dir = scratch("real-through-llvm").unwrap();
    let written = dir.join("written.txt");
    let source = format!(
        "test \"prints\"(stdout: Stdout):\n    stdout.print(\"hello from a test\")\n\n\
         test \"a real file\"(fs: Fs, stdout: Stdout):\n    fs.write_text({written:?}, \"kept\")?\n    assert fs.read_text({written:?})? == \"kept\"\n    stdout.print(\"read it back\")\n\n\
         test \"fails\"(stdout: Stdout):\n    stdout.print(\"before the failure\")\n    assert 1 + 1 == 3\n"
    );
    let path = dir.join("real.fen");
    std::fs::write(&path, source).unwrap();
    let path = path.to_str().unwrap();
    let llvm = fennec(&["test", "--real", "--verbose", path], "").unwrap();
    let said = text(&llvm.stderr);
    assert!(said.contains("note: 3 of 3 tests ran through LLVM"), "{said}");
    assert!(
        text(&llvm.stdout).starts_with("hello from a test\nread it back\nbefore the failure\n"),
        "{}",
        text(&llvm.stdout)
    );
    assert!(said.contains("assert 1 + 1 == 3"), "{said}");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "kept");
    assert!(!llvm.status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A standard library type's default, like `Response`'s `headers = dict[str, str]()`, works in a
/// test that doesn't run through LLVM: here the file's other test reaches what LLVM can't build
/// yet, so both go another way, and with `--real` too.
#[test]
fn a_standard_library_default_works_in_a_test_off_llvm() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-std-default-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("response.fen");
    let source = "from std.net import Response\n\ndef handle(path: str) -> Response:\n    match path:\n        case \"/hello\":\n            Response(status=200, body=\"hi\".encode())\n        case _:\n            Response(status=404)\n\ntest \"plain\":\n    assert handle(\"/hello\").status == 200\n    assert handle(\"/\").headers.len() == 0\n\ntest \"net only\"(net: Net):\n    with net.listen(0)? as server:\n        assert server.port >= 0\n";
    std::fs::write(&path, source).unwrap();
    let path = path.to_str().unwrap();
    for how in ["--all", "--real"] {
        let ran = fennec(&["test", how, path], "").unwrap();
        assert!(ran.status.success(), "{how}: {}{}", text(&ran.stdout), text(&ran.stderr));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_clock_is_fixed_unless_asked_for_the_real_one() {
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-clock-{}", std::process::id())));
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
fn version_shows_the_fox() {
    let output = fennec(&["--version"], "").unwrap();
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), format!("🦊 fennec {}\n", env!("CARGO_PKG_VERSION")));
}

/// A program with nothing in it that asks for a notice says so.
#[test]
fn notices_says_when_a_program_holds_none() {
    let dir = scratch("notices").unwrap();
    std::fs::write(dir.join("program"), b"\x7fELF and nothing else").unwrap();
    let output = fennec_in(&dir, &["notices", "program"], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "program holds no third-party notices: nothing in it asks for one\n");
}

#[test]
fn unknown_commands_show_usage() {
    let output = fennec(&["fly"], "").unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).starts_with("usage:"));
}

/// A fresh empty folder for one test.
fn scratch(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-cli-{name}-{}", std::process::id())));
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
    let output = fennec_in(&dir, &["run", "main.fen"], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "True\n\
         [\"saves/slots/a.txt\", \"saves/slots/b.txt\"] ay\n\
         3 True False False True\n\
         a folder has to be empty to remove\n\
         False [Path(text=\"./main.fen\")]\n"
    );
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
    let output = fennec_in(&dir, &["run", "main.fen"], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "bytes([0, 1, 254, 255]) not text\nbytes([104, 195, 169]) bytes([])\n");
    assert_eq!(std::fs::read(dir.join("save.bin")).unwrap(), [0, 1, 254, 255]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program that changes files, which runs in a folder of its own.
const PROGRAM_WITH_FILES: &str = r#"from std.path import Path

def main(stdout: Stdout, fs: Fs):
    slots = Path("saves") / "slots"
    fs.make_dir(slots)?
    # Making a folder that's already there is fine.
    fs.make_dir(slots)?
    stdout.print(fs.exists(slots)?)
    fs.write_text(slots / "b.txt", "bee")?
    fs.write_text("saves/slots/a.txt", "ay")?
    stdout.print([path.text for path in fs.list_dir(slots)?], fs.read_text(slots / "a.txt")?)
    found = fs.metadata(slots / "b.txt")?
    folder = fs.metadata(slots)?
    stdout.print(found.size, found.is_file, found.is_dir, folder.is_file, folder.is_dir)
    fs.remove("saves") else as why:
        match why:
            case .Other(): stdout.print("a folder has to be empty to remove")
            case _: stdout.print(why.message())
    for path in fs.list_dir(slots)?:
        fs.remove(path)?
    fs.remove(slots)?
    fs.remove("saves")?
    stdout.print(fs.exists("saves")?, fs.list_dir(".")?)
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
fn run_takes_a_kits_folder() {
    let expected = std::fs::read_to_string(repo().join("examples/projects/arena.out")).unwrap();
    let output = fennec(&["run", "examples/projects/arena"], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), expected);
    // LLVM takes arena, so there's nothing to say.
    let said = text(&output.stderr);
    assert_eq!(said.lines().count(), 0, "{said}");
}

/// Whether `fennec run` builds through LLVM here: LLVM's tools are found, and
/// the runtime library is beside the `fennec` these tests run, which is where `fennec run`
/// looks. Under coverage, `fennec` is built somewhere else, without one.
fn llvm_ready_for_run() -> bool {
    fennec_llvm::Tools::find().is_ok()
        && crate::executables::runtime_built()
        && crate::executables::runtime_beside_fennec()
}

/// A conversion to the type a value has already gives it back. The linter takes these out of the examples, since they change nothing.
#[test]
fn conversions_to_what_a_value_is_already_give_it_back() {
    let dir = scratch("same-type").unwrap();
    let program = dir.join("same_type.fen");
    let source = "def main(stdout: Stdout):\n    name = \"fox\"\n    half = 0.5\n    seven = 7\n    \
                  stdout.print(str(name), float(half), int(seven), float(-0.0))\n";
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["run", "--no-test", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "fox 0.5 7 -0.0\n", "{}", text(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

/// `fennec fix` writes a key looked up after asking for it with `get`, and however the program
/// runs, it prints what it did.
#[test]
fn a_double_lookup_fixed_prints_the_same() {
    let dir = scratch("double-lookup").unwrap();
    let program = dir.join("double_lookup.fen");
    let source = "def main(stdout: Stdout):\n    counts = {\"a\": 1}\n    names = {\"a\": \"fox\"}\n    \
                  for key in [\"a\", \"b\"]:\n        stdout.print(counts[key] if key in counts else 7)\n        \
                  stdout.print(8 if key not in counts else counts[key])\n        \
                  stdout.print((names[key] if key in names else \"wolf\").len())\n";
    std::fs::write(&program, source).unwrap();
    let path = program.to_str().unwrap();
    let expected = "1\n1\n3\n7\n8\n4\n";
    assert_eq!(text(&fennec(&["run", path], "").unwrap().stdout), expected);
    let fixed = fennec(&["fix", path], "").unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    let now = std::fs::read_to_string(&program).unwrap();
    assert_eq!(now.matches(".get(").count(), 3, "{now}");
    assert!(!now.contains("in counts") && !now.contains("in names"), "{now}");
    let output = fennec(&["run", path], "").unwrap();
    assert_eq!(text(&output.stdout), expected, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "", "it ran as asked");
    std::fs::remove_dir_all(dir).unwrap();
}

/// `fennec fix` writes what used `Ok`, `Err` and `Result`, which are refused now: a `match` on
/// `Ok` and `Err` with `else`, where a function's last line returns from its failure case, a named
/// value keeps its name, and a loop's last statement continues; `-> Result[T, E]` as
/// `-> T fails E`; and `return Err(e)` as `fail e`. The program runs once fixed.
#[test]
fn what_used_ok_err_and_result_is_fixed_and_runs() {
    let dir = scratch("results-fix").unwrap();
    let program = dir.join("results_fix.fen");
    let source = "error Bad:\n    why: str\n\ndef lower(n: int) -> Result[int, Bad]:\n    if n < 1:\n        \
                  return Err(Bad(\"small\"))\n    n - 1\n\ndef first(n: int) -> int:\n    match lower(n):\n        \
                  case Ok(lowered): lowered\n        case Err(_): 0\n\ndef main(stdout: Stdout):\n    \
                  for n in [0, 3]:\n        given = match lower(n):\n            case Ok(lowered): lowered\n            \
                  case Err(_): 9\n        stdout.print(given, first(n))\n        match lower(n):\n            \
                  case Ok(lowered):\n                stdout.print(lowered)\n            case Err(problem):\n                \
                  stdout.print(problem.why)\n";
    std::fs::write(&program, source).unwrap();
    let path = program.to_str().unwrap();
    let expected = "9 0\nsmall\n2 2\n2\n";
    let refused = fennec(&["run", path], "").unwrap();
    assert!(text(&refused.stderr).contains("`Ok` and `Err` are gone"), "{}", text(&refused.stderr));
    let fixed = fennec(&["fix", path], "").unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    let now = std::fs::read_to_string(&program).unwrap();
    assert!(!now.contains("Ok(") && !now.contains("Err(") && !now.contains("Result["), "{now}");
    for written in [
        "def lower(n: int) -> int fails Bad:\n",
        "        fail Bad(\"small\")\n",
        "lowered = lower(n) else:\n        return 0\n    lowered\n",
        "given = lower(n) else:\n            9\n",
        "lowered = lower(n) else as problem:\n            stdout.print(problem.why)\n            continue\n",
    ] {
        assert!(now.contains(written), "{now}");
    }
    let output = fennec(&["run", path], "").unwrap();
    assert_eq!(text(&output.stdout), expected, "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "", "it ran as asked");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A program that fails while running exits with 1, and one that returns normally with 0.
#[test]
fn run_exits_with_1_when_the_program_fails() {
    let dir = scratch("exit-codes").unwrap();
    let failing = dir.join("failing.fen");
    std::fs::write(
        &failing,
        "def at(n: int) -> int:\n    [1][n]\n\ndef main(stdout: Stdout):\n    stdout.print(at(1))\n",
    )
    .unwrap();
    let fine = dir.join("fine.fen");
    std::fs::write(&fine, "def main(stdout: Stdout):\n    stdout.print(1)\n").unwrap();
    for (program, succeeds) in [(&failing, false), (&fine, true)] {
        let path = program.to_str().unwrap();
        let code = fennec(&["run", "--no-test", path], "").unwrap().status.code();
        assert_eq!(code, Some(i32::from(!succeeds)), "{path}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// With `FENNEC_RUN_WAITS`, `fennec run` starts the program and waits for it rather than becoming
/// it, as the edit-to-run budget counts it, and the run is otherwise the same: what it prints, and
/// the exit code `main` gives back.
#[test]
fn a_run_that_waits_for_its_program_prints_and_exits_the_same() {
    let dir = scratch("run-waits").unwrap();
    let program = dir.join("two.fen");
    std::fs::write(&program, "def main(stdout: Stdout) -> int:\n    stdout.print(\"ran\")\n    return 2\n")
        .unwrap();
    let path = program.to_str().unwrap();
    let mut command = crate::executables::fennec_command();
    let output = command
        .args(["run", "--no-test", path])
        .env("FENNEC_NO_CACHE", "1")
        .env("FENNEC_RUN_WAITS", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "ran\n");
    std::fs::remove_dir_all(dir).unwrap();
}

/// `main` may give back an int, which the process exits with: `return 2` exits with 2, and
/// `return 0` with 0. One that gives back an int or a failure exits with the int, or prints the
/// failure on standard error and exits with 1. The same on every engine, and built.
#[test]
fn main_gives_back_its_exit_code_on_every_engine() {
    let dir = scratch("main-exit-codes").unwrap();
    let programs = [
        ("two", "def main() -> int:\n    return 2\n", 2),
        ("zero", "def main() -> int:\n    return 0\n", 0),
        (
            "failing",
            "error Broke:\n    why: str\n\ndef main(stdout: Stdout) fails Broke:\n    stdout.print(\"before\")\n    fail Broke(why=\"no\")\n",
            1,
        ),
        (
            "given_or_failing",
            "error Broke:\n    why: str\n\ndef main(args: Args) -> int fails Broke:\n    if args.len() > 5:\n        fail Broke(why=\"many\")\n    return 3\n",
            3,
        ),
    ];
    for (name, source, code) in programs {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source).unwrap();
        let path = program.to_str().unwrap();
        let output = fennec(&["run", "--no-test", path], "").unwrap();
        let said = text(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{name}: {said}");
        if name == "failing" {
            assert!(said.starts_with("error: `main` failed: Broke(why=\"no\")"), "{said}");
            assert_eq!(text(&output.stdout), "before\n");
        }
        if llvm_ready_for_run() {
            let built = dir.join(name);
            let made = fennec(&["build", path, "-o", built.to_str().unwrap()], "").unwrap();
            assert!(made.status.success(), "{name}: {}", text(&made.stderr));
            let ran = std::process::Command::new(&built).output().unwrap();
            assert_eq!(ran.status.code(), Some(code), "{name}, built: {}", text(&ran.stderr));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
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
    crate::executables::fennec_command()
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
        for at in range(2):\n        stdout.print(f\"{at}\")\n    stdout.print(snapshot.len(), xs.len())\n";
    let dir = scratch("copies").unwrap();
    let program = dir.join("game.fen");
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["dev", "copies", &program.to_string_lossy()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    let lines: Vec<&str> = printed.lines().filter(|line| line.contains(" in `")).collect();
    assert!(lines[0].contains("(in a loop): this f-string may allocate"), "{printed}");
    assert!(
        printed.contains("`xs` is copied here, since `snapshot` still holds it and is used again"),
        "{printed}"
    );
    assert!(!printed.contains("unused"), "only what `main` reaches: {printed}");
    assert!(printed.ends_with("of them in loops\n"), "{printed}");
}

/// The last lines of `fennec dev copies --run` on a program that does nothing else.
fn counted_in_a_run(name: &str, body: &str) -> String {
    let dir = match scratch(name) {
        Ok(dir) => dir,
        Err(err) => return format!("couldn't make its folder: {err}"),
    };
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

/// The tour says every sample not marked planned compiles, so each one is checked as it's shown.
#[test]
fn every_tour_sample_not_marked_planned_compiles() {
    let tour = std::fs::read_to_string(repo().join("site/src/pages/tour.md")).unwrap();
    let dir = scratch("tour").unwrap();
    let mut checked = 0;
    let mut lines = tour.lines().enumerate();
    while let Some((at, line)) = lines.next() {
        let Some(info) = line.strip_prefix("```fennec") else { continue };
        let sample: Vec<&str> =
            lines.by_ref().map(|(_, line)| line).take_while(|line| !line.starts_with("```")).collect();
        if info.contains("planned") {
            continue;
        }
        let name = format!("line_{}.fen", at + 2);
        std::fs::write(dir.join(&name), sample.join("\n") + "\n").unwrap();
        let output = fennec_in(&dir, &["check", &name], "").unwrap();
        assert!(
            output.status.success(),
            "the tour's sample at line {} doesn't compile:\n{}",
            at + 2,
            text(&output.stderr)
        );
        checked += 1;
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(checked > 30, "found only {checked} samples in the tour");
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

/// After a run, an edit that changes only functions' bodies is checked on its own, and one that
/// changes a signature checks the whole kit: either way, a mistake anywhere is found, and a good
/// edit runs.
#[test]
fn a_run_after_an_edit_finds_its_mistakes_however_it_checks() {
    let dir = scratch("edits_checked").unwrap();
    assert!(fennec_in(&dir, &["new", "demo"], "").unwrap().status.success());
    let kit = dir.join("demo");
    let maths = "## Twice the number.\nexport def twice(number: int) -> int:\n    number * 2\n";
    std::fs::write(kit.join("src/maths.fen"), maths).unwrap();
    std::fs::write(
        kit.join("src/main.fen"),
        "from maths import twice\n\ndef main(stdout: Stdout):\n    stdout.print(twice(21))\n",
    )
    .unwrap();
    let run = || {
        crate::executables::fennec_command()
            .args(["run", "--no-test"])
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    let first = run();
    assert_eq!(text(&first.stdout), "42\n", "{}", text(&first.stderr));
    // Only a body changes, and it's good.
    std::fs::write(kit.join("src/maths.fen"), maths.replace("number * 2", "number + number + 1")).unwrap();
    assert_eq!(text(&run().stdout), "43\n");
    // Only a body changes, and it's a mistake.
    std::fs::write(kit.join("src/maths.fen"), maths.replace("number * 2", "number * \"two\"")).unwrap();
    let broken = run();
    assert!(
        !broken.status.success() && text(&broken.stderr).contains("maths.fen"),
        "{}",
        text(&broken.stderr)
    );
    // The signature changes, which breaks the call in another file.
    std::fs::write(
        kit.join("src/maths.fen"),
        maths.replace("number: int", "number: str").replace("number * 2", "0"),
    )
    .unwrap();
    let caller = run();
    assert!(
        !caller.status.success() && text(&caller.stderr).contains("main.fen"),
        "{}",
        text(&caller.stderr)
    );
    // Put right, it runs.
    std::fs::write(kit.join("src/maths.fen"), maths).unwrap();
    assert_eq!(text(&run().stdout), "42\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Everything Fennec keeps between commands goes in its store, so a kit's folder holds only its own
/// files after running, testing, checking, linting, formatting and building it, cache and all. A
/// build writes the program where `-o` says. See `docs/store.md`.
#[test]
fn a_kit_holds_only_its_own_files_after_every_command() {
    let dir = scratch("own_files").unwrap();
    assert!(fennec_in(&dir, &["new", "demo"], "").unwrap().status.success());
    let kit = dir.join("demo");
    let listed = |kit: &Path| -> Vec<String> {
        fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                found.push(path.strip_prefix(root).unwrap().display().to_string());
                if path.is_dir() {
                    walk(&path, root, found);
                }
            }
        }
        let mut found = Vec::new();
        walk(kit, kit, &mut found);
        found.sort();
        found
    };
    let before = listed(&kit);
    let program = dir.join("built");
    let program = program.to_string_lossy();
    let mut commands: Vec<Vec<&str>> =
        vec![vec!["run"], vec!["test"], vec!["check"], vec!["lint"], vec!["fmt"]];
    if crate::llvm::ready() {
        commands.push(vec!["build", "-o", &program]);
        commands.push(vec!["build", "--release", "-o", &program]);
    }
    for command in &commands {
        let output = crate::executables::fennec_command()
            .args(command)
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap();
        assert!(output.status.success(), "`fennec {}` failed: {}", command.join(" "), text(&output.stderr));
    }
    let after = listed(&kit);
    let stored = dir.join("home/cache").is_dir();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(after, before, "the kit's folder changed");
    assert!(stored, "nothing went in the store");
}

/// The linter checks only what the program reaches of the standard library, and still finds what
/// a function costs when it loops `for ref` over a library type, whose `len` the loop reaches.
#[test]
fn lint_finds_costs_past_a_loop_over_a_library_type() {
    let source = "from std.collections import Grid\n\ndef main(stdout: Stdout):\n    \
        mut grid = Grid.filled(width=2, height=2, value=1)\n    for ref cell in grid:\n        cell += 1\n    \
        mut names = list[str]()\n    for n in range(3):\n        names.append(f\"{n}\")\n    stdout.print(names)\n";
    let dir = scratch("lint_costs").unwrap();
    let program = dir.join("game.fen");
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["lint", "--json", &program.to_string_lossy()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let printed = text(&output.stdout);
    assert!(printed.contains("growing it with `list.append` may allocate"), "{printed}");
    assert!(printed.contains("this f-string may allocate"), "{printed}");
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
        "def one() -> int:\n    pass\n    return 1\n\ndef main(stdout: Stdout):\n    mut items = [1]\n    \
         held = items\n    items.append(2)\n    stdout.print(held.len(), items.len(), one())\n",
    )
    .unwrap();
    let (code, report) = json(&["lint", "--json", &path]);
    assert_eq!(code, Some(1));
    assert_eq!(rules(&report), ["needless-pass", "copies"]);
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

    // A format the value can't take is found by the check, not when the line runs.
    std::fs::write(&program, "def main(stdout: Stdout):\n    b = 7\n    stdout.print(f\"{b:02x}\")\n")
        .unwrap();
    let (code, report) = json(&["check", "--json", &path]);
    let message = report.at(&["diagnostics"]).and_then(fennec_lsp::Json::as_array).and_then(<[_]>::first);
    let message = message.and_then(|item| item.get("message")).and_then(fennec_lsp::Json::as_str);
    assert_eq!((code, message), (Some(1), Some("Fennec has no format `02x`")));
    let output = fennec(&["check", &path], "").unwrap();
    assert!(text(&output.stderr).contains("Fennec has no format `02x`"), "{}", text(&output.stderr));

    // A duration's unit is checked as it's read.
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(5m)\n").unwrap();
    let (code, report) = json(&["check", "--json", &path]);
    let message = report.at(&["diagnostics"]).and_then(fennec_lsp::Json::as_array).and_then(<[_]>::first);
    let message = message.and_then(|item| item.get("message")).and_then(fennec_lsp::Json::as_str);
    assert_eq!(code, Some(1));
    assert!(message.is_some_and(|message| message.contains("`5m` uses `m`, which isn't a unit of time")));

    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print((\n").unwrap();
    let (code, report) = json(&["lint", "--json", &path]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!((code, rules(&report)), (Some(1), vec!["load".to_owned()]));
}

/// What `fennec check --json` says first about something that fails to load, with how many errors
/// it counts.
#[derive(Debug, Default, PartialEq)]
struct LoadProblem {
    errors: Option<usize>,
    fixable: Option<usize>,
    rule: Option<String>,
    /// The file's last part.
    file: Option<String>,
    line: Option<usize>,
    column: Option<usize>,
    message: Option<String>,
    help: Option<String>,
}

fn load_problem(path: &Path) -> std::io::Result<LoadProblem> {
    let output = fennec(&["check", "--json", &path.to_string_lossy()], "")?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let report = fennec_lsp::Json::parse(text(&output.stdout).trim()).unwrap_or(fennec_lsp::Json::Null);
    let found = report.at(&["diagnostics"]).and_then(fennec_lsp::Json::as_array).and_then(<[_]>::first);
    let words = |key: &str| found?.get(key)?.as_str().map(str::to_owned);
    let number = |key: &str| found?.get(key)?.as_usize();
    Ok(LoadProblem {
        errors: report.get("errors").and_then(fennec_lsp::Json::as_usize),
        fixable: report.get("fixable").and_then(fennec_lsp::Json::as_usize),
        rule: words("rule"),
        file: words("file")
            .and_then(|file| Path::new(&file).file_name().map(|name| name.to_string_lossy().into_owned())),
        line: number("line"),
        column: number("column"),
        message: words("message"),
        help: words("help"),
    })
}

#[test]
fn check_json_gives_what_stops_a_load_a_place_and_its_help() {
    let dir = scratch("load_json").unwrap();
    let program = dir.join("game.fen");
    let some = |text: &str| Some(text.to_owned());

    std::fs::write(&program, "from nowhere import x\n\ndef main():\n    pass\n").unwrap();
    let found = load_problem(&program).unwrap();
    assert_eq!(found.help.as_deref().map(|help| help.ends_with("nowhere.fen")), Some(true), "{found:?}");
    assert_eq!(
        LoadProblem { help: None, ..found },
        LoadProblem {
            errors: Some(1),
            fixable: Some(0),
            rule: some("load"),
            file: some("game.fen"),
            line: Some(1),
            column: Some(1),
            message: some("there's no module called `nowhere`"),
            help: None,
        }
    );

    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print((\n").unwrap();
    let found = load_problem(&program).unwrap();
    assert_eq!((found.errors, &found.rule, &found.file), (Some(1), &some("load"), &some("game.fen")));
    assert!(found.line.is_some(), "{found:?}");
    assert!(found.message.as_deref().is_some_and(|message| !message.contains("-->")), "{found:?}");

    std::fs::write(&program, "MAP = embed_file(\"not_there.bin\")\n\ndef main():\n    pass\n").unwrap();
    let found = load_problem(&program).unwrap();
    assert_eq!((found.errors, &found.rule, &found.file), (Some(1), &some("load"), &some("game.fen")));
    assert_eq!((found.line, found.column), (Some(1), Some(7)));
    assert_eq!(found.message, some("there's no file `not_there.bin` to embed"));
    assert!(found.help.as_deref().is_some_and(|help| help.ends_with("not_there.bin")), "{found:?}");

    let kit = dir.join("kit");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::write(kit.join("kit.toml"), "name = \"demo\"\nversoin = \"0.1.0\"\n").unwrap();
    let found = load_problem(&kit).unwrap();
    assert_eq!(
        found,
        LoadProblem {
            errors: Some(3),
            fixable: Some(0),
            rule: some("load"),
            file: some("kit.toml"),
            line: Some(2),
            column: Some(1),
            message: some("kit.toml has no key called `versoin`"),
            help: some("kit.toml has `name`, `version` and `fennec`"),
        }
    );

    // With nothing to point at, the place is null and the message and help are still apart.
    std::fs::remove_file(kit.join("kit.toml")).unwrap();
    let found = load_problem(&kit).unwrap();
    assert_eq!((found.errors, &found.rule, &found.file, found.line), (Some(1), &some("load"), &None, None));
    assert!(found.message.as_deref().is_some_and(|message| message.starts_with("there's no kit.toml in ")));
    assert!(found.help.as_deref().is_some_and(|help| help.contains("`fennec new <name>`")));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit whose `Parser` gets `step` from a file that only extends it.
fn kit_with_an_extension(name: &str, imports: &str) -> std::io::Result<fennec_temp::Temp> {
    let kit = scratch(name)?;
    std::fs::create_dir_all(kit.join("src/parser"))?;
    std::fs::write(kit.join("kit.toml"), "name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")?;
    std::fs::write(kit.join("src/parser/core.fen"), "struct Parser:\n    at = 0\n")?;
    std::fs::write(
        kit.join("src/parser/step.fen"),
        "from parser.core import Parser\n\nextend Parser:\n    def step(self) -> int:\n        self.at + 1\n",
    )?;
    // Files it mustn't name: one extends `Parser` with something else, and one gives another type
    // a `step`. They sort before the file it should name.
    std::fs::write(
        kit.join("src/parser/also.fen"),
        "from parser.core import Parser\n\nextend Parser:\n    def other(self) -> int:\n        self.at\n",
    )?;
    std::fs::write(
        kit.join("src/parser/rival.fen"),
        "struct Rival:\n    at = 0\n\nextend Rival:\n    def step(self) -> int:\n        self.at\n",
    )?;
    std::fs::write(
        kit.join("src/main.fen"),
        format!(
            "from parser.core import Parser\n{imports}\ndef main(stdout: Stdout):\n    stdout.print(Parser().step())\n"
        ),
    )?;
    Ok(kit)
}

#[test]
fn a_run_says_which_file_has_a_missing_methods_extension() {
    let kit = kit_with_an_extension("extension_missing", "").unwrap();
    let path = kit.to_string_lossy().into_owned();
    let output = fennec(&["run", "--no-test", &path], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    let shown = text(&output.stderr);
    assert!(shown.contains("`Parser` has no method `step`"), "{shown}");
    assert!(
        shown.contains("`step` is in an `extend` in ")
            && shown.contains("which this program doesn't import: add `import parser.step`"),
        "{shown}"
    );
    assert!(shown.contains("parser/step.fen"), "{shown}");

    // `check` and `lint` say what a run would, with the same help.
    for command in ["check", "lint"] {
        let output = fennec(&[command, &path], "").unwrap();
        assert_eq!(output.status.code(), Some(1), "{command}");
        let shown = text(&output.stderr);
        assert!(shown.contains("`Parser` has no method `step`"), "{command}: {shown}");
        assert!(shown.contains("add `import parser.step`"), "{command}: {shown}");
    }
    let output = fennec(&["check", "--json", &path], "").unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = fennec_lsp::Json::parse(text(&output.stdout).trim()).unwrap_or(fennec_lsp::Json::Null);
    let found = report.at(&["diagnostics"]).and_then(fennec_lsp::Json::as_array).and_then(<[_]>::first);
    let words = |key: &str| found.and_then(|item| item.get(key)).and_then(fennec_lsp::Json::as_str);
    assert!(words("file").is_some_and(|file| file.ends_with("main.fen")), "{found:?}");
    assert!(words("help").is_some_and(|help| help.contains("add `import parser.step`")), "{found:?}");

    // Doing what it says works, and the linter leaves the import that does it alone.
    let kit = kit_with_an_extension("extension_imported", "import parser.step\n").unwrap();
    let path = kit.to_string_lossy().into_owned();
    let output = fennec(&["run", "--no-test", &path], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "1\n");
    let linted = fennec(&["lint", &path], "").unwrap();
    assert!(linted.status.success(), "{}", text(&linted.stderr));
    let checked = fennec(&["check", &path], "").unwrap();
    assert!(checked.status.success(), "{}", text(&checked.stderr));

    // A file nothing imports doesn't matter if nothing needs what it adds.
    let kit = kit_with_an_extension("extension_unneeded", "").unwrap();
    let main = "from parser.core import Parser\n\ndef main(stdout: Stdout):\n    stdout.print(Parser().at)\n";
    std::fs::write(kit.join("src/main.fen"), main).unwrap();
    let checked = fennec(&["check", &kit.to_string_lossy()], "").unwrap();
    assert!(checked.status.success(), "{}", text(&checked.stderr));

    // A kit with no `main` has no run to differ from.
    std::fs::remove_file(kit.join("src/main.fen")).unwrap();
    let checked = fennec(&["check", &kit.to_string_lossy()], "").unwrap();
    assert!(checked.status.success(), "{}", text(&checked.stderr));
}

/// A function a type parameter names, as in `T.make()`, runs natively.
#[test]
fn a_type_parameters_function_runs() {
    let dir = scratch("type_param").unwrap();
    let program = dir.join("made.fen");
    std::fs::write(
        &program,
        "interface Made:\n    def make() -> Self\n\nstruct Part:\n    x = 1\n\n    def make() -> Part:\n        Part(x=7)\n\n\
         def build[T: Made]() -> T:\n    T.make()\n\ndef main(stdout: Stdout):\n    stdout.print(build[Part]().x)\n",
    )
    .unwrap();
    let output = fennec(&["run", "--no-test", &program.to_string_lossy()], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(text(&output.stdout), "7\n", "{}", text(&output.stderr));
}

#[test]
fn lint_reports_and_fix_fixes_and_hints_dont_fail() {
    let dir = scratch("lint").unwrap();
    let program = dir.join("game.fen");
    let path = program.to_string_lossy().into_owned();
    let hinted = "def main(stdout: Stdout):\n    mut items = [1]\n    held = items\n    items.append(2)\n    \
                  stdout.print(held.len(), items.len())\n";
    std::fs::write(&program, hinted).unwrap();
    let clean = fennec(&["lint", &path], "").unwrap();
    assert!(clean.status.success(), "{}", text(&clean.stderr));
    assert!(text(&clean.stdout).contains("hint: "), "{}", text(&clean.stdout));
    assert!(
        text(&clean.stdout).contains("`items` is copied here, since `held` still holds it and is used again"),
        "{}",
        text(&clean.stdout)
    );

    let lints =
        "def one() -> int:\n    pass\n    return 1\n\ndef main(stdout: Stdout):\n    stdout.print(one())\n";
    std::fs::write(&program, lints).unwrap();
    let flagged = fennec(&["lint", &path], "").unwrap();
    assert_eq!(flagged.status.code(), Some(1));
    assert!(text(&flagged.stderr).contains("needless-pass:"), "{}", text(&flagged.stderr));
    assert!(text(&flagged.stderr).contains("1 lints, 1 of which"), "{}", text(&flagged.stderr));

    assert!(fennec(&["fix", &path], "").unwrap().status.success());
    let after = std::fs::read_to_string(&program).unwrap();
    assert!(!after.contains("pass") && after.contains("    return 1\n"), "{after}");
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
    crate::executables::fennec_command()
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
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-cli-{}-fix", std::process::id())));
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

/// Adding `mut` may change what a program does, so `fennec fix` shows it and leaves it.
#[test]
fn fix_leaves_a_fix_that_may_change_what_a_program_does_to_be_made_by_hand() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-by-hand", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("by_hand.fen");
    let source = "def main(stdout: Stdout):\n    people = [\"Ash\"]\n    people = [\"Vex\"]\n    stdout.print(people)\n";
    std::fs::write(&program, source).unwrap();
    let path = program.to_string_lossy().into_owned();
    let output = fennec(&["fix", &path], "").unwrap();
    let after = std::fs::read_to_string(&program).unwrap();
    let json = text(&fennec(&["check", "--json", &path], "").unwrap().stdout);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(after, source, "the source is left as it was");
    assert!(!output.status.success());
    let said = text(&output.stderr);
    assert!(said.contains("= fix: declare it `mut people`"), "{said}");
    assert!(said.contains("left to be made by hand"), "{said}");
    assert!(json.contains(r#""fixes":[],"hint":{"says":"declare it `mut people`","edits":[{"#), "{json}");
    assert!(json.contains(r#""line":2,"column":5,"end_line":2,"end_column":5"#), "{json}");
}

/// `fennec fix` renames a top-level `_name` everywhere it's written, without the `_`, in each of
/// a kit's files. It leaves one alone when the new name is taken, or an f-string uses it.
#[test]
fn fix_renames_a_top_level_name_that_starts_with_an_underscore() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-underscore", std::process::id())),
    );
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"tidy\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(src.join("parts.fen"), "_LIMIT = 3\n\nstruct _Box:\n    size: int\n\ndef _made(_count: int) -> _Box:\n    _Box(size=_count + _LIMIT)\n").unwrap();
    std::fs::write(
        src.join("main.fen"),
        "from parts import _made\n\ndef main(stdout: Stdout):\n    stdout.print(_made(_count=2).size)\n",
    )
    .unwrap();
    let path = dir.to_string_lossy().into_owned();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let parts = std::fs::read_to_string(src.join("parts.fen")).unwrap();
    let main = std::fs::read_to_string(src.join("main.fen")).unwrap();
    let ran = fennec(&["run", &path], "").unwrap();
    std::fs::write(src.join("main.fen"), "def helper() -> int:\n    1\n\ndef _helper() -> int:\n    2\n\ndef main(stdout: Stdout):\n    stdout.print(helper(), _helper())\n").unwrap();
    std::fs::write(
        src.join("parts.fen"),
        "def _shown() -> int:\n    3\n\ndef show() -> str:\n    f\"{_shown()}\"\n",
    )
    .unwrap();
    let left = fennec(&["fix", &path], "").unwrap();
    let kept = std::fs::read_to_string(src.join("main.fen")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        parts,
        "LIMIT = 3\n\nstruct Box:\n    size: int\n\ndef made(_count: int) -> Box:\n    Box(size=_count + LIMIT)\n"
    );
    assert_eq!(
        main,
        "from parts import made\n\ndef main(stdout: Stdout):\n    stdout.print(made(_count=2).size)\n"
    );
    assert_eq!(text(&ran.stdout), "5\n");
    assert!(!left.status.success());
    assert!(kept.contains("def _helper()"), "{kept}");
    let said = text(&left.stderr);
    assert!(
        said.contains("`_helper` is a top-level name") && said.contains("`_shown` is a top-level name"),
        "{said}"
    );
}

/// `fennec fix` gives a function that uses a capability it wasn't given the parameter, passes it
/// at each call, and gives each caller the parameter in turn, across a kit's files, up to the
/// first that has one, `main`, or a test. A function named other than in a call is left alone.
#[test]
fn fix_threads_a_capability_up_to_the_first_caller_that_has_it() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-threaded", std::process::id())),
    );
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"hello\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(
        src.join("main.fen"),
        "from greet import welcome\n\ndef main(out: Stdout):\n    welcome(\"Ann\")\n",
    )
    .unwrap();
    std::fs::write(
        src.join("greet.fen"),
        "export def welcome(name: str):\n    shout(f\"hello {name}\")\n    shout(\"again\")\n\ndef shout(text: str):\n    stdout.print(text)\n\ntest \"welcomes\":\n    welcome(\"Bo\")\n",
    )
    .unwrap();
    let path = dir.to_string_lossy().into_owned();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let main = std::fs::read_to_string(src.join("main.fen")).unwrap();
    let greet = std::fs::read_to_string(src.join("greet.fen")).unwrap();
    let ran = fennec(&["run", "--no-test", &path], "").unwrap();
    let as_value = "def shout(text: str):\n    stdout.print(text)\n\ndef main(out: Stdout):\n    each = shout\n    each(\"hi\")\n";
    std::fs::write(src.join("main.fen"), as_value).unwrap();
    std::fs::remove_file(src.join("greet.fen")).unwrap();
    let left = fennec(&["fix", &path], "").unwrap();
    let kept = std::fs::read_to_string(src.join("main.fen")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(main, "from greet import welcome\n\ndef main(out: Stdout):\n    welcome(out, \"Ann\")\n");
    assert_eq!(
        greet,
        "export def welcome(stdout: Stdout, name: str):\n    shout(stdout, f\"hello {name}\")\n    shout(stdout, \"again\")\n\ndef shout(stdout: Stdout, text: str):\n    stdout.print(text)\n\ntest \"welcomes\"(stdout: Stdout):\n    welcome(stdout, \"Bo\")\n"
    );
    assert_eq!(text(&ran.stdout), "hello Ann\nagain\n", "{}", text(&ran.stderr));
    assert!(!left.status.success());
    assert_eq!(kept, as_value);
    assert!(text(&left.stderr).contains("`stdout` doesn't exist"), "{}", text(&left.stderr));
}

/// `fennec fix` takes a plain `copy(...)` off, since every copy is a snapshot already, and keeps
/// one that changes fields. A loop over a copy goes over a name made the line before, as the loop
/// rule asks, rather than over what it changes.
#[test]
fn fix_takes_plain_copies_off_and_names_what_a_loop_goes_over() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-plain-copy", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("copies.fen");
    std::fs::write(
        &program,
        "struct Point:\n    x: int\n    y: int\n\ndef main(stdout: Stdout):\n    mut names = [\"a\", \"b\"]\n    for name in copy(names):\n        names.append(name)\n    mut grid = {1: [2]}\n    for key, row in copy(grid).items():\n        grid[key + 1] = row\n    kept = copy(names)\n    moved = copy(Point(x=1, y=2), x=5)\n    stdout.print(names, grid, kept, moved.x)\n",
    )
    .unwrap();
    let path = program.to_string_lossy().into_owned();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let ran = fennec(&["run", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        "struct Point:\n    x: int\n    y: int\n\ndef main(stdout: Stdout):\n    mut names = [\"a\", \"b\"]\n    names_before = names\n    for name in names_before:\n        names.append(name)\n    mut grid = {1: [2]}\n    grid_before = grid\n    for key, row in grid_before.items():\n        grid[key + 1] = row\n    kept = names\n    moved = copy(Point(x=1, y=2), x=5)\n    stdout.print(names, grid, kept, moved.x)\n"
    );
    assert_eq!(
        text(&ran.stdout),
        "[\"a\", \"b\", \"a\", \"b\"] {1: [2], 2: [2]} [\"a\", \"b\", \"a\", \"b\"] 5\n",
        "{}",
        text(&ran.stderr)
    );
}

/// `fennec fix` names the fields of a pattern matching another kit's type by position, and a
/// part that's only `_` goes, so it reads no field.
#[test]
fn fix_names_the_fields_a_pattern_matches_in_another_kits_type() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-named-pattern", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("read.fen");
    std::fs::write(&program, "def checked(text: str) -> str fails InvalidValue:\n    fail InvalidValue(message=text)\n\ndef main(stdout: Stdout):\n    text = checked(\"/nothing/here\") else as problem:\n        match problem:\n            case InvalidValue(why): str(why != \"\")\n    stdout.print(text)\n    said = checked(\"/nothing/here\") else as problem:\n        match problem:\n            case InvalidValue(_): \"failed\"\n    stdout.print(said)\n").unwrap();
    let path = program.to_string_lossy().into_owned();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let ran = fennec(&["run", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert!(written.contains("case InvalidValue(message=why):"), "{written}");
    assert!(written.contains("case InvalidValue():"), "{written}");
    assert_eq!(text(&ran.stdout), "True\nfailed\n", "{}", text(&ran.stderr));
}

/// Enums with a case in common, which only the expected type can tell apart, and an error.
const CASES_BEFORE: &str = "enum Color:
    Red
    Green(shade: int)

    def warm() -> Color:
        Color.Red

enum Light:
    Red
    Off

error DialError:
    NotFound(host: str)
    Closed

struct Car:
    color: Color

def pick(n: int) -> Color:
    if n == 0:
        return Color.Red
    Color.Green(n)

def maybe() -> Color?:
    Color.Red

def shown(color: Color) -> str:
    match color:
        case Red: \"red\"
        case Green(shade): f\"green {shade}\"

def dial(n: int) -> str fails DialError:
    if n == 0:
        fail DialError.Closed
    fail DialError.NotFound(\"host\")

def unsaid() -> str:
    fail DialError.Closed

def main(stdout: Stdout):
    car = Car(color=Color.Red)
    mut other: Color = Color.Green(1)
    stdout.print(shown(other))
    other = Color.Red
    kept = Color.Red
    many = [Color.Red]
    light = Light.Red
    warm: Color = Color.warm()
    stdout.print(shown(pick(0)), shown(car.color), other == Color.Red, shown(kept), many.len(), light == Light.Off)
    stdout.print(shown(warm), maybe() == Color.Red)
    dialled = dial(0) else as why:
        match why:
            case Closed: \"closed\"
            case _: \"other\"
    stdout.print(dialled)
    stdout.print(unsaid() else \"failed\")
";

/// What `CASES_AFTER` prints, as pinned.
const CASES_PRINTED: &str = "green 1\nred red True red 1 False\nred True\nclosed\nfailed\n";

/// What `fennec fix` makes of `CASES_BEFORE`: a dot wherever the type is known, and nothing where
/// it isn't, or where a dot would mean something else.
const CASES_AFTER: &str = "enum Color:
    Red
    Green(shade: int)

    def warm() -> Color:
        .Red

enum Light:
    Red
    Off

error DialError:
    NotFound(host: str)
    Closed

struct Car:
    color: Color

def pick(n: int) -> Color:
    if n == 0:
        return .Red
    .Green(n)

def maybe() -> Color?:
    .Red

def shown(color: Color) -> str:
    match color:
        case .Red: \"red\"
        case .Green(shade): f\"green {shade}\"

def dial(n: int) -> str fails DialError:
    if n == 0:
        fail .Closed
    fail .NotFound(\"host\")

def unsaid() -> str fails DialError:
    fail .Closed

def main(stdout: Stdout):
    car = Car(color=.Red)
    mut other: Color = .Green(1)
    stdout.print(shown(other))
    other = .Red
    kept = Color.Red
    many = [Color.Red]
    light = Light.Red
    warm: Color = Color.warm()
    stdout.print(shown(pick(0)), shown(car.color), other == .Red, shown(kept), many.len(), light == .Off)
    stdout.print(shown(warm), maybe() == .Red)
    dialled = dial(0) else as why:
        match why:
            case .Closed: \"closed\"
            case _: \"other\"
    stdout.print(dialled)
    stdout.print(unsaid() else \"failed\")
";

/// A case written with its type where the type is known doesn't compile, and `fennec fix` writes
/// `.Case` there, which runs and prints what it should.
#[test]
fn fix_writes_leading_dots_where_the_type_is_known() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-short-cases", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("cases.fen");
    let path = program.to_string_lossy().into_owned();
    std::fs::write(&program, CASES_BEFORE).unwrap();
    let before = fennec(&["check", &path], "").unwrap();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let again = fennec(&["fix", &path], "").unwrap();
    let native = fennec(&["run", "--no-test", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!before.status.success());
    assert!(text(&before.stderr).contains("the type here is known, so write"), "{}", text(&before.stderr));
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(written, CASES_AFTER);
    assert!(again.status.success() && again.stdout.is_empty(), "{}", text(&again.stdout));
    assert!(native.status.success(), "{}", text(&native.stderr));
    assert_eq!(text(&native.stdout), CASES_PRINTED, "{}", text(&native.stderr));
}

/// A constant worked out while compiling is written back as a literal, with its enum values named
/// in full. That's nothing to fix, even where one sits in a field whose type is known, as in a
/// constant list of an enum that holds itself, with a declared type or without.
#[test]
fn a_worked_out_constant_is_nothing_to_fix() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-baked-cases", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let declared = ["", ": list[Tok]"];
    for (at, written) in declared.iter().enumerate() {
        let items = if written.is_empty() {
            "[Tok.Name(\"x\"), Tok.Eof, Tok.Wrapped(PAREN)]"
        } else {
            "[.Name(\"x\"), .Eof, .Wrapped(PAREN)]"
        };
        let source = format!(
            "enum Tok:\n    Name(text: str)\n    Punct(text: str)\n    Wrapped(inner: Tok)\n    Eof\n\n\
             PAREN = Tok.Punct(\"(\")\nKNOWN{written} = {items}\n\n\
             def main(stdout: Stdout):\n    stdout.print(KNOWN[0] == .Name(\"x\"), KNOWN[2] == .Wrapped(.Punct(\"(\")))\n"
        );
        let program = dir.join(format!("baked{at}.fen"));
        let path = program.to_string_lossy().into_owned();
        std::fs::write(&program, &source).unwrap();
        let checked = fennec(&["check", &path], "").unwrap();
        assert!(checked.status.success(), "{written}: {}", text(&checked.stderr));
        let ran = fennec(&["run", "--no-test", &path], "").unwrap();
        assert_eq!(text(&ran.stdout), "True True\n", "{written}: {}", text(&ran.stderr));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// In a kit, the leading dot of a case its own file declares comes to the checker with its enum
/// written in, as another file may have a case by that name. That's nothing to fix, so `fennec fix`
/// has to settle.
#[test]
fn fix_leaves_leading_dots_alone_in_a_kit() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-short-cases-kit", std::process::id())),
    );
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"cases\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    let main = "from lights import Light, on\n\nenum Color:\n    Red\n    Blue\n\ndef warm() -> Color:\n    .Red\n\ndef shown(color: Color) -> str:\n    match color:\n        case .Red: \"red\"\n        case .Blue: \"blue\"\n\ndef main(stdout: Stdout):\n    light: Light = .Red\n    stdout.print(shown(warm()), on() == light)\n";
    let lights = "enum Light:\n    Red\n    Off\n\ndef on() -> Light:\n    .Red\n";
    std::fs::write(dir.join("src/lights.fen"), lights).unwrap();
    std::fs::write(dir.join("src/main.fen"), main).unwrap();
    let folder = dir.to_string_lossy().into_owned();
    let fixed = fennec(&["fix", &folder], "").unwrap();
    let written = std::fs::read_to_string(dir.join("src/main.fen")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}{}", text(&fixed.stdout), text(&fixed.stderr));
    assert!(fixed.stdout.is_empty(), "{}", text(&fixed.stdout));
    assert_eq!(written, main);
}

/// `print` where the function has a `Stdout` is fixed, and with none there's nothing to write.
#[test]
fn fix_writes_the_stdout_a_function_has() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-fix-print", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("print.fen");
    let path = program.to_string_lossy().into_owned();
    std::fs::write(&program, "def main(out: Stdout):\n    print(\"hello\")\n").unwrap();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    std::fs::write(&program, "def main():\n    print(\"hello\")\n").unwrap();
    let unfixed = fennec(&["fix", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(written, "def main(out: Stdout):\n    out.print(\"hello\")\n");
    assert_eq!(unfixed.status.code(), Some(1));
    assert!(text(&unfixed.stderr).contains("def main(stdout: Stdout):"), "{}", text(&unfixed.stderr));
}

/// A program with `mut` in each place that moves to `ref`.
const MUT_IN_EACH_PLACE: &str = "struct Counter:\n    count: int\n\n    def bump(mut self):\n        self.count += 1\n\ndef grow(mut items: list[int]):\n    items.append(3)\n\ndef main(stdout: Stdout):\n    mut items = [1, 2]\n    grow(mut items)\n    for mut item in items:\n        item += 1\n    mut counter = Counter(count=0)\n    counter.bump()\n    stdout.print(items, counter.count)\n";

/// `mut` in a place that's now `ref` stops the program, and `fennec fix` writes `ref` in every one.
#[test]
fn fix_writes_ref_where_mut_was_on_a_parameter_loop_or_argument() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-mut-to-ref", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("program.fen");
    let path = program.to_string_lossy().into_owned();
    std::fs::write(&program, MUT_IN_EACH_PLACE).unwrap();
    let before = fennec(&["run", &path], "").unwrap();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let again = fennec(&["fix", &path], "").unwrap();
    let after = fennec(&["run", &path], "").unwrap();
    let formatted = fennec(&["fmt", "--check", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();

    let said = text(&before.stderr);
    assert!(!before.status.success());
    assert!(said.starts_with("error: a parameter is written `ref`, not `mut`"), "{said}");
    assert!(said.contains("`fennec fix` writes `ref` here and in 3 other places"), "{said}");
    assert!(fixed.status.success() && text(&fixed.stdout).starts_with("fixed "), "{}", text(&fixed.stderr));
    assert_eq!(written, MUT_IN_EACH_PLACE.replace("(mut ", "(ref ").replace("for mut", "for ref"));
    assert!(written.contains("    mut items = [1, 2]\n"), "{written}");
    assert!(again.status.success() && again.stdout.is_empty(), "{}", text(&again.stdout));
    assert_eq!(text(&after.stdout), "[2, 3, 4] 1\n", "{}", text(&after.stderr));
    assert!(formatted.status.success(), "{}", text(&formatted.stderr));
}

/// A program from before `IdMap` became `Table`, which `fennec fix` moves over.
const OLD_TABLE_NAMES: &str = "from std.game import Id, IdMap\n\nstruct Unit:\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units: IdMap[Unit] = IdMap[Unit]()\n    first: Id[Unit] = units.insert(Unit(hp=1))\n    stdout.print(units.len(), first)\n";

/// `fennec fix` moves a program from `std.game`'s `IdMap` to `std.collections`' `Table`, with
/// `add` for its `insert`, and then it runs.
#[test]
fn fix_moves_idmap_to_table() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-table-names", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("table.fen");
    let path = program.to_string_lossy().into_owned();
    std::fs::write(&program, OLD_TABLE_NAMES).unwrap();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let ran = fennec(&["run", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        OLD_TABLE_NAMES
            .replace("from std.game import Id, IdMap", "from std.collections import Id, Table")
            .replace("IdMap[", "Table[")
            .replace("units.insert(", "units.add(")
    );
    assert_eq!(text(&ran.stdout), "1 Id(0)\n", "{}", text(&ran.stderr));
}

/// A `shared Name:` declaration and a `with` on its values, written before `shared T` and `lock`:
/// `fennec fix` makes the type a `struct`, writes `shared` where a value is shared, and `lock`
/// for the `with`, and then it runs.
#[test]
fn fix_moves_a_shared_declaration_to_shared_t_and_lock() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-shared-declaration", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("account.fen");
    let path = program.to_string_lossy().into_owned();
    let old = "shared Account:\n    total = 0\n\ndef pay(account: Account, amount: int):\n    \
               with account as ref held:\n        held.total += amount\n\ndef main(stdout: Stdout):\n    \
               account = Account()\n    pay(account, 5)\n    with account as seen:\n        stdout.print(seen.total)\n";
    std::fs::write(&program, old).unwrap();
    let fixed = fennec(&["fix", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let ran = fennec(&["run", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        "struct Account:\n    total = 0\n\ndef pay(account: shared Account, amount: int):\n    \
         lock account as ref held:\n        held.total += amount\n\ndef main(stdout: Stdout):\n    \
         shared account = Account()\n    pay(account, 5)\n    lock account as seen:\n        stdout.print(seen.total)\n"
    );
    assert_eq!(text(&ran.stdout), "5\n", "{}", text(&ran.stderr));
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
    crate::executables::fennec_command()
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

/// Makes a folder a git repository with everything in it committed, giving back the commit.
fn committed_to_git(repository: &Path) -> std::io::Result<String> {
    let git = |args: &[&str]| -> std::io::Result<String> {
        let done = std::process::Command::new("git")
            .arg("-C")
            .arg(repository)
            .args([
                "-c",
                "user.name=Fennec",
                "-c",
                "user.email=fennec@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()?;
        if !done.status.success() {
            return Err(std::io::Error::other(text(&done.stderr)));
        }
        Ok(text(&done.stdout).trim().to_owned())
    };
    git(&["init", "--quiet"])?;
    git(&["add", "--all"])?;
    git(&["commit", "--quiet", "-m", "a kit"])?;
    git(&["rev-parse", "HEAD"])
}

/// `fennec update` pins a kit from git in kit.lock, and says so; run again, it has nothing to do.
#[test]
fn fennec_update_pins_a_kit_from_git() {
    let dir = scratch("update").unwrap();
    let repository = dir.join("physics");
    std::fs::create_dir_all(repository.join("src")).unwrap();
    std::fs::write(
        repository.join("kit.toml"),
        "name = \"physics\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n",
    )
    .unwrap();
    std::fs::write(
        repository.join("src/lib.fen"),
        "## How far a thing falls.\nexport def fall() -> int:\n    7\n",
    )
    .unwrap();
    let commit = committed_to_git(&repository).unwrap();
    let kit = dir.join("game");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::write(
        kit.join("kit.toml"),
        format!(
            "name = \"game\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\nphysics = {{ git = \"file:///{}\", commit = \"{commit}\" }}\n",
            repository.display().to_string().replace('\\', "/").trim_start_matches('/')
        ),
    )
    .unwrap();
    std::fs::write(
        kit.join("src/main.fen"),
        "from physics import fall\n\ndef main(stdout: Stdout):\n    stdout.print(fall())\n",
    )
    .unwrap();
    let run = || {
        crate::executables::fennec_command()
            .args(["run", "."])
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_OFFLINE")
            .output()
            .unwrap()
    };
    let unpinned = run();
    assert!(!unpinned.status.success());
    assert!(text(&unpinned.stderr).contains("`fennec update` fetches it"), "{}", text(&unpinned.stderr));
    let update = || {
        crate::executables::fennec_command()
            .arg("update")
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_OFFLINE")
            .output()
            .unwrap()
    };
    let first = update();
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert!(
        text(&first.stdout).starts_with("kit.lock: `physics` pinned to file://"),
        "{}",
        text(&first.stdout)
    );
    assert!(std::fs::read_to_string(kit.join("kit.lock")).unwrap().contains(&format!(" {commit}\n")));
    assert_eq!(text(&update().stdout), "kit.lock pins every dependency already\n");
    let ran = run();
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout), "7\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Writes a kit with these files, and the dependencies its `kit.toml` lines name.
fn write_kit(at: &Path, name: &str, dependencies: &str, files: &[(&str, &str)]) -> std::io::Result<()> {
    std::fs::create_dir_all(at.join("src"))?;
    std::fs::write(
        at.join("kit.toml"),
        format!("name = \"{name}\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n{dependencies}"),
    )?;
    for (file, source) in files {
        std::fs::write(at.join("src").join(file), source)?;
    }
    Ok(())
}

/// Every command run from a folder deep inside a kit works on that kit.
#[test]
fn every_command_finds_the_kit_from_a_folder_inside_it() {
    let dir = scratch("walks-up").unwrap();
    let kit = dir.join("demo");
    let main =
        "def main(stdout: Stdout):\n    stdout.print(\"ran\")\n\ntest \"adds\":\n    assert 1 + 1 == 2\n";
    write_kit(&kit, "demo", "", &[("main.fen", main)]).unwrap();
    let inside = kit.join("src/deep/er");
    std::fs::create_dir_all(&inside).unwrap();
    let output = dir.join("built");
    let built = output.to_string_lossy();
    let commands: [&[&str]; 9] = [
        &["run"],
        &["run", "--no-test"],
        &["build", "-o", &built],
        &["test"],
        &["check"],
        &["lint"],
        &["fix"],
        &["fmt", "--check"],
        &["update"],
    ];
    for command in commands {
        let ran = crate::executables::fennec_command()
            .args(command)
            .current_dir(&inside)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap();
        assert!(ran.status.success(), "{command:?}: {}", text(&ran.stderr));
    }
    assert!(output.is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The kits the import test depends on: `physics`, which depends on `maths`.
fn write_physics_and_maths(dir: &Path) -> std::io::Result<()> {
    write_kit(
        &dir.join("maths"),
        "maths",
        "",
        &[("lib.fen", "export def twice(x: int) -> int:\n    x * 2\n")],
    )?;
    write_kit(
        &dir.join("physics"),
        "physics",
        "[dependencies]\nmaths = { path = \"../maths\" }\n",
        &[
            (
                "lib.fen",
                "from shapes import area, Circle\nfrom maths import twice\n\nLIMIT: int = 10\n\n\
                 export struct Body:\n    mass: int\n    shape: Circle\n\n    \
                 def weight(self) -> int:\n        twice(self.mass) + area(self.shape) + helper()\n\n\
                 export enum Kind:\n    Solid\n    Liquid(depth: int)\n\n\
                 export error TooHeavy:\n    Over(by: int)\n\n\
                 export interface Weighed:\n    def weight(self) -> int\n\n\
                 export def total[T: Weighed](items: list[T]) -> int fails TooHeavy:\n    \
                 mut sum: int = 0\n    for item in items:\n        sum += item.weight()\n    \
                 if sum > 100:\n        fail .Over(by=sum - 100)\n    sum\n\n\
                 export def describe(kind: Kind) -> str:\n    match kind:\n        \
                 case .Solid:\n            \"solid\"\n        case .Liquid(depth=d):\n            \
                 f\"liquid {d}\"\n\n\
                 def helper() -> int:\n    LIMIT\n\n\
                 test \"not the program's\":\n    assert false\n",
            ),
            (
                "shapes.fen",
                "export struct Circle:\n    radius: int\n\n\
                 export def area(circle: Circle) -> int:\n    3 * circle.radius * circle.radius\n",
            ),
        ],
    )?;
    Ok(())
}

/// A kit's names are its own, in every place a name can be written, another kit reaches only what
/// it exports, and only the kits a kit names itself; its tests and lints stay with it.
#[test]
fn a_kit_imports_another_and_their_names_stay_apart() {
    let dir = scratch("imports").unwrap();
    write_physics_and_maths(&dir).unwrap();
    let kit = dir.join("game");
    let main = "import physics\nfrom physics import Body, total, describe\nfrom physics.shapes import Circle as Round\n\n\
        struct Circle:\n    name: str\n\n\
        def helper() -> str:\n    \"mine\"\n\n\
        def main(stdout: Stdout):\n    \
        body: physics.Body = Body(mass=2, shape=Round(radius=1))\n    \
        stdout.print(body.weight())\n    \
        stdout.print(describe(.Liquid(depth=3)))\n    \
        stdout.print(Circle(name=\"own\").name + helper())\n    \
        stdout.print(f\"{body}\")\n    \
        weighed = total([body]) else as why:\n        \
        match why:\n            \
        case .Over(by=n): n\n    \
        stdout.print(weighed)\n";
    let manifest = "[dependencies]\nphysics = { path = \"../physics\" }\n";
    write_kit(&kit, "game", manifest, &[("main.fen", main)]).unwrap();
    let home = dir.join("home");
    let fennec = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &home)
            .output()
            .unwrap()
    };
    let ran = fennec(&["run", "."]);
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout), "17\nliquid 3\nownmine\nBody(mass=2, shape=Circle(radius=1))\n17\n");
    assert!(!text(&ran.stderr).contains("physics"), "{}", text(&ran.stderr));
    let tested = fennec(&["test", "."]);
    assert!(tested.status.success(), "{}{}", text(&tested.stdout), text(&tested.stderr));
    let linted = fennec(&["lint", "."]);
    assert!(!text(&linted.stderr).contains("physics/src"), "{}", text(&linted.stderr));

    // What a kit's own code sees of another.
    let refused = |source: String, says: &str| {
        std::fs::write(kit.join("src/main.fen"), source).unwrap();
        let checked = fennec(&["check", "."]);
        assert!(!checked.status.success());
        assert!(text(&checked.stderr).contains(says), "{}", text(&checked.stderr));
    };
    refused(format!("from physics import helper as theirs\n{main}"), "`physics` doesn't export `helper`");
    refused(
        format!("{main}\ndef peek() -> int:\n    physics.helper()\n"),
        "`physics` doesn't export `helper`",
    );
    refused(format!("from maths import twice\n{main}"), "there's no module called `maths`");
    refused(format!("{main}\ndef wrong() -> physics.Body:\n    1\n"), "physics.Body");

    // A change to a kit from a path is seen by the next run.
    std::fs::write(kit.join("src/main.fen"), main).unwrap();
    std::fs::write(
        dir.join("physics/src/shapes.fen"),
        "export struct Circle:\n    radius: int\n\n\
         export def area(circle: Circle) -> int:\n    4 * circle.radius * circle.radius\n",
    )
    .unwrap();
    let again = fennec(&["run", "."]);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert!(text(&again.stdout).starts_with("18\n"), "{}", text(&again.stdout));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A small kit and a folder for Fennec's own files, in a folder that holds both and goes when the
/// first is dropped.
fn small_kit(name: &str) -> std::io::Result<(fennec_temp::Temp, PathBuf, PathBuf)> {
    let dir = scratch(name)?;
    std::fs::create_dir_all(dir.join("kit/src"))?;
    std::fs::write(dir.join("kit/kit.toml"), "name = \"demo\"\n")?;
    std::fs::write(dir.join("kit/src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n")?;
    let (kit, home) = (dir.join("kit"), dir.join("home"));
    Ok((dir, kit, home))
}

/// A Fennec folder too deep for a socket, as one in a long temporary folder is, still gets a
/// daemon: it listens in a short folder of its own under `/tmp`, which goes when it stops.
#[cfg(unix)]
#[test]
fn a_daemon_serves_a_fennec_folder_too_deep_for_a_socket() {
    let (folder, kit, _) = small_kit("deep-daemon").unwrap();
    let home = folder.join("a folder with a long name, ".repeat(4));
    let deep = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &home)
            .env("TMPDIR", &home)
            .env("FENNEC_DAEMON_IDLE", "30")
            .env_remove("FENNEC_NO_DAEMON")
            .env_remove("XDG_RUNTIME_DIR")
            .output()
            .unwrap()
    };
    let first = deep(&["dev", "changes"]);
    assert_eq!(
        text(&first.stdout),
        "first run: hashed 1 files and 1 definitions\n",
        "{}",
        text(&first.stderr)
    );
    let status = text(&deep(&["daemon", "status"]).stdout);
    assert!(status.starts_with("running: Fennec") && status.contains("1 request served\n"), "{status}");
    assert_eq!(text(&deep(&["daemon", "stop"]).stdout), "stopped\n");
    let hash = fennec_blake3::hash(home.to_string_lossy().as_bytes()).to_hex();
    let short = Path::new("/tmp").join(format!("fennec-{}", hash.get(..16).unwrap()));
    let gone = (0..250).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        text(&deep(&["daemon", "status"]).stdout) == "not running\n" && !short.exists()
    });
    assert!(gone, "the daemon didn't stop, or left {}", short.display());
}

#[cfg(unix)]
#[test]
fn the_daemon_starts_on_demand_and_keeps_runs_in_memory() {
    let (_folder, kit, home) = small_kit("daemon").unwrap();
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
    let (_folder, kit, home) = small_kit("idle-daemon").unwrap();
    with_daemon(&kit, &home, "1", &["dev", "changes"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't exit after a second with nothing to do");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn the_daemon_keeps_the_cache_under_its_cap() {
    let (_folder, kit, home) = small_kit("cache-daemon").unwrap();
    let old = home.join("cache").join("0.0.0-old").join("functions").join("x");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, vec![0_u8; 10_000]).unwrap();
    // Used before what's always kept, an entry stored or used in the last twenty minutes.
    aged(&old, fennec_build::KEPT_RECENT.as_secs() + 3_600).unwrap();
    crate::executables::fennec_command()
        .args(["dev", "changes"])
        .current_dir(&kit)
        .env("FENNEC_HOME", &home)
        .env("FENNEC_DAEMON_IDLE", "30")
        .env("FENNEC_CACHE_CAP", "5000")
        .env_remove("FENNEC_NO_DAEMON")
        .output()
        .unwrap();
    // The daemon cleans when nothing else is going on, which on a busy machine can take a while.
    let started = std::time::Instant::now();
    let cleaned = loop {
        if !old.exists() {
            break true;
        }
        if started.elapsed() > std::time::Duration::from_secs(120) {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    with_daemon(&kit, &home, "30", &["daemon", "stop"]).unwrap();
    assert!(cleaned, "the daemon should drop another version's entries once over its cap");
    assert!(stops(&kit, &home));
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// Aged by `seconds`: the file's modified time, which is how the cache knows when it was used.
fn aged(path: &Path, seconds: u64) -> std::io::Result<()> {
    let used = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
    std::fs::File::options().write(true).open(path)?.set_modified(used)
}

/// A cache that can't be written costs only work done again: the command does what it was asked,
/// and says, once, that the cache couldn't keep what it made, rather than nothing at all.
#[test]
#[cfg(unix)]
fn a_cache_that_cant_be_written_is_said_once_and_the_command_goes_on() -> std::io::Result<()> {
    use crate::executables::Within as _;
    use std::os::unix::fs::PermissionsExt as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_cache_unwritable_{}", std::process::id())),
    );
    let home = dir.join("home");
    std::fs::create_dir_all(home.join("cache"))?;
    let program = dir.join("hello.fen");
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(\"hello\")\n    return\n")?;
    std::fs::set_permissions(home.join("cache"), std::fs::Permissions::from_mode(0o555))?;
    let ran = crate::executables::fennec_command()
        .arg("run")
        .arg(&program)
        .env("FENNEC_HOME", &home)
        .env("FENNEC_NO_DAEMON", "1")
        .output_within();
    std::fs::set_permissions(home.join("cache"), std::fs::Permissions::from_mode(0o755))?;
    let ran = ran?;
    let said = text(&ran.stderr);
    assert_eq!((ran.status.code(), text(&ran.stdout)), (Some(0), "hello\n".to_owned()), "{said}");
    assert_eq!(said.matches("warning: Fennec's cache couldn't").count(), 1, "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// With no daemon, a command starts a cleanup once none has run for an hour, which removes another
/// build's folder that's gone unused for a day; `fennec cache clean` runs one when asked.
#[test]
fn a_command_cleans_the_cache_hourly_with_no_daemon() {
    let (_folder, kit, home) = small_kit("cache-hourly").unwrap();
    let old = home.join("cache").join("0.0.1").join("llvm").join("x");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, vec![0_u8; 1_000]).unwrap();
    aged(&old, 2 * 24 * 60 * 60).unwrap();
    let mark = home.join("cache").join("cleaned");
    std::fs::write(&mark, "").unwrap();
    aged(&mark, 2 * 60 * 60).unwrap();
    let fennec = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &home)
            .env("FENNEC_NO_DAEMON", "1")
            .output()
            .unwrap()
    };
    assert!(fennec(&["dev", "changes"]).status.success());
    // The cleanup is a process of its own, which a busy machine may take a while to start: it's
    // waited for as long as two minutes, which ends the moment it's done.
    let done = |gone: &dyn Fn() -> bool| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while std::time::Instant::now() < until {
            if gone() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    };
    let cleaned = done(&|| !old.parent().unwrap().exists());
    assert!(cleaned, "a build folder unused for two days should go once a cleanup is due");

    // The background cleanup may still be letting go of its lock, and one asked for meanwhile
    // would say so.
    assert!(
        done(&|| !home.join("cache").join("cleanup.lock").exists()),
        "the cleanup never let go of its lock"
    );
    let asked = fennec(&["cache", "clean"]);
    assert!(asked.status.success());
    assert!(
        text(&asked.stdout).starts_with("🦊 cache: removed 0 entries, 0.0 MB; it holds "),
        "{}",
        text(&asked.stdout)
    );
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
fn run_compiles_by_default_and_shows_output_before_it_reads_input() {
    let dir = scratch("default-run").unwrap();
    let program = dir.join("prompt.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout, stdin: Stdin):\n    stdout.print(\"name?\")\n    stdout.print(stdin.lines())\n",
    )
    .unwrap();
    let mut child = crate::executables::fennec_command()
        .arg("run")
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
    std::fs::remove_dir_all(dir).unwrap();
}

/// A program whose memory has to stay small: it's stopped, and fails, past `limit_kb`.
#[cfg(unix)]
fn peak_is_under(program: &Path, limit_kb: u64) -> Result<(), String> {
    let mut child = crate::executables::fennec_command()
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
        let rss = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
            .map_err(|err| err.to_string())?;
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
        "struct Counter:\n    counts: dict[int, int]\n\n    def bump(ref self, key: int):\n        \
         self.counts[key] = (self.counts.get(key) else 0) + 1\n\ndef main(stdout: Stdout):\n    mut counter = \
         Counter(counts={})\n    for i in range(20_000):\n        counter.bump(i * 7919 % 10_007)\n    \
         stdout.print(counter.counts.len())\n",
    )
    .unwrap();
    peak_is_under(&program, 200_000).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// A copy made each time round a loop is a hint, which names where the other holder is used
/// again: every copy is a snapshot, as the program says, so it checks and runs. In a `@hot`
/// function it's a compile error.
#[test]
fn a_copy_each_time_round_a_loop_is_a_hint_and_a_hot_path_refuses_it() {
    let dir = scratch("copy-rule").unwrap();
    let program = dir.join("loop.fen");
    let body = "    mut seen = [1, 2]\n    for _ in range(3):\n        kept = seen\n        seen.append(kept.len())\n        \
                stdout.print(kept.len(), seen.len())\n";
    let said = "`seen` is copied each time round the loop, since `kept` still holds it";
    std::fs::write(&program, format!("def main(stdout: Stdout):\n{body}")).unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "2 3\n3 4\n4 5\n", "{}", text(&output.stderr));
    let linted = fennec(&["lint", program.to_str().unwrap()], "").unwrap();
    assert!(linted.status.success(), "{}", text(&linted.stderr));
    assert!(text(&linted.stdout).contains(said), "{}", text(&linted.stdout));
    std::fs::write(
        &program,
        format!("@hot\ndef grow(stdout: Stdout):\n{body}\ndef main(stdout: Stdout):\n    grow(stdout)\n"),
    )
    .unwrap();
    let output = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    assert!(!output.status.success());
    let refused = "`seen` is copied here, since `kept` still holds it";
    assert!(text(&output.stderr).contains(refused), "{}", text(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

/// A `@hot` loop over a `Table`, `for ref` or not, walks its slots in place: nothing's made for it,
/// so it checks, and it runs.
#[test]
fn a_hot_loop_over_a_table_makes_nothing() {
    let dir = scratch("hot_table").unwrap();
    let program = dir.join("hot.fen");
    let source = "from std.collections import Table\n\nstruct Enemy:\n    hp: int\n\nstruct World:\n    \
                  enemies = Table[Enemy]()\n\n@hot\ndef step(ref world: World) -> int:\n    mut total = 0\n    \
                  for ref enemy in world.enemies:\n        enemy.hp -= 1\n    for enemy in world.enemies:\n        \
                  total += enemy.hp\n    total\n\ndef main(stdout: Stdout):\n    mut world = World()\n    \
                  _ = world.enemies.add(Enemy(hp=3))\n    _ = world.enemies.add(Enemy(hp=5))\n    \
                  stdout.print(step(ref world))\n";
    std::fs::write(&program, source).unwrap();
    let checked = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    assert!(checked.status.success(), "{}", text(&checked.stderr));
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "6\n", "{}", text(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

/// `@hot` on a function means it, and everything it calls, may not allocate or copy: each place
/// that may is a compile error that says why, and one that does neither builds and runs.
#[test]
fn a_hot_function_and_what_it_calls_cant_allocate() {
    let dir = scratch("hot").unwrap();
    let program = dir.join("hot.fen");
    let source = "struct Unit:\n    hp: float\n    tags: list[str]\n\ndef tagged(ref unit: Unit, tag: str):\n    \
                  unit.tags.append(tag)\n\n@hot\ndef tick(ref units: list[Unit], dt: float) -> float:\n    \
                  mut total = 0.0\n    for ref unit in units:\n        unit.hp -= dt\n        total += unit.hp\n    \
                  tagged(ref units[0], \"hit\")\n    total\n\ndef main(stdout: Stdout):\n    \
                  mut units = [Unit(hp=10.0, tags=[]), Unit(hp=20.0, tags=[])]\n    stdout.print(tick(ref units, 1.0))\n";
    std::fs::write(&program, source).unwrap();
    for command in ["check", "run"] {
        let output = fennec(&[command, program.to_str().unwrap()], "").unwrap();
        assert!(!output.status.success(), "{command}");
        let printed = text(&output.stderr);
        assert!(
            printed.contains(
                "growing it with `list.append` may allocate in `tagged`, which `@hot` `tick` calls"
            ),
            "{printed}"
        );
    }
    std::fs::write(&program, source.replace("    tagged(ref units[0], \"hit\")\n", "")).unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "28.0\n", "{}", text(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

/// `@hot` also means no check is left in native code: an index the compiler can't place inside
/// its list is a compile error. A loop over a list keeps none, and an overflow check stays.
#[test]
fn a_hot_function_keeps_no_checks() {
    let dir = scratch("hot-checks").unwrap();
    let program = dir.join("hot.fen");
    let source = "@hot\ndef total(xs: list[float], at: int) -> float:\n    mut sum = 0.0\n    for x in xs:\n        \
                  sum += x\n    sum + xs[at]\n\ndef main(stdout: Stdout):\n    stdout.print(total([1.0, 2.0], 1))\n";
    std::fs::write(&program, source).unwrap();
    let output = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    let printed = text(&output.stderr);
    assert!(!output.status.success());
    assert!(
        printed.contains(
            "this index may be outside what it indexes, so it keeps its bounds check in `@hot` `total`"
        ),
        "{printed}"
    );
    assert_eq!(printed.matches("keeps its").count(), 1, "the loop keeps no check: {printed}");
    std::fs::write(&program, source.replace("sum + xs[at]", "sum")).unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "3.0\n", "{}", text(&output.stderr));
    // An overflow check the compiler can't prove away stays, as the safety net it is everywhere.
    std::fs::write(&program, "@hot\ndef add(a: int, b: int) -> int:\n    a + b\n\ndef main(stdout: Stdout):\n    stdout.print(add(1, b=2))\n").unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "3\n", "{}", text(&output.stderr));
    // So does a float divide check, and an integer one doesn't.
    std::fs::write(&program, "@hot\ndef ratio(a: float, b: float) -> float:\n    a / b\n\ndef main(stdout: Stdout):\n    stdout.print(ratio(1.0, b=4.0))\n").unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "0.25\n", "{}", text(&output.stderr));
    std::fs::write(&program, "@hot\ndef parts(a: int, b: int) -> int:\n    a // b\n\ndef main(stdout: Stdout):\n    stdout.print(parts(9, b=4))\n").unwrap();
    let output = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    assert!(
        text(&output.stderr).contains("this may divide by zero, so it keeps its check"),
        "{}",
        text(&output.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A `shared` value is one value: every name for it, and every function it's passed to, changes
/// the same one, each inside a `lock`.
#[test]
fn a_shared_value_is_one_cell_however_its_reached() {
    let dir = scratch("shared").unwrap();
    let program = dir.join("account.fen");
    std::fs::write(
        &program,
        "struct Account:\n    total = 0\n\n    def add(ref self, amount: int):\n        self.total += amount\n\n    \
         def balance(self) -> int:\n        self.total\n\n\
         def pay(account: shared Account, amount: int):\n    lock account as ref held:\n        held.add(amount)\n\n\
         def main(stdout: Stdout):\n    shared account = Account()\n    same = account\n    pay(account, 5)\n    \
         lock same as ref held:\n        held.add(10)\n    mut through_same = 0\n    lock same as held:\n        \
         through_same = held.total\n    lock account as held:\n        stdout.print(held.balance(), through_same, held)\n",
    )
    .unwrap();
    let output = fennec(&["run", "--no-test", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "15 15 Account(total=15)\n", "{}", text(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn constants_are_worked_out_while_compiling() {
    let dir = scratch("constants").unwrap();
    let program = dir.join("consts.fen");
    let path = program.to_string_lossy().into_owned();
    std::fs::write(
        &program,
        "from std.math import PI, sin\n\nstruct Point:\n    x: float\n    y: float\n\n\
         SINE = [sin(float(i) * PI / 512.0) for i in range(1024)]\nORIGIN = Point(x=0.0, y=-1.5)\n\
         SQUARES = {n: n * n for n in range(5)}\n\ndef main(stdout: Stdout):\n    \
         stdout.print(SINE.len(), SINE[256], ORIGIN.y, SQUARES[4])\n",
    )
    .unwrap();
    let output = fennec(&["run", &path], "").unwrap();
    assert_eq!(text(&output.stdout), "1024 1.0 -1.5 16\n", "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");

    // One that takes too long is worked out when the program starts, with a hint.
    std::fs::write(
        &program,
        "def slow() -> int:\n    mut total = 0\n    for i in range(2000000):\n        total += i\n    total\n\n\
         SLOW = slow()\n\ndef main(stdout: Stdout):\n    stdout.print(SLOW)\n",
    )
    .unwrap();
    let output = fennec(&["run", &path], "").unwrap();
    assert_eq!(text(&output.stdout), "1999999000000\n");
    assert!(text(&output.stderr).contains("hint: `SLOW` takes more than"), "{}", text(&output.stderr));

    // One that fails would fail every time, so it's a compile error.
    std::fs::write(
        &program,
        "ITEMS = [1, 2]\nBAD = ITEMS[5]\n\ndef main(stdout: Stdout):\n    stdout.print(BAD)\n",
    )
    .unwrap();
    let output = fennec(&["run", &path], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("index 5 is out of range for length 2"),
        "{}",
        text(&output.stderr)
    );
}

/// A build refuses what `check` and a run refuse, including a copy each time round a loop in a
/// `@hot` function.
#[test]
fn a_build_refuses_what_a_check_refuses() -> std::io::Result<()> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_build_refuses_{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("copies.fen");
    std::fs::write(
        &program,
        "@hot\ndef record(stdout: Stdout):\n    mut xs = [1]\n    mut history = [[0]]\n    for frame in range(3):\n        \
         history.append(xs)\n        xs.append(frame)\n    stdout.print(history, xs)\n\ndef main(stdout: Stdout):\n    \
         record(stdout)\n",
    )?;
    let program = program.to_string_lossy().into_owned();
    let executable = dir.join("copies").to_string_lossy().into_owned();
    let checked = fennec(&["check", &program], "")?;
    assert!(!checked.status.success());
    assert!(text(&checked.stderr).contains("`xs` is copied here"), "{}", text(&checked.stderr));
    for build in
        [vec!["build", &program, "-o", &executable], vec!["build", "--llvm", &program, "-o", &executable]]
    {
        let built = fennec(&build, "")?;
        assert!(!built.status.success(), "{build:?}");
        assert!(text(&built.stderr).contains("`xs` is copied here"), "{build:?}: {}", text(&built.stderr));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A program built again under the same name, and run each time, is a new file every time: macOS
/// remembers the code signature of a file it has launched and kills a program written into one,
/// with no word. So two programs take turns at one path, built through the store, which links one
/// the first time and gives the stored one after, and through a run, whose programs are the
/// store's own files.
#[test]
fn a_program_built_again_under_its_name_runs_each_time() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_built_again_{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("turns.fen");
    let executable = dir.join("turns");
    let fennec_at = |args: &[&std::ffi::OsStr]| {
        crate::executables::fennec_command().args(args).env("FENNEC_HOME", dir.join("home")).output_within()
    };
    for turn in 0..10 {
        let said = if turn % 2 == 0 { "even" } else { "odd" };
        std::fs::write(
            &program,
            format!("def main(stdout: Stdout):\n    stdout.print(\"{said}\")\n    return\n"),
        )?;
        let built =
            fennec_at(&["build".as_ref(), program.as_os_str(), "-o".as_ref(), executable.as_os_str()])?;
        assert!(built.status.success(), "turn {turn}: {}", text(&built.stderr));
        let ran = std::process::Command::new(&executable).output_within()?;
        assert_eq!(
            (ran.status.code(), text(&ran.stdout)),
            (Some(0), format!("{said}\n")),
            "turn {turn}: {ran:?}"
        );
        let run = fennec_at(&["run".as_ref(), program.as_os_str()])?;
        assert_eq!(
            (run.status.code(), text(&run.stdout)),
            (Some(0), format!("{said}\n")),
            "turn {turn}: {run:?}"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A plain run builds a program with an `f32` or a `u64` natively, as it does any other.
#[test]
fn a_run_builds_u64_and_f32_natively() -> std::io::Result<()> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_build_sized_{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("sized.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    x: u64 = 1\n    y: f32 = 0.1\n    stdout.print(x, y + 0.2)\n",
    )?;
    let ran = fennec(&["run", &program.to_string_lossy()], "")?;
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout), "1 0.3\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A `u8` that overflows stops the program.
#[test]
fn a_sized_number_that_overflows_stops_the_program() -> std::io::Result<()> {
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec_sized_{}", std::process::id())));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("pixel.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    mut pixel: u8 = 255\n    pixel += 1\n    stdout.print(pixel)\n",
    )?;
    let program = program.to_string_lossy().into_owned();
    let ran = fennec(&["run", &program], "")?;
    assert!(!ran.status.success());
    assert!(
        text(&ran.stderr).contains("this calculation overflows a u8, which holds 0 to 255"),
        "{}",
        text(&ran.stderr)
    );
    assert!(!text(&ran.stdout).contains("256"));
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What a copy names is the item it's in, as it's written.
#[test]
fn a_copy_names_the_item_it_is_in() -> std::io::Result<()> {
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec_copy_names_{}", std::process::id())));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("items.fen");
    for (at, said) in [("0", "units[0].tags"), ("at", "units[at].tags")] {
        std::fs::write(
            &program,
            format!(
                "struct Unit:\n    tags: list[int]\n\ndef main(stdout: Stdout):\n    mut units = [Unit(tags=[1])]\n    \
                 at = 0\n    for round in range(3):\n        held = units[{at}]\n        units[{at}].tags.append(round)\n        \
                 stdout.print(held.tags, at)\n"
            ),
        )?;
        let linted = fennec(&["lint", &program.to_string_lossy()], "")?;
        let told = text(&linted.stdout);
        assert!(told.contains(&format!("`{said}` is copied each time round the loop")), "{told}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A file with no kit embeds only what's in its own folder or under it: not what's above it, and
/// not through a link that leads out of it. One inside a kit's folder goes by the kit's.
#[test]
#[cfg(unix)]
fn a_file_with_no_kit_embeds_only_from_its_own_folder() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-embed-edge-{}", std::process::id())),
    );
    let inner = dir.join("inner");
    std::fs::create_dir_all(inner.join("data")).unwrap();
    std::fs::write(dir.join("secret.txt"), "above").unwrap();
    std::fs::write(inner.join("data/here.txt"), "here").unwrap();
    std::os::unix::fs::symlink(dir.join("secret.txt"), inner.join("link.txt")).unwrap();
    let game = inner.join("game.fen");
    let checked = |path: &str| {
        let source = format!("def main(stdout: Stdout):\n    stdout.print(embed_file({path:?}).len())\n");
        std::fs::write(&game, source).unwrap();
        let output = fennec(&["check", &game.to_string_lossy()], "").unwrap();
        (output.status.success(), text(&output.stderr))
    };
    assert!(checked("data/here.txt").0);
    for outside in ["../secret.txt", "link.txt"] {
        let (passed, said) = checked(outside);
        assert!(!passed && said.contains(&format!("`{outside}` is outside this file's folder")), "{said}");
    }
    // Inside a kit's folder, the kit's is the edge, and what's above the file but in the kit is fine.
    std::fs::write(dir.join("kit.toml"), "name = \"edge\"\nversion = \"0.1.0\"\n").unwrap();
    assert!(checked("../secret.txt").0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `embed_folder` bakes in a folder's files by their paths from it, sorted, leaving hidden ones
/// out, as a read-only map from path to `bytes`. It refuses a missing folder, a file, and a link
/// that leads out of the file's folder.
#[test]
#[cfg(unix)]
fn embed_folder_gives_a_folders_files_by_path() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-embed-folder-{}", std::process::id())),
    );
    let site = dir.join("game/site");
    std::fs::create_dir_all(site.join("css")).unwrap();
    std::fs::create_dir_all(site.join(".git")).unwrap();
    std::fs::write(site.join("css/app.css"), "body{}").unwrap();
    std::fs::write(site.join("index.html"), "<h1>hi</h1>").unwrap();
    std::fs::write(site.join(".env"), "secret").unwrap();
    std::fs::write(site.join(".git/HEAD"), "ref").unwrap();
    std::fs::write(dir.join("outside.txt"), "above").unwrap();
    let game = dir.join("game/main.fen");
    let source = "SITE = embed_folder(\"site\")\n\ndef main(stdout: Stdout):\n    \
        stdout.print(SITE.len(), SITE.keys())\n    stdout.print(SITE.get(\"css/app.css\"))\n    \
        stdout.print(SITE.get(\"missing\") is None)\n    for path, data in SITE.items():\n        stdout.print(path, data.len())\n";
    std::fs::write(&game, source).unwrap();
    let ran = fennec(&["run", "--no-test", &game.to_string_lossy()], "").unwrap();
    assert_eq!(
        text(&ran.stdout),
        "2 [\"css/app.css\", \"index.html\"]\nbytes([98, 111, 100, 121, 123, 125])\nTrue\ncss/app.css 6\nindex.html 11\n",
        "{}",
        text(&ran.stderr)
    );
    let refused = |path: &str| {
        std::fs::write(
            &game,
            format!("def main(stdout: Stdout):\n    stdout.print(embed_folder({path:?}).len())\n"),
        )
        .unwrap();
        let output = fennec(&["check", &game.to_string_lossy()], "").unwrap();
        assert!(!output.status.success(), "{path}");
        text(&output.stderr)
    };
    assert!(refused("nowhere").contains("there's no folder `nowhere` to embed"));
    assert!(refused("site/index.html").contains("`site/index.html` is a file"));
    std::os::unix::fs::symlink(dir.join("outside.txt"), site.join("link.txt")).unwrap();
    assert!(refused("site").contains("`site/link.txt` is outside this file's folder"));
    std::fs::remove_dir_all(&dir).unwrap();
}
