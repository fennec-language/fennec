//! The `fennec` command itself: `run`, `test`, `new`, `dev`, `--version`, and what it does
//! with bad input. These run the real binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

#[cfg(unix)]
use crate::executables::{DAEMON_IDLE, DaemonsStopped};

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
            said.contains("needless-mut") && said.contains("1 lint, which `fennec fix` can fix"),
            "{time}: {said}"
        );
        // The run goes on, so a lint says it's one, and that `fennec lint` fails on it.
        assert!(
            said.contains("lint: needless-mut: ") && said.contains("(`fennec lint` fails on this)"),
            "{time}: {said}"
        );
        assert!(!said.contains("error: "), "{time}: {said}");
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
    let maths = "## Twice the number.\nexport def twice(number: int) -> int:\n    return number * 2\n";
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
    let spare = "## Half the number.\nexport def half(number: int) -> int:\n    mut halved = number / 2\n    return halved\n";
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
         def transaction(ref log: Log) -> with int fails DbError:\n    lend 1 except:\n        \
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
         stdout.print(stdin.read_line()?)\n    decode_value = stdin.read_chunk(5)?.decode() except InvalidValue(): \"?\"\n    stdout.print(decode_value)\n    \
         stdout.print(stdin.lines())\n    stdout.print(stdin.read_line()?, len(stdin.read_chunk(5)?))\n",
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
         mut count = 0\n    for _ in src.lines()?:\n        count += 1\n    return count\n\n\
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
    let source = "def forever(n: int) -> int:\n    return forever(n + 1)\n\ndef main(stdout: Stdout):\n    stdout.print(forever(0))\n";
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

/// What a test's launched tasks and its `parallel` loop's bodies print reaches the test's
/// `captured()` and `sorted_lines()`, whatever thread they ran on, however many threads there are
/// and whether the loop was split to an item a range or left whole.
#[test]
fn what_tasks_and_parallel_bodies_print_is_captured_by_their_test() {
    if !crate::executables::threads_runtime_built() {
        return;
    }
    for threads in ["1", "4", "16"] {
        for stressed in [false, true] {
            let mut command = crate::executables::fennec_command();
            command
                .args(["test", "crates/fennec/tests/fixtures/tasks_print_into_tests.fen"])
                .current_dir(repo())
                .env("FENNEC_NO_CACHE", "1")
                .env("FENNEC_THREADS", threads);
            if stressed {
                command.env("FENNEC_PARALLEL_STRESS", "1");
            }
            let output = command.output().unwrap();
            assert_eq!(
                text(&output.stdout),
                "2 passed, 0 failed\n",
                "{threads} threads, stressed {stressed}: {}",
                text(&output.stderr)
            );
        }
    }
}

/// A constant a test's launched task works out before the test does is one the test finds too: a
/// test works out its program's constants first, as `main` does in a program that uses tasks.
/// Before, the task worked it out on its worker, where the test's own push to a field starting as
/// it couldn't find it, and stopped.
#[test]
fn a_constant_a_test_s_task_reads_first_is_found_by_the_test() {
    if !crate::executables::threads_runtime_built() {
        return;
    }
    for threads in ["1", "4", "16"] {
        let output = crate::executables::fennec_command()
            .args(["test", "crates/fennec/tests/fixtures/constants_in_test_tasks.fen"])
            .current_dir(repo())
            .env("FENNEC_NO_CACHE", "1")
            .env("FENNEC_THREADS", threads)
            .output()
            .unwrap();
        assert_eq!(
            text(&output.stdout),
            "2 passed, 0 failed\n",
            "{threads} threads: {}",
            text(&output.stderr)
        );
    }
}

/// Each line a test and its tasks print reaches the test's capture whole. A `print` arrives in
/// pieces, and before the fix, pieces of a task's line and the test's own mixed in most runs of
/// this fixture: 42 of 50 at 4 threads and 30 of 50 at 1 thread, both stressed. So three runs at
/// each nearly always catch it.
#[test]
fn every_line_a_test_and_its_tasks_print_reaches_it_whole() {
    if !crate::executables::threads_runtime_built() {
        return;
    }
    for threads in ["1", "4"] {
        for run in 0..3 {
            let output = crate::executables::fennec_command()
                .args(["test", "crates/fennec/tests/fixtures/whole_lines_into_tests.fen"])
                .current_dir(repo())
                .env("FENNEC_NO_CACHE", "1")
                .env("FENNEC_THREADS", threads)
                .env("FENNEC_PARALLEL_STRESS", "1")
                .output()
                .unwrap();
            assert_eq!(
                text(&output.stdout),
                "2 passed, 0 failed\n",
                "{threads} threads, run {run}: {}",
                text(&output.stderr)
            );
        }
    }
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
    let source = "from std.net import Response\n\ndef handle(path: str) -> Response:\n    match path:\n        case \"/hello\":\n            return Response(status=200, body=\"hi\".encode())\n        case _:\n            return Response(status=404)\n\ntest \"plain\":\n    assert handle(\"/hello\").status == 200\n    assert len(handle(\"/\").headers) == 0\n\ntest \"net only\"(net: Net):\n    with net.listen(0)? as server:\n        assert server.port >= 0\n";
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

/// Help asked for is the answer, on standard output, and no failure: a newcomer's first
/// `fennec --help` shouldn't read as a mistake to a script or an editor.
#[test]
fn help_asked_for_shows_usage_and_succeeds() {
    for asked in ["--help", "-h", "help"] {
        let output = fennec(&[asked], "").unwrap();
        assert!(output.status.success(), "{asked}");
        assert!(text(&output.stdout).starts_with("usage:"), "{asked}");
        assert!(output.stderr.is_empty(), "{asked}");
    }
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
    saved = fs.read_bytes("save.bin")?
    text = fs.read_text("save.bin") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): "not text"
    stdout.print(saved, text)
    fs.write_text("note.txt", "hé")?
    note = fs.read_bytes("note.txt")?
    missing = fs.read_bytes("missing.bin") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): bytes()
    stdout.print(note, missing)
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
    fs.remove("saves") except why:
        case .Other():
            stdout.print("a folder has to be empty to remove")
        case _:
            stdout.print(why.message())
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
                  stdout.print(8 if key not in counts else counts[key])\n        stdout.print(len(names[key] if key in names else \"wolf\"))\n";
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
/// `Ok` and `Err` with `except`, where a function's last line returns from its failure case, a named
/// value keeps its name, and a loop's last statement continues; `-> Result[T, E]` as
/// `-> T fails E`; and `return Err(e)` as `fail e`. The program runs once fixed.
#[test]
fn what_used_ok_err_and_result_is_fixed_and_runs() {
    let dir = scratch("results-fix").unwrap();
    let program = dir.join("results_fix.fen");
    let source = "error Bad:\n    why: str\n\ndef lower(n: int) -> Result[int, Bad]:\n    if n < 1:\n        \
                  return Err(Bad(\"small\"))\n    return n - 1\n\ndef first(n: int) -> int:\n    match lower(n):\n        case Ok(lowered): return lowered\n        case Err(_): return 0\n\ndef main(stdout: Stdout):\n    \
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
        "lowered = lower(n) except Bad():\n        return 0\n    return lowered\n",
        "given = lower(n) except Bad():\n            9\n",
        "lowered = lower(n) except problem:\n            stdout.print(problem.why)\n            continue\n",
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
        "def at(n: int) -> int:\n    return [1][n]\n\ndef main(stdout: Stdout):\n    stdout.print(at(1))\n",
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

/// A program that's ended by a signal, rather than giving back an exit code, is said to have been,
/// by `fennec run` that waited for it, and the run exits with 1. It used to exit with 1 and say
/// nothing, as if the program had failed quietly, which is how a crash read on Windows, where
/// `fennec run` always waits.
#[cfg(unix)]
#[test]
fn a_run_whose_program_a_signal_ends_says_so() {
    let dir = scratch("run-killed").unwrap();
    let program = dir.join("killed.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout, process: Process):\n    stdout.print(\"before\")\n    _ = process.run(\"sh\", [\"-c\", \"kill -KILL $PPID\"]) except why:\n        stdout.print(f\"{why}\")\n        return\n    stdout.print(\"after\")\n    return\n",
    )
    .unwrap();
    let path = program.to_str().unwrap();
    let output = crate::executables::fennec_command()
        .args(["run", "--no-test", path])
        .env("FENNEC_NO_CACHE", "1")
        .env("FENNEC_RUN_WAITS", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert!(
        text(&output.stderr).contains("error: the program it built was ended by signal 9 (SIGKILL)"),
        "{}",
        text(&output.stderr)
    );
    assert!(!text(&output.stdout).contains("after"), "{}", text(&output.stdout));
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
            "error Broke:\n    why: str\n\ndef main(args: Args) -> int fails Broke:\n    if len(args) > 5:\n        fail Broke(why=\"many\")\n    return 3\n",
            3,
        ),
        // The most an exit code holds, and one past it, of which the process gets the lowest 8 bits.
        ("most", "def main() -> int:\n    return 255\n", 255),
        ("wrapped", "def main() -> int:\n    return 300\n", 44),
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

/// What a built program writes before it ends is there after it: a file, and a row in a SQLite
/// database it closed, both from a program that gives back 3 and from one that fails, which exits
/// with 1, each code passed on exactly. On Windows a program ends with `TerminateProcess`, which runs
/// none of the C runtime's or a library's teardown, so this checks nothing written needs it.
#[test]
fn what_a_built_program_wrote_before_it_ended_is_there_after_it() {
    if !llvm_ready_for_run() {
        return;
    }
    let dir = scratch("written-before-the-end").unwrap();
    let programs = [
        (
            "writer",
            "from std import sqlite\nfrom std.sql import SqlError\n\ndef main(fs: Fs, stdout: Stdout) -> int fails SqlError | IoError:\n    fs.write_text(\"note.txt\", \"written just before the end\")?\n    with sqlite.open(fs, \"kept.db\")? as db:\n        _ = db.execute(t\"create table words (word text)\")?\n        _ = db.execute(t\"insert into words values ('kept')\")?\n    stdout.print(\"wrote both\")\n    return 3\n",
        ),
        (
            "failer",
            "from std import sqlite\nfrom std.sql import SqlError\n\nerror Broke:\n    why: str\n\ndef main(fs: Fs, stdout: Stdout) fails SqlError | IoError | Broke:\n    fs.write_text(\"note.txt\", \"written just before the failure\")?\n    with sqlite.open(fs, \"kept.db\")? as db:\n        _ = db.execute(t\"insert into words values ('also kept')\")?\n    stdout.print(\"wrote both, then failed\")\n    fail Broke(why=\"on purpose\")\n",
        ),
        (
            "reader",
            "from std import sqlite\nfrom std.sql import SqlError\n\nstruct Word:\n    word: str\n\ndef main(fs: Fs, stdout: Stdout) fails SqlError | IoError:\n    stdout.print(fs.read_text(\"note.txt\")?)\n    with sqlite.open(fs, \"kept.db\")? as db:\n        for found in db.query[Word](t\"select word from words order by rowid\")?:\n            stdout.print(found.word)\n",
        ),
    ];
    for (name, source) in programs {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source).unwrap();
        let built = dir.join(name);
        let made = fennec(&["build", program.to_str().unwrap(), "-o", built.to_str().unwrap()], "").unwrap();
        assert!(made.status.success(), "{name}: {}", text(&made.stderr));
    }
    let run = |name: &str| std::process::Command::new(dir.join(name)).current_dir(&dir).output().unwrap();
    let wrote = run("writer");
    assert_eq!(wrote.status.code(), Some(3), "{}", text(&wrote.stderr));
    assert_eq!(text(&wrote.stdout), "wrote both\n");
    let read = run("reader");
    assert!(read.status.success(), "{}", text(&read.stderr));
    assert_eq!(text(&read.stdout), "written just before the end\nkept\n");
    let failed = run("failer");
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stderr));
    assert_eq!(text(&failed.stdout), "wrote both, then failed\n");
    assert!(
        text(&failed.stderr).contains("error: `main` failed: Broke(why=\"on purpose\")"),
        "{}",
        text(&failed.stderr)
    );
    let read = run("reader");
    assert!(read.status.success(), "{}", text(&read.stderr));
    assert_eq!(text(&read.stdout), "written just before the failure\nkept\nalso kept\n");
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

/// Two tests of one name, in two files of a kit or twice in one file, each run, told apart by
/// their module, and in one module by their line. Neither ran before: both failed, saying LLVM
/// couldn't build them (#1860).
#[test]
fn tests_of_one_name_in_a_kit_each_run_told_apart_by_their_modules() {
    let dir = scratch("one-name").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"dup\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(
        dir.join("src/one.fen"),
        "def twice(n: int) -> int:\n    return n * 2\n\ntest \"it works\":\n    assert twice(2) == 4\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/two.fen"),
        "def thrice(n: int) -> int:\n    return n * 3\n\ntest \"it works\":\n    assert thrice(2) == 6\n\n\
         test \"again\":\n    assert thrice(1) == 3\n\ntest \"again\":\n    assert thrice(0) == 0\n",
    )
    .unwrap();
    let test = fennec_in(&dir, &["test", "--verbose"], "").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let stdout = text(&test.stdout);
    assert!(test.status.success(), "{stdout}{}", text(&test.stderr));
    for shown in [
        "✓ it works (in one)",
        "✓ it works (in two)",
        "✓ again (in two, line 7)",
        "✓ again (in two, line 10)",
    ] {
        assert!(stdout.contains(shown), "{shown}:\n{stdout}");
    }
    assert!(stdout.ends_with("4 passed, 0 failed\n"), "{stdout}");
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
        "def twice(x: int) -> int:\n    return x * 2\n\ntest \"twice\":\n    assert twice(2) == 4\n",
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

/// A kit's files are in `src/`. One whose only file is beside `kit.toml` has nothing in it, and
/// checking, linting or testing it fails and says to move the file. It passed without a word
/// before, so `fennec lint` there found none of the file's lints.
#[test]
fn a_kit_with_its_file_beside_kit_toml_says_to_move_it_to_src() {
    let dir = scratch("file-beside").unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"beside\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(
        dir.join("main.fen"),
        "def shown(count: int) -> int:\n    return int(count) + 1\n\ndef main(stdout: Stdout):\n    stdout.print(shown(3))\n",
    )
    .unwrap();
    let said: Vec<_> =
        ["check", "lint", "test"].iter().map(|command| fennec_in(&dir, &[command], "").unwrap()).collect();
    std::fs::remove_dir_all(&dir).unwrap();
    for (command, output) in ["check", "lint", "test"].iter().zip(&said) {
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{command}: {stderr}");
        assert!(
            stderr.starts_with("error: src/ has no .fen file, so there's nothing in this kit"),
            "{command}: {stderr}"
        );
        assert!(stderr.contains("move main.fen to src/main.fen"), "{command}: {stderr}");
    }
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

const BENCHED: &str = "def double(x: int) -> int:\n    return x * 2\n\n\
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
    let source = "def unused() -> list[int]:\n    return [1]\n\n\
        def main(stdout: Stdout):\n    mut xs = [1]\n    snapshot = xs\n    xs.append(2)\n    \
        for at in range(2):\n        stdout.print(f\"{at}\")\n    stdout.print(len(snapshot), len(xs))\n";
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
    let shared = "    mut xs = [1, 2, 3]\n    snapshot = xs\n    xs.append(4)\n    stdout.print(len(snapshot) + len(xs))\n";
    let counted = counted_in_a_run("copies_shared", shared);
    assert!(counted.contains("in a run: 1 copy of 24 B"), "{counted}");

    let alone = "    mut xs = [1, 2, 3]\n    xs.append(4)\n    stdout.print(len(xs))\n";
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
    let looped = "    mut total = 0\n    for at in range(100):\n        xs = [at, at]\n        total += len(xs)\n    stdout.print(total)\n";
    let counted = counted_in_a_run("copies_loop", looped);
    let allocations = counted.split("no copies, ").nth(1).and_then(|rest| rest.split(' ').next());
    let allocations: u64 = allocations.and_then(|count| count.parse().ok()).unwrap_or(0);
    assert!(allocations >= 100, "{counted}");
}

/// `dev copies` says what the native code copies, where it does: an item copied out, changed and
/// put back where it was, which the native code changes in place, isn't listed, and a value sent on
/// a channel, which the receiving end gets a copy of, is, at the send. A number sent isn't.
#[test]
fn dev_copies_names_the_copies_the_native_code_makes() {
    let dir = scratch("copies_where_made").unwrap();
    let report = |name: &str, source: &str| {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source).unwrap();
        let output = fennec(&["dev", "copies", &program.to_string_lossy()], "").unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        text(&output.stdout)
    };
    let put_back = report("put_back", PUT_BACK);
    assert!(!put_back.contains("copied"), "{put_back}");
    let sends = report("sends", SENDS);
    assert!(sends.contains("sends.fen:11:13 in `main`: what this sends is copied as it's sent"), "{sends}");
    let numbers = report("numbers", NUMBERS_SENT);
    assert!(!numbers.contains("copied"), "{numbers}");
    std::fs::remove_dir_all(&dir).unwrap();
}

const PUT_BACK: &str = r#"from std.collections import Id, Table

struct Entity:
    health: int
    inventory: dict[str, int]
    x: int

def step(ref entities: Table[Entity], ids: list[Id[Entity]], order: list[int]):
    for me in order:
        mut entity = entities[ids[me]]
        entity.health -= 1
        if entity.health <= 0:
            gold = entity.inventory.get("gold") else 0
            entity.inventory["gold"] = gold // 2
            entity.x = me * 3 + 1
        entities[ids[me]] = entity
    return

def main(stdout: Stdout):
    mut entities = Table[Entity]()
    mut ids = list[Id[Entity]]()
    for n in range(100):
        ids.append(entities.add(Entity(health=n % 3, inventory={"gold": n}, x=0)))
    order = [n for n in range(100)]
    step(ref entities, ids, order)
    mut total = 0
    for entity in entities:
        total += entity.inventory["gold"] + entity.x
    stdout.print(total)
    return
"#;

const SENDS: &str = r"from std.concurrent import Closed, channel

enum Command:
    Move(who: int, vx: int, vy: int)
    Heal(who: int, amount: int)

def main(stdout: Stdout) fails Closed:
    mut total = 0
    with channel[Command](size=64) as commands:
        for n in range(50):
            commands.send(.Move(who=n, vx=1, vy=2))?
            command = commands.receive() else break
            match command:
                case .Move(who, vx, vy):
                    total += who + vx + vy
                case .Heal(who, amount):
                    total += who * amount
    stdout.print(total)
    return
";

const NUMBERS_SENT: &str = r"from std.concurrent import Closed, channel

enum Command:
    Move(who: int, vx: int, vy: int)
    Heal(who: int, amount: int)

def main(stdout: Stdout) fails Closed:
    mut total = 0
    with channel[int](size=64) as commands:
        for n in range(50):
            commands.send(n)?
            command = commands.receive() else break
            total += command
    stdout.print(total)
    return
";

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

/// The front page's program is the first Fennec most visitors read, and each other page of the
/// site shows samples too: each compiles as it's shown, as the tour's do, unless marked planned.
#[test]
fn the_sites_front_page_program_and_every_pages_sample_compile() {
    let pages = repo().join("site/src/pages");
    let front = std::fs::read_to_string(pages.join("index.astro")).unwrap();
    let program = front
        .split("const program = `")
        .nth(1)
        .and_then(|rest| rest.split("`;").next())
        .expect("the front page has its program in `const program`");
    let mut samples = vec![("index.astro".to_owned(), program.to_owned())];
    let mut names: Vec<String> = std::fs::read_dir(&pages)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            Path::new(name).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && name != "tour.md"
        })
        .collect();
    names.sort();
    for name in names {
        let page = std::fs::read_to_string(pages.join(&name)).unwrap();
        let mut lines = page.lines().enumerate();
        while let Some((at, line)) = lines.next() {
            let Some(info) = line.strip_prefix("```fennec") else { continue };
            let sample: Vec<&str> =
                lines.by_ref().map(|(_, line)| line).take_while(|line| !line.starts_with("```")).collect();
            if !info.contains("planned") {
                samples.push((format!("{name} at line {}", at + 2), sample.join("\n")));
            }
        }
    }
    let dir = scratch("site").unwrap();
    for (at, (shown, sample)) in samples.iter().enumerate() {
        let name = format!("sample_{at}.fen");
        std::fs::write(dir.join(&name), format!("{sample}\n")).unwrap();
        let output = fennec_in(&dir, &["check", &name], "").unwrap();
        assert!(
            output.status.success(),
            "the site's sample in {shown} doesn't compile:\n{}",
            text(&output.stderr)
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(samples.len() > 3, "found only {} samples on the site", samples.len());
}

/// The language page is the source of truth, so a whole program it shows has to compile as it's
/// shown. A sample marked `fragment`, a piece of a program, or `planned`, not built yet, is left
/// out, and the page says which are which.
#[test]
fn the_language_pages_whole_programs_compile() {
    let page = std::fs::read_to_string(repo().join("docs/language.md")).unwrap();
    let dir = scratch("language").unwrap();
    let mut checked = 0;
    let mut lines = page.lines().enumerate();
    while let Some((at, line)) = lines.next() {
        let Some(info) = line.strip_prefix("```fennec") else { continue };
        let sample: Vec<&str> =
            lines.by_ref().map(|(_, line)| line).take_while(|line| !line.starts_with("```")).collect();
        match info.trim() {
            "fragment" | "planned" => continue,
            "" => {}
            other => panic!("the language page's sample at line {} is marked `{other}`", at + 2),
        }
        let name = format!("line_{}.fen", at + 2);
        std::fs::write(dir.join(&name), sample.join("\n") + "\n").unwrap();
        let output = fennec_in(&dir, &["check", &name], "").unwrap();
        assert!(
            output.status.success(),
            "the language page's sample at line {} doesn't compile:\n{}",
            at + 2,
            text(&output.stderr)
        );
        checked += 1;
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(checked >= 4, "found only {checked} whole programs on the language page");
}

/// The guide says every sample compiles and passes its tests on main. Each whole program is
/// checked as it's shown. One whose fence names a file, as ```` ```fennec src/store.fen ````, is
/// that file of Hoard, the kit the guide builds: they're put together, in order, as the kit, which
/// is checked, and tested where LLVM is here. A fence marked `fragment` or `planned` is left out.
#[test]
fn the_guides_samples_compile_and_its_kit_passes_its_tests() {
    let guide = std::fs::read_to_string(repo().join("docs/guide.md")).unwrap();
    let dir = scratch("guide").unwrap();
    let mut kit_files: Vec<(String, String)> = Vec::new();
    let mut checked = 0;
    let mut lines = guide.lines().enumerate();
    while let Some((at, line)) = lines.next() {
        let Some(info) = line.strip_prefix("```fennec") else { continue };
        let sample: Vec<&str> =
            lines.by_ref().map(|(_, line)| line).take_while(|line| !line.starts_with("```")).collect();
        let info = info.trim();
        if info == "fragment" || info == "planned" {
            continue;
        }
        if !info.is_empty() {
            match kit_files.iter_mut().find(|(file, _)| file == info) {
                Some((_, text)) => {
                    text.push('\n');
                    text.push_str(&sample.join("\n"));
                    text.push('\n');
                }
                None => kit_files.push((info.to_owned(), sample.join("\n") + "\n")),
            }
            continue;
        }
        let name = format!("line_{}.fen", at + 2);
        std::fs::write(dir.join(&name), sample.join("\n") + "\n").unwrap();
        let output = fennec_in(&dir, &["check", &name], "").unwrap();
        assert!(
            output.status.success(),
            "the guide's sample at line {} doesn't compile:\n{}",
            at + 2,
            text(&output.stderr)
        );
        checked += 1;
    }
    assert!(checked >= 8, "found only {checked} whole programs in the guide");
    let names: Vec<&str> = kit_files.iter().map(|(file, _)| file.as_str()).collect();
    assert_eq!(names, ["src/store.fen", "src/index.fen", "src/main.fen"], "the guide's kit has other files");
    assert!(fennec_in(&dir, &["new", "hoard"], "").unwrap().status.success());
    let kit = dir.join("hoard");
    std::fs::remove_file(kit.join("src/greeting.fen")).unwrap();
    for (file, source) in &kit_files {
        std::fs::write(kit.join(file), source).unwrap();
    }
    let checked = fennec_in(&kit, &["check"], "").unwrap();
    assert!(checked.status.success(), "the guide's kit doesn't compile:\n{}", text(&checked.stderr));
    if crate::llvm::ready() {
        let tested = fennec_in(&kit, &["test"], "").unwrap();
        assert!(tested.status.success(), "the guide's kit fails its tests:\n{}", text(&tested.stderr));
        assert!(text(&tested.stdout).contains("3 passed, 0 failed"), "{}", text(&tested.stdout));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Every Fennec program a script, a workflow or a tool runs is a file of its own, checked here
/// against the language as it is now: `.github/programs`, with its kits checked as kits, and the AI
/// test's reference answers in `tools/ai-test/answers`. Written inline, the release dry run's
/// programs went stale unseen when the language changed, and so did 18 of the AI test's 30
/// answers, kept in a TOML file. The test after this one keeps programs out of scripts.
#[test]
fn every_program_a_script_or_tool_runs_compiles() {
    let mut refused = Vec::new();
    let mut checked = 0;
    for folder in [".github/programs", "tools/ai-test/answers"] {
        let mut names: Vec<String> = std::fs::read_dir(repo().join(folder))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| {
                Path::new(name).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("fen"))
            })
            .collect();
        names.sort();
        for name in names {
            let shown = format!("{folder}/{name}");
            let output = fennec(&["check", &shown], "").unwrap();
            if !output.status.success() {
                refused.push(format!("{shown}:\n{}", text(&output.stderr)));
            }
            checked += 1;
        }
    }
    assert!(checked >= 35, "found only {checked} programs");
    // The release dry run's kits, together, as one depends on the other by its path.
    let dir = scratch("script-kits").unwrap();
    copy_folder(&repo().join(".github/programs/kits"), &dir).unwrap();
    for kit in ["greeting", "hello"] {
        let output = fennec_in(&dir.join(kit), &["check"], "").unwrap();
        if !output.status.success() {
            refused.push(format!(".github/programs/kits/{kit}:\n{}", text(&output.stderr)));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(refused.is_empty(), "these programs don't compile:\n\n{}", refused.join("\n"));
}

/// No script, workflow or tool holds a Fennec program inline, where no test sees it: each goes in a
/// file of its own in `.github/programs`, which the test before this one checks. A line is taken
/// for Fennec when it starts a `def` with typed parameters, or none, where code starts, as at the
/// start of a heredoc's line or a `printf`'s text, or when it has `stdout.print(` or `: Stdout`.
/// The Python a workflow runs inline, with its untyped parameters, isn't taken for it. Soak's
/// `soak-copy.pl` writes Fennec around a program, and the test after this one checks what it
/// writes.
#[test]
fn no_script_workflow_or_tool_holds_a_fennec_program_inline() {
    let mut files = Vec::new();
    scripts_under(&repo(), &mut files).unwrap();
    assert!(files.len() > 40, "found only {} scripts", files.len());
    let mut inline = Vec::new();
    for file in &files {
        let shown = file.strip_prefix(repo()).unwrap_or(file).to_string_lossy().replace('\\', "/");
        if shown == ".github/scripts/soak-copy.pl" {
            continue;
        }
        let source = std::fs::read_to_string(file).unwrap();
        let python = file.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
        for (at, line) in source.lines().enumerate() {
            if holds_fennec(line, python) {
                inline.push(format!("{shown}:{}: {}", at + 1, line.trim()));
            }
        }
    }
    assert!(
        inline.is_empty(),
        "these lines hold Fennec inline, which no test checks: put each program in a file of its own in \
         .github/programs, which `every_program_a_script_or_tool_runs_compiles` checks, and have the \
         script copy it\n{}",
        inline.join("\n")
    );
}

/// Soak's copy of a program is Fennec that `soak-copy.pl` writes around it, so it's checked as the
/// soak writes it, for each program the nightly soaks and both ways the soak runs one, in a task
/// and on the program's own thread.
#[test]
#[cfg(unix)]
fn soaks_copy_of_each_program_it_soaks_compiles() {
    let proofs = std::fs::read_to_string(repo().join(".github/workflows/proofs.yml")).unwrap();
    let soak = proofs.split("\n  soak:\n").nth(1).expect("proofs.yml has its soak job");
    let listed = soak
        .lines()
        .find_map(|line| line.trim().strip_prefix("program: ["))
        .expect("the soak job lists its programs, as `program: [...]`");
    let programs: Vec<&str> = listed.trim_end_matches(']').split(',').map(str::trim).collect();
    assert!(programs.len() >= 2, "found only {programs:?} in the soak job");
    let dir = scratch("soak-copies").unwrap();
    for program in &programs {
        let source = std::fs::read(repo().join(format!("bench/realworld/{program}.fen"))).unwrap();
        for way in ["task", "main"] {
            let mut copying = std::process::Command::new("perl")
                .arg(repo().join(".github/scripts/soak-copy.pl"))
                .env("SOAK_IN", way)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            copying.stdin.take().unwrap().write_all(&source).unwrap();
            let copied = copying.wait_with_output().unwrap();
            assert!(copied.status.success(), "soak-copy.pl refused {program}: {}", text(&copied.stderr));
            let name = format!("{program}_{way}.fen");
            std::fs::write(dir.join(&name), &copied.stdout).unwrap();
            let output = fennec_in(&dir, &["check", &name], "").unwrap();
            assert!(
                output.status.success(),
                "soak's copy of {program}, run in {way}, doesn't compile:\n{}",
                text(&output.stderr)
            );
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Copies a folder and everything in it into `to`, which it makes.
fn copy_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let copy = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_folder(&entry.path(), &copy)?;
        } else {
            std::fs::copy(entry.path(), &copy)?;
        }
    }
    Ok(())
}

/// Every script under `dir`, as `no_script_workflow_or_tool_holds_a_fennec_program_inline` reads
/// them: shell, Perl, Python, workflows and TOML, and any file without an extension that starts
/// with `#!`. What's built, std and the examples, which have tests of their own, are left out, and
/// so is every hidden folder but `.github`.
fn scripts_under(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            let left_out = ["target", "node_modules", "std", "examples"].contains(&name.as_str())
                || (name.starts_with('.') && name != ".github");
            if !left_out {
                scripts_under(&path, found)?;
            }
            continue;
        }
        let script = match path.extension() {
            Some(extension) => ["sh", "bash", "pl", "py", "yml", "yaml", "toml"]
                .iter()
                .any(|kind| extension.eq_ignore_ascii_case(kind)),
            None => std::fs::read(&path)?.starts_with(b"#!"),
        };
        if script {
            found.push(path);
        }
    }
    Ok(())
}

/// Whether a line of a script holds Fennec, as `no_script_workflow_or_tool_holds_a_fennec_program_inline`
/// says. A comment never does.
fn holds_fennec(line: &str, python: bool) -> bool {
    if line.trim_start().starts_with('#') {
        return false;
    }
    if line.contains("stdout.print(") || line.contains(": Stdout") {
        return true;
    }
    if python {
        return false;
    }
    line.match_indices("def ").any(|(at, _)| {
        let before = line.get(..at).unwrap_or_default();
        let starts_code =
            before.is_empty() || before.ends_with([' ', '\t', '\'', '"']) || before.ends_with("\\n");
        let rest = line.get(at + 4..).unwrap_or_default();
        let name = rest
            .find(|letter: char| !(letter.is_ascii_alphanumeric() || letter == '_'))
            .unwrap_or(rest.len());
        let Some(parameters) = rest.get(name..).and_then(|after| after.strip_prefix('(')) else {
            return false;
        };
        let parameters = parameters.split(')').next().unwrap_or_default();
        starts_code && name > 0 && (parameters.is_empty() || parameters.contains(": "))
    })
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
    let maths = "## Twice the number.\nexport def twice(number: int) -> int:\n    return number * 2\n";
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

/// A single file is kept for its next run as a kit is, with the files it read: itself and what it
/// imports. An edit to either runs the edit, and a mistake in either is found.
#[test]
fn a_single_files_run_after_an_edit_to_it_or_what_it_imports_runs_the_edit() {
    let dir = scratch("single_file_edits").unwrap();
    let maths = "export def twice(number: int) -> int:\n    return number * 2\n";
    let main = "from maths import twice\n\ndef main(stdout: Stdout):\n    stdout.print(twice(21))\n";
    std::fs::write(dir.join("maths.fen"), maths).unwrap();
    std::fs::write(dir.join("main.fen"), main).unwrap();
    let run = || {
        crate::executables::fennec_command()
            .args(["run", "--no-test", "main.fen"])
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    let first = run();
    assert_eq!(text(&first.stdout), "42\n", "{}", text(&first.stderr));
    // Unchanged, from what the first run kept.
    assert_eq!(text(&run().stdout), "42\n");
    // What it imports changes.
    std::fs::write(dir.join("maths.fen"), maths.replace("number * 2", "number + number + 1")).unwrap();
    assert_eq!(text(&run().stdout), "43\n");
    // What it imports has a mistake.
    std::fs::write(dir.join("maths.fen"), maths.replace("number * 2", "number * \"two\"")).unwrap();
    let broken = run();
    assert!(
        !broken.status.success() && text(&broken.stderr).contains("maths.fen"),
        "{}",
        text(&broken.stderr)
    );
    // Put right, then the file itself changes.
    std::fs::write(dir.join("maths.fen"), maths).unwrap();
    assert_eq!(text(&run().stdout), "42\n");
    std::fs::write(dir.join("main.fen"), main.replace("twice(21)", "twice(5)")).unwrap();
    assert_eq!(text(&run().stdout), "10\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The run after a change starts from what the run before it built, as every run after that does:
/// the run that keeps the program's image notes what it built for it, so the next run, finding the
/// image, has nothing to check or build. It once checked and built the program all over again, a
/// third of a cold run, and only the run after that was free.
#[test]
fn the_run_after_a_change_starts_from_what_the_run_before_built() {
    let dir = scratch("second_run_kept").unwrap();
    let main = "def main(stdout: Stdout):\n    stdout.print(42)\n    return\n";
    std::fs::write(dir.join("main.fen"), main).unwrap();
    let home = dir.join("home");
    let run = || {
        crate::executables::fennec_command()
            .args(["run", "--no-test", "main.fen"])
            .current_dir(&dir)
            .env("FENNEC_HOME", &home)
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    // What runs have noted they built for a kept image, one entry each, in every version's cache.
    let builds_noted = || -> usize {
        std::fs::read_dir(home.join("cache"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|version| std::fs::read_dir(version.path().join("run-roots")).ok())
            .map(|entries| entries.flatten().count())
            .sum()
    };
    let first = run();
    assert_eq!(text(&first.stdout), "42\n", "{}", text(&first.stderr));
    assert_eq!(builds_noted(), 1, "the first run notes what it built for the image it kept");
    // The next run finds it, and builds nothing to note.
    assert_eq!(text(&run().stdout), "42\n");
    assert_eq!(builds_noted(), 1);
    // A change is another program: its first run notes its own.
    std::fs::write(dir.join("main.fen"), main.replace("42", "43")).unwrap();
    assert_eq!(text(&run().stdout), "43\n");
    assert_eq!(builds_noted(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A single file's kept image holds every file it read, however deep: an edit to what an import
/// imports, one of them swapped for a link to a file that says otherwise, the link pointed at
/// another, or taken away, is seen, and never runs the old program. Each file is noted by the path
/// it was read by: noted by where a link led, a link pointed elsewhere or taken away was missed,
/// and a later run ran what it once led to.
#[test]
fn a_single_files_kept_image_is_passed_over_when_any_file_it_read_changes() {
    let dir = scratch("single_file_deep").unwrap();
    std::fs::write(dir.join("leaf.fen"), "export def base() -> int:\n    return 1\n").unwrap();
    std::fs::write(
        dir.join("mid.fen"),
        "from leaf import base\n\nexport def twice() -> int:\n    return base() * 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.fen"),
        "from mid import twice\n\ndef main(stdout: Stdout):\n    stdout.print(twice())\n",
    )
    .unwrap();
    let run = || {
        crate::executables::fennec_command()
            .args(["run", "--no-test", "main.fen"])
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    let first = run();
    assert_eq!(text(&first.stdout), "2\n", "{}", text(&first.stderr));
    assert_eq!(text(&run().stdout), "2\n");
    // What an import imports changes.
    std::fs::write(dir.join("leaf.fen"), "export def base() -> int:\n    return 5\n").unwrap();
    assert_eq!(text(&run().stdout), "10\n");
    // It's swapped for a link to a file that says otherwise.
    #[cfg(unix)]
    {
        std::fs::write(dir.join("other.fen"), "export def base() -> int:\n    return 7\n").unwrap();
        std::fs::remove_file(dir.join("leaf.fen")).unwrap();
        std::os::unix::fs::symlink(dir.join("other.fen"), dir.join("leaf.fen")).unwrap();
        assert_eq!(text(&run().stdout), "14\n");
        assert_eq!(text(&run().stdout), "14\n");
        // The link is pointed at another file, after a run that started from what the last built.
        std::fs::write(dir.join("third.fen"), "export def base() -> int:\n    return 3\n").unwrap();
        std::fs::remove_file(dir.join("leaf.fen")).unwrap();
        std::os::unix::fs::symlink(dir.join("third.fen"), dir.join("leaf.fen")).unwrap();
        assert_eq!(text(&run().stdout), "6\n");
        assert_eq!(text(&run().stdout), "6\n");
    }
    // It's taken away.
    std::fs::remove_file(dir.join("leaf.fen")).unwrap();
    let gone = run();
    assert!(!gone.status.success() && text(&gone.stderr).contains("leaf"), "{}", text(&gone.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A build of files that haven't changed is the program the store has from the last build of them,
/// put where this one goes; an edit, to a kit's own file or to what a single file imports, builds
/// the new program; and the same program in two folders is two programs, each naming its own.
#[test]
fn a_build_of_unchanged_files_is_the_program_the_last_one_made() {
    if !crate::llvm::ready() {
        return;
    }
    let dir = scratch("build_reuse").unwrap();
    let says = |word: &str| format!("def main(stdout: Stdout):\n    stdout.print(\"{word}\")\n");
    let kit = dir.join("kit");
    write_kit(&kit, "greeter", "", &[("main.fen", &says("first"))]).unwrap();
    let build = |at: &Path, args: &[&str]| {
        let output = crate::executables::fennec_command()
            .arg("build")
            .args(args)
            .current_dir(at)
            .env("FENNEC_HOME", dir.join("home"))
            .env("FENNEC_NO_DAEMON", "1")
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
    };
    let ran = |program: &Path| {
        let output = std::process::Command::new(program).output().unwrap();
        (text(&output.stdout), text(&output.stderr))
    };
    let out = dir.join("out");
    let out_text = out.to_string_lossy().into_owned();
    build(&kit, &["-o", &out_text]);
    assert_eq!(ran(&out).0, "first\n");
    // Unchanged, from the store, to the same place and to another.
    build(&kit, &["-o", &out_text]);
    assert_eq!(ran(&out).0, "first\n");
    let other = dir.join("other");
    build(&kit, &["-o", &other.to_string_lossy()]);
    assert_eq!(ran(&other).0, "first\n");
    // With no `-o`, named for the kit, as the build that stored it was.
    build(&kit, &[]);
    build(&kit, &[]);
    assert_eq!(ran(&kit.join("greeter")).0, "first\n");
    // An edit builds the new program.
    std::fs::write(kit.join("src/main.fen"), says("second")).unwrap();
    build(&kit, &["-o", &out_text]);
    assert_eq!(ran(&out).0, "second\n");
    // A single file, and what it imports.
    let single = dir.join("single");
    std::fs::create_dir_all(&single).unwrap();
    std::fs::write(single.join("word.fen"), "export def word() -> str:\n    return \"one\"\n").unwrap();
    std::fs::write(
        single.join("main.fen"),
        "from word import word\n\ndef main(stdout: Stdout):\n    stdout.print(word())\n",
    )
    .unwrap();
    build(&single, &["main.fen", "-o", &out_text]);
    build(&single, &["main.fen", "-o", &out_text]);
    assert_eq!(ran(&out).0, "one\n");
    std::fs::write(single.join("word.fen"), "export def word() -> str:\n    return \"two\"\n").unwrap();
    build(&single, &["main.fen", "-o", &out_text]);
    assert_eq!(ran(&out).0, "two\n");
    // The same failing program in two folders: each build's program names its own.
    let failing = "def main(stdout: Stdout):\n    zero = len(\"\")\n    stdout.print(1 // zero)\n";
    for folder in ["a", "b", "a", "b"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
        std::fs::write(dir.join(folder).join("main.fen"), failing).unwrap();
        let program = dir.join(format!("{folder}.program"));
        build(&dir, &[&format!("{folder}/main.fen"), "-o", &program.to_string_lossy()]);
        let said = ran(&program).1;
        assert!(said.contains(&format!("{folder}/main.fen")), "{folder}: {said}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A build handed to a process of its own, as the daemon hands its builds, makes the same program
/// byte for byte as one built in the command's own process, dev and release alike.
#[cfg(unix)]
#[test]
fn a_build_in_its_own_process_makes_the_same_program() {
    if !crate::llvm::ready() {
        return;
    }
    let dir = scratch("build_child_same").unwrap();
    let kit = dir.join("kit");
    let main = "def main(stdout: Stdout):\n    stdout.print(\"same\")\n";
    write_kit(&kit, "greeter", "", &[("main.fen", main)]).unwrap();
    let out = dir.join("out");
    for release in [false, true] {
        let kind = if release { "release" } else { "dev" };
        let built = |home: &str, child: bool| {
            let mut command = crate::executables::fennec_command();
            command.arg("build");
            if release {
                command.arg("--release");
            }
            command
                .args(["-o", &out.to_string_lossy()])
                .current_dir(&kit)
                .env("FENNEC_HOME", dir.join(home))
                .env_remove("FENNEC_NO_CACHE");
            if child {
                command.env("FENNEC_BUILD_CHILD", "1");
            } else {
                command.env_remove("FENNEC_BUILD_CHILD");
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{}", text(&output.stderr));
            assert!(!text(&output.stderr).contains("own process"), "{}", text(&output.stderr));
            std::fs::read(&out).unwrap()
        };
        let here = built(&format!("home-here-{kind}"), false);
        let apart = built(&format!("home-child-{kind}"), true);
        assert!(here == apart, "a {kind} build in its own process made another program");
    }
}

/// A build whose own process crashes, or never says it started, is built in the command's process
/// instead, saying why.
#[cfg(unix)]
#[test]
fn a_build_whose_own_process_fails_is_built_here_saying_why() {
    if !crate::llvm::ready() || !cfg!(debug_assertions) {
        return;
    }
    let dir = scratch("build_child_fails").unwrap();
    let kit = dir.join("kit");
    write_kit(
        &kit,
        "greeter",
        "",
        &[("main.fen", "def main(stdout: Stdout):\n    stdout.print(\"here\")\n")],
    )
    .unwrap();
    let out = dir.join("out");
    for (failing, said) in [("crash", "was ended by signal 9"), ("hang", "it didn't start within 2 s")] {
        let output = crate::executables::fennec_command()
            .args(["build", "-o", &out.to_string_lossy()])
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join(format!("home-{failing}")))
            .env("FENNEC_BUILD_CHILD", "1")
            .env("FENNEC_TEST_BUILD_CHILD", failing)
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap();
        let stderr = text(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(stderr.contains("in its own process gave no answer, so it's built here"), "{stderr}");
        assert!(stderr.contains(said), "{stderr}");
        let ran = std::process::Command::new(&out).output().unwrap();
        assert_eq!(text(&ran.stdout), "here\n");
    }
}

/// A command killed while its build's own process is working leaves no process behind: that
/// process sees its socket close and ends with everything it started.
#[cfg(unix)]
#[test]
fn a_command_killed_mid_build_leaves_no_build_process() {
    if !crate::llvm::ready() || !cfg!(debug_assertions) {
        return;
    }
    let dir = scratch("build_child_killed").unwrap();
    let kit = dir.join("kit");
    write_kit(
        &kit,
        "greeter",
        "",
        &[("main.fen", "def main(stdout: Stdout):\n    stdout.print(\"gone\")\n")],
    )
    .unwrap();
    let noted = dir.join("child-pid");
    let mut command = crate::executables::fennec_command()
        .args(["build", "-o", &dir.join("out").to_string_lossy()])
        .current_dir(&kit)
        .env("FENNEC_HOME", dir.join("home"))
        .env("FENNEC_BUILD_CHILD", "1")
        .env("FENNEC_TEST_BUILD_CHILD", format!("hang-building:{}", noted.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&noted).ok().filter(|pid| !pid.is_empty()) {
            break pid;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "the build's process never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    command.kill().unwrap();
    command.wait().unwrap();
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let gone = std::time::Instant::now();
    while alive() && gone.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!alive(), "the build's process {pid} outlived the command");
}

/// The same program in two folders is two programs to the store: each run's failure names its own
/// folder, and quotes its own text, as a comment changed to one just as long, which leaves the
/// program the same, shows.
#[test]
fn a_failure_names_the_folder_and_quotes_the_text_its_run_was_given() {
    let dir = scratch("kept_by_path").unwrap();
    let source = "def main(stdout: Stdout):\n    zero = len(\"\")\n    stdout.print(1 // zero)  # first\n";
    for folder in ["one", "two"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
        std::fs::write(dir.join(folder).join("main.fen"), source).unwrap();
    }
    let run = |folder: &str| {
        let output = crate::executables::fennec_command()
            .args(["run", "--no-test", "main.fen"])
            .current_dir(dir.join(folder))
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", text(&output.stdout));
        text(&output.stderr)
    };
    // Each twice, so the second run of each is the one the store has.
    for folder in ["one", "two", "one", "two"] {
        let said = run(folder);
        assert!(said.contains("division by zero") && said.contains("# first"), "{said}");
    }
    std::fs::write(dir.join("one/main.fen"), source.replace("# first", "# other")).unwrap();
    for _ in 0..2 {
        let said = run("one");
        assert!(said.contains("# other") && !said.contains("# first"), "{said}");
    }
    // Run from outside the folder, so the path it's given names it.
    for folder in ["one", "two", "one", "two"] {
        let output = crate::executables::fennec_command()
            .args(["run", "--no-test", &format!("{folder}/main.fen")])
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap();
        let said = text(&output.stderr);
        assert!(said.contains(&format!("{folder}/main.fen")), "{folder}: {said}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit's kept image holds its manifest and the files of what it depends on by path: a manifest
/// that names another dependency, or an edit to a dependency's file, runs the new program.
#[test]
fn a_kits_kept_image_is_passed_over_when_its_manifest_or_a_dependency_changes() {
    let dir = scratch("kit_image_manifest").unwrap();
    let says = |word: &str| format!("export def word() -> str:\n    return \"{word}\"\n");
    write_kit(&dir.join("first"), "first", "", &[("lib.fen", &says("first"))]).unwrap();
    write_kit(&dir.join("second"), "second", "", &[("lib.fen", &says("second"))]).unwrap();
    let main = |kit: &str| {
        format!("from {kit} import word\n\ndef main(stdout: Stdout):\n    stdout.print(word())\n")
    };
    let app = dir.join("app");
    write_kit(
        &app,
        "app",
        "[dependencies]\nfirst = { path = \"../first\" }\n",
        &[("main.fen", &main("first"))],
    )
    .unwrap();
    let run = || {
        crate::executables::fennec_command()
            .args(["run", "--no-test"])
            .current_dir(&app)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    let first = run();
    assert_eq!(text(&first.stdout), "first\n", "{}", text(&first.stderr));
    assert_eq!(text(&run().stdout), "first\n");
    // A dependency's file changes.
    std::fs::write(dir.join("first/src/lib.fen"), says("changed")).unwrap();
    assert_eq!(text(&run().stdout), "changed\n");
    // The manifest names another dependency, which the program then imports.
    write_kit(
        &app,
        "app",
        "[dependencies]\nsecond = { path = \"../second\" }\n",
        &[("main.fen", &main("second"))],
    )
    .unwrap();
    assert_eq!(text(&run().stdout), "second\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What std's code does with names a program may also define: each line is the same whoever
/// defines what.
const STD_SAYS: &str = r#"
def std_says() -> list[str]:
    mut said = list[str]()
    said.append(f"{time.Duration.seconds(1.5)}")
    said.append("ab".ljust(5) + "|" + "ab".rjust(5) + "|" + "ab".center(6) + "|")
    said.append("hello world".title() + " " + "abc".capitalize())
    mut tree = collections.Tree[str]()
    root = tree.add("root")
    left = tree.add("left", parent=root)
    _ = tree.add("leaf", parent=left)
    _ = tree.add("right", parent=root)
    said.append(", ".join([tree[node] for node in tree.descendants(root)]))
    mut router = net.Router()
    router.handle("GET /a/{first}/{second}", (request, found) => net.Response(status=200, body=found.text_at(1).encode()))
    answer = router.respond(net.Request(method="GET", path="/a/1/2"))
    said.append(f"{answer.status}")
    said.append(deep_json())
    said.append(twice_toml())
    return said

def deep_json() -> str:
    mut deep = ""
    for _ in range(600):
        deep = deep + "["
    for _ in range(600):
        deep = deep + "]"
    _ = json.loads[list[int]](deep) except why:
        return f"{why}"
    return "read"

def twice_toml() -> str:
    _ = toml.loads[dict[str, int]]("a = 1\na = 2") except why:
        return f"{why}"
    return "read"
"#;

/// What the builtins give, called by name.
const BUILTINS_SAY: &str = r#"
struct Spot:
    x: int
    y: int

## What the builtins give, called by name.
export def builtins_say() -> list[str]:
    numbers = [3, 1, 2]
    moved = copy(Spot(x=1, y=2), x=5)
    return [
        f"{round(2.5)} {round(2.567, 2)}",
        f"{reversed(numbers)} {sorted(numbers)}",
        f"{any([False, True])} {all([True, False])}",
        f"{moved.x} {moved.y} {len(numbers)} {abs(-4)}",
        f"{min(numbers)} {max(numbers)} {sum(numbers)}",
    ]
"#;

/// Every name a program may define that the language or the standard library also has at the top
/// level: each builtin function a program can name, and each of std's own functions and constants,
/// read from std's files, so a name added later is covered too.
fn names_std_has() -> Result<(Vec<String>, Vec<String>), String> {
    let mut functions = std::collections::BTreeSet::new();
    let mut constants = std::collections::BTreeSet::new();
    for name in fennec_check::builtin_function_names() {
        functions.insert(name.to_owned());
    }
    for (file, source) in fennec_syntax::prelude_files() {
        let module =
            fennec_syntax::parse_prelude(source).map_err(|problem| format!("{file}: {problem:?}"))?;
        for item in module.items {
            match item {
                fennec_syntax::Item::Function(def) => {
                    functions.insert(def.name);
                }
                fennec_syntax::Item::Const { name, .. } => {
                    constants.insert(name);
                }
                _ => {}
            }
        }
    }
    let free = |name: &String| !name.starts_with('_') && name != "main" && !fennec_check::reserved_name(name);
    let functions =
        functions.into_iter().filter(|name| free(name) && name.starts_with(|c: char| c.is_ascii_lowercase()));
    let constants =
        constants.into_iter().filter(|name| free(name) && name.starts_with(|c: char| c.is_ascii_uppercase()));
    Ok((functions.collect(), constants.collect()))
}

/// A function of every name in `functions` and a constant of every name in `constants`, with
/// `own_total` adding up what each gives, `export`ed when `export` says.
fn defining(functions: &[String], constants: &[String], export: &str) -> String {
    let defined =
        functions.iter().flat_map(|name| ["def ", name, "(number: int) -> int:\n    return number + 7\n\n"]);
    let constant = constants.iter().flat_map(|name| [name.as_str(), " = 7\n"]);
    let added = functions.iter().flat_map(|name| ["    total += ", name, "(1)\n"]);
    let read = constants.iter().flat_map(|name| ["    total += ", name, "\n"]);
    let total = [
        "\n## What every name defined here gives, added up.\n",
        export,
        "def own_total() -> int:\n    mut total = 0\n",
    ];
    defined.chain(constant).chain(total).chain(added).chain(read).chain(["    return total\n"]).collect()
}

/// A program or a kit defining every name the language and the standard library also have at the
/// top level changes nothing std's code does: a program's `round` is its own, and std's
/// `Duration.seconds` still rounds with the builtin, as std's `ljust` still pads with its own
/// helper and `Router` still holds its own limit. A dependency that defines none of them still
/// calls the builtins, beside a program defining all of them. Each runs as a program defining
/// nothing does.
#[test]
fn a_program_or_kit_defining_every_builtin_name_changes_nothing_std_does() {
    let dir = scratch("names_std_has").unwrap();
    let (functions, constants) = names_std_has().unwrap();
    assert!(
        functions.iter().any(|name| name == "round")
            && constants.iter().any(|name| name == "MOST_ROUTE_PARAMETERS")
    );
    let imports = "from std import collections, json, log, net, time, toml\n";
    let std_says = STD_SAYS.replacen("def std_says", "## What std's code says.\nexport def std_says", 1);
    let logged = "    lines = log.Log(out=stdout, level=.Info)\n    lines.with_fields({\"user\": \"ann\"}).info(\"hello\")\n";
    let printed = |from: &str| {
        format!("    for line in {from}():\n        stdout.print(line)\n    stdout.print(\"--\")\n")
    };
    let run = |at: &Path| {
        let output = crate::executables::fennec_command()
            .args(["run", "--no-test"])
            .current_dir(at)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        text(&output.stdout)
    };
    // What a program defining none of the names gets.
    let plain_main = format!(
        "{imports}{STD_SAYS}{BUILTINS_SAY}\ndef main(stdout: Stdout):\n{logged}{}{}",
        printed("std_says"),
        printed("builtins_say")
    );
    write_kit(&dir.join("alone"), "alone", "", &[("main.fen", &plain_main)]).unwrap();
    let alone = run(&dir.join("alone"));
    // The program and one kit define every name, and another kit, defining none, calls the builtins.
    write_kit(&dir.join("plain"), "plain", "", &[("lib.fen", BUILTINS_SAY)]).unwrap();
    let hostile = format!("{imports}{std_says}\n{}", defining(&functions, &constants, "export "));
    write_kit(&dir.join("hostile"), "hostile", "", &[("lib.fen", &hostile)]).unwrap();
    // A kit that imports every name the program imports, each as another module.
    let swapped = "from std import json as collections, log as json, net as log, time as net, toml as time, \
        collections as toml\n\n## What its own `time` is.\nexport def swapped() -> str:\n    return time.dumps({\"a\": 1}).strip()\n";
    write_kit(&dir.join("swapped"), "swapped", "", &[("lib.fen", swapped)]).unwrap();
    let app_main = format!(
        "import hostile\nimport plain\nimport swapped\n{imports}{STD_SAYS}\n{}\ndef main(stdout: Stdout):\n{logged}{}{}{}    stdout.print(own_total(), hostile.own_total(), swapped.swapped())\n",
        defining(&functions, &constants, ""),
        printed("std_says"),
        printed("plain.builtins_say"),
        printed("hostile.std_says"),
    );
    write_kit(
        &dir.join("app"),
        "app",
        "[dependencies]\nhostile = { path = \"../hostile\" }\nplain = { path = \"../plain\" }\nswapped = { path = \"../swapped\" }\n",
        &[("main.fen", &app_main)],
    )
    .unwrap();
    let (log_and_std, builtins) = alone.split_once("--\n").unwrap();
    let (_, std_lines) = log_and_std.split_once('\n').unwrap();
    let total = 8 * functions.len() + 7 * constants.len();
    assert_eq!(
        run(&dir.join("app")),
        format!("{log_and_std}--\n{builtins}{std_lines}--\n{total} {total} a = 1\n")
    );
    // A dependency can't name what std keeps to itself, any more than a program can.
    let peeks =
        "## Pads with std's own helper.\nexport def peeks() -> str:\n    return padding(\"x\", count=2)\n";
    write_kit(&dir.join("peek"), "peek", "", &[("lib.fen", peeks)]).unwrap();
    let main = "from peek import peeks\n\ndef main(stdout: Stdout):\n    stdout.print(peeks())\n";
    let peeking = dir.join("peeking");
    write_kit(&peeking, "peeking", "[dependencies]\npeek = { path = \"../peek\" }\n", &[("main.fen", main)])
        .unwrap();
    let refused = crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .current_dir(&peeking)
        .env("FENNEC_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        text(&refused.stderr).contains("`padding` is private to the standard library"),
        "{}",
        text(&refused.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A `_` field is its file's: a kit's own file reaches it, and a program that depends on the kit
/// reaches none of it, to read, set or build the type with one. The same as another file of the
/// kit would be told.
#[test]
fn another_kits_underscore_fields_are_out_of_reach() {
    let dir = scratch("underscore_kits").unwrap();
    let vault = "## A secret kept in its file.\nexport struct Vault:\n    label: str\n    _secret = \"\"\n\n\
                 ## A vault holding `secret`, which only this file can set.\nexport def sealed(label: str, secret: str) -> Vault:\n    \
                 return Vault(label=label, _secret=secret)\n\n## Whether `guess` is the secret.\n\
                 export def opens(vault: Vault, guess: str) -> bool:\n    return vault._secret == guess\n";
    write_kit(&dir.join("vault"), "vault", "", &[("lib.fen", vault)]).unwrap();
    let check = |main: &str| {
        write_kit(
            &dir.join("app"),
            "app",
            "[dependencies]\nvault = { path = \"../vault\" }\n",
            &[("main.fen", main)],
        )
        .unwrap();
        crate::executables::fennec_command()
            .args(["check"])
            .current_dir(dir.join("app"))
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap()
    };
    let fine = check(
        "from vault import Vault, opens, sealed\n\ndef main(stdout: Stdout):\n    stdout.print(opens(sealed(\"a\", secret=\"b\"), \"b\"))\n",
    );
    assert!(fine.status.success(), "{}", text(&fine.stderr));
    for body in [
        "stdout.print(sealed(\"a\", secret=\"b\")._secret)",
        "stdout.print(Vault(label=\"a\", _secret=\"b\").label)",
    ] {
        let refused = check(&format!(
            "from vault import Vault, opens, sealed\n\ndef main(stdout: Stdout):\n    {body}\n"
        ));
        assert!(!refused.status.success(), "{body}");
        assert!(
            text(&refused.stderr).contains("`_secret` is private to `vault.Vault`'s file"),
            "{}",
            text(&refused.stderr)
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What each file imports is its own, in a program's files and in its kits'. A kit's
/// `from std.html import escape as sha256` once made the program's own `sha256(...)` give escaped
/// text, as one table of imports served every file and the last import of a name decided it for
/// all. Now each file's names are its own:
/// - `sha256` beside a kit's, `dumps` from JSON beside a kit's from TOML, and `pick` as `floor`
///   beside a kit's or another of the program's files' `pick` as `ceil`;
/// - a name only a kit, or only another file, imports is refused where it isn't imported;
/// - a hash given its chunks, imported by name or through its module, goes to std's function for
///   chunks, as the checker sends it there.
#[test]
fn what_each_file_imports_is_its_own() {
    let dir = scratch("imports_by_file").unwrap();
    let fennec = |at: &Path| {
        crate::executables::fennec_command()
            .args(["run", "--no-test"])
            .current_dir(at)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap()
    };
    let ran = |at: &Path| {
        let output = fennec(at);
        assert!(output.status.success(), "{}", text(&output.stderr));
        text(&output.stdout)
    };
    let kit =
        |name: &str, source: &str| write_kit(&dir.join(name), name, "", &[("lib.fen", source)]).unwrap();
    kit(
        "escaping",
        "from std.html import escape as sha256\nfrom std.toml import dumps\nfrom std.math import ceil as pick\n\n\
         ## Its own `sha256`, `dumps` and `pick`.\nexport def own() -> str:\n    escaped = sha256(\"<kit>\")\n    \
         saved = dumps({\"a\": 1}).strip()\n    return f\"{escaped} {saved} {pick(2.2)}\"\n",
    );
    let app = dir.join("app");
    let main = "from std.crypto import sha256\nfrom std.json import dumps\nfrom std.math import floor as pick\n\
        import escaping\nimport other\n\n\
        def main(stdout: Stdout):\n    stdout.print(sha256(\"abc\"), dumps({\"a\": 1}), pick(2.7))\n    \
        stdout.print(escaping.own(), other.picked())\n    \
        stdout.print(sha256([\"a\", \"bc\"]) == sha256(\"abc\"), other.chunked())\n";
    let other = "from std import crypto\nfrom std.math import ceil as pick\n\n## Its own `pick`.\nexport def picked() -> int:\n    return pick(2.2)\n\n\
        ## Hashed in chunks.\nexport def chunked() -> bool:\n    return crypto.blake3([\"a\", \"bc\"]) == crypto.blake3(\"abc\")\n";
    write_kit(
        &app,
        "app",
        "[dependencies]\nescaping = { path = \"../escaping\" }\n",
        &[("main.fen", main), ("other.fen", other)],
    )
    .unwrap();
    assert_eq!(
        ran(&app),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad {\"a\": 1} 2\n&lt;kit&gt; a = 1 3 3\nTrue True\n"
    );
    // A name only a kit imports, or only another of the program's files.
    kit(
        "lifting",
        "from std.math import ceil as lift\nfrom std import math as mathy\n\n## Uses both.\nexport def plain() -> float:\n    return lift(1.5) + mathy.sqrt(4.0)\n",
    );
    for (main, other, refused) in [
        (
            "import lifting\n\ndef main(stdout: Stdout):\n    stdout.print(lift(2.2), lifting.plain())\n",
            None,
            "`lift` doesn't exist",
        ),
        (
            "import lifting\n\ndef main(stdout: Stdout):\n    stdout.print(mathy.sqrt(16.0), lifting.plain())\n",
            None,
            "`mathy` doesn't exist",
        ),
        (
            "import lifting\nimport other\n\ndef main(stdout: Stdout):\n    stdout.print(other.lifted(), lifting.plain())\n",
            Some("## Calls what only the kit imported.\nexport def lifted() -> int:\n    return lift(2.2)\n"),
            "`lift` doesn't exist",
        ),
    ] {
        let mut files = vec![("main.fen", main)];
        files.extend(other.map(|other| ("other.fen", other)));
        let refusing = dir.join("refusing");
        let _ = std::fs::remove_dir_all(&refusing);
        write_kit(&refusing, "refusing", "[dependencies]\nlifting = { path = \"../lifting\" }\n", &files)
            .unwrap();
        let output = fennec(&refusing);
        assert!(!output.status.success(), "{main}");
        assert!(text(&output.stderr).contains(refused), "{main}: {}", text(&output.stderr));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program the store has is the one built with the output switches a run has now: one run
/// with `FENNEC_BOXED_RESULTS` or `FENNEC_STRIP_FACTS` makes a program of its own, and runs
/// without them go on using theirs.
///
/// Counted by the programs the store holds, each under its root, since how a run puts one out
/// differs: on a Mac it's the store's own file, and elsewhere a copy, a new file every run.
#[test]
fn a_run_with_another_output_switch_never_gets_the_program_built_without_it() {
    let dir = scratch("run_switches").unwrap();
    std::fs::write(dir.join("main.fen"), "def main(stdout: Stdout):\n    stdout.print(42)\n").unwrap();
    // The programs the store holds, one a root: one written as bytes is a file named for the root,
    // and one kept as the file that ran is named by the root's `.sum` file, beside the program.
    let stored = || {
        let mut found = 0;
        for version in std::fs::read_dir(dir.join("home/cache")).into_iter().flatten().flatten() {
            let programs = std::fs::read_dir(version.path().join("programs")).into_iter().flatten().flatten();
            found += programs
                .filter(|entry| {
                    let path = entry.path();
                    path.extension().is_none_or(|ext| ext.eq_ignore_ascii_case("sum"))
                })
                .count();
        }
        found
    };
    // How many programs a run added to the store, and that it printed what it should.
    let run = |switch: Option<&str>| {
        let before = stored();
        let mut command = crate::executables::fennec_command();
        command
            .args(["run", "--no-test", "main.fen"])
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .env_remove("FENNEC_BOXED_RESULTS")
            .env_remove("FENNEC_STRIP_FACTS");
        if let Some(switch) = switch {
            command.env(switch, "1");
        }
        let output = command.output().unwrap();
        assert_eq!(text(&output.stdout), "42\n", "{}", text(&output.stderr));
        stored() - before
    };
    assert_eq!(run(None), 1, "the first run builds its program");
    assert_eq!(run(None), 0, "a second runs the one the store has");
    for switch in ["FENNEC_BOXED_RESULTS", "FENNEC_STRIP_FACTS"] {
        assert_eq!(run(Some(switch)), 1, "a run with {switch} builds a program of its own");
        assert_eq!(run(Some(switch)), 0, "and runs it again with {switch}");
    }
    assert_eq!(run(None), 0, "a run without them has its own still");
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
         held = items\n    items.append(2)\n    stdout.print(len(held), len(items), one())\n",
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
        "from parser.core import Parser\n\nextend Parser:\n    def step(self) -> int:\n        return self.at + 1\n",
    )?;
    // Files it mustn't name: one extends `Parser` with something else, and one gives another type
    // a `step`. They sort before the file it should name.
    std::fs::write(
        kit.join("src/parser/also.fen"),
        "from parser.core import Parser\n\nextend Parser:\n    def other(self) -> int:\n        return self.at\n",
    )?;
    std::fs::write(
        kit.join("src/parser/rival.fen"),
        "struct Rival:\n    at = 0\n\nextend Rival:\n    def step(self) -> int:\n        return self.at\n",
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
        "interface Made:\n    def make() -> Self\n\nstruct Part:\n    x = 1\n\n    def make() -> Part:\n        return Part(x=7)\n\n\
         def build[T: Made]() -> T:\n    return T.make()\n\ndef main(stdout: Stdout):\n    stdout.print(build[Part]().x)\n",
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
    let hinted = "def main(stdout: Stdout):\n    mut items = [1]\n    held = items\n    items.append(2)\n    stdout.print(len(held), len(items))\n";
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
    assert!(
        text(&flagged.stderr).contains("1 lint, which `fennec fix` can fix"),
        "{}",
        text(&flagged.stderr)
    );
    // `fennec lint` fails on it, so there it's an error.
    assert!(text(&flagged.stderr).contains("error: needless-pass: "), "{}", text(&flagged.stderr));

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

/// A build the launcher runs as `dev`, linked from a checkout, makes kits that ask for `dev`, as
/// there's no release of its version to run. Linked by another name, it asks for its version.
#[test]
fn new_made_by_a_build_linked_as_dev_asks_for_dev() {
    let dir = scratch("new-dev").unwrap();
    for (linked, asks) in [("dev", "dev"), ("mine", env!("CARGO_PKG_VERSION"))] {
        let made = crate::executables::fennec_command()
            .args(["new", linked])
            .current_dir(&dir)
            .env("FENNEC_LINKED_AS", linked)
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", text(&made.stderr));
        let manifest = std::fs::read_to_string(dir.join(linked).join("kit.toml")).unwrap();
        assert!(manifest.contains(&format!("fennec = \"{asks}\"")), "{manifest}");
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
    std::fs::write(kit.join("src/other.fen"), "def other() -> int:\n    return 1\n").unwrap();

    let first = changes_in(&kit.join("src"), &home).unwrap();
    assert_eq!(text(&first.stdout), "first run: hashed 2 files and 2 definitions\n");
    let again = changes_in(&kit, &home).unwrap();
    assert_eq!(text(&again.stdout), "0 files parsed, 2 unchanged and not parsed again\nnothing changed\n");

    std::fs::write(kit.join("src/main.fen"), "# hi\ndef main(stdout: Stdout):\n    stdout.print( 1 )\n")
        .unwrap();
    std::fs::write(
        kit.join("src/other.fen"),
        "def other() -> int:\n    return 2\n\ndef more() -> int:\n    return 3\n",
    )
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
        "def square(n: int) -> int:\n    return n * n\n\ndef greet(stdout: Stdout):\n    stdout.print(square(3))\n\ndef main(stdout: Stdout):\n    greet(stdout)\n"
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
    std::fs::write(src.join("parts.fen"), "_LIMIT = 3\n\nstruct _Box:\n    size: int\n\ndef _made(_count: int) -> _Box:\n    return _Box(size=_count + _LIMIT)\n").unwrap();
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
    std::fs::write(src.join("main.fen"), "def helper() -> int:\n    return 1\n\ndef _helper() -> int:\n    return 2\n\ndef main(stdout: Stdout):\n    stdout.print(helper(), _helper())\n").unwrap();
    std::fs::write(
        src.join("parts.fen"),
        "def _shown() -> int:\n    return 3\n\ndef show() -> str:\n    return f\"{_shown()}\"\n",
    )
    .unwrap();
    let left = fennec(&["fix", &path], "").unwrap();
    let kept = std::fs::read_to_string(src.join("main.fen")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        parts,
        "LIMIT = 3\n\nstruct Box:\n    size: int\n\ndef made(_count: int) -> Box:\n    return Box(size=_count + LIMIT)\n"
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
/// first that has one, or a test. A function named other than in a call is left alone.
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

/// `fennec fix` never adds to `main`: what `main` takes is the program's grant, so only a person
/// widens it. A chain reaching a `main` without the capability changes nothing anywhere, and the
/// error says what to add to `main`; once a person has, the same fix threads it the rest of the way.
#[test]
fn fix_never_gives_main_a_capability() {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-{}-main-grant", std::process::id())),
    );
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(dir.join("kit.toml"), "name = \"hello\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    let main = "from settings import load_settings\n\ndef main(stdout: Stdout):\n    stdout.print(load_settings())\n";
    let settings = "export def load_settings() -> str:\n    return read_named(\"settings.toml\")\n\ndef read_named(name: str) -> str:\n    text = fs.read_text(name) except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): \"none\"\n    return text\n";
    std::fs::write(src.join("main.fen"), main).unwrap();
    std::fs::write(src.join("settings.fen"), settings).unwrap();
    let path = dir.to_string_lossy().into_owned();
    let refused = fennec(&["fix", &path], "").unwrap();
    let main_after = std::fs::read_to_string(src.join("main.fen")).unwrap();
    let settings_after = std::fs::read_to_string(src.join("settings.fen")).unwrap();
    // Used in `main` itself: the same.
    let own = dir.join("own.fen");
    let in_main = "def main(stdout: Stdout):\n    text = fs.read_text(\"settings.toml\") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): \"none\"\n    stdout.print(text)\n";
    std::fs::write(&own, in_main).unwrap();
    let own_path = own.to_string_lossy().into_owned();
    let own_refused = fennec(&["fix", &own_path], "").unwrap();
    let own_after = std::fs::read_to_string(&own).unwrap();
    // Once a person gives `main` an `Fs`, the fix passes it down.
    std::fs::write(src.join("main.fen"), main.replace("(stdout: Stdout)", "(stdout: Stdout, fs: Fs)"))
        .unwrap();
    let threaded = fennec(&["fix", &path], "").unwrap();
    let main_threaded = std::fs::read_to_string(src.join("main.fen")).unwrap();
    let settings_threaded = std::fs::read_to_string(src.join("settings.fen")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!refused.status.success());
    assert_eq!(main_after, main);
    assert_eq!(settings_after, settings);
    let said = text(&refused.stderr);
    assert!(said.contains("`read_named` uses an `Fs`, and `main` doesn't take one"), "{said}");
    assert!(
        said.contains("`fennec fix` doesn't give `main` a capability: add `fs: Fs` to `main` if this program should touch files"),
        "{said}"
    );
    assert!(!own_refused.status.success());
    assert_eq!(own_after, in_main);
    let said = text(&own_refused.stderr);
    assert!(said.contains("`main` uses an `Fs`, and doesn't take one"), "{said}");
    assert!(said.contains("add `fs: Fs` to `main`"), "{said}");
    assert!(threaded.status.success(), "{}", text(&threaded.stderr));
    assert_eq!(
        main_threaded,
        "from settings import load_settings\n\ndef main(stdout: Stdout, fs: Fs):\n    stdout.print(load_settings(fs))\n"
    );
    assert_eq!(
        settings_threaded,
        "export def load_settings(fs: Fs) -> str:\n    return read_named(fs, \"settings.toml\")\n\ndef read_named(fs: Fs, name: str) -> str:\n    text = fs.read_text(name) except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): \"none\"\n    return text\n"
    );
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
    std::fs::write(&program, "def checked(text: str) -> str fails InvalidValue:\n    fail InvalidValue(message=text)\n\ndef main(stdout: Stdout):\n    text = checked(\"/nothing/here\") except problem:\n        case InvalidValue(why):\n            str(why != \"\")\n    stdout.print(text)\n    said = checked(\"/nothing/here\") except problem:\n        case InvalidValue(_):\n            \"failed\"\n    stdout.print(said)\n").unwrap();
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
        return Color.Red

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
    return Color.Green(n)

def maybe() -> Color?:
    return Color.Red

def shown(color: Color) -> str:
    match color:
        case Red: return \"red\"
        case Green(shade): return f\"green {shade}\"

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
    stdout.print(shown(pick(0)), shown(car.color), other == Color.Red, shown(kept), len(many), light == Light.Off)
    stdout.print(shown(warm), maybe() == Color.Red)
    dialled = dial(0) except why:
        case Closed: \"closed\"
        case .NotFound(): \"other\"
    stdout.print(dialled)
    unsaid_value = unsaid() except .NotFound() | .Closed: \"failed\"
    stdout.print(unsaid_value)
";

/// What `CASES_AFTER` prints, as pinned.
const CASES_PRINTED: &str = "green 1\nred red True red 1 False\nred True\nclosed\nfailed\n";

/// What `fennec fix` makes of `CASES_BEFORE`: a dot wherever the type is known, and nothing where
/// it isn't, or where a dot would mean something else.
const CASES_AFTER: &str = "enum Color:
    Red
    Green(shade: int)

    def warm() -> Color:
        return .Red

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
    return .Green(n)

def maybe() -> Color?:
    return .Red

def shown(color: Color) -> str:
    match color:
        case .Red: return \"red\"
        case .Green(shade): return f\"green {shade}\"

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
    stdout.print(shown(pick(0)), shown(car.color), other == .Red, shown(kept), len(many), light == .Off)
    stdout.print(shown(warm), maybe() == .Red)
    dialled = dial(0) except why:
        case .Closed: \"closed\"
        case .NotFound(): \"other\"
    stdout.print(dialled)
    unsaid_value = unsaid() except .NotFound() | .Closed: \"failed\"
    stdout.print(unsaid_value)
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
    let main = "from lights import Light, on\n\nenum Color:\n    Red\n    Blue\n\ndef warm() -> Color:\n    return .Red\n\ndef shown(color: Color) -> str:\n    match color:\n        case .Red: return \"red\"\n        case .Blue: return \"blue\"\n\ndef main(stdout: Stdout):\n    light: Light = .Red\n    stdout.print(shown(warm()), on() == light)\n";
    let lights = "enum Light:\n    Red\n    Off\n\ndef on() -> Light:\n    return .Red\n";
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
const OLD_TABLE_NAMES: &str = "from std.game import Id, IdMap\n\nstruct Unit:\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units: IdMap[Unit] = IdMap[Unit]()\n    first: Id[Unit] = units.insert(Unit(hp=1))\n    stdout.print(len(units), first)\n";

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

// The migration's own tests, whose programs are in the old forms on purpose.
// fennec-migrate: off
/// Every failure form written before 2026-11-11 (`else` on a failure, `else as`, `lend x else:`
/// and a def's last value as what it returns) is refused, the two that no longer parse where
/// they're read, and its rewrite is the fix: plain
/// `fennec fix` and `fennec fix --migrate` both write `except` and `return` for them, a second run
/// finds nothing more, and the program prints what it did.
#[test]
fn fix_migrate_moves_a_program_to_the_failure_forms_and_it_runs_the_same() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let old = std::fs::read_to_string(fixtures.join("old_failure_forms.fen")).unwrap();
    let migrated = std::fs::read_to_string(fixtures.join("migrated_failure_forms.fen")).unwrap();
    let dir = scratch("failure-forms").unwrap();
    let program = dir.join("forms.fen");
    let path = program.to_string_lossy().into_owned();
    std::fs::write(&program, &old).unwrap();
    let refused = fennec(&["run", &path], "").unwrap();
    let plain = fennec(&["fix", &path], "").unwrap();
    let plainly = std::fs::read_to_string(&program).unwrap();
    std::fs::write(&program, &old).unwrap();
    let fixed = fennec(&["fix", "--migrate", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let again = fennec(&["fix", "--migrate", &path], "").unwrap();
    let rewritten = std::fs::read_to_string(&program).unwrap();
    let after = fennec(&["run", &path], "").unwrap();
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("is written `lend x except:`"), "{}", text(&refused.stderr));
    assert!(plain.status.success(), "{}", text(&plain.stderr));
    assert_eq!(plainly, migrated);
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(written, migrated);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert_eq!(rewritten, migrated);
    assert!(after.status.success(), "{}", text(&after.stderr));
    assert_eq!(text(&after.stdout), "0 9 7 -1\nno load\nBad.Empty\nopen t\nclose t\n0 3 t\n");
}

/// An `else` on a failing call inside a statement gets a line of its own before it only when
/// nothing before it in the statement can print, fail or crash, so what runs runs in the same
/// order: each function here prints as it runs, and the migrated program prints what the old one
/// did. A call or a `?` before it keeps it where it is, for a person to write.
#[test]
fn fix_migrate_moves_no_call_past_another() {
    let dir = scratch("migrate-order").unwrap();
    let program = dir.join("order.fen");
    let path = program.to_string_lossy().into_owned();
    let common = "error Bad:\n    Gone\n\n\
        def noisy(n: int, stdout: Stdout) -> int:\n    stdout.print(\"noisy\", n)\n    return n\n\n\
        def risky(n: int, stdout: Stdout) -> int fails Bad:\n    stdout.print(\"risky\", n)\n    if n == 0: fail .Gone\n    return n\n\n";
    let moved = format!(
        "{common}def main(stdout: Stdout):\n    \
         stdout.print(\"first\", risky(1, stdout) else -1, risky(0, stdout) else -2, noisy(3, stdout))\n"
    );
    std::fs::write(&program, &moved).unwrap();
    let fixed = fennec(&["fix", "--migrate", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    let ran = fennec(&["run", &path], "").unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        moved.replace(
            "    stdout.print(\"first\", risky(1, stdout) else -1, risky(0, stdout) else -2, noisy(3, stdout))\n",
            "    risky_value = risky(1, stdout) except .Gone: -1\n    risky_value2 = risky(0, stdout) except .Gone: -2\n    \
             stdout.print(\"first\", risky_value, risky_value2, noisy(3, stdout))\n"
        )
    );
    // The order the old program ran in: each argument left to right, then the print.
    assert_eq!(text(&ran.stdout), "risky 1\nrisky 0\nnoisy 3\nfirst 1 -2 3\n", "{}", text(&ran.stderr));
    let kept = format!(
        "{common}def both(stdout: Stdout) -> int fails Bad:\n    \
         return risky(6, stdout)? + (risky(7, stdout) else -7)\n\n\
         def main(stdout: Stdout) fails Bad:\n    stdout.print(noisy(4, stdout), risky(5, stdout) else -5)\n    \
         stdout.print(both(stdout))\n"
    );
    std::fs::write(&program, &kept).unwrap();
    let _ = fennec(&["fix", "--migrate", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    assert!(written.contains("    stdout.print(noisy(4, stdout), risky(5, stdout) else -5)\n"), "{written}");
    assert!(written.contains("    return risky(6, stdout)? + (risky(7, stdout) else -7)\n"), "{written}");
}

/// `--migrate` makes only the migration's rewrites: a function nothing calls keeps its place,
/// with only its `return` added, and a line the rewrite makes longer than `fennec fmt` keeps
/// lines is left as it's written, in a file that was formatted before.
#[test]
fn fix_migrate_deletes_nothing_and_reflows_nothing() {
    let dir = scratch("migrate-only").unwrap();
    let program = dir.join("only.fen");
    let path = program.to_string_lossy().into_owned();
    // 96 columns, which `return ` takes past 100.
    let long = format!(
        "    {}",
        ["first_value", "second_value", "third_value", "fourth_value"].join(" + ")
            + " + 1000000000000000000 + 2000000000000000"
    );
    let old = format!(
        "def unused(n: int) -> int:\n    return n + 1\n\n\
         def sum(first_value: int, second_value: int, third_value: int, fourth_value: int) -> int:\n{long}\n\n\
         def main(stdout: Stdout):\n    stdout.print(sum(1, 2, 3, 4))\n"
    )
    .replace("    return n + 1", "    n + 1");
    assert!(long.len() <= 100 && long.len() + "return ".len() > 100, "{}", long.len());
    std::fs::write(&program, &old).unwrap();
    let formatted = fennec(&["fmt", "--check", &path], "").unwrap();
    let fixed = fennec(&["fix", "--migrate", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    assert!(formatted.status.success(), "{}", text(&formatted.stderr));
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        old.replace("    n + 1", "    return n + 1")
            .replace(&long, &format!("    return {}", long.trim_start()))
    );
}

/// A program that doesn't check, like an example of a mistake, gets the rewrites that need no
/// types: `return`, `except why:` for `else as why:`, and `len(x)`. The mistake stays.
#[test]
fn fix_migrate_rewrites_what_needs_no_types_in_a_program_that_doesnt_check() {
    let dir = scratch("migrate-unchecked").unwrap();
    let program = dir.join("mistake.fen");
    let path = program.to_string_lossy().into_owned();
    let old = "def size(items: list[int]) -> int:\n    items.len()\n\n\
        def main(stdout: Stdout):\n    got = int.parse(\"1\") else as why:\n        stdout.print(why)\n        0\n    \
        stdout.print(size([got]), nowhere)\n";
    std::fs::write(&program, old).unwrap();
    let fixed = fennec(&["fix", "--migrate", &path], "").unwrap();
    let written = std::fs::read_to_string(&program).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        old.replace("    items.len()", "    return len(items)").replace("else as why:", "except why:")
    );
}

/// `--rust-strings` moves the programs in a Rust file's string literals, raw or plain, writing
/// back only their changed lines in each literal's own quoting and indent.
#[test]
fn fix_migrate_rewrites_the_programs_in_rust_strings() {
    let dir = scratch("migrate-rust-strings").unwrap();
    let file = dir.join("tests.rs");
    let path = file.to_string_lossy().into_owned();
    let old = "fn raw() -> &'static str {\n    r#\"\n        def two() -> int:\n            1 + 1\n\n        \
        def main(stdout: Stdout):\n            stdout.print(\"two\", two())\n    \"#\n}\n\n\
        fn plain() -> &'static str {\n    \"def three() -> int:\\n    3\\n\\ndef main(stdout: Stdout):\\n    \
        stdout.print(three())\\n\"\n}\n";
    std::fs::write(&file, old).unwrap();
    let fixed = fennec(&["fix", "--migrate", "--rust-strings", &path], "").unwrap();
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(
        written,
        old.replace("            1 + 1\n", "            return 1 + 1\n")
            .replace("\\n    3\\n", "\\n    return 3\\n")
    );
}
// fennec-migrate: on

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
        .env("FENNEC_DAEMONS", crate::executables::daemons_of(home))
        .output()
}

/// Waits up to 5 seconds for the daemon to be gone, with nothing of it left noted.
#[cfg(unix)]
fn stops(dir: &Path, home: &Path) -> bool {
    let noted = || {
        std::fs::read_dir(crate::executables::daemons_of(home)).map_or(0, |listing| {
            listing
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "daemon"))
                .count()
        })
    };
    (0..250).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        noted() == 0
            && with_daemon(dir, home, DAEMON_IDLE, &["daemon", "status"])
                .is_ok_and(|output| text(&output.stdout) == "not running\n")
    })
}

/// A command for Fennec's folder in the temp folder, as a fuzzer's or a script's is, never starts a
/// build daemon, which would wait half an hour for a folder nothing builds in again: it does its
/// work itself. A test's own (`FENNEC_DAEMONS`) does start.
#[cfg(unix)]
#[test]
fn a_fennec_folder_in_the_temp_folder_starts_no_daemon() {
    let (_folder, kit, home) = small_kit("temp-no-daemon").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let changes = crate::executables::fennec_command()
        .args(["dev", "changes"])
        .current_dir(&kit)
        .env("FENNEC_HOME", &home)
        .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
        .env_remove("FENNEC_NO_DAEMON")
        .env_remove("FENNEC_DAEMONS")
        .output()
        .unwrap();
    assert!(changes.status.success(), "{}", text(&changes.stderr));
    let status = text(&with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
    assert_eq!(status, "not running\n");
    let started = with_daemon(&kit, &home, DAEMON_IDLE, &["dev", "changes"]).unwrap();
    assert!(started.status.success(), "{}", text(&started.stderr));
    let status = text(&with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
    assert!(status.starts_with("running:"), "{status}");
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

/// A user runs at most four build daemons: starting a fifth stops the one used longest ago, and
/// what's noted of each goes with it.
#[cfg(unix)]
#[test]
fn past_four_daemons_the_one_used_longest_ago_stops() {
    let (_folder, kit, home) = small_kit("daemon-cap").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let in_environment = |side: &str, args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &home)
            .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
            .env("DAEMON_TEST_SIDE", side)
            .env_remove("FENNEC_NO_DAEMON")
            .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
            .output()
            .unwrap()
    };
    let running = || {
        text(&with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout)
            .lines()
            .filter(|line| line.starts_with("running:"))
            .count()
    };
    for side in ["first", "second", "third", "fourth"] {
        assert!(in_environment(side, &["dev", "changes"]).status.success());
        // Each used after the last, as the notes' times tell.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(running(), 4);
    assert!(in_environment("fifth", &["dev", "changes"]).status.success());
    let four = (0..250).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        running() == 4
    });
    assert!(four, "{} daemons run", running());
    let noted = std::fs::read_dir(crate::executables::daemons_of(&home)).unwrap().count();
    assert_eq!(noted, 4);
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemons didn't stop");
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

/// A kit in group-writable folders, as a umask of 002 makes them, is the user's own and checks.
/// In a folder anyone can write to, `fennec check` and `fennec update` stop and say the `chmod`
/// that fixes it, rather than saying there's no kit.toml with one right there.
#[test]
#[cfg(unix)]
fn a_kit_in_group_writable_folders_checks_and_one_anyone_can_write_says_why() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("kit-group").unwrap();
    let kit = dir.join("game");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::write(kit.join("kit.toml"), "name = \"game\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(kit.join("src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n    return\n")
        .unwrap();
    let mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    for (path, bits) in [(&kit, 0o775), (&kit.join("src"), 0o775), (&kit.join("kit.toml"), 0o664)] {
        mode(path, bits);
    }
    let checked = fennec_in(&kit.join("src"), &["check"], "").unwrap();
    assert!(checked.status.success(), "{}", String::from_utf8_lossy(&checked.stderr));
    mode(&kit, 0o777);
    let fix = format!("`chmod o-w {}` fixes it", std::fs::canonicalize(&kit).unwrap().display());
    for args in [&["check"][..], &["update"]] {
        let refused = fennec_in(&kit.join("src"), args, "").unwrap();
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success() && said.contains(&fix), "{args:?}: {said}");
    }
    mode(&kit, 0o755);
}

/// A kit fetched from git can't name another by `file://`: it could carry a bare repository and
/// name it, or point at any repository of the user's, and have it fetched into the build. The
/// real `fennec` refuses it, naming the kit and the address, however the tests' own library calls
/// may allow it. The user's own kit naming one by `file://` is fine.
#[test]
fn a_kit_from_git_naming_a_file_repository_is_refused() {
    let dir = scratch("git-file").unwrap();
    let carried = dir.join("carried");
    std::fs::create_dir_all(carried.join("src")).unwrap();
    std::fs::write(carried.join("kit.toml"), "name = \"carried\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(carried.join("src/lib.fen"), "export def one() -> int:\n    return 1\n").unwrap();
    let carried_commit = committed_to_git(&carried).unwrap();
    let address = |folder: &Path| {
        format!("file:///{}", folder.display().to_string().replace('\\', "/").trim_start_matches('/'))
    };
    let engine = dir.join("engine");
    std::fs::create_dir_all(engine.join("src")).unwrap();
    std::fs::write(
        engine.join("kit.toml"),
        format!(
            "name = \"engine\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\ncarried = {{ git = \"{}\", commit = \"{carried_commit}\" }}\n",
            address(&carried)
        ),
    )
    .unwrap();
    std::fs::write(engine.join("src/lib.fen"), "export def two() -> int:\n    return 2\n").unwrap();
    let engine_commit = committed_to_git(&engine).unwrap();
    let kit = dir.join("game");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::write(
        kit.join("kit.toml"),
        format!(
            "name = \"game\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\nengine = {{ git = \"{}\", commit = \"{engine_commit}\" }}\n",
            address(&engine)
        ),
    )
    .unwrap();
    std::fs::write(kit.join("src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n    return\n")
        .unwrap();
    let update = crate::executables::fennec_command()
        .arg("update")
        .current_dir(&kit)
        .env("FENNEC_HOME", dir.join("home"))
        .env_remove("FENNEC_OFFLINE")
        .output()
        .unwrap();
    assert!(!update.status.success(), "{}", text(&update.stdout));
    let said = text(&update.stderr);
    assert!(said.contains("`engine` is from git, and names `carried` at `file:///"), "{said}");
    assert!(said.contains("a repository on this machine"), "{said}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A pinned kit that uses a capability it wasn't given is broken, or changed: the error says so,
/// and offers no fix. `fennec fix` once handed such a kit the program's `Fs`, editing the kit in the
/// store and `main` to pass it, and left the store unlike its pin. Now it changes neither.
#[test]
fn no_tool_changes_a_kit_this_one_depends_on() {
    let dir = scratch("fix-dependency").unwrap();
    let repository = dir.join("helper");
    std::fs::create_dir_all(repository.join("src")).unwrap();
    std::fs::write(
        repository.join("kit.toml"),
        "name = \"helper\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n",
    )
    .unwrap();
    let library = "export def tidy(text: str) -> str:\n    _ = fs\n    return text.strip()\n";
    std::fs::write(repository.join("src/lib.fen"), library).unwrap();
    let commit = committed_to_git(&repository).unwrap();
    let kit = dir.join("app");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::write(
        kit.join("kit.toml"),
        format!(
            "name = \"app\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\nhelper = {{ git = \"file:///{}\", commit = \"{commit}\" }}\n",
            repository.display().to_string().replace('\\', "/").trim_start_matches('/')
        ),
    )
    .unwrap();
    let main = "from helper import tidy\n\ndef main(stdout: Stdout, fs: Fs):\n    stdout.print(tidy(\"  hello  \"))\n    return\n";
    std::fs::write(kit.join("src/main.fen"), main).unwrap();
    let fennec = |command: &str| {
        crate::executables::fennec_command()
            .arg(command)
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_OFFLINE")
            .output()
            .unwrap()
    };
    // It pins the kit, and then says the program doesn't check, as it doesn't.
    let updated = fennec("update");
    assert!(kit.join("kit.lock").is_file(), "{}", text(&updated.stderr));
    assert!(
        text(&updated.stderr).contains("this is in a kit this one depends on"),
        "{}",
        text(&updated.stderr)
    );
    let checked = fennec("check");
    let said = text(&checked.stderr);
    assert!(!checked.status.success());
    assert!(
        said.contains("`fs` doesn't exist")
            && said.contains(
                "this is in a kit this one depends on, which no tool changes: the kit changed or is broken"
            ),
        "{said}"
    );
    assert!(!said.contains("parameter"), "the error still offers to hand the kit a capability: {said}");
    let fixed = fennec("fix");
    assert!(!fixed.status.success());
    assert!(!text(&fixed.stdout).contains("fixed"), "{}", text(&fixed.stdout));
    let stored: Vec<_> = std::fs::read_dir(dir.join("home/kits")).unwrap().flatten().collect();
    assert_eq!(stored.len(), 1);
    assert_eq!(std::fs::read_to_string(stored[0].path().join("src/lib.fen")).unwrap(), library);
    assert_eq!(std::fs::read_to_string(kit.join("src/main.fen")).unwrap(), main);
    std::fs::remove_dir_all(&dir).unwrap();
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
        "## How far a thing falls.\nexport def fall() -> int:\n    return 7\n",
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
        &[("lib.fen", "export def twice(x: int) -> int:\n    return x * 2\n")],
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
                 def weight(self) -> int:\n        return twice(self.mass) + area(self.shape) + helper()\n\n\
                 export enum Kind:\n    Solid\n    Liquid(depth: int)\n\n\
                 export error TooHeavy:\n    Over(by: int)\n\n\
                 export interface Weighed:\n    def weight(self) -> int\n\n\
                 export def total[T: Weighed](items: list[T]) -> int fails TooHeavy:\n    \
                 mut subtotal: int = 0\n    for item in items:\n        subtotal += item.weight()\n    \
                 if subtotal > 100:\n        fail .Over(by=subtotal - 100)\n    return subtotal\n\n\
                 export def describe(kind: Kind) -> str:\n    match kind:\n        \
                 case .Solid:\n            return \"solid\"\n        case .Liquid(depth=d):\n            return f\"liquid {d}\"\n\n\
                 def helper() -> int:\n    return LIMIT\n\n\
                 test \"not the program's\":\n    assert false\n",
            ),
            (
                "shapes.fen",
                "export struct Circle:\n    radius: int\n\n\
                 export def area(circle: Circle) -> int:\n    return 3 * circle.radius * circle.radius\n",
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
        def helper() -> str:\n    return \"mine\"\n\n\
        def main(stdout: Stdout):\n    \
        body: physics.Body = Body(mass=2, shape=Round(radius=1))\n    \
        stdout.print(body.weight())\n    \
        stdout.print(describe(.Liquid(depth=3)))\n    \
        stdout.print(Circle(name=\"own\").name + helper())\n    \
        stdout.print(f\"{body}\")\n    weighed = total([body]) except why:\n        case .Over(by=n):\n            n\n    \
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
         export def area(circle: Circle) -> int:\n    return 4 * circle.radius * circle.radius\n",
    )
    .unwrap();
    let again = fennec(&["run", "."]);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert!(text(&again.stdout).starts_with("18\n"), "{}", text(&again.stdout));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program or kit handed nothing can't reach the standard library's internals: `std.intrinsics`
/// turns a number into a pointer, so reaching it would make "a kit does I/O only with what it's
/// handed" false. Every way of writing the import is refused, in a program and in a dependency,
/// by the module it reaches rather than how it's spelled, and so is an `@intrinsic` of its own.
#[test]
fn a_program_or_kit_handed_nothing_cant_reach_std_internals() {
    let dir = scratch("std-internals").unwrap();
    let uses = |module: &str| {
        format!(
            "def main(stdout: Stdout):\n    channel = {module}.channel_new(1)\n    stdout.print(channel)\n"
        )
    };
    let refused = "`std.intrinsics` is for the standard library";
    let programs = [
        format!("from std import intrinsics\n\n{}", uses("intrinsics")),
        format!("from std import intrinsics as inner\n\n{}", uses("inner")),
        format!("from std import json, intrinsics\n\n{}", uses("intrinsics")),
        format!("from std import intrinsics, json\n\n{}", uses("intrinsics")),
        format!("import std.intrinsics\n\n{}", uses("std.intrinsics")),
        format!("import std.intrinsics as inner\n\n{}", uses("inner")),
        "from std.intrinsics import channel_new\n\ndef main(stdout: Stdout):\n    stdout.print(channel_new(1))\n"
            .to_owned(),
        "from std.intrinsics import channel_new as made\n\ndef main(stdout: Stdout):\n    stdout.print(made(1))\n"
            .to_owned(),
    ];
    for (at, source) in programs.iter().enumerate() {
        let program = dir.join(format!("reach_{at}.fen"));
        std::fs::write(&program, source).unwrap();
        // `run` checks as `check` does: once is enough to show it does.
        let commands: &[&str] = if at == 0 { &["check", "run"] } else { &["check"] };
        for &command in commands {
            let output = fennec(&[command, program.to_str().unwrap()], "").unwrap();
            assert!(!output.status.success(), "{command} {source}");
            assert!(text(&output.stderr).contains(refused), "{command} {source}: {}", text(&output.stderr));
            assert!(text(&output.stdout).is_empty(), "{command} {source}: {}", text(&output.stdout));
        }
    }
    // The other names of a list are still imported, so the refusal is the only error.
    let listed = dir.join("listed.fen");
    std::fs::write(
        &listed,
        "from std import json, intrinsics\n\ndef main(stdout: Stdout):\n    stdout.print(json.dumps(1))\n",
    )
    .unwrap();
    let output = fennec(&["check", listed.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stderr).matches("error:").count(), 1, "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains(refused), "{}", text(&output.stderr));
    let own = dir.join("own_intrinsic.fen");
    std::fs::write(&own, "@intrinsic\ndef channel_new(size: int) -> int\n\ndef main(stdout: Stdout):\n    stdout.print(channel_new(1))\n").unwrap();
    let output = fennec(&["check", own.to_str().unwrap()], "").unwrap();
    assert!(
        text(&output.stderr).contains("only the standard library has `@intrinsic` functions"),
        "{}",
        text(&output.stderr)
    );

    // A dependency that writes the import is refused too, though the program never names it.
    write_kit(
        &dir.join("helper"),
        "helper",
        "",
        &[(
            "lib.fen",
            "from std import intrinsics\n\nexport def size() -> int:\n    return intrinsics.channel_new(1)\n",
        )],
    )
    .unwrap();
    let game = dir.join("game");
    write_kit(
        &game,
        "game",
        "[dependencies]\nhelper = { path = \"../helper\" }\n",
        &[("main.fen", "from helper import size\n\ndef main(stdout: Stdout):\n    stdout.print(size())\n")],
    )
    .unwrap();
    for command in ["check", "build"] {
        let output = crate::executables::fennec_command()
            .args([command, "."])
            .current_dir(&game)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{command}");
        assert!(text(&output.stderr).contains(refused), "{command}: {}", text(&output.stderr));
        assert!(text(&output.stderr).contains("lib.fen"), "{command}: {}", text(&output.stderr));
        assert!(text(&output.stdout).is_empty(), "{command}: {}", text(&output.stdout));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An `extend` has to be in the kit that declares the type. Another kit's extension would change
/// what that kit's own code does with it: a small kit gave `b.Amount` an `==` that's always true,
/// and `b`'s own comparison ran it. Refused in a dependency extending another dependency's type or
/// the program's own, and in the program extending a dependency's; a kit still extends its own
/// types.
#[test]
fn only_the_kit_that_declares_a_type_extends_it() {
    let dir = scratch("extend-own-kit").unwrap();
    write_kit(
        &dir.join("b"),
        "b",
        "",
        &[(
            "lib.fen",
            "export struct Amount:\n    cents: int\n\n\
             export def same(first: Amount, second: Amount) -> bool:\n    return first == second\n\n\
             export def doubled_of(amount: Amount) -> int:\n    return amount.doubled()\n\n\
             extend Amount:\n    def doubled(self) -> int:\n        return self.cents * 2\n",
        )],
    )
    .unwrap();
    let a = dir.join("a");
    let extends_b = "from b import Amount\n\nexport def one() -> int:\n    return 1\n\n\
                     extend Amount:\n    def ==(self, other: Amount) -> bool:\n        return True\n";
    write_kit(&a, "a", "[dependencies]\nb = { path = \"../b\" }\n", &[("lib.fen", extends_b)]).unwrap();
    let game = dir.join("game");
    let main = "import a\nimport b\n\ndef main(stdout: Stdout):\n    \
                stdout.print(a.one(), b.same(b.Amount(cents=1999), second=b.Amount(cents=1)))\n";
    let manifest = "[dependencies]\na = { path = \"../a\" }\nb = { path = \"../b\" }\n";
    write_kit(&game, "game", manifest, &[("main.fen", main)]).unwrap();
    let fennec = |command: &str| {
        crate::executables::fennec_command()
            .args([command, "."])
            .current_dir(&game)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap()
    };
    let refused = |says: &str, file: &str| {
        let checked = fennec("check");
        assert!(!checked.status.success());
        let said = text(&checked.stderr);
        assert!(said.contains(says), "{said}");
        assert!(said.contains(file), "{said}");
    };
    refused("`Amount` is declared in the kit `b`, so only that kit can extend it", "a/src/lib.fen");
    // A dependency extending the program's own type, which it doesn't import, as `len`.
    std::fs::write(
        a.join("src/lib.fen"),
        "export def one() -> int:\n    return 1\n\nextend Player:\n    def len(self) -> int:\n        return 99\n",
    )
    .unwrap();
    std::fs::write(
        game.join("src/main.fen"),
        "import a\n\nstruct Player:\n    hp: int\n\n    def len(self) -> int:\n        return self.hp\n\n\
         def main(stdout: Stdout):\n    stdout.print(a.one(), len(Player(hp=3)))\n",
    )
    .unwrap();
    refused("`Player` is declared in the program's own kit, so only that kit can extend it", "a/src/lib.fen");
    // The program extending a dependency's type.
    std::fs::write(a.join("src/lib.fen"), "export def one() -> int:\n    return 1\n").unwrap();
    std::fs::write(
        game.join("src/main.fen"),
        "import b\nfrom b import Amount\n\nextend Amount:\n    def tripled(self) -> int:\n        return self.cents * 3\n\n\
         def main(stdout: Stdout):\n    stdout.print(Amount(cents=1).tripled())\n",
    )
    .unwrap();
    refused("`Amount` is declared in the kit `b`, so only that kit can extend it", "main.fen");
    // `b`'s own extension, in another of its files, stands.
    std::fs::write(
        game.join("src/main.fen"),
        "import b\n\ndef main(stdout: Stdout):\n    \
         stdout.print(b.doubled_of(b.Amount(cents=2)), b.same(b.Amount(cents=1999), second=b.Amount(cents=1)))\n",
    )
    .unwrap();
    let ran = fennec("run");
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout), "4 False\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A dependency sees its own kit's names, what it imports, and the language's: never the program's.
/// The program's names have no prefix in the merged module, so a dependency writing one it doesn't
/// define read the program's private constant and called its private function. A builtin it calls
/// is the language's, though the program defines one of that name.
#[test]
fn a_dependency_sees_only_its_own_names_what_it_imports_and_the_languages() {
    let dir = scratch("dependency-names").unwrap();
    let helper = dir.join("helper");
    let peeks = "export def peek() -> str:\n    return f\"{API_KEY} {secret_total()}\"\n";
    write_kit(&helper, "helper", "", &[("lib.fen", peeks)]).unwrap();
    let game = dir.join("game");
    let main = "import helper\n\nAPI_KEY = \"hunter2\"\n\nstruct Player:\n    hp: int\n\n\
                def secret_total() -> int:\n    return 42\n\n\
                def main(stdout: Stdout):\n    stdout.print(helper.peek())\n";
    write_kit(&game, "game", "[dependencies]\nhelper = { path = \"../helper\" }\n", &[("main.fen", main)])
        .unwrap();
    let fennec = |command: &str| {
        crate::executables::fennec_command()
            .args([command, "."])
            .current_dir(&game)
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap()
    };
    let refused = |says: &[&str]| {
        for command in ["check", "build"] {
            let output = fennec(command);
            assert!(!output.status.success(), "{command}");
            let said = text(&output.stderr);
            for one in says {
                assert!(said.contains(one), "{command}: {said}");
            }
            assert!(said.contains("helper/src/lib.fen"), "{command}: {said}");
        }
    };
    refused(&["`API_KEY` doesn't exist", "`secret_total` doesn't exist"]);
    // A type of the program's, too.
    std::fs::write(
        helper.join("src/lib.fen"),
        "export def hurt(player: Player) -> int:\n    return player.hp\n",
    )
    .unwrap();
    refused(&["`Player` doesn't exist"]);
    // The program's own `round` is its own: the dependency's `round` is still the language's.
    std::fs::write(helper.join("src/lib.fen"), "export def size() -> int:\n    return round(2.6)\n").unwrap();
    std::fs::write(
        game.join("src/main.fen"),
        "import helper\n\ndef round(number: float) -> int:\n    _ = number\n    return 99\n\n\
         def main(stdout: Stdout):\n    stdout.print(helper.size(), round(1.0))\n",
    )
    .unwrap();
    let ran = fennec("run");
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout), "3 99\n");
    // A name the dependency defines itself is its own, though the program has one too.
    std::fs::write(
        helper.join("src/lib.fen"),
        "API_KEY = \"theirs\"\n\nexport def peek() -> str:\n    return f\"{API_KEY} {len(API_KEY)}\"\n",
    )
    .unwrap();
    std::fs::write(game.join("src/main.fen"), main).unwrap();
    let ran = fennec("run");
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout), "theirs 6\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program whose main kit is `app` and whose dependencies are written as `(name, kit.toml's
/// dependencies, lib.fen)`, in a scratch folder of their own, with what `fennec` says there.
struct Kits {
    dir: fennec_temp::Temp,
}

impl Kits {
    fn new(name: &str, app: (&str, &str), dependencies: &[(&str, &str, &str)]) -> std::io::Result<Kits> {
        let dir = scratch(name)?;
        let (manifest, main) = app;
        write_kit(&dir.join("app"), "app", manifest, &[("main.fen", main)])?;
        for (kit, manifest, lib) in dependencies {
            write_kit(&dir.join(kit), kit, manifest, &[("lib.fen", lib)])?;
        }
        Ok(Kits { dir })
    }

    fn fennec(&self, command: &[&str]) -> std::io::Result<Output> {
        crate::executables::fennec_command()
            .args(command)
            .current_dir(self.dir.join("app"))
            .env("FENNEC_HOME", self.dir.join("home"))
            .output()
    }

    /// What `check` and `build` both say when they stop the program. Nothing changes a file: not
    /// `fix`, as accepting what a kit holds is a person's step, nor the build itself.
    fn refused(&self) -> std::io::Result<String> {
        let main = std::fs::read_to_string(self.dir.join("app/src/main.fen"))?;
        let lock = std::fs::read_to_string(self.dir.join("app/kit.lock")).unwrap_or_default();
        let checked = self.fennec(&["check", "."])?;
        assert!(!checked.status.success(), "it checks");
        let said = text(&checked.stderr);
        let built = text(&self.fennec(&["build", "."])?.stderr);
        assert!(
            said.lines().filter(|line| line.starts_with("error")).all(|line| built.contains(line)),
            "{said}\n{built}"
        );
        let fixed = self.fennec(&["fix", "."])?;
        assert!(!text(&fixed.stdout).contains("fixed"), "{}", text(&fixed.stdout));
        assert_eq!(std::fs::read_to_string(self.dir.join("app/src/main.fen"))?, main);
        assert_eq!(std::fs::read_to_string(self.dir.join("app/kit.lock")).unwrap_or_default(), lock);
        Ok(said)
    }

    /// What `fennec update --accept` says, accepting what each kit holds, once it's said by
    /// `fennec update --check`, which fails until then; after it, the program checks.
    fn accepted(&self) -> std::io::Result<String> {
        let pending = self.fennec(&["update", "--check"])?;
        assert!(!pending.status.success(), "{}", text(&pending.stdout));
        let updated = self.fennec(&["update", "--accept"])?;
        assert!(updated.status.success(), "{}", text(&updated.stderr));
        self.checks()?;
        let current = self.fennec(&["update", "--check"])?;
        assert!(current.status.success(), "{}", text(&current.stderr));
        Ok(text(&updated.stdout))
    }

    fn checks(&self) -> std::io::Result<()> {
        let checked = self.fennec(&["check", "."])?;
        assert!(checked.status.success(), "{}", text(&checked.stderr));
        Ok(())
    }

    fn lock(&self) -> std::io::Result<String> {
        std::fs::read_to_string(self.dir.join("app/kit.lock"))
    }

    fn write(&self, kit: &str, file: &str, source: &str) -> std::io::Result<()> {
        std::fs::write(self.dir.join(kit).join(file), source)
    }
}

/// A dependency handed a capability `kit.lock` doesn't list for it stops every check and build,
/// however the capability is held: on its own, in a struct the program builds of the dependency's
/// type, in a list, a tuple or an optional, or as an open `File`. Only `fennec update` accepts it,
/// saying so; handing a kit less never stops anything, and `fennec update` then narrows the lock.
#[test]
fn a_dependency_handed_a_capability_stops_until_update_accepts_it() {
    let helper = "from std.fs import File\n\n\
                  export def save(fs: Fs, text: str) fails IoError:\n    fs.write_text(\"out.txt\", text)?\n\n\
                  export def size(file: File) -> int:\n    return 1\n\n\
                  export struct Holder:\n    files: Fs\n\n\
                  export def count[T](items: list[T]) -> int:\n    return len(items)\n\n\
                  export def first[T](pair: (T, int)) -> int:\n    return pair[1]\n\n\
                  export def maybe[T](item: T?) -> bool:\n    return item is None\n\n\
                  export def tidy(text: str) -> str:\n    return text.strip()\n";
    let main = "import helper\n\ndef main(fs: Fs) fails IoError:\n    helper.save(fs, text=\"hi\")?\n";
    let kits = Kits::new(
        "holds-direct",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[("helper", "", helper)],
    )
    .unwrap();
    let said = kits.refused().unwrap();
    assert!(
        said.contains("error: `helper` would now receive an `Fs` here, which kit.lock doesn't list for it"),
        "{said}"
    );
    assert!(
        said.contains("`fennec update --accept` shows what each kit would now hold and records it in kit.lock, to be reviewed with this change. Nothing here is fixed for you"),
        "{said}"
    );
    for (handed, says) in [
        ("helper.Holder(files=fs)", "`helper` would now receive an `Fs` here"),
        ("helper.count([fs])", "`helper` would now receive an `Fs`, in a `list[Fs]`, here"),
        ("helper.first((fs, 1))", "`helper` would now receive an `Fs`, in a `(Fs, int)`, here"),
        ("helper.maybe(found(fs))", "`helper` would now receive an `Fs`, in an `Fs?`, here"),
    ] {
        kits.write(
            "app",
            "src/main.fen",
            &format!(
                "import helper\n\ndef found(fs: Fs) -> Fs?:\n    return fs\n\ndef main(fs: Fs, stdout: Stdout):\n    _ = {handed}\n    stdout.print(\"done\")\n"
            ),
        )
        .unwrap();
        let said = kits.refused().unwrap();
        assert!(said.contains(says), "{handed}: {said}");
    }
    // A file already open is reach into the disk, as an `Fs` is.
    kits.write(
        "app",
        "src/main.fen",
        "import helper\n\ndef main(fs: Fs, stdout: Stdout) fails IoError:\n    with fs.open(\"note.txt\")? as file:\n        stdout.print(helper.size(file))\n",
    )
    .unwrap();
    let said = kits.refused().unwrap();
    assert!(
        said.contains("`helper` would now receive a `File` here, which kit.lock doesn't list for it"),
        "{said}"
    );
    let said = kits.accepted().unwrap();
    assert!(
        said.contains("kit.lock: `helper` now holds `File`, first handed over at src/main.fen:5:34"),
        "{said}"
    );
    assert!(kits.lock().unwrap().contains("holds helper path:../helper File\n"));
    // An `Fs` too is a new thing to hold, though the kit holds a `File` already.
    kits.write("app", "src/main.fen", main).unwrap();
    let said = kits.refused().unwrap();
    assert!(said.contains("`helper` would now receive an `Fs` here"), "{said}");
    let said = kits.accepted().unwrap();
    assert!(
        said.contains("kit.lock: `helper` now holds `Fs`")
            && said.contains("kit.lock: `helper` no longer holds `File`"),
        "{said}"
    );
    assert!(kits.lock().unwrap().contains("holds helper path:../helper Fs\n"));
    // Handed nothing, it's narrowed without a stop, and `fennec update` says so.
    kits.write(
        "app",
        "src/main.fen",
        "import helper\n\ndef main(stdout: Stdout):\n    stdout.print(helper.tidy(\" a \"))\n",
    )
    .unwrap();
    kits.checks().unwrap();
    let said = kits.accepted().unwrap();
    assert!(said.contains("kit.lock: `helper` no longer holds `Fs`"), "{said}");
    assert!(!kits.lock().unwrap().contains("holds helper"));
}

/// What a kit holds is worked out in the copy of each generic function the program makes:
/// `helper` never names `Fs`, and is handed one. The kits a dependency depends on are counted too,
/// and a capability `helper` hands on to `inner` is said to come from `helper`'s code.
#[test]
fn a_capability_smuggled_through_a_generic_is_found_in_every_kit_it_reaches() {
    let inner = "export interface Store:\n    def write_text(self, path: str, text: str) fails IoError\n\n\
                 export def write[S: Store](store: S, text: str) fails IoError:\n    store.write_text(\"inner.txt\", text)?\n";
    let helper = "import inner\n\n\
                  export def pass_on[S: inner.Store](store: S, text: str) fails IoError:\n    inner.write(store, text=text)?\n";
    let main = "import helper\n\ndef main(fs: Fs) fails IoError:\n    helper.pass_on(fs, text=\"hi\")?\n";
    let kits = Kits::new(
        "holds-generic",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[("helper", "[dependencies]\ninner = { path = \"../inner\" }\n", helper), ("inner", "", inner)],
    )
    .unwrap();
    let said = kits.refused().unwrap();
    assert!(
        said.contains("`helper` would now receive an `Fs` here, which kit.lock doesn't list for it"),
        "{said}"
    );
    assert!(
        said.contains(
            "`inner` would now receive an `Fs` here, from `helper`'s code, which kit.lock doesn't list for it"
        ),
        "{said}"
    );
    assert!(said.contains("helper/src/lib.fen"), "{said}");
    let said = kits.accepted().unwrap();
    assert!(said.contains("`helper` now holds `Fs`") && said.contains("`inner` now holds `Fs`"), "{said}");
    let lock = kits.lock().unwrap();
    assert!(
        lock.contains("holds helper path:../helper Fs\n") && lock.contains("holds inner path:../inner Fs\n"),
        "{lock}"
    );
}

/// `fennec update` never widens what a kit holds: a new version of a dependency that starts
/// handing on a capability fails it, naming each kit and what it would get, and leaves kit.lock as
/// it was. Only `fennec update --accept` widens, saying the same.
#[test]
fn update_narrows_and_only_update_accept_widens() {
    let inner = "export def keep[T](value: T) -> int:\n    return 1\n";
    let helper = |body: &str| format!("import inner\n\nexport def save(fs: Fs) -> int:\n    {body}\n");
    let main = "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(helper.save(fs))\n";
    let kits = Kits::new(
        "holds-accept",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[
            ("helper", "[dependencies]\ninner = { path = \"../inner\" }\n", &helper("return 1")),
            ("inner", "", inner),
        ],
    )
    .unwrap();
    // What the program itself hands over waits for `--accept` too.
    let plain = kits.fennec(&["update"]).unwrap();
    assert!(!plain.status.success());
    assert!(text(&plain.stderr).contains("`helper` would now hold `Fs`"), "{}", text(&plain.stderr));
    assert!(!kits.lock().unwrap_or_default().contains("holds helper"));
    kits.accepted().unwrap();
    // A new version of `helper` hands the `Fs` on to `inner`.
    kits.write("helper", "src/lib.fen", &helper("return inner.keep(fs)")).unwrap();
    let said = kits.refused().unwrap();
    assert!(said.contains("`inner` would now receive an `Fs` here, from `helper`'s code"), "{said}");
    let lock = kits.lock().unwrap();
    let plain = kits.fennec(&["update"]).unwrap();
    assert!(!plain.status.success());
    let told = text(&plain.stderr);
    assert!(
        told.contains(
            "a kit would now hold more than kit.lock says, which only `fennec update --accept` accepts"
        ) && told.contains("`inner` would now hold `Fs`, first handed over at"),
        "{told}"
    );
    assert_eq!(kits.lock().unwrap(), lock);
    let said = kits.accepted().unwrap();
    assert!(said.contains("kit.lock: `inner` now holds `Fs`, first handed over at"), "{said}");
    assert!(kits.lock().unwrap().contains("holds inner path:../inner Fs\n"));
    // Handed less, a plain update narrows, and says so.
    kits.write("helper", "src/lib.fen", &helper("return 1")).unwrap();
    kits.checks().unwrap();
    let narrowed = kits.fennec(&["update"]).unwrap();
    assert!(narrowed.status.success(), "{}", text(&narrowed.stderr));
    assert!(
        text(&narrowed.stdout).contains("kit.lock: `inner` no longer holds `Fs`"),
        "{}",
        text(&narrowed.stdout)
    );
    assert!(!kits.lock().unwrap().contains("holds inner"));
}

/// What a kit holds belongs to where it comes from, not to its name: pointed at another kit of the
/// same name, with the same code and the same grants, the program stops until `fennec update
/// --accept` accepts them again, listing them in full.
#[test]
fn a_kits_grants_belong_to_its_source_not_its_name() {
    let helper = "export def size(fs: Fs) -> int:\n    return 7\n";
    let main = "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(helper.size(fs))\n";
    let kits = Kits::new(
        "holds-source",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[("helper", "", helper)],
    )
    .unwrap();
    kits.accepted().unwrap();
    assert!(kits.lock().unwrap().contains("holds helper path:../helper Fs\n"));
    write_kit(&kits.dir.join("other"), "helper", "", &[("lib.fen", helper)]).unwrap();
    kits.write(
        "app",
        "kit.toml",
        "name = \"app\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\nhelper = { path = \"../other\" }\n",
    )
    .unwrap();
    let said = kits.refused().unwrap();
    assert!(
        said.contains("`helper` would now receive an `Fs` here, which kit.lock doesn't list for it"),
        "{said}"
    );
    assert!(said.contains("kit.lock's line for `helper` was accepted for another source of it"), "{said}");
    let plain = kits.fennec(&["update"]).unwrap();
    assert!(!plain.status.success());
    assert!(
        text(&plain.stdout).contains("kit.lock: `helper` comes from path:../other now, not path:../helper"),
        "{}",
        text(&plain.stdout)
    );
    assert!(text(&plain.stderr).contains("`helper` would now hold `Fs`"), "{}", text(&plain.stderr));
    let said = kits.accepted().unwrap();
    assert!(said.contains("kit.lock: `helper` now holds `Fs`"), "{said}");
    let lock = kits.lock().unwrap();
    assert!(lock.contains("holds helper path:../other Fs\n") && !lock.contains("path:../helper"), "{lock}");
}

/// A function handed to a dependency holds what it captures. An arrow function or a nested `def`
/// written where it's handed holds what it reads from around it, a function named at the top of a
/// file holds nothing, and one whose making can't be seen, as a parameter, may hold anything the
/// program holds. A task, another kit's function used as a value, and a function a kit makes,
/// called with what its parameters take, count too.
#[test]
fn a_function_handed_to_a_dependency_holds_what_it_captures() {
    let helper = "export def twice(work: (int) -> int) -> int:\n    return work(work(1))\n\n\
                  export def call(job: () -> int) -> int:\n    return job()\n\n\
                  export def touch(fs: Fs) -> int:\n    return 1\n\n\
                  export def printer() -> (Stdout) -> int:\n    return (out) => 1\n";
    let kits = Kits::new(
        "holds-functions",
        (
            "[dependencies]\nhelper = { path = \"../helper\" }\n",
            "import helper\n\ndef main(stdout: Stdout):\n    stdout.print(helper.twice((n) => n + 1))\n",
        ),
        &[("helper", "", helper)],
    )
    .unwrap();
    // What reads nothing from around it holds nothing, and so does a function named at the top.
    kits.checks().unwrap();
    kits.write(
        "app",
        "src/main.fen",
        "import helper\n\ndef double(n: int) -> int:\n    return n * 2\n\ndef main(stdout: Stdout):\n    stdout.print(helper.twice(double))\n",
    )
    .unwrap();
    kits.checks().unwrap();
    for (main, says) in [
        (
            "import helper\n\ndef main(stdout: Stdout):\n    stdout.print(helper.twice((n) => shout(stdout, n)))\n\n\
             def shout(stdout: Stdout, n: int) -> int:\n    stdout.print(n)\n    return n\n",
            "`helper` would now receive a function that holds a `Stdout` here, which kit.lock doesn't list for it",
        ),
        (
            "import helper\n\ndef main(stdout: Stdout):\n    def loud(n: int) -> int:\n        stdout.print(n)\n        return n\n    \
             stdout.print(helper.twice(loud))\n",
            "`helper` would now receive a function that holds a `Stdout` here",
        ),
        (
            "import helper\n\ndef main(stdout: Stdout):\n    stdout.print(run(stdout, (n) => n + 1))\n\n\
             def run(stdout: Stdout, step: (int) -> int) -> int:\n    return helper.twice(step)\n",
            "`helper` would now receive a function here whose making can't be seen, so it may hold anything the program holds: a `Stdout`",
        ),
        (
            "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    touching = helper.touch\n    stdout.print(\"kept\")\n",
            "`helper.touch` takes an `Fs` and is used here as a value, so `helper` would now receive an `Fs`",
        ),
        (
            "from std.concurrent import Crashed\nimport helper\n\ndef main(fs: Fs, stdout: Stdout) fails Crashed:\n    \
             with launch(() => helper.touch(fs))?:\n        stdout.print(\"working\")\n",
            "`helper` would now receive an `Fs` here, which kit.lock doesn't list for it",
        ),
        (
            "import helper\n\ndef main(stdout: Stdout):\n    show = helper.printer()\n    stdout.print(show(stdout))\n",
            "`helper` makes a function here that takes a `Stdout`, so it would now receive a `Stdout`",
        ),
        // A name an arrow function was given in a block, declared again after it by a loop: it
        // holds what the loop gives it, not that arrow, which held nothing. On main before this,
        // the arrow was found by name, and `helper` was handed a `Stdout` with nothing said.
        (
            "import helper\n\ndef main(stdout: Stdout):\n    if len(\"a\") > 0:\n        work = () => 1\n        \
             stdout.print(helper.call(work))\n    for work in [loud(stdout)]:\n        stdout.print(helper.call(work))\n\n\
             def loud(stdout: Stdout) -> () -> int:\n    return () => shout(stdout)\n\n\
             def shout(stdout: Stdout) -> int:\n    stdout.print(1)\n    return 1\n",
            "`helper` would now receive a function here whose making can't be seen, so it may hold anything the program holds: a `Stdout`",
        ),
    ] {
        kits.write("app", "src/main.fen", main).unwrap();
        let said = kits.refused().unwrap();
        assert!(said.contains(says), "{main}\n{said}");
    }
    // Accepted, the function a kit makes is its own to hold.
    let said = kits.accepted().unwrap();
    assert!(said.contains("kit.lock: `helper` now holds `Stdout`"), "{said}");
}

/// A program that hands nothing to another kit never meets kit.lock: a single file, and a kit
/// with no dependencies, check, run and test with none, `fennec update --check` passes, and
/// nothing writes one.
#[test]
fn a_program_with_no_dependencies_never_needs_kit_lock() {
    let dir = scratch("holds-no-dependencies").unwrap();
    let main = "def main(fs: Fs, stdout: Stdout) fails IoError:\n    fs.write_text(\"note.txt\", \"kept\")?\n    stdout.print(\"saved\")\n\n\
                test \"it saves\":\n    assert True\n";
    std::fs::write(dir.join("single.fen"), main).unwrap();
    write_kit(&dir.join("kit"), "solo", "", &[("main.fen", main)]).unwrap();
    let fennec = |args: &[&str], at: &str| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(dir.join(at))
            .env("FENNEC_HOME", dir.join("home"))
            .output()
            .unwrap()
    };
    let mut commands: Vec<(Vec<&str>, &str)> = vec![
        (vec!["check", "single.fen"], "."),
        (vec!["test", "single.fen"], "."),
        (vec!["check", "."], "kit"),
        (vec!["test", "."], "kit"),
        (vec!["update", "--check"], "kit"),
        (vec!["update"], "kit"),
    ];
    if crate::llvm::ready() {
        commands
            .extend([(vec!["run", "--no-test", "single.fen"], "."), (vec!["run", "--no-test", "."], "kit")]);
    }
    for (args, at) in commands {
        let output = fennec(&args, at);
        assert!(output.status.success(), "{args:?}: {}", text(&output.stderr));
        assert!(!text(&output.stderr).contains("kit.lock"), "{args:?}: {}", text(&output.stderr));
    }
    assert!(!dir.join("kit.lock").exists() && !dir.join("kit/kit.lock").exists());
}

/// kit.lock is part of what a run's kept program was checked against: one changed by hand to hold
/// less than the program hands over stops the next run, though no source file changed.
#[test]
fn a_run_checks_again_once_kit_lock_changes() {
    if !crate::llvm::ready() {
        return;
    }
    let helper = "export def size(fs: Fs) -> int:\n    return 7\n";
    let main = "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(helper.size(fs))\n";
    let kits = Kits::new(
        "holds-warm-run",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[("helper", "", helper)],
    )
    .unwrap();
    kits.accepted().unwrap();
    for _ in 0..2 {
        let ran = kits.fennec(&["run", "--no-test", "."]).unwrap();
        assert!(ran.status.success(), "{}", text(&ran.stderr));
        assert_eq!(text(&ran.stdout), "7\n");
    }
    let lock = kits.lock().unwrap();
    kits.write("app", "kit.lock", &lock.replace("holds helper path:../helper Fs\n", "")).unwrap();
    let ran = kits.fennec(&["run", "--no-test", "."]).unwrap();
    assert!(!ran.status.success(), "{}", text(&ran.stdout));
    assert!(text(&ran.stderr).contains("`helper` would now receive an `Fs` here"), "{}", text(&ran.stderr));
}

/// Each program in turn, with its helper kit's `lib.fen`, stops with `says`, through `check` and
/// `build`, and `fix` changes nothing.
fn each_stops(kits: &Kits, cases: &[(&str, &str, &str)]) -> std::io::Result<()> {
    for (main, lib, says) in cases {
        kits.write("app", "src/main.fen", main)?;
        kits.write("helper", "src/lib.fen", lib)?;
        let said = kits.refused()?;
        assert!(said.contains(says), "{main}\n{said}");
    }
    Ok(())
}

/// What's written into a kit's value after it's built is handed to the kit, whose code reads it
/// later: a field set, an item appended or set, a place lent with `ref` to a function that writes
/// it. So is what an operator of the kit's type is given, the value on its right.
#[test]
fn what_goes_into_a_kits_value_after_its_built_is_handed_to_it() {
    let holder = "export struct Holder:\n    files: Fs?\n    items: list[Fs]\n    slots: dict[str, Fs]\n\n\
                  export def empty() -> Holder:\n    return Holder(files=None, items=[], slots={})\n\n\
                  export def count(holder: Holder) -> int:\n    return len(holder.items)\n";
    let sink = "export struct Sink:\n    name: str\n\n    def +(self, other: Fs) -> Sink:\n        return self\n\n    \
                def ==[T](self, other: T) -> bool:\n        return False\n";
    let kits = Kits::new(
        "holds-writes",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", "def main():\n    pass\n"),
        &[("helper", "", holder)],
    )
    .unwrap();
    each_stops(
        &kits,
        &[
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    mut holder = helper.empty()\n    holder.files = fs\n    stdout.print(helper.count(holder))\n",
                holder,
                "`helper` would now receive an `Fs` here",
            ),
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    mut holder = helper.empty()\n    holder.items.append(fs)\n    stdout.print(helper.count(holder))\n",
                holder,
                "`helper` would now receive an `Fs` here",
            ),
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    mut holder = helper.empty()\n    holder.slots[\"a\"] = fs\n    stdout.print(helper.count(holder))\n",
                holder,
                "`helper` would now receive an `Fs` here",
            ),
            (
                "import helper\n\ndef fill(ref slot: Fs?, fs: Fs):\n    slot = fs\n\n\
                 def main(fs: Fs, stdout: Stdout):\n    mut holder = helper.empty()\n    fill(ref holder.files, fs=fs)\n    stdout.print(helper.count(holder))\n",
                holder,
                "`helper` would now receive an `Fs`",
            ),
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    sink = helper.Sink(name=\"x\") + fs\n    stdout.print(sink.name)\n",
                sink,
                "`helper` would now receive an `Fs` here",
            ),
            (
                "import helper\n\nstruct Mine:\n    files: Fs\n\ndef main(fs: Fs, stdout: Stdout):\n    \
                 stdout.print(helper.Sink(name=\"x\") == Mine(files=fs))\n",
                sink,
                "`helper` would now receive an `Fs`, in a `Mine`, here",
            ),
        ],
    )
    .unwrap();
}

/// A function a kit hands out is called with what its parameters take, a named one and a nested
/// `def` too, and a function inside a list, an open network handle, a failure from one of several,
/// and a generator the program hands over each hold what's in them.
#[test]
fn functions_handles_and_failures_a_kit_is_handed_count_whatever_holds_them() {
    let keep = "export def keep[T](value: T) -> int:\n    return 1\n";
    let kits = Kits::new(
        "holds-inside",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", "def main():\n    pass\n"),
        &[("helper", "", keep)],
    )
    .unwrap();
    each_stops(
        &kits,
        &[
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    work = helper.make()\n    stdout.print(work(fs))\n",
                "def take(fs: Fs) -> int:\n    return 1\n\nexport def make() -> (Fs) -> int:\n    return take\n",
                "`helper.take` takes an `Fs` and is used here as a value",
            ),
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    work = helper.make(3)\n    stdout.print(work(fs))\n",
                "export def make(n: int) -> (Fs) -> int:\n    def inner(fs: Fs) -> int:\n        return n\n    return inner\n",
                "takes an `Fs` and is used here as a value",
            ),
            (
                "import helper\n\ndef peek(fs: Fs) -> int:\n    return 1\n\ndef main(fs: Fs, stdout: Stdout):\n    \
                 stdout.print(helper.run_all([() => peek(fs)]))\n",
                "export def run_all(works: list[() -> int]) -> int:\n    mut total = 0\n    for work in works:\n        total += work()\n    return total\n",
                "`helper` would now receive",
            ),
            (
                "import helper\n\nerror Lost:\n    files: Fs\n\nerror Late:\n    seconds: int\n\n\
                 def risky(fs: Fs, late: bool) -> int fails Lost | Late:\n    if late:\n        fail Late(seconds=3)\n    fail Lost(files=fs)\n\n\
                 def main(fs: Fs, stdout: Stdout):\n    got = risky(fs, late=False) except why:\n        helper.keep(why)\n    stdout.print(got)\n",
                keep,
                "`helper` would now receive an `Fs`",
            ),
            (
                "from std.net import NetError\nimport helper\n\ndef main(net: Net, stdout: Stdout) fails NetError:\n    \
                 with net.listen(9000)? as server:\n        stdout.print(helper.keep(server))\n",
                keep,
                "`helper` would now receive a `Server` here",
            ),
            (
                "import helper\n\ndef countdown(out: Stdout, start: int) -> Iterator[int]:\n    mut at = start\n    while at > 0:\n        \
                 out.print(at)\n        yield at\n        at -= 1\n\ndef main(stdout: Stdout):\n    stdout.print(helper.drain(countdown(stdout, start=3)))\n",
                "export def drain(items: Iterator[int]) -> int:\n    mut total = 0\n    for item in items:\n        total += item\n    return total\n",
                "`helper` would now receive a function that holds a `Stdout` here",
            ),
        ],
    )
    .unwrap();
}

/// The near neighbours of the ways through Fable's review found: a generator a method or a nested
/// function makes, an operator on a type parameter, a function a kit makes that takes a function,
/// and a union a type parameter stands for.
#[test]
fn the_neighbours_of_each_way_through_count_too() {
    let keep = "export def keep[T](value: T) -> int:\n    return 1\n";
    let drain = "export def drain(items: Iterator[int]) -> int:\n    mut total = 0\n    for item in items:\n        \
                 total += item\n    return total\n";
    let sink =
        "export struct Sink:\n    name: str\n\n    def +(self, other: Fs) -> Sink:\n        return self\n";
    let kits = Kits::new(
        "holds-neighbours",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", "def main():\n    pass\n"),
        &[("helper", "", keep)],
    )
    .unwrap();
    each_stops(
        &kits,
        &[
            (
                "import helper\n\nstruct Counter:\n    out: Stdout\n\n    def count(self, start: int) -> Iterator[int]:\n        \
                 mut at = start\n        while at > 0:\n            self.out.print(at)\n            yield at\n            at -= 1\n\n\
                 def main(stdout: Stdout):\n    counter = Counter(out=stdout)\n    stdout.print(helper.drain(counter.count(3)))\n",
                drain,
                "`helper` would now receive a function here whose making can't be seen",
            ),
            (
                "import helper\n\ndef main(stdout: Stdout):\n    def count(start: int) -> Iterator[int]:\n        mut at = start\n        \
                 while at > 0:\n            stdout.print(at)\n            yield at\n            at -= 1\n    \
                 stdout.print(helper.drain(count(3)))\n",
                drain,
                "`helper` would now receive a function here whose making can't be seen",
            ),
            (
                "import helper\n\ninterface Adds:\n    def +(self, other: Fs) -> helper.Sink\n\n\
                 def add[T: Adds](sink: T, fs: Fs) -> str:\n    more = sink + fs\n    return more.name\n\n\
                 def main(fs: Fs, stdout: Stdout):\n    stdout.print(add(helper.Sink(name=\"x\"), fs))\n",
                sink,
                "`helper` would now receive an `Fs` here",
            ),
            (
                "import helper\n\ndef peek(fs: Fs) -> int:\n    return 1\n\ndef main(fs: Fs, stdout: Stdout):\n    \
                 run = helper.runner()\n    stdout.print(run(() => peek(fs)))\n",
                "export def runner() -> (() -> int) -> int:\n    return (work) => work()\n",
                "`helper` makes a function here that takes an `Fs`",
            ),
            (
                "import helper\n\ninterface Handles:\n    def handle(self, fs: Fs) -> int\n\nstruct Right:\n    name: str\n\n    \
                 def handle(self, fs: Fs) -> int:\n        return 2\n\n\
                 def pick(first: bool) -> helper.Left | Right:\n    if first:\n        return helper.Left(name=\"l\")\n    \
                 return Right(name=\"r\")\n\ndef run[H: Handles](handler: H, fs: Fs) -> int:\n    return handler.handle(fs)\n\n\
                 def main(fs: Fs, stdout: Stdout):\n    stdout.print(run(pick(False), fs))\n",
                "export struct Left:\n    name: str\n\n    def handle(self, fs: Fs) -> int:\n        return 1\n",
                "`helper` would now receive an `Fs` here",
            ),
        ],
    )
    .unwrap();
}

/// A program's own generic type with `methods` methods, built with `kinds` types in a function
/// named after `main`, beside one that hands the kit `helper` an `Fs`: thousands of copies of
/// methods that hand nothing over.
fn wide_program(methods: usize, kinds: usize) -> String {
    let mut lines = vec!["import helper\n\nstruct Slot[T]:\n    value: T\n".to_owned()];
    lines.extend((0..methods).map(|at| format!("    def method{at}(self) -> int:\n        return {at}\n")));
    lines.push(
        "def hand(fs: Fs) -> int:\n    return helper.take(fs)\n\ndef zmaker() -> int:\n    mut total = 0"
            .to_owned(),
    );
    let values = ["1", "\"a\"", "True", "1.5"];
    for (at, value) in values.iter().cycle().take(kinds).enumerate() {
        let depth = at / values.len();
        let value = format!("{}{value}{}", "[".repeat(depth), "]".repeat(depth));
        lines.push(format!("    built{at} = Slot(value={value})\n    total += built{at}.method0()"));
    }
    lines.push(
        "    return total\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(zmaker() + hand(fs))\n"
            .to_owned(),
    );
    lines.join("\n")
}

/// The walk over copies of generic functions follows the rules lowering does, and never passes over
/// what it can't follow. A kit's generic that comes back to itself with a bigger type each time is
/// refused where it's written, since it can't be built. A chain of generics each asking for two
/// copies of the next reaches the backstop, 16 copies for each function the program has, and past
/// it every kit may receive anything the program holds, which stops the check rather than passing
/// it.
#[test]
fn the_walk_never_passes_over_what_it_cant_follow() {
    let take = "export def take(fs: Fs) -> int:\n    return 7\n";
    let kits = Kits::new(
        "holds-bound",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", "def main():\n    pass\n"),
        &[("helper", "", take)],
    )
    .unwrap();
    let wide = wide_program(60, 40);
    each_stops(
        &kits,
        &[
            (wide.as_str(), take, "`helper` would now receive an `Fs` here"),
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(helper.take(fs))\n",
                "def grow[T](value: T, left: int) -> int:\n    if left == 0:\n        return 0\n    return grow([value], left - 1)\n\n\
                 export struct Spin:\n    name: str\n\n    def go(self) -> int:\n        return grow(1, 3)\n\n\
                 export def take(fs: Fs) -> int:\n    return 7\n",
                "`grow` calls itself with a bigger type each time",
            ),
            (
                "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(helper.take(fs))\n",
                &doubling_chain(16),
                "past the copies of generic functions a program of its size can ask for",
            ),
        ],
    )
    .unwrap();
}

/// A kit whose `Spin.go` starts a chain of `steps` generics, each asking for two copies of the next,
/// one with a tuple and one with a list, beside a `take` that's handed an `Fs`: no type nests past
/// `steps`, but the copies double at every step.
fn doubling_chain(steps: usize) -> String {
    let mut parts: Vec<String> = (0..steps)
        .map(|step| {
            let next = step + 1;
            format!("def step{step}[T](value: T) -> int:\n    return step{next}((value, 1)) + step{next}([value])\n")
        })
        .collect();
    parts.push(format!("def step{steps}[T](value: T) -> int:\n    return 1\n"));
    parts.push(
        "export struct Spin:\n    name: str\n\n    def go(self) -> int:\n        return step0(1)\n\n\
         export def take(fs: Fs) -> int:\n    return 7\n"
            .to_owned(),
    );
    parts.join("\n")
}

/// A generic type's method that only the standard library calls, as printing calls `str`, is
/// followed in the copy for each type the program has a value of, and a method called on a union of
/// two kits' types hands each kit's own method what the call gives.
#[test]
fn a_generic_types_method_std_calls_and_each_member_of_a_union_count() {
    let boxed = "import inner\n\nexport struct Box[T]:\n    value: T\n\n    def str(self, ref out: StrWriter):\n        \
                 out.write(str(inner.keep(self.value)))\n";
    let inner = "export def keep[T](value: T) -> int:\n    return 1\n";
    let kits = Kits::new(
        "holds-generic-method",
        (
            "[dependencies]\nhelper = { path = \"../helper\" }\n",
            "import helper\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(helper.Box(value=fs))\n",
        ),
        &[("helper", "[dependencies]\ninner = { path = \"../inner\" }\n", boxed), ("inner", "", inner)],
    )
    .unwrap();
    let said = kits.refused().unwrap();
    assert!(said.contains("`inner` would now receive an `Fs` here, from `helper`'s code"), "{said}");
    let left =
        "export struct Left:\n    name: str\n\n    def handle(self, fs: Fs) -> int:\n        return 1\n";
    let right =
        "export struct Right:\n    name: str\n\n    def handle(self, fs: Fs) -> int:\n        return 2\n";
    let kits = Kits::new(
        "holds-union",
        (
            "[dependencies]\nhelper = { path = \"../helper\" }\nother = { path = \"../other\" }\n",
            "import helper\nimport other\n\ndef pick(first: bool) -> helper.Left | other.Right:\n    if first:\n        \
             return helper.Left(name=\"l\")\n    return other.Right(name=\"r\")\n\n\
             def main(fs: Fs, stdout: Stdout):\n    thing = pick(False)\n    stdout.print(thing.handle(fs))\n",
        ),
        &[("helper", "", left), ("other", "", right)],
    )
    .unwrap();
    let said = kits.refused().unwrap();
    assert!(
        said.contains("`helper` would now receive an `Fs` here")
            && said.contains("`other` would now receive an `Fs` here"),
        "{said}"
    );
}

/// A function that calls itself, handed to a kit, checks: what it captures is worked out once,
/// whether it's at the top of a file, nested, or one of two that call each other.
#[test]
fn a_function_that_calls_itself_handed_to_a_kit_checks() {
    let apply = "export def apply(work: (int) -> int) -> int:\n    return work(3)\n";
    let kits = Kits::new(
        "holds-recursive",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", "def main():\n    pass\n"),
        &[("helper", "", apply)],
    )
    .unwrap();
    for main in [
        "import helper\n\ndef count(n: int) -> int:\n    if n == 0:\n        return 0\n    return 1 + count(n - 1)\n\n\
         def main(stdout: Stdout):\n    stdout.print(helper.apply(count))\n",
        "import helper\n\ndef main(stdout: Stdout):\n    def count(n: int) -> int:\n        if n == 0:\n            return 0\n        \
         return 1 + count(n - 1)\n    stdout.print(helper.apply(count))\n",
        "import helper\n\ndef even(n: int) -> int:\n    if n == 0:\n        return 1\n    return odd(n - 1)\n\n\
         def odd(n: int) -> int:\n    if n == 0:\n        return 0\n    return even(n - 1)\n\n\
         def main(stdout: Stdout):\n    stdout.print(helper.apply(even))\n",
    ] {
        kits.write("app", "src/main.fen", main).unwrap();
        kits.checks().unwrap();
        let current = kits.fennec(&["update", "--check"]).unwrap();
        assert!(current.status.success(), "{main}\n{}", text(&current.stderr));
    }
}

/// After warm runs, an edit to a generic function in another of the program's files that starts
/// handing its value on to a kit stops the next run, as does one handing on a function whose
/// making can't be seen: the run's quick check of the changed file doesn't decide it alone.
#[test]
fn a_run_after_an_edit_in_another_file_stops_at_what_it_starts_handing_on() {
    if !crate::llvm::ready() {
        return;
    }
    let helper = "export def keep[T](value: T) -> int:\n    return 1\n\nexport def apply(step: () -> int) -> int:\n    return step()\n";
    let main = "import helper\nimport util\n\ndef peek(fs: Fs) -> int:\n    return 1\n\n\
                def main(fs: Fs, stdout: Stdout):\n    stdout.print(util.pass_on(fs))\n    stdout.print(util.run(() => peek(fs)))\n";
    let kits = Kits::new(
        "holds-edited-run",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[("helper", "", helper)],
    )
    .unwrap();
    let util = |pass_on: &str, run: &str| {
        format!(
            "import helper\n\nexport def pass_on[T](value: T) -> int:\n    {pass_on}\n\nexport def run(step: () -> int) -> int:\n    {run}\n"
        )
    };
    kits.write("app", "src/util.fen", &util("return 1", "return step()")).unwrap();
    for _ in 0..2 {
        let ran = kits.fennec(&["run", "--no-test", "."]).unwrap();
        assert!(ran.status.success(), "{}", text(&ran.stderr));
    }
    kits.write("app", "src/util.fen", &util("return helper.keep(value)", "return step()")).unwrap();
    let ran = kits.fennec(&["run", "--no-test", "."]).unwrap();
    assert!(text(&ran.stderr).contains("`helper` would now receive an `Fs` here"), "{}", text(&ran.stderr));
    kits.write("app", "src/util.fen", &util("return 1", "return helper.apply(step)")).unwrap();
    let ran = kits.fennec(&["run", "--no-test", "."]).unwrap();
    assert!(
        text(&ran.stderr).contains("`helper` would now receive a function here whose making can't be seen"),
        "{}",
        text(&ran.stderr)
    );
}

/// What isn't handed to another kit is never stopped at: passing a capability to the standard
/// library, a dependency calling back into the program's own code through an interface, and
/// values of the dependency's own types that hold nothing.
#[test]
fn what_stays_in_the_program_or_std_needs_no_lock() {
    let helper = "export interface Speaker:\n    def speak(self, words: str) -> int\n\n\
                  export def greet[S: Speaker](speaker: S) -> int:\n    return speaker.speak(\"hello\")\n\n\
                  export struct Point:\n    x: int\n\n\
                  export def moved(point: Point) -> Point:\n    return Point(x=point.x + 1)\n";
    let main = "import helper\n\nstruct Plain:\n    count: int\n\n\
                extend Plain:\n    def speak(self, words: str) -> int:\n        return len(words) + self.count\n\n\
                def main(stdout: Stdout, fs: Fs) fails IoError:\n    fs.write_text(\"note.txt\", \"kept\")?\n    \
                stdout.print(helper.greet(Plain(count=1)))\n    stdout.print(helper.moved(helper.Point(x=1)).x)\n";
    let kits = Kits::new(
        "holds-none-needed",
        ("[dependencies]\nhelper = { path = \"../helper\" }\n", main),
        &[("helper", "", helper)],
    )
    .unwrap();
    kits.checks().unwrap();
    let current = kits.fennec(&["update", "--check"]).unwrap();
    assert!(current.status.success(), "{}", text(&current.stderr));
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
    let _stopped = DaemonsStopped { home: &home };
    let deep = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &home)
            .env("TMPDIR", &home)
            .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
            .env_remove("FENNEC_NO_DAEMON")
            .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
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
    let _stopped = DaemonsStopped { home: &home };
    let status = || text(&with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
    assert_eq!(status(), "not running\n");

    let first = with_daemon(&kit, &home, DAEMON_IDLE, &["dev", "changes"]).unwrap();
    assert_eq!(text(&first.stdout), "first run: hashed 1 files and 1 definitions\n");
    assert!(
        status().starts_with("running: Fennec") && status().contains("1 request served\n"),
        "{}",
        status()
    );
    // The one snapshot is a few hundred bytes, under a tenth of a megabyte. The cap is 20 GiB, or a
    // tenth of the space free on the disk if that's less, so it's read rather than pinned.
    let said = status();
    let cap = said
        .strip_suffix(" MB\n")
        .and_then(|rest| rest.rsplit_once("\ncache: 0.0 MB of "))
        .and_then(|(_, cap)| cap.parse::<f64>().ok());
    assert!(cap.is_some_and(|cap| cap > 0.0 && cap <= 21_474.9), "{said}");

    let again = with_daemon(&kit, &home, DAEMON_IDLE, &["dev", "changes"]).unwrap();
    assert_eq!(text(&again.stdout), "0 files parsed, 1 unchanged and not parsed again\nnothing changed\n");
    assert!(status().contains("2 requests served\n"), "{}", status());

    let stop = with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert_eq!(text(&stop.stdout), "stopped\n");
    assert!(stops(&kit, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// The build daemon holds what a kit's run found while the kit's files are the same, so the next
/// run starts from it, and lets go of it when any of them changes: a file of the kit's own, a
/// dependency's, `kit.toml`, `kit.lock`, and the toolchain `kit.lock` pins. Each such run is
/// worked out as one with no daemon is, and the run after it is held again.
#[cfg(unix)]
#[test]
fn the_daemon_holds_a_kits_run_till_one_of_its_files_changes() {
    let dir = scratch("daemon_holds_runs").unwrap();
    let home = dir.join("home");
    let _stopped = DaemonsStopped { home: &home };
    let says = |word: &str| format!("export def word() -> str:\n    return \"{word}\"\n");
    write_kit(&dir.join("first"), "first", "", &[("lib.fen", &says("first"))]).unwrap();
    // `f` is a name the lint finds too short, and the test fails once the dependency says "broken".
    let main = |end: &str| {
        format!(
            "from first import word\n\ndef f() -> str:\n    return word()\n\n\
def main(stdout: Stdout):\n    stdout.print(f() + \"{end}\")\n\n\
test \"the word isn't broken\":\n    assert word() != \"broken\"\n"
        )
    };
    let app = dir.join("app");
    let manifest = "[dependencies]\nfirst = { path = \"../first\" }\n";
    write_kit(&app, "app", manifest, &[("main.fen", &main(""))]).unwrap();
    let ran = || {
        crate::executables::fennec_command()
            .arg("run")
            .current_dir(&app)
            .env("FENNEC_HOME", &home)
            .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
            .env_remove("FENNEC_NO_DAEMON")
            .env_remove("FENNEC_NO_CACHE")
            .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
            .output()
            .unwrap()
    };
    let first = ran();
    let lint = text(&first.stderr);
    assert!(lint.contains("short-name"), "{lint}");
    // Each run that ends well shows the same lint errors, whether it was held or not.
    let run = || {
        let ran = ran();
        assert!(ran.status.success(), "{}", text(&ran.stderr));
        assert_eq!(text(&ran.stderr), lint);
        text(&ran.stdout)
    };
    // How many runs the daemon has started from what it held.
    let held = || {
        let status = text(&with_daemon(&app, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
        status
            .split_once("runs held: ")
            .and_then(|(_, rest)| rest.split_once(", and "))
            .and_then(|(_, rest)| rest.split_once(" run"))
            .map_or(0, |(count, _)| count.parse::<u64>().unwrap())
    };
    // Each edit is made a moment later than the files were last looked at, as a person's is.
    let edit = |file: PathBuf, text: &str| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(file, text).unwrap();
    };

    // The run after an edit is worked out again and shows it, and the one after that is held.
    let seen_then_held = |said: &str, held_before: u64| {
        assert_eq!(run(), said);
        assert_eq!(held(), held_before);
        assert_eq!(run(), said);
        assert_eq!(held(), held_before + 1);
    };
    assert_eq!((text(&first.stdout).as_str(), held()), ("first\n", 0));
    assert_eq!(run(), "first\n");
    assert_eq!(held(), 1);
    assert_eq!(run(), "first\n");
    assert_eq!(held(), 2);

    // The kit's own file.
    edit(app.join("src/main.fen"), &main("!"));
    seen_then_held("first!\n", 2);
    // A dependency's file.
    edit(dir.join("first/src/lib.fen"), &says("other"));
    seen_then_held("other!\n", 3);
    // `kit.toml`, though nothing it says changes what the program does.
    let manifest = format!("name = \"app\"\nversion = \"0.1.1\"\nfennec = \"0.0.1\"\n{manifest}");
    edit(app.join("kit.toml"), &manifest);
    seen_then_held("other!\n", 4);
    // `kit.lock`, given a toolchain to pin, and then the pin moved to another.
    let lock = std::fs::read_to_string(app.join("kit.lock")).unwrap_or_default();
    let pin = |version: &str, byte: &str| format!("{lock}toolchain {version} {}\n", byte.repeat(64));
    edit(app.join("kit.lock"), &pin("0.0.1", "a"));
    seen_then_held("other!\n", 5);
    edit(app.join("kit.lock"), &pin("0.0.2", "b"));
    seen_then_held("other!\n", 6);

    // Its tests passing is held with the rest: a dependency changed so one fails stops the run.
    edit(dir.join("first/src/lib.fen"), &says("broken"));
    let failed = ran();
    assert!(!failed.status.success() && text(&failed.stdout).is_empty(), "{}", text(&failed.stderr));
    assert_eq!(held(), 7);

    let stop = with_daemon(&app, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert_eq!(text(&stop.stdout), "stopped\n");
    assert!(stops(&app, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `fennec run` in `dir` through the daemon, using the cache, as a person's run does.
#[cfg(unix)]
fn run_held(dir: &Path, home: &Path, args: &[&str]) -> std::io::Result<Output> {
    crate::executables::fennec_command()
        .arg("run")
        .args(args)
        .current_dir(dir)
        .env("FENNEC_HOME", home)
        .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
        .env_remove("FENNEC_NO_DAEMON")
        .env_remove("FENNEC_NO_CACHE")
        .env("FENNEC_DAEMONS", crate::executables::daemons_of(home))
        .output()
}

/// How many runs the daemon of `home` has started from what it held.
#[cfg(unix)]
fn runs_held(dir: &Path, home: &Path) -> Option<u64> {
    let status = text(&with_daemon(dir, home, DAEMON_IDLE, &["daemon", "status"]).ok()?.stdout);
    let Some((_, held)) = status.split_once("runs held: ") else { return Some(0) };
    held.split_once(", and ")?.1.split_once(" run")?.0.parse().ok()
}

/// A program the daemon holds is the program of the path the run was given, which names every file
/// in what it says: a kit run as `k` and then as `.`, or through a link, and a single file run from
/// another folder, each say where they fail by the path they were given.
#[cfg(unix)]
#[test]
fn a_held_run_names_files_by_the_path_it_was_given() {
    let dir = scratch("daemon_held_paths").unwrap();
    let home = dir.join("home");
    let _stopped = DaemonsStopped { home: &home };
    let source = "def pick(items: list[int], at: int) -> int:\n    return items[at]\n\n\
def main(stdout: Stdout):\n    mut at = 0\n    for step in range(7):\n        at += step\n    stdout.print(pick([1, 2, 3], at=at))\n";
    write_kit(&dir.join("k"), "k", "", &[("main.fen", source)]).unwrap();
    std::os::unix::fs::symlink("k", dir.join("link")).unwrap();
    std::fs::write(dir.join("prog.fen"), source).unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    // A run given `given` in `at` fails, saying it failed at `said`. The lint lines before it still
    // name the path the kit was first run by (#2541), so only the failure's own line is looked at.
    let fails_at = |at: &Path, given: &str, said: &str| {
        let ran = run_held(at, &home, &[given]).unwrap();
        let printed = text(&ran.stderr);
        assert!(!ran.status.success() && printed.contains(&format!("--> {said}\n")), "{given}: {printed}");
    };
    for _ in 0..3 {
        fails_at(&dir, "k", "k/src/main.fen:2:12");
    }
    assert_eq!(runs_held(&dir, &home), Some(2));
    fails_at(&dir.join("k"), ".", "src/main.fen:2:12");
    fails_at(&dir, "link", "link/src/main.fen:2:12");
    fails_at(&dir, "./k", "./k/src/main.fen:2:12");
    for _ in 0..3 {
        fails_at(&dir, "prog.fen", "prog.fen:2:12");
    }
    fails_at(&dir.join("sub"), "../prog.fen", "../prog.fen:2:12");
    fails_at(&dir, "./prog.fen", "./prog.fen:2:12");

    with_daemon(&dir, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&dir, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit that embeds a folder is held, and what's held goes when what's in the folder changes:
/// a file in it saved as an editor saves it, by a new file renamed over it, and run at once, and
/// then a file added to it. Each run shows what the folder holds, and the runs after a change are
/// held again.
#[cfg(unix)]
#[test]
fn a_held_run_sees_what_an_embedded_folder_holds() {
    let dir = scratch("daemon_held_folder").unwrap();
    let home = dir.join("home");
    let _stopped = DaemonsStopped { home: &home };
    let main = "SITE = embed_folder(\"../site\")\n\ndef main(stdout: Stdout):\n    \
                for path, data in SITE.items():\n        stdout.print(path, len(data))\n";
    let kit = dir.join("k");
    write_kit(&kit, "k", "", &[("main.fen", main)]).unwrap();
    std::fs::create_dir_all(kit.join("site")).unwrap();
    std::fs::write(kit.join("site/app.css"), "body {}\n").unwrap();
    std::fs::write(kit.join("site/index.html"), "<p>hi</p>\n").unwrap();
    let run = || {
        let ran = run_held(&kit, &home, &[]).unwrap();
        assert!(ran.status.success(), "{}", text(&ran.stderr));
        text(&ran.stdout)
    };
    // Runs until one starts from what's held, at most `most`, each printing `said`.
    let held_within = |most: u64, said: &str| {
        let before = runs_held(&kit, &home).unwrap();
        for _ in 0..most {
            assert_eq!(run(), said);
            if runs_held(&kit, &home).unwrap() > before {
                return;
            }
        }
        panic!("none of {most} runs started from what was held");
    };
    held_within(3, "app.css 8\nindex.html 10\n");

    // An editor's save: the folder's time changes, and the kit is run at once.
    std::fs::write(kit.join("site/.app.css.tmp"), "body { color: red }\n").unwrap();
    std::fs::rename(kit.join("site/.app.css.tmp"), kit.join("site/app.css")).unwrap();
    held_within(3, "app.css 20\nindex.html 10\n");

    // A file added to the folder, after the time it changed has settled.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    std::fs::write(kit.join("site/logo.svg"), "<svg/>\n").unwrap();
    held_within(3, "app.css 20\nindex.html 10\nlogo.svg 7\n");
    // A hidden file changes nothing it holds.
    std::fs::write(kit.join("site/.DS_Store"), "").unwrap();
    held_within(1, "app.css 20\nindex.html 10\nlogo.svg 7\n");

    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What a name in an embedded folder is counts, not only the name: what's read through a link is
/// noted by where it led, so the folder says where each link leads. A link to a file pointed at
/// another, a link to a folder pointed at another, a link made a file of its own, and an empty
/// folder made an empty file are each seen, by the cache's own look and so by the daemon's.
#[cfg(unix)]
#[test]
fn a_held_run_sees_a_link_in_an_embedded_folder_changed() {
    use std::os::unix::fs::symlink;
    let dir = scratch("daemon_held_links").unwrap();
    let home = dir.join("home");
    let _stopped = DaemonsStopped { home: &home };
    let main = "SITE = embed_folder(\"../site\")\n\ndef main(stdout: Stdout):\n    \
                for path, data in SITE.items():\n        stdout.print(path, len(data))\n";
    let kit = dir.join("k");
    write_kit(&kit, "k", "", &[("main.fen", main)]).unwrap();
    let (site, data) = (kit.join("site"), kit.join("data"));
    for folder in [site.join("empty"), data.join("a"), data.join("b")] {
        std::fs::create_dir_all(folder).unwrap();
    }
    for (file, text) in
        [("v1.txt", "one\n"), ("v2.txt", "three33\n"), ("a/x.txt", "x\n"), ("b/x.txt", "xyz\n")]
    {
        std::fs::write(data.join(file), text).unwrap();
    }
    std::fs::write(site.join("app.css"), "body {}\n").unwrap();
    symlink("../data/v1.txt", site.join("current.txt")).unwrap();
    symlink("../data/v1.txt", site.join("pinned.txt")).unwrap();
    symlink("../data/a", site.join("shelf")).unwrap();
    // Runs until one starts from what's held, at most three, each printing what the site holds.
    let held_within = |said: &str| {
        let before = runs_held(&kit, &home).unwrap();
        for _ in 0..3 {
            let ran = run_held(&kit, &home, &[]).unwrap();
            assert_eq!(text(&ran.stdout), said, "{}", text(&ran.stderr));
            if runs_held(&kit, &home).unwrap() > before {
                return;
            }
        }
        panic!("none of 3 runs started from what was held");
    };
    held_within("app.css 8\ncurrent.txt 4\npinned.txt 4\nshelf/x.txt 2\n");
    std::fs::remove_file(site.join("current.txt")).unwrap();
    symlink("../data/v2.txt", site.join("current.txt")).unwrap();
    held_within("app.css 8\ncurrent.txt 8\npinned.txt 4\nshelf/x.txt 2\n");
    std::fs::remove_file(site.join("shelf")).unwrap();
    symlink("../data/b", site.join("shelf")).unwrap();
    held_within("app.css 8\ncurrent.txt 8\npinned.txt 4\nshelf/x.txt 4\n");
    std::fs::remove_file(site.join("pinned.txt")).unwrap();
    std::fs::write(site.join("pinned.txt"), "own file\n").unwrap();
    held_within("app.css 8\ncurrent.txt 8\npinned.txt 9\nshelf/x.txt 4\n");
    std::fs::remove_dir(site.join("empty")).unwrap();
    std::fs::write(site.join("empty"), "").unwrap();
    held_within("app.css 8\ncurrent.txt 8\nempty 0\npinned.txt 9\nshelf/x.txt 4\n");

    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An embedded folder's note can't be read two ways, whatever a name in it holds: a link `latest`
/// to `v2.txt` made a file named `latest -> v2.txt` is seen, with no daemon, as the cache looks.
#[cfg(unix)]
#[test]
fn a_file_named_like_a_link_in_an_embedded_folder_is_seen() {
    let dir = scratch("embedded_link_name").unwrap();
    let main = "SITE = embed_folder(\"../site\")\n\ndef main(stdout: Stdout):\n    \
                for path, data in SITE.items():\n        stdout.print(path, len(data))\n";
    let kit = dir.join("k");
    write_kit(&kit, "k", "", &[("main.fen", main)]).unwrap();
    let site = kit.join("site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("v2.txt"), "two\n").unwrap();
    std::os::unix::fs::symlink("v2.txt", site.join("latest")).unwrap();
    let run = || {
        let ran = crate::executables::fennec_command()
            .arg("run")
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap();
        assert!(ran.status.success(), "{}", text(&ran.stderr));
        text(&ran.stdout)
    };
    assert_eq!(run(), "latest 4\nv2.txt 4\n");
    assert_eq!(run(), "latest 4\nv2.txt 4\n");
    std::fs::remove_file(site.join("latest")).unwrap();
    std::fs::write(site.join("latest -> v2.txt"), "a file of its own\n").unwrap();
    assert_eq!(run(), "latest -> v2.txt 18\nv2.txt 4\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `fennec daemon status` and `stop` reach every daemon the user has running, one for each
/// environment commands ran in: a terminal set up another way still sees and stops them all.
#[cfg(unix)]
#[test]
fn status_and_stop_reach_the_daemon_of_every_environment() {
    let (_folder, kit, home) = small_kit("daemons").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    // Commands whose environments differ in one variable, so each has a daemon of its own. It's
    // none of Fennec's, so `docs/environment.md` doesn't list it.
    let run_in = |environment: &str, args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &home)
            .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
            .env("DAEMON_TEST_SIDE", environment)
            .env_remove("FENNEC_NO_DAEMON")
            .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
            .output()
            .unwrap()
    };
    for environment in ["first", "second"] {
        let ran = run_in(environment, &["dev", "changes"]);
        assert!(ran.status.success(), "{}", text(&ran.stderr));
    }
    let status = text(&run_in("third", &["daemon", "status"]).stdout);
    assert_eq!(status.matches("running: Fennec").count(), 2, "{status}");
    assert_eq!(text(&run_in("third", &["daemon", "stop"]).stdout), "stopped 2 daemons\n");
    assert!(stops(&kit, &home), "the daemons didn't stop");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// `fennec build` in `kit` to `prog` there, with the Fennec folder `home`, through the daemon or
/// in its own process, as CI would have it if `ci`.
#[cfg(unix)]
fn build_in(kit: &Path, home: &Path, daemon: bool, ci: bool) -> std::io::Result<Output> {
    let mut command = crate::executables::fennec_command();
    command.args(["build", ".", "-o", "prog"]).current_dir(kit).env("FENNEC_HOME", home);
    command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env_remove("CI");
    if daemon {
        command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
    }
    if ci {
        command.env("CI", "true");
    }
    command.output()
}

/// What a build printed, with how long each step took left out: it's never the same twice.
#[cfg(unix)]
fn without_times(printed: &[u8]) -> String {
    let mut out = String::new();
    let mut digits = String::new();
    for c in text(printed).chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        out.push_str(if c == ' ' && !digits.is_empty() { "N" } else { &digits });
        digits.clear();
        out.push(c);
    }
    out + &digits
}

/// A program built by the daemon is the one built in the command's own process: the same exit code,
/// the same output but for how long its steps took, the same problems, and the same file, byte
/// for byte. A build under CI never starts a daemon.
#[cfg(unix)]
#[test]
fn a_build_the_daemon_makes_is_the_one_made_here() {
    let (folder, kit, home) = small_kit("build-daemon").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let here_home = folder.join("home-here");
    let main = kit.join("src/main.fen");
    let wrong = "def main(stdout: Stdout):\n    stdout.print(1 + \"x\")\n";
    for (source, built) in [("def main(stdout: Stdout):\n    stdout.print(1)\n", true), (wrong, false)] {
        std::fs::write(&main, source).unwrap();
        let _ = std::fs::remove_file(kit.join("prog"));
        let here = build_in(&kit, &here_home, false, false).unwrap();
        let made_here = std::fs::read(kit.join("prog")).ok();
        let _ = std::fs::remove_file(kit.join("prog"));
        let made = build_in(&kit, &home, true, false).unwrap();
        assert_eq!(made.status.code(), here.status.code(), "{}", text(&made.stderr));
        assert_eq!(without_times(&made.stdout), without_times(&here.stdout));
        assert_eq!(text(&made.stderr), text(&here.stderr));
        assert_eq!(std::fs::read(kit.join("prog")).ok(), made_here);
        assert_eq!(made_here.is_some(), built);
    }
    let status = text(&with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
    assert!(status.contains("2 requests served"), "{status}");
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    // Under CI it's built here, and no daemon is started.
    let ci_home = folder.join("home-ci");
    assert_eq!(build_in(&kit, &ci_home, true, true).unwrap().status.code(), Some(1));
    let status = text(&with_daemon(&kit, &ci_home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
    assert_eq!(status, "not running\n");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// The daemon keeps each function's analysis from one build to the next: after a body edit it
/// uses most of them again, each checked against analyzing it again, and makes the program a
/// build here makes, byte for byte. A `fennec run` through it uses them again too.
#[cfg(unix)]
#[test]
fn a_daemon_build_uses_the_last_builds_analyses_again() {
    let (folder, kit, home) = small_kit("kept-analyses").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let here_home = folder.join("home-here");
    let build = |home: &Path, daemon: bool| {
        let mut command = crate::executables::fennec_command();
        command.args(["build", ".", "-o", "prog"]).current_dir(&kit).env("FENNEC_HOME", home);
        command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env("FENNEC_CHECK_WHOLE", "1").env_remove("CI");
        if daemon {
            // A test's own daemons, which start for a Fennec folder in the temp folder.
            command
                .env_remove("FENNEC_NO_DAEMON")
                .env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
        }
        command.output().unwrap()
    };
    // The note's counts: "note: used 12 of 15 functions' analyses again, ...".
    let used = |stderr: &str| -> (usize, usize) {
        let line = stderr.lines().find(|line| line.starts_with("note: used ")).unwrap_or("");
        let words: Vec<&str> = line.split(' ').collect();
        let count = |at: usize| words.get(at).and_then(|word| word.parse().ok()).unwrap_or(0);
        (count(2), count(4))
    };
    let program = |printed: u32| {
        format!(
            "def doubled(number: int) -> int:\n    return number * 2\n\n\
             def main(stdout: Stdout):\n    stdout.print(doubled(3))\n    stdout.print({printed})\n"
        )
    };
    for (edit, printed) in [3, 4].into_iter().enumerate() {
        std::fs::write(kit.join("src/main.fen"), program(printed)).unwrap();
        let _ = std::fs::remove_file(kit.join("prog"));
        let here = build(&here_home, false);
        assert_eq!(here.status.code(), Some(0), "{}", text(&here.stderr));
        let made_here = std::fs::read(kit.join("prog")).unwrap();
        let _ = std::fs::remove_file(kit.join("prog"));
        let made = build(&home, true);
        let stderr = text(&made.stderr);
        assert_eq!(made.status.code(), Some(0), "{stderr}");
        assert_eq!(std::fs::read(kit.join("prog")).unwrap(), made_here);
        let (again, of) = used(&stderr);
        assert!(of > 1, "{stderr}");
        if edit == 0 {
            assert_eq!(again, 0, "{stderr}");
        } else {
            // Every analysis but `main`'s, whose body changed: `doubled` is still called with 3.
            assert!(again > 0 && again < of, "{stderr}");
        }
    }
    // `fennec run` builds through the daemon too, using what the build before kept.
    for printed in [5, 6] {
        std::fs::write(kit.join("src/main.fen"), program(printed)).unwrap();
        let mut command = crate::executables::fennec_command();
        command.args(["run", "."]).current_dir(&kit).env("FENNEC_HOME", &home);
        command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env("FENNEC_CHECK_WHOLE", "1").env_remove("CI");
        command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", crate::executables::daemons_of(&home));
        let ran = command.output().unwrap();
        let stderr = text(&ran.stderr);
        assert_eq!(ran.status.code(), Some(0), "{stderr}");
        assert!(text(&ran.stdout).ends_with(&format!("6\n{printed}\n")), "{}", text(&ran.stdout));
        let (again, of) = used(&stderr);
        assert!(again > 0 && again < of, "{stderr}");
    }
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    // The daemon wrote its index once each build had answered, and before it went: a build without
    // it uses the units the last one kept, compiling only the function edited since.
    std::fs::write(kit.join("src/main.fen"), program(7)).unwrap();
    let made = build(&home, false);
    let said = text(&made.stdout);
    let compiled =
        said.split("compiled ").nth(1).and_then(|rest| rest.split(' ').next()?.parse::<usize>().ok());
    assert!(compiled.is_some_and(|compiled| compiled <= 2), "{said}");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// A `fennec run` whose edit changes what a test reaches builds that test first, then the program,
/// and the program's build uses again what the run before kept of its own functions: the ones no
/// test reaches too. The test's text was written twice, once to see LLVM took it and once to build
/// it, and two builds between the program's pushed every analysis of the program's own functions
/// out of what the daemon keeps, so they were all worked out again at every such edit.
#[cfg(unix)]
#[test]
fn a_run_with_tests_uses_the_programs_own_analyses_again() {
    let (_folder, kit, home) = small_kit("run-kept-analyses").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    // `tested` is what the test reaches; the six others only the program does.
    let program = |added: u32| {
        let mut text = format!(
            "def tested(number: int) -> int:\n    return number + {added}\n\n\
             test \"tested adds\":\n    assert tested(1) == {}\n\n",
            added + 1
        );
        let alone = (0..6).map(|at| {
            format!("def alone_{at}(number: int) -> int:\n    return number * {} + {at}\n\n", at + 2)
        });
        text.extend(alone);
        text += "def main(stdout: Stdout):\n    stdout.print(tested(1))\n";
        text.extend((0..6).map(|at| format!("    stdout.print(alone_{at}(3))\n")));
        text
    };
    // The program's build says its note last: "note: used 7 of 8 functions' analyses again, ...".
    let used = |stderr: &str| -> (usize, usize) {
        let line = stderr.lines().rev().find(|line| line.starts_with("note: used ")).unwrap_or("");
        let words: Vec<&str> = line.split(' ').collect();
        let count = |at: usize| words.get(at).and_then(|word| word.parse().ok()).unwrap_or(0);
        (count(2), count(4))
    };
    for (edit, added) in [1, 2, 3].into_iter().enumerate() {
        std::fs::write(kit.join("src/main.fen"), program(added)).unwrap();
        let mut command = crate::executables::fennec_command();
        command.args(["run", "."]).current_dir(&kit).env("FENNEC_HOME", &home);
        command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env("FENNEC_CHECK_WHOLE", "1").env_remove("CI");
        command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", crate::executables::daemons_of(&home));
        let ran = command.output().unwrap();
        let stderr = text(&ran.stderr);
        assert_eq!(ran.status.code(), Some(0), "{stderr}");
        assert!(text(&ran.stdout).starts_with(&format!("{}\n", added + 1)), "{}", text(&ran.stdout));
        let (again, of) = used(&stderr);
        assert!(of >= 8, "{stderr}");
        if edit > 0 {
            // All but `tested`, whose body changed, and at most `main`, which calls it.
            assert!(again + 2 >= of, "edit {edit}: {stderr}");
        }
    }
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

/// A release build through the daemon uses no analysis a build before it kept, being the build
/// that ships, and keeps its own: the dev build after it, of an edit to `main`, uses `doubled`'s.
#[cfg(unix)]
#[test]
fn a_release_build_uses_no_kept_analysis() {
    let (_folder, kit, home) = small_kit("kept-analyses-release").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    // Each build is of an edit, so none is put out from the store.
    let build = |release: bool, printed: u32| -> (usize, usize) {
        let source = format!(
            "def doubled(number: int) -> int:\n    return number * 2\n\n\
             def main(stdout: Stdout):\n    stdout.print(doubled(3))\n    stdout.print({printed})\n"
        );
        std::fs::write(kit.join("src/main.fen"), source).unwrap();
        let mut command = crate::executables::fennec_command();
        command.arg("build");
        if release {
            command.arg("--release");
        }
        command.args([".", "-o", "prog"]).current_dir(&kit).env("FENNEC_HOME", &home);
        command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env("FENNEC_CHECK_WHOLE", "1").env_remove("CI");
        command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", crate::executables::daemons_of(&home));
        let made = command.output().unwrap();
        let said = format!("{}{}", text(&made.stdout), text(&made.stderr));
        assert_eq!(made.status.code(), Some(0), "{said}");
        // "note: used 12 of 15 functions' analyses again, ...".
        let line = said.lines().find(|line| line.starts_with("note: used ")).unwrap_or("");
        let words: Vec<&str> = line.split(' ').collect();
        let count = |at: usize| words.get(at).and_then(|word| word.parse().ok()).unwrap_or(0);
        assert!(count(4) > 1, "{said}");
        (count(2), count(4))
    };
    assert_eq!(build(false, 1).0, 0);
    assert!(build(false, 2).0 > 0, "the daemon kept nothing");
    assert_eq!(build(true, 3).0, 0, "the release build used what was kept");
    assert!(build(false, 4).0 > 0, "the release build kept nothing");
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

/// A build after an edit carries over what the daemon kept of the kit's last check for what the
/// edit left as it was, and makes the program a build here makes, byte for byte: here a function in
/// the middle of the file grows, which moves a type and a function after it. Every build that
/// carried anything over is compared with checking it all again (`FENNEC_COMPARE_KEPT`), which
/// fails the build on a difference.
#[cfg(unix)]
#[test]
fn a_build_after_an_edit_carries_over_what_the_daemon_kept_and_is_the_one_made_here() {
    let (folder, kit, home) = small_kit("kept-daemon").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let here_home = folder.join("home-here");
    let main = kit.join("src/main.fen");
    let before = "def main(stdout: Stdout):\n    stdout.print(first(1) + edited(2) + later(3))\n\n\
                  def first(n: int) -> int:\n    return n + 1\n\n\
                  def edited(n: int) -> int:\n    return n * 2\n\n\
                  struct Point:\n    x = 1\n\n\
                  def later(n: int) -> int:\n    return Point().x + n\n";
    let after = before.replace("    return n * 2\n", "    total = n * 2 + 1\n    return total\n");
    let fennec = |args: &[&str], daemon: bool, home: &Path| {
        let mut command = crate::executables::fennec_command();
        command.args(args).current_dir(&kit).env("FENNEC_HOME", home);
        command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env("FENNEC_COMPARE_KEPT", "1").env_remove("CI");
        if daemon {
            command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", home.join("noted-daemons"));
        }
        command
    };
    let build =
        |daemon: bool, home: &Path| fennec(&["build", ".", "-o", "prog"], daemon, home).output().unwrap();
    // The first build is the daemon's, so the edit's has a kept check to carry over.
    let status = || fennec(&["daemon", "status"], true, &home).output();
    crate::executables::daemon_started(fennec(&["daemon", "serve"], true, &home), &status).unwrap();
    for source in [before, after.as_str()] {
        std::fs::write(&main, source).unwrap();
        let _ = std::fs::remove_file(kit.join("prog"));
        let here = build(false, &here_home);
        let made_here = std::fs::read(kit.join("prog")).ok();
        let _ = std::fs::remove_file(kit.join("prog"));
        let made = build(true, &home);
        assert_eq!(made.status.code(), Some(0), "{}", text(&made.stderr));
        assert_eq!(made.status.code(), here.status.code());
        assert_eq!(text(&made.stderr), text(&here.stderr));
        assert!(made_here.is_some());
        assert_eq!(std::fs::read(kit.join("prog")).ok(), made_here);
    }
    let run = std::process::Command::new(kit.join("prog")).output().unwrap();
    assert_eq!(text(&run.stdout), "11\n");
    let mut command = crate::executables::fennec_command();
    command.args(["daemon", "status"]).current_dir(&kit).env("FENNEC_HOME", &home);
    command
        .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
        .env("FENNEC_COMPARE_KEPT", "1")
        .env_remove("CI")
        .env_remove("FENNEC_NO_DAEMON")
        .env("FENNEC_DAEMONS", home.join("noted-daemons"));
    let status = text(&command.output().unwrap().stdout);
    assert!(status.contains("checks kept: 1 kit"), "{status}");
    let carried = status.split("the last build carried over ").nth(1).and_then(|rest| rest.split(' ').next());
    assert!(
        carried.and_then(|carried| carried.parse::<usize>().ok()).is_some_and(|carried| carried > 0),
        "{status}"
    );
    let mut command = crate::executables::fennec_command();
    command.args(["daemon", "stop"]).current_dir(&kit).env("FENNEC_HOME", &home);
    command
        .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
        .env("FENNEC_COMPARE_KEPT", "1")
        .env_remove("CI")
        .env_remove("FENNEC_NO_DAEMON")
        .env("FENNEC_DAEMONS", home.join("noted-daemons"));
    command.output().unwrap();
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// Past the daemon's budget (`FENNEC_KEPT_BUDGET`), the kit built longest ago is let go of once its
/// build is done, this one too when it's past by itself: the next build after an edit carries
/// nothing over, and makes the same program as one made here.
#[cfg(unix)]
#[test]
fn a_kit_past_the_daemons_budget_is_let_go_of_and_its_next_build_checks_all_of_it() {
    let (folder, kit, home) = small_kit("kept-budget").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let here_home = folder.join("home-here");
    let main = kit.join("src/main.fen");
    let before = "def main(stdout: Stdout):\n    stdout.print(doubled(2))\n\n\
                  def doubled(n: int) -> int:\n    return n * 2\n";
    let after = before.replace("    return n * 2\n", "    total = n * 2 + 1\n    return total\n");
    let fennec = |args: &[&str], daemon: bool, home: &Path| {
        let mut command = crate::executables::fennec_command();
        command.args(args).current_dir(&kit).env("FENNEC_HOME", home);
        command.env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env("FENNEC_KEPT_BUDGET", "1").env_remove("CI");
        if daemon {
            command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", home.join("noted-daemons"));
        }
        command
    };
    let status = || fennec(&["daemon", "status"], true, &home).output();
    crate::executables::daemon_started(fennec(&["daemon", "serve"], true, &home), &status).unwrap();
    for source in [before, after.as_str()] {
        std::fs::write(&main, source).unwrap();
        let _ = std::fs::remove_file(kit.join("prog"));
        let here = fennec(&["build", ".", "-o", "prog"], false, &here_home).output().unwrap();
        let made_here = std::fs::read(kit.join("prog")).ok();
        let _ = std::fs::remove_file(kit.join("prog"));
        let made = fennec(&["build", ".", "-o", "prog"], true, &home).output().unwrap();
        assert_eq!(made.status.code(), Some(0), "{}", text(&made.stderr));
        assert_eq!(text(&made.stderr), text(&here.stderr));
        assert!(made_here.is_some());
        assert_eq!(std::fs::read(kit.join("prog")).ok(), made_here);
    }
    let run = std::process::Command::new(kit.join("prog")).output().unwrap();
    assert_eq!(text(&run.stdout), "5\n");
    let said = text(&status().unwrap().stdout);
    assert!(said.contains("checks kept: 0 kits"), "{said}");
    assert!(said.contains("the last build carried over 0 functions"), "{said}");
    fennec(&["daemon", "stop"], true, &home).output().unwrap();
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// When the daemon checks all of a kit again in the background, after a build that carried
/// anything over, and disagrees with that build, the next build says so and checks all of it, with
/// the files as they were too: what that build put in the store is taken back, so the program it
/// made isn't put out again. A dev compiler here leaves out the rule that checks a callee's callers
/// again once its bounds change (`FENNEC_KEPT_RULE_OFF`), and nothing compares before a build goes
/// on (`FENNEC_COMPARE_KEPT=0`), so only the check in the background can catch it.
#[cfg(unix)]
#[test]
fn a_build_the_daemon_finds_wrong_later_is_taken_back() {
    if !cfg!(debug_assertions) {
        return;
    }
    let (_folder, kit, home) = small_kit("kept-disagrees").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let main = kit.join("src/main.fen");
    let before = "interface Shape:\n    def area(self) -> float\n\n\
                  struct Dot:\n    x: int\n\n\
                  def main(stdout: Stdout):\n    stdout.print(measure(Dot(1)))\n\n\
                  def measure[T](item: T) -> int:\n    return 1\n";
    let after = before.replace("measure[T](item", "measure[T: Shape](item");
    let command = |args: &[&str], daemon: bool| {
        let mut command = crate::executables::fennec_command();
        command.args(args).current_dir(&kit).env("FENNEC_HOME", &home).env("FENNEC_DAEMON_IDLE", DAEMON_IDLE);
        command.env("FENNEC_COMPARE_KEPT", "0").env("FENNEC_KEPT_RULE_OFF", "bounds").env_remove("CI");
        if daemon {
            command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", home.join("noted-daemons"));
        }
        command
    };
    let fennec = |args: &[&str], daemon: bool| command(args, daemon).output().unwrap();
    // The first build is the daemon's, so it keeps the check the edit's build carries over. One
    // made in the command's own process, as one is when a daemon it starts is slow to answer,
    // keeps nothing, and the edit's build then refuses the program, rightly.
    let status = || command(&["daemon", "status"], true).output();
    crate::executables::daemon_started(command(&["daemon", "serve"], true), &status).unwrap();
    std::fs::write(&main, before).unwrap();
    let first = fennec(&["build", ".", "-o", "prog"], true);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    // With the rule left out, the edit's build carries over `main`, which no longer fits.
    std::fs::write(&main, &after).unwrap();
    let wrong = fennec(&["build", ".", "-o", "prog"], true);
    assert_eq!(wrong.status.code(), Some(0), "{}", text(&wrong.stderr));
    let reported = || {
        std::fs::read_dir(home.join("daemon")).is_ok_and(|listing| {
            listing
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with("kept-check-differs-"))
        })
    };
    let waiting = std::time::Instant::now();
    while !reported() {
        assert!(waiting.elapsed().as_secs() < 120, "the daemon never found the build wrong");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Built again with the files as they are, it says so, checks all of it and refuses it.
    let again = fennec(&["build", ".", "-o", "prog"], true);
    let said = text(&again.stderr);
    assert_eq!(again.status.code(), Some(1), "{said}");
    assert!(said.contains("checked the last build of this kit all again, and it disagreed"), "{said}");
    assert!(said.contains("may be wrong"), "{said}");
    let refused = fennec(&["build", ".", "-o", "prog"], false);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    fennec(&["daemon", "stop"], true);
}

/// The daemon knows the objects its last build found or made, and doesn't look for them again
/// while no cleanup has taken anything. Gone anyway, as deleted by hand, the link finds that and
/// the build writes every unit again, saying so. A cleanup that took anything is counted, and
/// the next build looks for every object again, writing only the units whose objects are gone.
#[cfg(unix)]
#[test]
fn a_daemon_build_knows_its_objects_till_a_cleanup_takes_any() {
    let (folder, kit, home) = small_kit("known-objects").unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let build = |printed: u32| {
        let source = format!(
            "def doubled(number: int) -> int:\n    return number * 2\n\n\
             def main(stdout: Stdout):\n    stdout.print(doubled(3))\n    stdout.print({printed})\n"
        );
        std::fs::write(kit.join("src/main.fen"), source).unwrap();
        let mut command = crate::executables::fennec_command();
        command.args(["run", "."]).current_dir(&kit).env("FENNEC_HOME", &home);
        command.env("FENNEC_DAEMON_IDLE", "30").env_remove("CI").env_remove("FENNEC_NO_DAEMON");
        // A test's own daemons, which start for a Fennec folder in the temp folder.
        command.env("FENNEC_DAEMONS", crate::executables::daemons_of(&home));
        let ran = command.output().unwrap();
        assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
        assert!(text(&ran.stdout).ends_with(&format!("6\n{printed}\n")), "{}", text(&ran.stdout));
        text(&ran.stderr)
    };
    // Every compiled object in the cache, and the cache's own folder.
    let objects = || -> (Vec<PathBuf>, Option<PathBuf>) {
        let mut found = Vec::new();
        let mut root = None;
        let mut folders = vec![home.clone()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|extension| extension == "o")
                    && path.parent().and_then(Path::file_name).is_some_and(|kind| kind == "llvm")
                {
                    root = path.parent().and_then(Path::parent).and_then(Path::parent).map(Path::to_path_buf);
                    found.push(path);
                }
            }
        }
        (found, root)
    };
    let said = "wrote every function again";
    build(1);
    // Gone by hand, with no cleanup counted: the link finds it, and every unit is written again.
    let (gone, root) = objects();
    assert!(!gone.is_empty(), "no objects in the cache");
    for object in &gone {
        std::fs::remove_file(object).unwrap();
    }
    let stderr = build(2);
    assert!(stderr.contains(said), "{stderr}");
    // Gone with a cleanup counted: every object is looked for, and only the missing ones written.
    let (gone, _) = objects();
    for object in &gone {
        std::fs::remove_file(object).unwrap();
    }
    let root = root.unwrap();
    let count = std::fs::read_to_string(root.join("removals")).unwrap_or_default();
    let next = count.trim().parse::<u64>().unwrap_or(0) + 1;
    std::fs::write(root.join("removals"), next.to_string()).unwrap();
    let stderr = build(3);
    assert!(!stderr.contains(said), "{stderr}");
    with_daemon(&kit, &home, "30", &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(folder.join("kit")).unwrap();
}

/// `fennec` with `args` in `kit` through its daemon, once its program's `picked` indexes a list by
/// `count`, with a test of it. The rule for using a kept analysis again is told to leave the
/// parameters' ranges out, and the in-build check is off, so `picked`'s analysis kept for a positive
/// count is used for a negative one, as a release compiler would use a wrong one the rule let
/// through. Gives back the exit code, what it printed and what it said.
#[cfg(unix)]
fn with_picked(
    kit: &Path,
    home: &Path,
    args: &[&str],
    count: i32,
) -> std::io::Result<(Option<i32>, String, String)> {
    let source = format!(
        "def picked(count: int) -> int:\n    return [5, 6, 7, 8, 9][count]\n\n\
         def main(stdout: Stdout):\n    stdout.print(picked({count}))\n\n\
         test \"picked\":\n    assert picked({count}) > 0\n"
    );
    std::fs::write(kit.join("src/main.fen"), source)?;
    let mut command = crate::executables::fennec_command();
    command.args(args).current_dir(kit).env("FENNEC_HOME", home);
    command.env("FENNEC_DAEMON_IDLE", "30").env("FENNEC_CHECK_WHOLE", "0");
    command.env("FENNEC_KEPT_ANALYSIS_RULE_OFF", "ranges").env_remove("CI").env_remove("FENNEC_NO_DAEMON");
    command.env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
    let done = command.output()?;
    Ok((done.status.code(), text(&done.stdout), text(&done.stderr)))
}

/// Whether the daemon has reported an analysis it kept wrong, waiting up to two minutes for it.
#[cfg(unix)]
fn kept_analysis_reported(home: &Path, wait: bool) -> bool {
    let reported = || {
        std::fs::read_dir(home.join("daemon"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("kept-analysis-differs-"))
    };
    let waiting = std::time::Instant::now();
    while wait && !reported() && waiting.elapsed().as_secs() < 120 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    reported()
}

/// What the daemon says once it has found an analysis it kept wrong.
#[cfg(unix)]
const KEPT_ANALYSIS_NOTE: &str = "analyzed again what builds used the analyses of, and it disagreed";

/// A build that used analyses kept from the build before is checked once it has answered: the
/// daemon analyzes those functions again, and where one disagrees, every kept analysis goes, a
/// report is written, and whatever builds next says so. The daemon checks before it ends, so
/// stopping it right after the build loses nothing; the note is kept for whatever builds next,
/// in another daemon too; and every program stored before is taken back, whatever its settings,
/// so the same files build again rather than being put out from the store.
#[cfg(unix)]
#[test]
fn a_kept_analysis_found_wrong_after_the_build_is_reported_and_let_go_of() {
    let (_folder, kit, home) = small_kit("kept-analysis-wrong").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let build = ["build", ".", "-o", "prog"];
    assert_eq!(with_picked(&kit, &home, &build, 2).unwrap().0, Some(0));
    // `picked`'s analysis, kept for 2, is used for -2: the check after the build finds it, even
    // with the daemon stopped at once.
    assert_eq!(with_picked(&kit, &home, &build, -2).unwrap().0, Some(0));
    with_daemon(&kit, &home, "30", &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    assert!(kept_analysis_reported(&home, false), "the daemon ended without checking the build");
    // A release build of the same files, in a new daemon, says so.
    let (code, _, said) = with_picked(&kit, &home, &["build", "--release", ".", "-o", "prog"], -2).unwrap();
    assert_eq!(code, Some(0), "{said}");
    assert!(said.contains(KEPT_ANALYSIS_NOTE) && said.contains("may be wrong"), "{said}");
    // And the dev build after it builds again, rather than put out what the wrong one made, as a
    // stored program is, in no time at all. What it compiles and links may still be found where
    // it's kept by what's in it, which is the same whatever analysis made it.
    let (code, printed, said) = with_picked(&kit, &home, &build, -2).unwrap();
    assert_eq!(code, Some(0), "{said}");
    assert!(!printed.contains("functions in 0 ms, linked in 0 ms"), "{printed}");
    assert!(!said.contains(KEPT_ANALYSIS_NOTE), "said twice: {said}");
}

/// A test build's analyses kept from the one before are checked after it too, and once one is
/// found wrong, the tests that passed with it run again rather than counting as unchanged.
#[cfg(unix)]
#[test]
fn tests_built_with_a_kept_analysis_found_wrong_run_again() {
    let (_folder, kit, home) = small_kit("kept-analysis-wrong-tests").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    let (code, printed, said) = with_picked(&kit, &home, &["test", "."], 2).unwrap();
    assert_eq!(code, Some(0), "{printed}{said}");
    let (code, printed, said) = with_picked(&kit, &home, &["test", "."], -2).unwrap();
    assert_eq!(code, Some(0), "{printed}{said}");
    assert!(kept_analysis_reported(&home, true), "the daemon never found the test build's analysis wrong");
    let (code, printed, said) = with_picked(&kit, &home, &["test", "."], -2).unwrap();
    assert_eq!(code, Some(0), "{printed}{said}");
    assert!(said.contains(KEPT_ANALYSIS_NOTE), "{said}");
    assert!(printed.contains("1 passed") && !printed.contains("1 unchanged"), "{printed}");
}

/// Two builds of one kit at once through the daemon queue, so neither's output takes in the
/// other's: each prints what a build made here prints, once.
#[cfg(unix)]
#[test]
fn two_builds_of_one_kit_at_once_queue_in_the_daemon() {
    let (folder, kit, home) = small_kit("queued-daemon").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest).unwrap();
    // Each side built once first, so the builds compared find what they need kept.
    let here_home = folder.join("home-here");
    for (side, daemon) in [(&here_home, false), (&home, true)] {
        let first = build_in(&kit, side, daemon, false).unwrap();
        assert!(first.status.success(), "{}", text(&first.stderr));
    }
    let here = build_in(&kit, &here_home, false, false).unwrap();
    let both: Vec<Output> = std::thread::scope(|scope| {
        let builds = [(); 2].map(|()| scope.spawn(|| build_in(&kit, &home, true, false).unwrap()));
        builds.into_iter().map(|build| build.join().unwrap()).collect()
    });
    for made in both {
        assert_eq!(made.status.code(), Some(0), "{}", text(&made.stderr));
        assert_eq!(without_times(&made.stdout), without_times(&here.stdout));
        assert_eq!(text(&made.stderr), text(&here.stderr));
    }
    let status = text(&with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "status"]).unwrap().stdout);
    assert!(status.contains("3 requests served"), "{status}");
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// `fennec` with `args` in `kit`, with the Fennec folder `home`, through the daemon or in its own
/// process, given `input`. With `one_file`, its standard output and error go to one file, as a
/// terminal or `2>&1` has them, and all it printed is given back as standard output.
#[cfg(unix)]
fn fennec_in_kit(
    kit: &Path,
    home: &Path,
    daemon: bool,
    (args, input): (&[&str], &str),
    one_file: bool,
) -> std::io::Result<Output> {
    let mut command = crate::executables::fennec_command();
    command.args(args).current_dir(kit).env("FENNEC_HOME", home).env("FENNEC_DAEMON_IDLE", DAEMON_IDLE);
    command.env_remove("CI").stdin(Stdio::piped());
    if daemon {
        command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
    }
    let printed = kit.with_file_name(format!("printed-{daemon}"));
    if one_file {
        let file = std::fs::File::create(&printed)?;
        command.stdout(file.try_clone()?).stderr(file);
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let mut output = child.wait_with_output()?;
    if one_file {
        output.stdout = std::fs::read(&printed)?;
        std::fs::remove_file(&printed)?;
    }
    Ok(output)
}

/// The daemon `fennec_in_kit` uses for `home` started, once its status says it's running: a test
/// counting what the daemon served starts it first, or a loaded machine's commands build in their
/// own processes while it starts (see `daemon_started`).
#[cfg(unix)]
fn kit_daemon_started(kit: &Path, home: &Path) -> std::io::Result<()> {
    let daemon = |args: &[&str]| {
        let mut command = crate::executables::fennec_command();
        command.args(args).current_dir(kit).env("FENNEC_HOME", home).env("FENNEC_DAEMON_IDLE", DAEMON_IDLE);
        command
            .env_remove("CI")
            .env_remove("FENNEC_NO_DAEMON")
            .env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
        command
    };
    let status = || daemon(&["daemon", "status"]).output();
    crate::executables::daemon_started(daemon(&["daemon", "serve"]), &status)
}

/// What a daemon's status says of how many requests it has served.
#[cfg(unix)]
fn served_by_daemon(kit: &Path, home: &Path) -> std::io::Result<String> {
    let status = text(&with_daemon(kit, home, DAEMON_IDLE, &["daemon", "status"])?.stdout);
    Ok(status.lines().next().and_then(|line| line.rsplit(", ").next()).unwrap_or_default().to_owned())
}

/// A kit for a daemon test, with a manifest a run and a test take, and `source` as its `main.fen`.
#[cfg(unix)]
fn kit_of(name: &str, source: &str) -> std::io::Result<(fennec_temp::Temp, PathBuf, PathBuf)> {
    let (folder, kit, home) = small_kit(name)?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(kit.join("kit.toml"), manifest)?;
    std::fs::write(kit.join("src/main.fen"), source)?;
    Ok((folder, kit, home))
}

/// `fennec run` through the daemon prints and ends as it does in its own process, byte for byte,
/// in one file and in two:
/// - the program reads the command's input and arguments, and its exit code is the command's;
/// - a lint error is shown before it runs;
/// - a program that doesn't check says why;
/// - a test that fails stops the run.
///
/// The daemon made each, and started none of them.
#[cfg(unix)]
#[test]
fn a_run_through_the_daemon_prints_and_ends_as_one_here_does() {
    let reads = "def main(stdout: Stdout, args: Args, stdin: Stdin) -> int:\n    \
                 stdout.print(\"args:\", [arg for arg in args])\n    \
                 stdout.print(\"lines:\", len(stdin.lines()))\n    return 3\n";
    let (folder, kit, home) = kit_of("run-daemon", reads).unwrap();
    let _stopped = DaemonsStopped { home: &home };
    kit_daemon_started(&kit, &home).unwrap();
    let here_home = folder.join("home-here");
    let linted = "def f(stdout: Stdout):\n    stdout.print(2)\n\ndef main(stdout: Stdout):\n    f(stdout)\n";
    let wrong = "def main(stdout: Stdout):\n    stdout.print(1 + \"x\")\n";
    let failing =
        "def main(stdout: Stdout):\n    stdout.print(1)\n\ntest \"one is two\":\n    assert 1 == 2\n";
    let sources = [
        (reads, Some(3), "args: [\"fire\", \"ice\"]\nlines: 2\n"),
        (linted, Some(0), "short-name"),
        (wrong, Some(1), "error: "),
        (failing, Some(1), "one is two"),
    ];
    for (source, code, said) in sources {
        std::fs::write(kit.join("src/main.fen"), source).unwrap();
        for one_file in [false, true] {
            let run = |home: &Path, daemon| {
                fennec_in_kit(&kit, home, daemon, (&["run", ".", "fire", "ice"], "one\ntwo\n"), one_file)
                    .unwrap()
            };
            let here = run(&here_home, false);
            let there = run(&home, true);
            let printed = format!("{}{}", text(&here.stdout), text(&here.stderr));
            assert_eq!(here.status.code(), code, "{source}: {printed}");
            assert!(printed.contains(said), "{source}: {printed}");
            assert_eq!(there.status.code(), here.status.code(), "{source}");
            assert_eq!(text(&there.stdout), text(&here.stdout), "{source}, in one file: {one_file}");
            assert_eq!(text(&there.stderr), text(&here.stderr), "{source}, in one file: {one_file}");
        }
    }
    assert_eq!(served_by_daemon(&kit, &home).unwrap(), "8 requests served");
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

/// `fennec test` through the daemon prints and ends as it does in its own process, byte for byte,
/// in one file and in two: the affected tests or all of them, listed or not, failing and passing.
/// `--real` runs in the command's own process.
#[cfg(unix)]
#[test]
fn tests_through_the_daemon_print_and_end_as_ones_here_do() {
    let source = "def double(n: int) -> int:\n    return n * 2\n\n\
                  test \"doubling one\":\n    assert double(1) == 2\n\n\
                  test \"doubling two\":\n    assert double(2) == 5\n\n\
                  test \"doubling three\":\n    assert double(3) == 7\n\n\
                  def main(stdout: Stdout):\n    stdout.print(double(4))\n";
    let (folder, kit, home) = kit_of("test-daemon", source).unwrap();
    let _stopped = DaemonsStopped { home: &home };
    kit_daemon_started(&kit, &home).unwrap();
    let here_home = folder.join("home-here");
    let asked: [&[&str]; 4] = [&["test"], &["test"], &["test", "--verbose"], &["test", "--all", "-v"]];
    for args in asked {
        for one_file in [false, true] {
            let here = fennec_in_kit(&kit, &here_home, false, (args, ""), one_file).unwrap();
            let there = fennec_in_kit(&kit, &home, true, (args, ""), one_file).unwrap();
            assert_eq!(here.status.code(), Some(1), "{args:?}: {}", text(&here.stderr));
            assert!(text(&here.stdout).contains("✗ doubling two"), "{args:?}: {}", text(&here.stdout));
            assert_eq!(there.status.code(), here.status.code(), "{args:?}");
            assert_eq!(text(&there.stdout), text(&here.stdout), "{args:?}, in one file: {one_file}");
            assert_eq!(text(&there.stderr), text(&here.stderr), "{args:?}, in one file: {one_file}");
        }
    }
    let real = fennec_in_kit(&kit, &home, true, (&["test", "--real"], ""), false).unwrap();
    assert_eq!(real.status.code(), Some(1), "{}", text(&real.stderr));
    assert!(text(&real.stdout).contains("✗ doubling two"), "{}", text(&real.stdout));
    assert_eq!(served_by_daemon(&kit, &home).unwrap(), "8 requests served");
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

/// `fennec` in `kit`, through the daemon of the Fennec folder `home` when `daemon`, whose build
/// processes hang as `noted` says (`FENNEC_TEST_BUILD_CHILD=hang-building`), or else in its own
/// process with a Fennec folder of its own.
#[cfg(unix)]
fn child_daemon_fennec(
    kit: &Path,
    home: &Path,
    noted: &Path,
    (args, daemon): (&[&str], bool),
) -> std::process::Command {
    let mut command = crate::executables::fennec_command();
    command.args(args).current_dir(kit).env("FENNEC_DAEMON_IDLE", DAEMON_IDLE).env_remove("CI");
    if daemon {
        command.env("FENNEC_HOME", home).env_remove("FENNEC_NO_DAEMON");
        command.env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
        command.env("FENNEC_TEST_BUILD_CHILD", format!("hang-building:{}", noted.display()));
    } else {
        command.env("FENNEC_HOME", home.with_file_name("home-here"));
    }
    command
}

/// A build through the daemon of `kit`'s program edited to print `word`, once its build process
/// has started and hangs, with that process's id.
#[cfg(unix)]
fn hung_build(
    kit: &Path,
    home: &Path,
    noted: &Path,
    word: &str,
) -> std::io::Result<(std::process::Child, String)> {
    std::fs::write(
        kit.join("src/main.fen"),
        format!("def main(stdout: Stdout):\n    stdout.print(\"{word}\")\n"),
    )?;
    let _ = std::fs::remove_file(noted);
    let mut build = child_daemon_fennec(kit, home, noted, (&["build", ".", "-o", "hung"], true));
    let build = build.stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let waiting = std::time::Instant::now();
    while waiting.elapsed() < std::time::Duration::from_secs(120) {
        if let Some(pid) = std::fs::read_to_string(noted).ok().filter(|pid| !pid.is_empty()) {
            return Ok((build, pid));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(std::io::Error::other("the build's process never started"))
}

/// Whether the process `pid` ends within 10 seconds.
#[cfg(unix)]
fn ends(pid: &str) -> bool {
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let waiting = std::time::Instant::now();
    while alive() && waiting.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    !alive()
}

/// The daemon hands each build to a process of its own, whose memory goes when it ends:
/// - what it builds is the program a command's own process builds, byte for byte;
/// - a command that goes mid-build, as on Ctrl-C, stops that process and everything it runs, and
///   the daemon goes on to the next build;
/// - a daemon killed mid-build leaves no build process behind.
#[cfg(unix)]
#[test]
fn the_daemons_builds_go_to_processes_that_never_outlive_them() {
    if !crate::llvm::ready() || !cfg!(debug_assertions) {
        return;
    }
    let (folder, kit, home) =
        kit_of("build-child-daemon", "def main(stdout: Stdout):\n    stdout.print(\"apart\")\n").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let noted = folder.join("child-pid");
    let fennec = |args: &[&str], daemon: bool| child_daemon_fennec(&kit, &home, &noted, (args, daemon));
    let status = || fennec(&["daemon", "status"], true).output();
    crate::executables::daemon_started(fennec(&["daemon", "serve"], true), &status).unwrap();
    // With the file there, the build's process doesn't hang.
    std::fs::write(&noted, "").unwrap();
    let mut programs = Vec::new();
    for daemon in [false, true] {
        let build = fennec(&["build", ".", "-o", "prog"], daemon).output().unwrap();
        assert!(build.status.success(), "{}", text(&build.stderr));
        assert!(!text(&build.stderr).contains("own process"), "{}", text(&build.stderr));
        programs.push(std::fs::read(kit.join("prog")).unwrap());
    }
    assert!(programs[0] == programs[1], "the daemon's build process made another program");
    assert_eq!(served_by_daemon(&kit, &home).unwrap(), "1 request served");
    // The command going stops its build's process, and the daemon builds the next.
    let (mut build, pid) = hung_build(&kit, &home, &noted, "going").unwrap();
    build.kill().unwrap();
    build.wait().unwrap();
    assert!(ends(&pid), "the build's process {pid} outlived its command");
    std::fs::write(kit.join("src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(\"next\")\n")
        .unwrap();
    let next = fennec(&["build", ".", "-o", "next"], true).output().unwrap();
    assert!(next.status.success(), "{}", text(&next.stderr));
    assert_eq!(text(&std::process::Command::new(kit.join("next")).output().unwrap().stdout), "next\n");
    assert_eq!(served_by_daemon(&kit, &home).unwrap(), "3 requests served");
    // A daemon killed mid-build takes its build's process with it.
    let (mut build, pid) = hung_build(&kit, &home, &noted, "killed").unwrap();
    let said = text(&status().unwrap().stdout);
    let daemon =
        said.split("process ").nth(1).and_then(|rest| rest.split(',').next()).unwrap_or_default().to_owned();
    assert!(std::process::Command::new("kill").args(["-9", &daemon]).status().unwrap().success());
    assert!(ends(&pid), "the build's process {pid} outlived the daemon");
    let _ = build.kill();
    build.wait().unwrap();
}

/// A build the daemon hands to a process of its own uses the analyses the build before it kept, as
/// one in the daemon does: the daemon holds what each build's process kept and hands it to the
/// next, so an edit analyzes what it changed, not every function. Each used again is checked here
/// against analyzing it again (`FENNEC_CHECK_WHOLE=1`), and the program is what it should be.
#[cfg(unix)]
#[test]
fn a_build_in_its_own_process_uses_the_analyses_the_one_before_kept() {
    if !crate::llvm::ready() || !cfg!(debug_assertions) {
        return;
    }
    let program = |word: &str| {
        format!(
            "def doubled(count: int) -> int:\n    return count * 2\n\ndef said(count: int) -> str:\n    return f\"{word} {{count}}\"\n\ndef main(stdout: Stdout):\n    stdout.print(said(doubled(2)))\n"
        )
    };
    let (folder, kit, home) = kit_of("build-child-kept", &program("first")).unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let noted = folder.join("child-pid");
    std::fs::write(&noted, "").unwrap();
    let fennec = |args: &[&str]| {
        let mut command = child_daemon_fennec(&kit, &home, &noted, (args, true));
        command.env("FENNEC_CHECK_WHOLE", "1");
        command
    };
    let status = || fennec(&["daemon", "status"]).output();
    crate::executables::daemon_started(fennec(&["daemon", "serve"]), &status).unwrap();
    let mut used = Vec::new();
    for word in ["first", "second"] {
        std::fs::write(kit.join("src/main.fen"), program(word)).unwrap();
        let build = fennec(&["build", ".", "-o", "prog"]).output().unwrap();
        let said = text(&build.stderr);
        assert!(build.status.success() && !said.contains("own process"), "{said}");
        let ran = std::process::Command::new(kit.join("prog")).output().unwrap();
        assert_eq!(text(&ran.stdout), format!("{word} 4\n"));
        let count = said.split("note: used ").nth(1).and_then(|rest| rest.split(' ').next());
        used.push(count.and_then(|count| count.parse::<usize>().ok()).unwrap_or_default());
    }
    assert_eq!(used[0], 0, "the first build had nothing to use again");
    assert!(used[1] > 0, "the edit's build used no analysis the first kept: {used:?}");
}

/// A `fennec test` that goes while the daemon runs its tests, as on Ctrl-C, stops them, even one
/// that never ends, and the daemon goes on to the next command.
#[cfg(unix)]
#[test]
fn a_test_command_gone_stops_its_tests_in_the_daemon() {
    let source = "def main(stdout: Stdout):\n    stdout.print(1)\n\n\
                  test \"never ends\":\n    mut count = 0\n    while count >= 0:\n        \
                  count = (count + 1) % 1000\n";
    let (_folder, kit, home) = kit_of("stopped-tests", source).unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let said = kit.with_file_name("said");
    let mut command = crate::executables::fennec_command();
    command.args(["test"]).current_dir(&kit).env("FENNEC_HOME", &home).env("FENNEC_DAEMON_IDLE", DAEMON_IDLE);
    command
        .env_remove("CI")
        .env_remove("FENNEC_NO_DAEMON")
        .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
        .stdout(Stdio::null());
    let mut testing = command.stderr(std::fs::File::create(&said).unwrap()).spawn().unwrap();
    // The tests' executable is in the Fennec folder's `work`, from while it's built, which can be
    // in a process of its own, till the tests are done. They're running once a process runs it.
    let made_in: Vec<String> = [Some(home.clone()), home.canonicalize().ok()]
        .into_iter()
        .flatten()
        .map(|home| home.join("work").join("fennec-tests-").to_string_lossy().into_owned())
        .collect();
    let tests_running = || {
        let listed = std::process::Command::new("ps").args(["-axww", "-o", "command="]).output().unwrap();
        text(&listed.stdout).lines().any(|line| made_in.iter().any(|made| line.contains(made.as_str())))
    };
    let executable_left = || {
        std::fs::read_dir(home.join("work")).is_ok_and(|listing| {
            listing.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("fennec-tests-"))
        })
    };
    let waiting = std::time::Instant::now();
    while !tests_running() {
        let ended = testing.try_wait().unwrap();
        let why = std::fs::read_to_string(&said).unwrap_or_default();
        assert!(ended.is_none(), "the command ended before its tests ran: {why}");
        assert!(waiting.elapsed().as_secs() < 300, "the tests never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(testing.try_wait().unwrap().is_none(), "the test that never ends ended");
    testing.kill().unwrap();
    testing.wait().unwrap();
    let stopping = std::time::Instant::now();
    while tests_running() || executable_left() {
        assert!(!tests_running() || stopping.elapsed().as_secs() < 30, "the daemon didn't stop the tests");
        assert!(stopping.elapsed().as_secs() < 30, "the tests' executable was left in `work`");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let built = fennec_in_kit(&kit, &home, true, (&["build", "."], ""), false).unwrap();
    assert!(built.status.success(), "{}", text(&built.stderr));
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

/// A `fennec test` that goes while its tests are being built in a process of their own, as on
/// Ctrl-C, stops that process, and leaves nothing of the tests' executable in `work`, though the
/// build had started it.
#[cfg(unix)]
#[test]
fn a_test_command_gone_while_its_tests_build_leaves_nothing_behind() {
    if !crate::llvm::ready() || !cfg!(debug_assertions) {
        return;
    }
    let source = "def main(stdout: Stdout):\n    stdout.print(1)\n\ntest \"one\":\n    assert 1 == 1\n";
    let (folder, kit, home) = kit_of("stopped-test-build", source).unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let noted = folder.join("child-pid");
    let fennec = |args: &[&str]| child_daemon_fennec(&kit, &home, &noted, (args, true));
    let status = || fennec(&["daemon", "status"]).output();
    crate::executables::daemon_started(fennec(&["daemon", "serve"]), &status).unwrap();
    let mut testing = fennec(&["test"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let waiting = std::time::Instant::now();
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&noted).ok().filter(|pid| !pid.is_empty()) {
            break pid;
        }
        assert!(testing.try_wait().unwrap().is_none(), "the command ended before its tests were built");
        assert!(waiting.elapsed().as_secs() < 300, "the tests' build process never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    testing.kill().unwrap();
    testing.wait().unwrap();
    assert!(ends(&pid), "the tests' build process outlived the command");
    let left = || {
        std::fs::read_dir(home.join("work")).is_ok_and(|listing| {
            listing.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("fennec-tests-"))
        })
    };
    let stopping = std::time::Instant::now();
    while left() {
        assert!(stopping.elapsed().as_secs() < 30, "the tests' executable was left in `work`");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    fennec(&["daemon", "stop"]).output().unwrap();
    assert!(stops(&kit, &home), "the daemon didn't stop");
}

#[cfg(unix)]
#[test]
fn an_idle_daemon_exits_by_itself() {
    let (_folder, kit, home) = small_kit("idle-daemon").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    with_daemon(&kit, &home, "1", &["dev", "changes"]).unwrap();
    assert!(stops(&kit, &home), "the daemon didn't exit after a second with nothing to do");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn the_daemon_keeps_the_cache_under_its_cap() {
    let (_folder, kit, home) = small_kit("cache-daemon").unwrap();
    let _stopped = DaemonsStopped { home: &home };
    let old = home.join("cache").join("0.0.0-old").join("functions").join("x");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, vec![0_u8; 10_000]).unwrap();
    // Used before what's always kept, an entry stored or used in the last twenty minutes.
    aged(&old, fennec_build::KEPT_RECENT.as_secs() + 3_600).unwrap();
    crate::executables::fennec_command_with_empty_store()
        .args(["dev", "changes"])
        .current_dir(&kit)
        .env("FENNEC_HOME", &home)
        .env("FENNEC_DAEMON_IDLE", DAEMON_IDLE)
        .env("FENNEC_CACHE_CAP", "5000")
        .env_remove("FENNEC_NO_DAEMON")
        .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
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
    with_daemon(&kit, &home, DAEMON_IDLE, &["daemon", "stop"]).unwrap();
    assert!(cleaned, "the daemon should drop another version's entries once over its cap");
    assert!(stops(&kit, &home));
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// Aged by `seconds`: the file's modified time, which is how the cache knows when it was used.
fn aged(path: &Path, seconds: u64) -> std::io::Result<()> {
    let used = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
    std::fs::File::options().write(true).open(path)?.set_modified(used)
}

/// An empty home reads the standard library's parse and the runtime library's stamp from the
/// toolchain's store, which depend only on this build of Fennec and the library, and keeps
/// neither itself: the first home to work them out keeps them there for every home after. With the
/// store turned off, a home works both out again, as every empty home once did.
#[test]
fn an_empty_home_reads_std_and_the_runtimes_stamp_from_the_toolchains_store() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_toolchain_store_{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("hello.fen");
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(\"hello\")\n    return\n")?;
    let store = dir.join("store");
    let run = |home: &str, store: &Path| {
        crate::executables::fennec_command()
            .args(["run", "--no-test"])
            .arg(&program)
            .env("FENNEC_HOME", dir.join(home))
            .env("FENNEC_TOOLCHAIN_STORE", store)
            .env("FENNEC_NO_DAEMON", "1")
            .env_remove("FENNEC_NO_CACHE")
            .output_within()
    };
    // How many entries of `kind` a home's cache, or the store, holds, in every build's folder.
    let entries = |root: &Path, kind: &str| -> usize {
        std::fs::read_dir(root.join("cache"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|build| std::fs::read_dir(build.path().join(kind)).ok())
            .map(|found| {
                found
                    .flatten()
                    .filter(|entry| entry.path().extension().is_none_or(|ext| ext != "sum"))
                    .count()
            })
            .sum()
    };
    let first = run("first", &store)?;
    assert_eq!(text(&first.stdout), "hello\n", "{}", text(&first.stderr));
    assert_eq!(
        (entries(&store, "prelude"), entries(&store, "runtime-stamps")),
        (1, 1),
        "the first home fills the store"
    );
    let second = run("second", &store)?;
    assert_eq!(text(&second.stdout), "hello\n", "{}", text(&second.stderr));
    let second_home = dir.join("second");
    assert_eq!(
        (entries(&second_home, "prelude"), entries(&second_home, "runtime-stamps")),
        (0, 0),
        "an empty home reads both from the store"
    );
    // Turned off, a home does both itself.
    let off = run("off", Path::new(""))?;
    assert_eq!(text(&off.stdout), "hello\n", "{}", text(&off.stderr));
    let off_home = dir.join("off");
    assert_eq!((entries(&off_home, "prelude"), entries(&off_home, "runtime-stamps")), (1, 1));
    std::fs::remove_dir_all(&dir)?;
    Ok(())
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
    let ran = crate::executables::fennec_command_with_empty_store()
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
        crate::executables::fennec_command_with_empty_store()
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
        "def total(xs: list[int]) -> int:\n    mut sum_of = 0\n    for i in range(len(xs)):\n        \
         sum_of += xs[i]\n    return sum_of\n\ndef main(stdout: Stdout):\n    stdout.print(total([i for i in range(10)]))\n",
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
    let source = "def main(stdout: Stdout):\n    stdout.print(abs(-4), abs(-2.5))\n";
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
        "def size(table: dict[int, int]) -> int:\n    return len(table)\n\ndef main(stdout: Stdout):\n    \
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
         Counter(counts={})\n    for i in range(20_000):\n        counter.bump(i * 7919 % 10_007)\n    stdout.print(len(counter.counts))\n",
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
    let body = "    mut seen = [1, 2]\n    for _ in range(3):\n        kept = seen\n        seen.append(len(kept))\n        stdout.print(len(kept), len(seen))\n";
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
                  total += enemy.hp\n    return total\n\ndef main(stdout: Stdout):\n    mut world = World()\n    \
                  _ = world.enemies.add(Enemy(hp=3))\n    _ = world.enemies.add(Enemy(hp=5))\n    \
                  stdout.print(step(ref world))\n";
    std::fs::write(&program, source).unwrap();
    let checked = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    assert!(checked.status.success(), "{}", text(&checked.stderr));
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "6\n", "{}", text(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

/// `@hot` refuses an item the native code copies to change: one read out of a list, changed and
/// put back with the list read in between, so the code generator can't take it out. The same
/// with nothing read in between, and a `for ref` loop over a `Table` of items holding a dict,
/// change their items where they are, so they check. `fennec dev copies` lists the copy too.
/// Before this, `@hot` saw none of these copies, though the program made them.
#[test]
fn a_hot_function_refuses_an_item_it_copies_to_change() {
    let dir = scratch("hot_item_copy").unwrap();
    let program = dir.join("hot.fen");
    let types =
        "from std.collections import Table\n\nstruct Enemy:\n    hp: int\n    bag: dict[str, int]\n\n";
    let main = "\ndef main(stdout: Stdout):\n    mut enemies = [Enemy(hp=3, bag={\"a\": 1}), Enemy(hp=4, bag={\"a\": 2})]\n    \
                mut table = Table[Enemy]()\n    _ = table.add(Enemy(hp=5, bag={\"a\": 3}))\n    \
                step(ref enemies, ref table)\n    stdout.print(enemies[0].hp, enemies[1].hp)\n";
    let step = |between: &str| {
        format!(
            "@hot\ndef step(ref enemies: list[Enemy], ref table: Table[Enemy]):\n    \
             for ref held in table:\n        held.hp -= 1\n    \
             for i in range(len(enemies)):\n        mut enemy = enemies[i]\n{between}        enemy.hp -= 1\n        \
             enemies[i] = enemy\n"
        )
    };
    let copying =
        format!("{types}{}{main}", step("        for other in enemies:\n            enemy.hp -= other.hp\n"));
    std::fs::write(&program, &copying).unwrap();
    let output = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    assert!(!output.status.success());
    let refused = "what this reads out of `enemies` is copied to change it in `@hot` `step`";
    assert!(text(&output.stderr).contains(refused), "{}", text(&output.stderr));
    std::fs::write(&program, copying.replace("@hot\n", "")).unwrap();
    let listed = fennec(&["dev", "copies", program.to_str().unwrap()], "").unwrap();
    assert!(
        text(&listed.stdout).contains("what this reads out of `enemies` is copied to change it"),
        "{}",
        text(&listed.stdout)
    );
    std::fs::write(&program, format!("{types}{}{main}", step(""))).unwrap();
    let checked = fennec(&["check", program.to_str().unwrap()], "").unwrap();
    assert!(checked.status.success(), "{}", text(&checked.stderr));
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
                  tagged(ref units[0], \"hit\")\n    return total\n\ndef main(stdout: Stdout):\n    \
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
    let source = "@hot\ndef total(xs: list[float], at: int) -> float:\n    mut running = 0.0\n    for x in xs:\n        \
                  running += x\n    return running + xs[at]\n\ndef main(stdout: Stdout):\n    stdout.print(total([1.0, 2.0], 1))\n";
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
    std::fs::write(&program, source.replace("running + xs[at]", "running")).unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "3.0\n", "{}", text(&output.stderr));
    // An overflow check the compiler can't prove away stays, as the safety net it is everywhere.
    std::fs::write(&program, "@hot\ndef add(a: int, b: int) -> int:\n    return a + b\n\ndef main(stdout: Stdout):\n    stdout.print(add(1, b=2))\n").unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "3\n", "{}", text(&output.stderr));
    // So does a float divide check, and an integer one doesn't.
    std::fs::write(&program, "@hot\ndef ratio(a: float, b: float) -> float:\n    return a / b\n\ndef main(stdout: Stdout):\n    stdout.print(ratio(1.0, b=4.0))\n").unwrap();
    let output = fennec(&["run", program.to_str().unwrap()], "").unwrap();
    assert_eq!(text(&output.stdout), "0.25\n", "{}", text(&output.stderr));
    std::fs::write(&program, "@hot\ndef parts(a: int, b: int) -> int:\n    return a // b\n\ndef main(stdout: Stdout):\n    stdout.print(parts(9, b=4))\n").unwrap();
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
         def balance(self) -> int:\n        return self.total\n\n\
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
         SQUARES = {n: n * n for n in range(5)}\n\ndef main(stdout: Stdout):\n    stdout.print(len(SINE), SINE[256], ORIGIN.y, SQUARES[4])\n",
    )
    .unwrap();
    let output = fennec(&["run", &path], "").unwrap();
    assert_eq!(text(&output.stdout), "1024 1.0 -1.5 16\n", "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");

    // One that takes too long is worked out when the program starts, with a hint.
    std::fs::write(
        &program,
        "def slow() -> int:\n    mut total = 0\n    for i in range(2000000):\n        total += i\n    return total\n\n\
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

/// A build checks all of the program, as `check` does, though it builds only what `main` reaches:
/// a mistake in a function nothing calls is refused. Of the standard library it checks what the
/// program reaches, whose own tests check the rest.
#[test]
fn a_build_refuses_a_mistake_in_a_function_nothing_calls() -> std::io::Result<()> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_build_unreached_{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("unreached.fen");
    std::fs::write(
        &program,
        "def unused(count: int) -> str:\n    return count + 1\n\ndef main(stdout: Stdout):\n    stdout.print(\"hi\")\n",
    )?;
    let program = program.to_string_lossy().into_owned();
    let executable = dir.join("unreached").to_string_lossy().into_owned();
    let built = fennec(&["build", &program, "-o", &executable], "")?;
    assert!(!built.status.success());
    assert!(
        text(&built.stderr).contains("`unused` returns a str, but this is an int"),
        "{}",
        text(&built.stderr)
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A build hands its check to lowering, cut to what `main` reaches. A function nothing calls that
/// starts tasks makes more types cross between threads than what's reached does, which a dev build's
/// check of the cut module finds within what was handed on, so the build goes ahead and runs.
#[test]
fn a_build_whose_unreached_function_starts_tasks_builds_and_runs() -> std::io::Result<()> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec_build_unreached_tasks_{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("tasks.fen");
    std::fs::write(
        &program,
        "struct Unit:\n    hp: int\n\ndef unused() -> int:\n    mut units = [Unit(hp=1), Unit(hp=2)]\n    \
         for ref unit in parallel(units):\n        unit.hp += 1\n    return units[0].hp\n\ndef main(stdout: Stdout):\n    \
         stdout.print(1)\n",
    )?;
    let program = program.to_string_lossy().into_owned();
    let executable = dir.join("tasks").to_string_lossy().into_owned();
    let built = fennec(&["build", &program, "-o", &executable], "")?;
    assert!(built.status.success(), "{}", text(&built.stderr));
    let ran = std::process::Command::new(&executable).output()?;
    assert_eq!(text(&ran.stdout), "1\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// One program built for release in two folders, with two Fennec folders, a second apart, is the
/// same bytes each time, as a signed release has to be to check by building it again, and a dev
/// build is the same bytes in one folder a second later: a kit built by its whole path, as `.`, and with a dependency it names by a path, and
/// a single file built by its whole path. Nothing of where or when it was built goes in: the files
/// it carries to show a failure in are named from the kit, not from the root of the disk, and its
/// A program built again to the path of one that has run is a new file there, and runs: macOS
/// remembers the code signature of a file it has launched, and kills, with signal 9 and nothing
/// printed, a program written into that same file. Built three times over, changed each time,
/// each build has to start and print what it says.
#[test]
fn a_program_built_again_where_one_ran_runs() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("built-again")?;
    let program = dir.join("again.fen");
    let output = dir.join("again");
    for round in 1..=3 {
        std::fs::write(
            &program,
            format!("def main(stdout: Stdout):\n    stdout.print({round})\n    return\n"),
        )?;
        let built = crate::executables::fennec_command()
            .args(["build", "again.fen", "-o", "again"])
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .output_within()?;
        assert!(built.status.success(), "round {round}: {}", text(&built.stderr));
        let ran = std::process::Command::new(&output).output_within()?;
        assert_eq!(
            (text(&ran.stdout), ran.status.code()),
            (format!("{round}\n"), Some(0)),
            "round {round}: {}",
            text(&ran.stderr)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A switch that changes what a build makes keys the store, so a program built one way is never
/// handed back for another: a plain program once came back for a build asked for the address
/// sanitizer. Each switch, set after a plain build of the same program, finds no program
/// kept and builds one, kept under a key of its own; a sanitized one is never the plain one.
#[test]
fn a_switch_that_changes_a_program_never_gets_one_built_without_it() -> std::io::Result<()> {
    use crate::executables::Within as _;
    // A build under `FENNEC_AUDIT` links the runtime with the audit in it.
    if !crate::llvm::ready() || !crate::executables::audited_runtimes_built() {
        return Ok(());
    }
    let dir = scratch("switches")?;
    // It runs `parallel`, so which runtime it links is the threads switch's to say.
    std::fs::write(
        dir.join("alone.fen"),
        "def main(stdout: Stdout):\n    mut values = [1, 2, 3, 4]\n    for ref value in parallel(values):\n        value += 1\n    stdout.print(values)\n    return\n",
    )?;
    let home = dir.join("home");
    let build = |switch: Option<(&str, &str)>, out: &str| -> std::io::Result<Output> {
        let mut command = crate::executables::fennec_command();
        command.args(["build", "alone.fen", "-o", out]).current_dir(&dir).env("FENNEC_HOME", &home);
        for name in [
            "FENNEC_SANITIZE",
            "FENNEC_CHECK_PROOFS",
            "FENNEC_STRIP_FACTS",
            "FENNEC_THREADS_RUNTIME",
            "FENNEC_BOXED_RESULTS",
            "FENNEC_BOXED_TYPES",
            "FENNEC_BOXED_DICT_VALUES",
            "FENNEC_AUDIT",
            "FENNEC_NO_CACHE",
        ] {
            command.env_remove(name);
        }
        if let Some((name, value)) = switch {
            command.env(name, value);
        }
        command.output_within()
    };
    // The programs the store keeps, by their keys.
    let kept = || -> usize {
        let Ok(builds) = std::fs::read_dir(home.join("cache")) else { return 0 };
        builds
            .flatten()
            .filter_map(|build| std::fs::read_dir(build.path().join("programs")).ok())
            .flat_map(Iterator::flatten)
            // One a key: one written as bytes is a file named for the key, and one kept as the file
            // that ran is named by the key's `.sum` file, beside the program.
            .filter(|entry| {
                let path = entry.path();
                path.extension().is_none_or(|ext| ext.eq_ignore_ascii_case("sum"))
            })
            .count()
    };
    let plain = build(None, "plain")?;
    assert!(plain.status.success(), "{}", text(&plain.stderr));
    assert_eq!(kept(), 1);
    for (at, (name, value)) in [
        ("FENNEC_CHECK_PROOFS", "1"),
        ("FENNEC_STRIP_FACTS", "1"),
        ("FENNEC_THREADS_RUNTIME", "0"),
        ("FENNEC_BOXED_RESULTS", "1"),
        ("FENNEC_BOXED_TYPES", "1"),
        ("FENNEC_BOXED_DICT_VALUES", "1"),
        ("FENNEC_AUDIT", "1"),
    ]
    .into_iter()
    .enumerate()
    {
        let switched = build(Some((name, value)), name)?;
        assert!(switched.status.success(), "{name}: {}", text(&switched.stderr));
        assert_eq!(kept(), 2 + at, "{name}={value} was handed a program built without it");
    }
    // A sanitized build either links its sanitizer, a program unlike the plain one, or can't
    // where its runtime isn't to be had, but it's never the plain program handed back.
    let sanitized = build(Some(("FENNEC_SANITIZE", "address")), "sanitized")?;
    if sanitized.status.success() {
        // Compared whole, but not printed: a program is megabytes.
        let same = std::fs::read(dir.join("plain"))? == std::fs::read(dir.join("sanitized"))?;
        assert!(!same, "a sanitized build was the plain program");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// cache key goes by the runtime library's build, not by where that is or when it changed.
#[test]
fn a_release_built_anywhere_is_the_same_bytes() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("same-bytes")?;
    let places = [dir.join("one/deeper/still"), dir.join("two")];
    for place in &places {
        let (kit, tools) = (place.join("game"), place.join("tools"));
        std::fs::create_dir_all(kit.join("src/world"))?;
        std::fs::create_dir_all(tools.join("src"))?;
        std::fs::write(
            tools.join("kit.toml"),
            "name = \"tools\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n",
        )?;
        std::fs::write(tools.join("src/lib.fen"), "export def twice(n: int) -> int:\n    return n * 2\n")?;
        std::fs::write(
            kit.join("kit.toml"),
            "name = \"game\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\ntools = { path = \"../tools\" }\n",
        )?;
        std::fs::write(kit.join("src/world/size.fen"), "export def size() -> int:\n    return 21\n")?;
        std::fs::write(
            kit.join("src/main.fen"),
            "from tools import twice\nfrom world.size import size\n\ndef main(stdout: Stdout):\n    stdout.print(twice(size()))\n    return\n",
        )?;
        std::fs::write(
            place.join("alone.fen"),
            "def main(stdout: Stdout):\n    stdout.print(42)\n    return\n",
        )?;
    }
    let build =
        |place: &Path, home: &str, target: &str, from: &Path, out: &str| -> std::io::Result<Vec<u8>> {
            let built = crate::executables::fennec_command()
                .args(["build", "--release", target, "-o"])
                .arg(place.join(out))
                .current_dir(from)
                .env("FENNEC_HOME", dir.join(home))
                .output_within()?;
            assert!(built.status.success(), "{target}: {}", text(&built.stderr));
            std::fs::read(place.join(out))
        };
    for (what, out) in
        [("kit by its whole path", "whole"), ("kit as .", "dot"), ("file by its whole path", "file")]
    {
        let mut made = Vec::new();
        for (at, place) in places.iter().enumerate() {
            let (target, from) = match out {
                "whole" => (place.join("game").to_string_lossy().into_owned(), place.clone()),
                "dot" => (".".to_owned(), place.join("game")),
                _ => (place.join("alone.fen").to_string_lossy().into_owned(), place.clone()),
            };
            made.push(build(place, &format!("home{at}"), &target, &from, out)?);
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        assert!(made[0] == made[1], "a {what} built in two places came out different");
    }
    // A dev build keeps the paths it was given, so it's the same bytes in the same folder, however
    // far apart in time, with a Fennec folder of its own each time so nothing comes from a cache.
    let place = &places[0];
    let mut dev_builds = Vec::new();
    for at in 0..2 {
        let dev_build = crate::executables::fennec_command()
            .args(["build"])
            .arg(place.join("game"))
            .arg("-o")
            .arg(place.join("dev"))
            .env("FENNEC_HOME", dir.join(format!("dev-home{at}")))
            .output_within()?;
        assert!(dev_build.status.success(), "{}", text(&dev_build.stderr));
        dev_builds.push(std::fs::read(place.join("dev"))?);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    assert!(dev_builds[0] == dev_builds[1], "a dev build in one folder came out different a second later");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A program is the same bytes whatever it's named and wherever it's written: built twice from
/// nothing, each with a Fennec folder of its own, under two names in two folders, dev and release.
/// macOS's linker made a program's UUID, and the signature over it, from the name it was written
/// to, so a recorded run's program and its replay's, named apart, differed.
#[test]
fn a_program_is_the_same_bytes_whatever_its_named() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("same-bytes-named")?;
    std::fs::write(dir.join("alone.fen"), "def main(stdout: Stdout):\n    stdout.print(42)\n    return\n")?;
    std::fs::create_dir_all(dir.join("elsewhere/deeper"))?;
    for how in [&[][..], &["--release"][..]] {
        let mut made = Vec::new();
        for (at, out) in ["first", "elsewhere/deeper/a-name-of-its-own"].into_iter().enumerate() {
            let built = crate::executables::fennec_command()
                .arg("build")
                .args(how)
                .args(["alone.fen", "-o", out])
                .current_dir(&dir)
                .env("FENNEC_HOME", dir.join(format!("home-{}-{at}", how.len())))
                .output_within()?;
            assert!(built.status.success(), "{how:?} {out}: {}", text(&built.stderr));
            made.push(std::fs::read(dir.join(out))?);
        }
        assert!(made[0] == made[1], "{how:?}: one program built under two names came out different");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A dev build after an edit links the cache's own object files where they lie, writing out only
/// what it compiled, and the program is the same bytes as one built with no cache at all, every
/// object written out fresh. Writing out and deleting a copy of every object was once most of an
/// edit's build on a Mac.
#[test]
fn a_program_linked_from_the_caches_objects_is_the_one_built_without_it() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("linked-from-cache")?;
    let program = |factor: u32| {
        format!(
            "def scaled(value: int) -> int:\n    return value * {factor}\n\ndef label(value: int) -> str:\n    return f\"value {{value}}\"\n\ndef main(stdout: Stdout):\n    stdout.print(label(scaled(21)))\n    return\n"
        )
    };
    let build = |out: &str, cached: bool| {
        let mut command = crate::executables::fennec_command_with_empty_store();
        command.args(["build", "edited.fen", "-o", out]).current_dir(&dir);
        if cached {
            command.env("FENNEC_HOME", dir.join("home")).env_remove("FENNEC_NO_CACHE");
        } else {
            command.env("FENNEC_HOME", dir.join("no-home")).env("FENNEC_NO_CACHE", "1");
        }
        command.output_within()
    };
    std::fs::write(dir.join("edited.fen"), program(2))?;
    let first = build("first", true)?;
    assert!(first.status.success(), "{}", text(&first.stderr));
    std::fs::write(dir.join("edited.fen"), program(3))?;
    let edited = build("edited", true)?;
    assert!(edited.status.success(), "{}", text(&edited.stderr));
    let said = text(&edited.stdout);
    let counts: Vec<usize> = said
        .split_once("compiled ")
        .and_then(|(_, rest)| rest.split_once(" functions"))
        .map(|(counts, _)| counts.split(" of ").filter_map(|count| count.parse().ok()).collect())
        .unwrap_or_default();
    assert!(
        matches!(counts[..], [fresh, total] if fresh > 0 && fresh < total),
        "the edit compiled only some functions: {said}"
    );
    let uncached = build("uncached", false)?;
    assert!(uncached.status.success(), "{}", text(&uncached.stderr));
    assert!(
        std::fs::read(dir.join("edited"))? == std::fs::read(dir.join("uncached"))?,
        "the program linked from the cache's objects isn't the one built without the cache"
    );
    let ran = std::process::Command::new(dir.join("edited")).output_within()?;
    assert_eq!(text(&ran.stdout), "value 63\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A kept object that goes from the cache after a build found it and before it links, as a cleanup
/// another command runs can take it, is compiled again from its function's text, so the build
/// still makes the program a build without the cache makes. `FENNEC_LOSE_KEPT_OBJECTS=1` takes
/// every kept object just before the link. The next build, finding them gone, compiles them again.
///
/// The edit changes `scaled` alone, so the build uses `label` and `main` again without writing
/// them, and they have no text to compile again from: that build is made again writing every
/// function, and says so.
#[test]
fn a_kept_object_gone_before_the_link_is_compiled_again() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("lost-objects")?;
    let program = |factor: u32| {
        format!(
            "def scaled(value: int) -> int:\n    return value * {factor}\n\ndef label(value: int) -> str:\n    return f\"value {{value}}\"\n\ndef main(stdout: Stdout):\n    stdout.print(label(scaled(21)))\n    return\n"
        )
    };
    let build = |out: &str, cached: bool, lose: bool| {
        let mut command = crate::executables::fennec_command_with_empty_store();
        command.args(["build", "edited.fen", "-o", out]).current_dir(&dir);
        if cached {
            command.env("FENNEC_HOME", dir.join("home")).env_remove("FENNEC_NO_CACHE");
        } else {
            command.env("FENNEC_HOME", dir.join("no-home")).env("FENNEC_NO_CACHE", "1");
        }
        if lose {
            command.env("FENNEC_LOSE_KEPT_OBJECTS", "1");
        } else {
            command.env_remove("FENNEC_LOSE_KEPT_OBJECTS");
        }
        command.output_within()
    };
    std::fs::write(dir.join("edited.fen"), program(2))?;
    let first = build("first", true, false)?;
    assert!(first.status.success(), "{}", text(&first.stderr));
    std::fs::write(dir.join("edited.fen"), program(3))?;
    let lost = build("lost", true, true)?;
    assert!(
        lost.status.success(),
        "a build whose kept objects went before the link failed: {}",
        text(&lost.stderr)
    );
    assert!(
        text(&lost.stderr).contains("so it wrote every function again"),
        "the build used no unit again, or didn't say it wrote them again: {}",
        text(&lost.stderr)
    );
    let uncached = build("uncached", false, false)?;
    assert!(uncached.status.success(), "{}", text(&uncached.stderr));
    assert!(
        std::fs::read(dir.join("lost"))? == std::fs::read(dir.join("uncached"))?,
        "the program linked from objects compiled again isn't the one built without the cache"
    );
    let ran = std::process::Command::new(dir.join("lost")).output_within()?;
    assert_eq!(text(&ran.stdout), "value 63\n");
    std::fs::write(dir.join("edited.fen"), program(4))?;
    let again = build("again", true, false)?;
    assert!(again.status.success(), "{}", text(&again.stderr));
    let ran = std::process::Command::new(dir.join("again")).output_within()?;
    assert_eq!(text(&ran.stdout), "value 84\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// The program [`an_edit_build_makes_what_a_build_without_the_cache_makes`] edits.
const EDITED_BASE: &str = r#"error Short:
    excess: int

struct Spot:
    across = 0
    down = 0

struct Named:
    name: str? = None

enum Twig:
    Leaf
    Node(left: Twig, right: Twig)

enum Bush:
    Bare
    Branch(inner: Bush, weight: int)

LIMIT = 40

def tiny(value: int) -> int:
    return value + 1

def picked(items: list[int], index: int) -> int:
    return items[index]

def bound(value: int) -> int:
    return value

def checked(value: int) -> int fails Short:
    limit = bound(value)
    if limit > 1000:
        fail Short(excess=limit - 1000)
    return limit

def pair_of[T](first: T, second: T) -> list[T]:
    return [first, second]

def moved(spot: Spot, steps: int) -> Spot:
    return Spot(across=spot.across + steps, down=spot.down)

def summed() -> int:
    mut total = 0
    for step in range(LIMIT):
        total += tiny(step)
    return total

def shelves() -> int:
    spots = [Spot(across=1)]
    places = {"home": Spot(down=2)}
    nested = [[1, 2], [3]]
    return len(spots) + len(places) + len(nested)

def twigs(depth: int) -> Twig:
    if depth == 0: return .Leaf
    return .Node(left=twigs(depth - 1), right=twigs(depth - 1))

def leaves(twig: Twig) -> int:
    match twig:
        case .Leaf:
            return 1
        case .Node(left, right):
            return leaves(left) + leaves(right)

def bushes(depth: int) -> Bush:
    if depth == 0: return .Bare
    return .Branch(inner=bushes(depth - 1), weight=depth)

def weights(bush: Bush) -> int:
    match bush:
        case .Bare:
            return 0
        case .Branch(inner, weight):
            return weight + weights(inner)

def kept(fs: Fs, path: str) fails IoError:
    with fs.create(path)? as file:
        file.write_line("kept")?

def main(stdout: Stdout, fs: Fs):
    items = [3, 5, 7, 9]
    stdout.print(picked(items, 2))
    stdout.print(summed())
    checked_value = checked(tiny(41)) except Short(): -1
    stdout.print(checked_value)
    stdout.print(len(pair_of(first=1, second=2)), pair_of(first="a", second="b")[0])
    stdout.print(moved(Spot(across=1, down=2), steps=3).across)
    stdout.print(Named(name="spot").name else "none")
    stdout.print(shelves(), leaves(twigs(2)), weights(bushes(3)))
    if len(items) > 100:
        kept(fs, "never.txt") except why:
            stdout.print(why)
    return
"#;

/// The edits [`an_edit_build_makes_what_a_build_without_the_cache_makes`] makes, each to
/// [`EDITED_BASE`]: what it is, and what it replaces with what.
const EDITS: &[(&str, &[(&str, &str)])] = &[
    ("a body", &[("        total += tiny(step)\n", "        total += tiny(step) * 2\n")]),
    ("a small function others hold a copy of", &[("    return value + 1\n", "    return value + 2\n")]),
    (
        "a caller whose argument changes what its callee may assume",
        &[("picked(items, 2)", "picked(items, len(items) - 2)")],
    ),
    (
        "a callee that starts to fail",
        &[
            (
                "def bound(value: int) -> int:\n    return value\n",
                "def bound(value: int) -> int fails Short:\n    if value < 0:\n        fail Short(excess=value)\n    return value\n",
            ),
            ("    limit = bound(value)\n", "    limit = bound(value)?\n"),
        ],
    ),
    ("a type's field", &[("    down = 0\n", "    down = 0\n    depth = 0\n")]),
    (
        "a use elsewhere that lays a type out in a dict and a list",
        &[(
            "    stdout.print(moved(",
            "    places = {\"home\": Spot(across=5, down=6)}\n    stdout.print(places[\"home\"].across, len([Spot(across=7)]))\n    stdout.print(moved(",
        )],
    ),
    ("a constant", &[("LIMIT = 40", "LIMIT = 41")]),
    ("a generic, with its copies", &[("    return [first, second]", "    return [second, first]")]),
    ("lines added above a function", &[("def picked(", "# Two lines above `picked`.\n\ndef picked(")]),
    (
        "the program's first `parallel`",
        &[(
            "    stdout.print(summed())\n",
            "    stdout.print(summed())\n    stdout.print(sum(tiny(step) for step in parallel([1, 2, 3])))\n",
        )],
    ),
    ("a function renamed, so one is added and one removed", &[("moved", "shifted")]),
    (
        "a type, a tuple and a closure added early",
        &[
            ("struct Spot:", "struct Early:\n    flag = False\n\nstruct Spot:"),
            (
                "    items = [3, 5, 7, 9]\n",
                "    items = [3, 5, 7, 9]\n    pairing = (Early(flag=True).flag, 3)\n    stdout.print(pairing[1], sorted(items, key=(item) => -item)[0])\n",
            ),
        ],
    ),
    (
        "a function std's text is lent into, written above another that has some",
        &[
            (
                "def picked(",
                "def kept_first(fs: Fs, path: str) fails IoError:\n    with fs.create(path)? as file:\n        file.write_line(\"first\")?\n\ndef picked(",
            ),
            (
                "    items = [3, 5, 7, 9]\n",
                "    items = [3, 5, 7, 9]\n    if len(items) > 200:\n        kept_first(fs, \"never.txt\") except why:\n            stdout.print(why)\n",
            ),
        ],
    ),
    (
        "two types that hold themselves, swapped",
        &[(
            "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\nenum Bush:\n    Bare\n    Branch(inner: Bush, weight: int)\n",
            "enum Bush:\n    Bare\n    Branch(inner: Bush, weight: int)\n\nenum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n",
        )],
    ),
];

/// A build of `edited.fen` in `dir` for [`an_edit_build_makes_what_a_build_without_the_cache_makes`]:
/// its output, the objects it linked in order, and the program. It's from the cache in `dir`, or
/// with none, and with types boxed or not.
type EditBuilt = (Output, Vec<Vec<u8>>, Vec<u8>);

fn edit_build(dir: &Path, name: &str, cached: bool, boxed: bool) -> std::io::Result<EditBuilt> {
    use crate::executables::Within as _;
    let objects = dir.join(format!("{name}-objects"));
    let mut command = crate::executables::fennec_command();
    command.args(["build", "edited.fen", "-o", name]).current_dir(dir).env("FENNEC_LINK_KEEP", &objects);
    if cached {
        command.env("FENNEC_HOME", dir.join("home")).env_remove("FENNEC_NO_CACHE");
    } else {
        command.env("FENNEC_HOME", dir.join("no-home")).env("FENNEC_NO_CACHE", "1");
    }
    if boxed {
        command.env("FENNEC_BOXED_TYPES", "1");
    } else {
        command.env_remove("FENNEC_BOXED_TYPES");
    }
    let output = command.output_within()?;
    assert!(output.status.success(), "{name}: {}", text(&output.stderr));
    // A program the store had whole links nothing, so it keeps no objects.
    let count = std::fs::read_dir(&objects).map_or(0, Iterator::count);
    let linked =
        (0..count).map(|at| std::fs::read(objects.join(format!("{at}.o")))).collect::<Result<_, _>>()?;
    Ok((output, linked, std::fs::read(dir.join(name))?))
}

/// How many functions' text a build wrote, of how many, as it says when it used others again.
fn written_of(output: &Output) -> Option<(usize, usize)> {
    let said = text(&output.stdout);
    let (_, rest) = said.split_once("(wrote ")?;
    let (counts, _) = rest.split_once(')')?;
    let (written, total) = counts.split_once(" of ")?;
    Some((written.parse().ok()?, total.parse().ok()?))
}

/// How many functions a build compiled, of how many, as it says.
fn compiled_of(output: &Output) -> Option<(usize, usize)> {
    let said = text(&output.stdout);
    let (_, rest) = said.split_once("compiled ")?;
    let (counts, _) = rest.split_once(" functions")?;
    let (fresh, total) = counts.split_once(" of ")?;
    Some((fresh.parse().ok()?, total.parse().ok()?))
}

/// What a dev build makes after an edit, from the cache the builds before it filled, is what a
/// build with no cache at all makes: every object it links and the program, byte for byte. Each
/// of the edits changes what some function's object is made from in a way of its own (see
/// [`EDITS`]), and a switch that lays types out another way goes on, then off again. So whatever
/// a build reuses, it can never be an object made from anything but what it would make now.
#[test]
fn an_edit_build_makes_what_a_build_without_the_cache_makes() -> std::io::Result<()> {
    use crate::executables::Within as _;
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("edit-kinds")?;
    let same = |what: &str, step: &str, cached: &EditBuilt, whole: &EditBuilt| {
        assert!(!whole.1.is_empty(), "{what}: the build without the cache kept no objects");
        assert_eq!(cached.1.len(), whole.1.len(), "{what}: {step} linked another number of objects");
        if let Some(at) = cached.1.iter().zip(&whole.1).position(|(cached, whole)| cached != whole) {
            panic!("{what}: object {at} of {} isn't the one built without the cache", cached.1.len());
        }
        assert!(cached.2 == whole.2, "{what}: the program isn't the one built without the cache");
        let ran = std::process::Command::new(dir.join(step)).output_within().expect("the program runs");
        assert!(ran.status.success(), "{what}: {}", text(&ran.stderr));
    };
    std::fs::write(dir.join("edited.fen"), EDITED_BASE)?;
    let base = (edit_build(&dir, "base", true, false)?, edit_build(&dir, "base-whole", false, false)?);
    same("the first build", "base", &base.0, &base.1);
    let mut reused = Vec::new();
    for (at, (what, changes)) in EDITS.iter().enumerate() {
        let mut edited = EDITED_BASE.to_owned();
        for (from, to) in *changes {
            assert!(edited.contains(from), "{what}: no {from:?}");
            edited = edited.replace(from, to);
        }
        std::fs::write(dir.join("edited.fen"), &edited)?;
        let step = format!("edit{at}");
        let cached = edit_build(&dir, &step, true, false)?;
        same(what, &step, &cached, &edit_build(&dir, &format!("{step}-whole"), false, false)?);
        reused.push((*what, compiled_of(&cached.0)));
        // A body's edit writes that function's text again, and a type's field the text of what
        // reads the type's layout, and each uses the rest as kept.
        if at == 0 || *what == "a type's field" {
            let written = written_of(&cached.0);
            assert!(
                written.is_some_and(|(written, total)| written > 0 && written < total),
                "{what}: {written:?}"
            );
        }
    }
    // Every edit compiles again only some of the functions, and an edit to a body at least one.
    let all_again: Vec<_> =
        reused.iter().filter(|(_, counts)| counts.is_none_or(|(fresh, total)| fresh >= total)).collect();
    assert!(all_again.is_empty(), "edits that reused nothing: {all_again:?}");
    assert!(
        reused.first().is_some_and(|(_, counts)| counts.is_some_and(|(fresh, _)| fresh > 0)),
        "{reused:?}"
    );
    std::fs::write(dir.join("edited.fen"), EDITED_BASE)?;
    let boxed = (edit_build(&dir, "boxed", true, true)?, edit_build(&dir, "boxed-whole", false, true)?);
    same("types boxed", "boxed", &boxed.0, &boxed.1);
    assert!(boxed.0.2 != base.1.2, "boxing types changed nothing: the switch tests nothing here");
    // Back as it was, the program is the first one again, from the store; and a program not built
    // before reuses what was compiled with types in place.
    assert!(
        edit_build(&dir, "back", true, false)?.2 == base.1.2,
        "the program after boxing and back changed"
    );
    std::fs::write(dir.join("edited.fen"), EDITED_BASE.replace("LIMIT = 40", "LIMIT = 42"))?;
    let last = (edit_build(&dir, "last", true, false)?, edit_build(&dir, "last-whole", false, false)?);
    same("types in place again", "last", &last.0, &last.1);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A kit built in one folder and then in another with the same Fennec home, as a second checkout
/// or a worktree of it is, uses the first build's units again on its first build there: the index
/// of kept units is named by the kit and its entry, not by where the kit is. Each unit is used
/// again only when its function and every read it made are as they were, so the second checkout's
/// program is the first's, byte for byte. Before, its first build wrote every function again.
#[test]
fn a_second_checkout_of_a_kit_uses_the_first_ones_units_again() -> std::io::Result<()> {
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("second-checkout")?;
    for checkout in ["first", "second"] {
        std::fs::create_dir_all(dir.join(checkout).join("src"))?;
        std::fs::write(
            dir.join(checkout).join("kit.toml"),
            "name = \"twice\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n\n[dependencies]\n",
        )?;
        std::fs::write(dir.join(checkout).join("src/main.fen"), EDITED_BASE)?;
    }
    let build = |checkout: &str| {
        crate::executables::fennec_command()
            .args(["build", ".", "-o", "program"])
            .current_dir(dir.join(checkout))
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("FENNEC_NO_CACHE")
            .output()
    };
    let first = build("first")?;
    assert!(first.status.success(), "{}", text(&first.stderr));
    let second = build("second")?;
    assert!(second.status.success(), "{}", text(&second.stderr));
    let written = written_of(&second);
    assert!(written.is_some_and(|(written, total)| written < total), "{written:?}: {}", text(&second.stdout));
    assert!(
        std::fs::read(dir.join("first/program"))? == std::fs::read(dir.join("second/program"))?,
        "the second checkout's program isn't the first's"
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// With `FENNEC_CACHE_AUDIT=1`, a build writes each unit it uses again anyway, into what's thrown
/// away, and fails if it doesn't come out as kept: its text, what it shares, every kind it asks
/// the layouts for, in order, and every read it makes. So each of these edit builds, one for
/// every kind of edit, checks every unit it uses again. The first three edits use some again, and
/// so does a type's field, which reaches only what reads the type's layout.
#[test]
fn an_edit_build_audited_writes_each_unit_it_uses_again_the_same() -> std::io::Result<()> {
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = scratch("edit-audited")?;
    let audited = |name: &str| {
        let mut command = crate::executables::fennec_command();
        command
            .args(["build", "edited.fen", "-o", name])
            .current_dir(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env("FENNEC_CACHE_AUDIT", "1")
            .env_remove("FENNEC_NO_CACHE");
        command.output()
    };
    let mut wrote = Vec::new();
    for (at, (what, changes)) in
        [("the first build", &[][..])].into_iter().chain(EDITS.iter().copied()).enumerate()
    {
        let mut edited = EDITED_BASE.to_owned();
        for (from, to) in changes {
            edited = edited.replace(from, to);
        }
        std::fs::write(dir.join("edited.fen"), &edited)?;
        let built = audited(&format!("audited{at}"))?;
        assert!(built.status.success(), "{what}: {}", text(&built.stderr));
        wrote.push((what, written_of(&built)));
    }
    assert!(
        wrote.iter().skip(1).take(3).all(|(_, written)| written.is_some()),
        "edits that used nothing again: {wrote:?}"
    );
    assert!(
        wrote.iter().any(|(what, written)| *what == "a type's field" && written.is_some()),
        "a type's field used nothing again: {wrote:?}"
    );
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
        let source = format!("def main(stdout: Stdout):\n    stdout.print(len(embed_file({path:?})))\n");
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
        stdout.print(len(SITE), SITE.keys())\n    stdout.print(SITE.get(\"css/app.css\"))\n    \
        stdout.print(SITE.get(\"missing\") is None)\n    for path, data in SITE.items():\n        stdout.print(path, len(data))\n";
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
            format!("def main(stdout: Stdout):\n    stdout.print(len(embed_folder({path:?})))\n"),
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

/// A folder linked into itself is walked once: `src/a -> .` and `src/b -> .` would double finding a
/// kit's modules at every level until the path was too long, and `assets/loop -> .` would have
/// `embed_folder` read the folder again inside itself, a few hundred levels deep. Each command
/// here finishes within its CPU time, or the test fails rather than taking the machine's memory.
#[test]
#[cfg(unix)]
fn a_folder_linked_into_itself_is_walked_once() {
    use crate::executables::Within as _;
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-cli-link-loop-{}", std::process::id())),
    );
    let kit = dir.join("kit");
    std::fs::create_dir_all(kit.join("src")).unwrap();
    std::fs::create_dir_all(kit.join("assets")).unwrap();
    std::fs::write(kit.join("kit.toml"), "name = \"looped\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")
        .unwrap();
    std::fs::write(kit.join("src/helper.fen"), "def twice(n: int) -> int:\n    return n * 2\n").unwrap();
    std::fs::write(
        kit.join("src/main.fen"),
        "from helper import twice\n\ndef main(stdout: Stdout):\n    stdout.print(twice(21))\n    return\n",
    )
    .unwrap();
    std::fs::write(kit.join("assets/a.txt"), "a").unwrap();
    std::os::unix::fs::symlink(".", kit.join("src/a")).unwrap();
    std::os::unix::fs::symlink(".", kit.join("src/b")).unwrap();
    std::os::unix::fs::symlink(".", kit.join("assets/loop")).unwrap();
    let run = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", dir.join("home"))
            .env("FENNEC_NO_DAEMON", "1")
            .output_within()
            .unwrap()
    };
    let checked = run(&["check"]);
    assert!(checked.status.success(), "{}", text(&checked.stderr));
    let ran = run(&["run", "--no-test"]);
    assert_eq!(text(&ran.stdout), "42\n", "{}", text(&ran.stderr));
    std::fs::write(
        kit.join("src/main.fen"),
        "ASSETS = embed_folder(\"../assets\")\n\ndef main(stdout: Stdout):\n    stdout.print(ASSETS.len())\n    return\n",
    )
    .unwrap();
    let embedded = run(&["check"]);
    assert!(!embedded.status.success());
    assert!(
        text(&embedded.stderr).contains("is a folder this embeds already, reached again through a link"),
        "{}",
        text(&embedded.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
