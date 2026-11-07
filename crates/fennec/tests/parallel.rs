//! `parallel` built natively through LLVM: its loops, comprehensions and sums run through the
//! runtime's parallel calls, as `FENNEC_COUNTS` shows, and print what the interpreter prints.
//! Skipped where LLVM's tools aren't installed, as `llvm.rs` is.

use std::path::Path;
use std::process::{Command, Output};

use crate::executables::{FENNEC_DEADLINE, Running, Within, runtime_built, threads_runtime_built};

fn fennec(args: &[&str], home: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command().args(args).env("FENNEC_HOME", home).output_before(FENNEC_DEADLINE)
}

fn ready() -> bool {
    fennec_llvm::Tools::find().is_ok()
        && runtime_built()
        // Coverage builds `fennec` in a folder of its own, where no runtime library is built.
        && crate::executables::runtime_beside_fennec()
}

/// A tick over units, as an entity system steps them, each changed in place by a `for mut` over
/// `parallel`, and a list worked out before what the loop's body reads, which `take` changes.
const PROGRAM: &str = "\
struct Unit:
    x: float
    health: float

def step(ref units: list[Unit], by: float):
    for ref unit in parallel(units):
        unit.x += by
        if unit.x > 5.0:
            unit.health -= 1.0

def take(ref state: list[int]) -> list[int]:
    state.append(9)
    [1, 2]

def show(stdout: Stdout, ref state: list[int]):
    for n in parallel(take(ref state)):
        stdout.print(n, state.len())

def main(stdout: Stdout):
    mut units = [Unit(float(n), health=10.0) for n in range(8)]
    step(ref units, 2.0)
    stdout.print([unit.x for unit in parallel(units)])
    stdout.print(sum(unit.health for unit in parallel(units)))
    mut state = [0]
    show(stdout, ref state)
";

#[test]
fn a_for_mut_over_parallel_and_its_kin_run_as_parallel_calls_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("ticks.fen");
    std::fs::write(&program, PROGRAM)?;
    let (source, executable) = (program.to_string_lossy().into_owned(), dir.join("ticks"));
    let built = fennec(&["build", "--llvm", &source, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
    let interpreted = fennec(&["run", "--interp", &source], &dir)?;
    // `fennec run` takes LLVM's dev build where it can, so it runs the same calls.
    let run = fennec(&["run", &source], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    // Prints inside `parallel` follow real call order, which can differ between runs, so they're
    // compared sorted (decisions.md): under `FENNEC_PARALLEL_STRESS` each item is a range of its own.
    let sorted = |out: &[u8]| {
        let mut lines: Vec<String> = String::from_utf8_lossy(out).lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    assert_eq!(sorted(&ran.stdout), sorted(&interpreted.stdout));
    assert_eq!(sorted(&run.stdout), sorted(&interpreted.stdout));
    // The `for mut`, the comprehension, the sum and the loop over `take`'s list: four calls, not
    // ordinary loops.
    let counts = String::from_utf8_lossy(&ran.stderr);
    assert!(counts.contains("parallel_calls 4"), "{counts}");
    Ok(())
}

/// A sum whose range fails partway, with enough items to be split into several ranges: the
/// program stops there, printing nothing more, as the interpreter does.
const FAILING: &str = "\
def main(stdout: Stdout):
    xs = [n for n in range(100_000)]
    stdout.print(sum(100 // (77_777 - x) for x in parallel(xs)) > 0)
    stdout.print(\"after\")
";

#[test]
fn a_range_that_fails_stops_the_program_natively_as_the_interpreter_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-fails-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("fails.fen");
    std::fs::write(&program, FAILING)?;
    let (source, executable) = (program.to_string_lossy().into_owned(), dir.join("fails"));
    let built = fennec(&["build", "--llvm", &source, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    let interpreted = fennec(&["run", "--interp", &source], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(!ran.status.success());
    assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    assert_eq!(String::from_utf8_lossy(&ran.stderr), String::from_utf8_lossy(&interpreted.stderr));
    Ok(())
}

/// A `for mut` over structs holding strings long enough to be blocks, and one over such strings,
/// changing some items and putting the rest back as they were. A struct's string is put back
/// held by nothing but the list, so letting go of the old item first freed it.
const PUT_BACK: &str = include_str!("fixtures/workers/put_back.fen");

#[test]
fn items_put_back_unchanged_stay_whole_with_text_inline_or_not() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-put-back-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("words.fen");
    std::fs::write(&program, PUT_BACK)?;
    let source = program.to_string_lossy().into_owned();
    let interpreted = fennec(&["run", "--interp", &source], &dir)?;
    // Headers make a `str` item three words in place; without them it's one, a block's address.
    for inline in ["1", "0"] {
        let executable = dir.join(format!("words-{inline}"));
        let built = crate::executables::fennec_command()
            .args(["build", "--llvm", &source, "-o"])
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .env("FENNEC_INLINE_TEXT", inline)
            .output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        assert!(ran.status.success(), "inline text {inline}: {}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Enough units to be split among workers: a `for mut` and a sum over them.
const ON_WORKERS: &str = include_str!("fixtures/workers/on_workers.fen");

/// A range on a worker that recurses deeper than the worker's stack allows. One item goes deep,
/// late in the range so a worker other than the first takes it, and the rest are quick: with
/// every item going deeper than the last, the workers made billions of calls before one was too
/// deep, and the test took 39 seconds.
const TOO_DEEP: &str = "\
def deep(n: int) -> int:
    if n == 0:
        return 0
    1 + deep(n - 1)

def main(stdout: Stdout):
    xs = [n for n in range(50_000)]
    stdout.print(sum(deep(1_000_000_000 if x == 40_000 else x % 10) for x in parallel(xs)))
    stdout.print(\"after\")
";

/// Builds `source` with the runtime with threads into `dir`, giving back the executable.
fn build_on_threads(dir: &Path, source: &str, name: &str) -> std::io::Result<std::path::PathBuf> {
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let executable = dir.join(name);
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_HOME", dir)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    Ok(executable)
}

#[test]
fn ranges_run_on_the_runtime_s_workers_each_with_its_own_stack_limit_and_failure_flag() -> std::io::Result<()>
{
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-threads-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let (workers, deep) =
        (build_on_threads(&dir, ON_WORKERS, "workers")?, build_on_threads(&dir, TOO_DEEP, "deep")?);
    let on = |executable: &Path| Command::new(executable).env("FENNEC_THREADS", "4").output_within();
    let (ran, too_deep) = (on(&workers)?, on(&deep)?);
    let source = |name: &str| dir.join(format!("{name}.fen")).to_string_lossy().into_owned();
    let interpreted = fennec(&["run", "--interp", &source("workers")], &dir)?;
    let interpreted_deep = fennec(&["run", "--interp", &source("deep")], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    // Too deep on a worker is the same failure at the same place, not a crash or a pass.
    assert!(!too_deep.status.success());
    assert_eq!(String::from_utf8_lossy(&too_deep.stdout), String::from_utf8_lossy(&interpreted_deep.stdout));
    assert_eq!(String::from_utf8_lossy(&too_deep.stderr), String::from_utf8_lossy(&interpreted_deep.stderr));
    Ok(())
}

/// A function `parallel` programs share with one that doesn't use it.
const SHARED: &str = "\
def half(n: int) -> int:
    n // 2
";

#[test]
fn a_parallel_program_built_after_a_plain_one_shares_no_cached_module_that_disagrees() -> std::io::Result<()>
{
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-cache-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let plain = format!("{SHARED}\ndef main(stdout: Stdout):\n    stdout.print(half(10))\n");
    let threaded = format!(
        "{SHARED}\ndef main(stdout: Stdout):\n    xs = [n for n in range(20_000)]\n    stdout.print(sum(half(x) for x in parallel(xs)))\n"
    );
    // The plain one warms the cache with `half`, declaring plain globals.
    let first = build_on_threads(&dir, &plain, "plain")?;
    let second = build_on_threads(&dir, &threaded, "threaded")?;
    let (ran_first, ran_second) =
        (Command::new(&first).output_within()?, Command::new(&second).output_within()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran_first.stdout), "5\n");
    assert_eq!(String::from_utf8_lossy(&ran_second.stdout), "99990000\n");
    Ok(())
}

/// Runtime calls in bodies on workers, over blocks the calling thread made, in every way a body
/// can use them: strings and dicts passed to the runtime, things kept past the call, a closure
/// made outside, a lent list copied before it's changed, `for mut` replacing strings, and
/// `parallel` inside `parallel`.
const LENT_BLOCKS: &str = include_str!("fixtures/workers/lent_blocks.fen");

#[test]
fn bodies_on_workers_use_the_calling_thread_s_blocks_as_the_interpreter_does() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-lent-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let lent = build_on_threads(&dir, LENT_BLOCKS, "lent")?;
    let source = dir.join("lent.fen").to_string_lossy().into_owned();
    let interpreted = fennec(&["run", "--interp", &source], &dir)?;
    for threads in ["1", "2", "4", "8"] {
        let ran = Command::new(&lent).env("FENNEC_THREADS", threads).output_within()?;
        assert!(ran.status.success(), "{threads} threads: {}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&interpreted.stdout),
            "{threads} threads"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A body that prints twice, each print in pieces, on enough items to run on workers, with text
/// long enough that workers write many times during the loop.
const PRINTS: &str = include_str!("fixtures/workers/prints.fen");

#[test]
fn what_bodies_print_on_workers_all_comes_out_whole_and_in_order_within_each_body() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-prints-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let prints = build_on_threads(&dir, PRINTS, "prints")?;
    let source = dir.join("prints.fen").to_string_lossy().into_owned();
    let interpreted = fennec(&["run", "--interp", &source], &dir)?;
    let ran = Command::new(&prints).env("FENNEC_THREADS", "8").output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    // Bodies run in any order and print in real call order, so this compares lines, sorted: none
    // lost and none mixed with another.
    let text = String::from_utf8_lossy(&ran.stdout);
    let sorted = |text: &str| {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    assert_eq!(sorted(&text), sorted(&String::from_utf8_lossy(&interpreted.stdout)));
    // What's printed before and after the loop comes before and after all of the loop's output.
    assert!(text.starts_with("before\n"), "{}", text.lines().next().unwrap_or_default());
    assert!(text.ends_with("after\n"), "{}", text.lines().next_back().unwrap_or_default());
    // Each body's first print comes before its second.
    let mut seen = std::collections::HashSet::new();
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("first", rest)) => {
                seen.insert(rest);
            }
            Some(("second", rest)) => assert!(seen.contains(rest), "second before first: {line}"),
            _ => {}
        }
    }
    Ok(())
}

/// A program that fails with an error nothing handles, having printed lines just before: in a
/// body on workers, or on the main thread.
const FAILS_ON_A_WORKER: &str = "\
def main(stdout: Stdout):
    stdout.print(\"before the loop\")
    xs = [n for n in range(100_000)]
    for x in parallel(xs):
        stdout.print(\"line\", x)
        stdout.print(10 // (77_777 - x))
    stdout.print(\"after the loop\")
";

const FAILS_ON_THE_MAIN_THREAD: &str = "\
def main(stdout: Stdout):
    xs = [1, 0]
    stdout.print(\"printed just before\")
    stdout.print(10 // xs[1])
";

#[test]
fn what_was_printed_before_a_failure_comes_out() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-failing-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let on_worker = build_on_threads(&dir, FAILS_ON_A_WORKER, "on_worker")?;
    let on_main = build_on_threads(&dir, FAILS_ON_THE_MAIN_THREAD, "on_main")?;
    let (worker, main) = (
        Command::new(&on_worker).env("FENNEC_THREADS", "4").output_within()?,
        Command::new(&on_main).output_within()?,
    );
    std::fs::remove_dir_all(&dir)?;
    assert!(!worker.status.success());
    let text = String::from_utf8_lossy(&worker.stdout);
    assert!(text.starts_with("before the loop\n"), "{text}");
    assert!(text.lines().any(|line| line == "line 77777"), "the failing body's line is missing");
    assert!(!text.contains("after the loop"), "ran on past the failure");
    assert!(String::from_utf8_lossy(&worker.stderr).contains("division by zero"));
    assert!(!main.status.success());
    assert_eq!(String::from_utf8_lossy(&main.stdout), "printed just before\n");
    assert!(String::from_utf8_lossy(&main.stderr).contains("division by zero"));
    Ok(())
}

/// Each character read from a string has a block for the whole run, made by the first thread to
/// read it. A worker's first read of one, and every later read in that call, has to find it.
const FIRST_READS: &str = include_str!("fixtures/workers/first_reads.fen");

#[test]
fn a_character_first_read_on_a_worker_is_found_by_every_range() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-first-reads-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let reads = build_on_threads(&dir, FIRST_READS, "reads")?;
    let ran = Command::new(&reads).env("FENNEC_THREADS", "4").output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "6000 6000\n6000 6000\n");
    Ok(())
}

/// `parallel` bodies that read the program's arguments and its standard input.
const INPUTS: &str = "\
def main(stdout: Stdout, args: Args, stdin: Stdin):
    mut counts = [0, 0, 0, 0, 0, 0, 0, 0]
    for ref count in parallel(counts):
        count = args.len()
    stdout.print(counts)
    mut lines = [0, 0, 0, 0]
    for ref count in parallel(lines):
        count = stdin.lines().len()
    stdout.print(sum(lines))
";

#[test]
fn every_worker_sees_the_arguments_and_reads_the_one_standard_input() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-inputs-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let inputs = build_on_threads(&dir, INPUTS, "inputs")?;
    let given = "a line\n".repeat(1000);
    let ran = Command::new(&inputs)
        .args(["one", "two"])
        .env("FENNEC_PARALLEL_STRESS", "1")
        .env("FENNEC_THREADS", "4")
        .output_given(given.as_bytes(), crate::executables::PROGRAM_DEADLINE)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "[2, 2, 2, 2, 2, 2, 2, 2]\n1000\n");
    Ok(())
}

/// A body on workers over text that isn't ASCII, in the ways a program reads text.
const NON_ASCII: &str = include_str!("fixtures/workers/non_ascii.fen");

/// A worker printing and making the text of values whose types write their own.
const OWN_TEXT: &str = include_str!("fixtures/workers/own_text.fen");

/// What a program printed, its lines sorted: bodies run and print in any order.
fn sorted_lines(text: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(text).lines().map(str::to_owned).collect();
    lines.sort();
    lines
}

/// `source`, built with the runtime that has threads, against the interpreter on every thread
/// count, each item on a worker in a range of its own.
fn matches_the_interpreter_on_workers(dir: &Path, name: &str, source: &str) -> std::io::Result<()> {
    let built = build_on_threads(dir, source, name)?;
    let path = dir.join(format!("{name}.fen")).to_string_lossy().into_owned();
    let interpreted = fennec(&["run", "--interp", &path], dir)?;
    for threads in ["1", "2", "4", "8"] {
        let ran = Command::new(&built)
            .env("FENNEC_THREADS", threads)
            .env("FENNEC_PARALLEL_STRESS", "1")
            .output_within()?;
        assert_eq!(ran.status.code(), interpreted.status.code(), "{name} on {threads} threads");
        assert_eq!(
            sorted_lines(&ran.stdout),
            sorted_lines(&interpreted.stdout),
            "{name} on {threads} threads"
        );
    }
    Ok(())
}

/// `shared` values changed from every worker come out as if the bodies ran one after another.
#[test]
fn workers_take_turns_at_a_shared_value() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-counters-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let source = include_str!("fixtures/workers/shared_counters.fen");
    matches_the_interpreter_on_workers(&dir, "shared_counters", source)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// `shared` values holding strings, lists, dicts and structs, changed from every worker.
#[test]
fn workers_take_turns_at_shared_values_holding_blocks() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-shared-blocks-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let source = include_str!("fixtures/workers/shared_blocks.fen");
    matches_the_interpreter_on_workers(&dir, "shared_blocks", source)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn a_worker_failing_under_a_lock_ends_the_program() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-failing-under-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    for (name, fields) in [("numbers", ""), ("blocks", "    names = [\"x\"]\n")] {
        let source = format!(
            "shared Counter:\n    total = 0\n{fields}\n\
             def main(stdout: Stdout):\n    one = Counter()\n    \
             mut items = [n for n in range(50_000)]\n    for ref item in parallel(items):\n        \
             with one as ref x:\n            x.total = 100 // (item - 25_000)\n        item += 1\n    \
             stdout.print(items.len())\n"
        );
        let built = build_on_threads(&dir, &source, name)?;
        for threads in ["1", "4"] {
            let ran = Command::new(&built).env("FENNEC_THREADS", threads).output_within()?;
            assert_eq!(ran.status.code(), Some(1), "{name} on {threads} threads");
            let said = String::from_utf8_lossy(&ran.stderr);
            assert!(said.contains("error:"), "{name} on {threads} threads: {said}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A body changing, through places, its own copy of what the caller holds: each block on the way
/// is the caller's until it's copied, from the root down, and the caller's stays as it was.
const CHANGED_PLACES: &str = include_str!("fixtures/workers/places.fen");

/// The same with built-ins and waiting to take a place out: a lent item and a lent grid's, changed
/// through methods, `append` reading the root, `extend`, `pop`, `insert`, and given what came from them.
const LENT_PLACES: &str = include_str!("fixtures/workers/places_lent.fen");

/// Enums and structs in place holding lists, read by workers from what the caller holds, which
/// retains those lists through the checks for who owns them: a tree holding itself through a list
/// with a long string inside, a copy of its list appended to, a struct in a list holding a tree,
/// and a struct's list changed by a `for mut` body.
const THROUGH_LISTS: &str = include_str!("fixtures/workers/through_lists.fen");

/// A map walked with no list made: captured by a body, where every thread reads the caller's, and
/// changed and walked by a `for mut` body, which has a copy of its own.
const WALKS: &str = include_str!("fixtures/workers/walks.fen");

/// A server: what's captured is read from the thread that's waiting, each connection answers until
/// the other side is done, and how a task fails ends only that task.
const SERVER: &str = "\
from std.net import Connection, NetError

def deep(n: int) -> int:
    if n == 0:
        return 0
    # Held across the call, so the call stays a call.
    kept = [n]
    deep(n - 1) + 1 + kept[0] - n

def waited(connection: Connection) -> str:
    match connection.read_line(timeout=20ms):
        case Err(error):
            error.message()
        case Ok(_):
            \"heard something\"

def answer(connection: Connection, greeting: str, marks: list[str]) -> int fails NetError:
    connection.write(greeting + \"\\n\")?
    mut count = 0
    for line in connection.lines():
        if line == \"divide\":
            connection.write(\"dividing\\n\")?
            count += 1 // (count - count)
        elif line == \"deep\":
            connection.write(f\"{deep(2000)}\\n\")?
        elif line == \"too deep\":
            connection.write(\"going deep\\n\")?
            connection.write(f\"{deep(100000000)}\\n\")?
        elif line == \"wait\":
            connection.write(waited(connection) + \"\\n\")?
        else:
            connection.write(line + marks[0] + \"\\n\")?
        count += 1
    count

def main(net: Net, stdout: Stdout):
    greeting = \"hello from \" + str(1 + 1)
    marks = [\"!\"]
    server = net.listen(0).expect(\"it listens\")
    stdout.print(server.port)
    for connection in parallel(server):
        match answer(connection, greeting, marks):
            case Ok(count):
                stdout.print(\"answered\", count, connection.peer != \"\")
            case Err(error):
                stdout.print(\"failed\", error.message())
";

/// One client's whole talk with a server: each line sent, what came back, and the end.
fn talk(port: u16, lines: &[&str]) -> std::io::Result<Vec<String>> {
    use std::io::{BufRead, BufReader, Write};
    let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut read = || -> std::io::Result<String> {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(line.trim_end().to_owned())
    };
    let mut heard = vec![read()?];
    for line in lines {
        writeln!(&stream, "{line}")?;
        heard.push(read()?);
    }
    Ok(heard)
}

#[test]
fn a_native_server_runs_each_connection_as_its_own_task() -> std::io::Result<()> {
    use std::io::{BufRead, BufReader};
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-server-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let executable = build_on_threads(&dir, SERVER, "server")?;
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_THREADS", "4")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    );
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let greeting = "hello from 2";
    let clients: Vec<_> = (0..50)
        .map(|at| {
            std::thread::spawn(move || {
                talk(port, &[&format!("line {at}"), "deep", "wait", &format!("again {at}")])
            })
        })
        .collect();
    for (at, client) in clients.into_iter().enumerate() {
        let heard = client.join().map_err(|_| std::io::ErrorKind::Other)??;
        let line = format!("line {at}!");
        let again = format!("again {at}!");
        assert_eq!(heard, [greeting, &line, "2000", "gave up after 0.02s", &again]);
    }
    // A failure ends only its own task, running out of stack too, and the server goes on.
    assert_eq!(talk(port, &["divide"])?, [greeting, "dividing"]);
    assert_eq!(talk(port, &["too deep"])?, [greeting, "going deep"]);
    assert_eq!(talk(port, &["after"])?, [greeting, "after!"]);
    // Each says why, once it gets there.
    let (says, said) = std::sync::mpsc::channel();
    let errors = BufReader::new(server.0.stderr.take().ok_or(std::io::ErrorKind::BrokenPipe)?);
    std::thread::spawn(move || errors.lines().map_while(Result::ok).try_for_each(|line| says.send(line)));
    let wait = std::time::Duration::from_secs(20);
    let mut lines = vec![said.recv_timeout(wait), said.recv_timeout(wait)];
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    lines.sort_by_key(|line| line.as_ref().is_ok_and(|line| line.contains("nested")));
    let wanted = ["error: division by zero".to_owned(), "error: calls nested too deeply".to_owned()];
    assert_eq!(lines, wanted.map(Ok), "{lines:?}");
    Ok(())
}

/// A server whose connections share a counter: one fails inside a `with` on it.
const SERVER_HOLDING: &str = "\
from std.net import Connection, NetError

shared Counter:
    total = 0

def answer(connection: Connection, counter: Counter) -> int fails NetError:
    connection.write(\"ready\\n\")?
    for line in connection.lines():
        if line == \"fail\":
            with counter as ref held:
                held.total = 1 // (held.total - held.total)
        else:
            with counter as ref held:
                held.total += 1
            connection.write(f\"{counter.total}\\n\")?
    0

def main(net: Net, stdout: Stdout):
    counter = Counter()
    server = net.listen(0).expect(\"it listens\")
    stdout.print(server.port)
    for connection in parallel(server):
        match answer(connection, counter):
            case Ok(_):
                stdout.print(\"answered\")
            case Err(error):
                stdout.print(\"failed\", error.message())
";

/// A connection's task that fails while it holds a lock ends the whole program, rather than only
/// itself: what the lock guards may be half changed, and other tasks may be waiting for it.
#[test]
fn a_server_task_failing_under_a_lock_ends_the_program() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-server-holding-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let executable = build_on_threads(&dir, SERVER_HOLDING, "server")?;
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_THREADS", "4")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    );
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    assert_eq!(talk(port, &["count"])?, ["ready", "1"]);
    let failing = std::thread::spawn(move || talk(port, &["fail"]));
    let _ = failing.join();
    // It ends by itself: nothing here kills it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = server.0.try_wait()? {
            break status;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "the server kept going after a task failed holding a lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut said = String::new();
    std::io::Read::read_to_string(
        &mut server.0.stderr.take().ok_or(std::io::ErrorKind::BrokenPipe)?,
        &mut said,
    )?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(status.code(), Some(1));
    assert!(said.contains("division by zero"), "{said}");
    Ok(())
}

#[test]
fn every_parallel_program_matches_the_interpreter_on_real_threads() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-stress-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let programs = [
        ("non_ascii", NON_ASCII),
        ("lent", LENT_BLOCKS),
        ("prints", PRINTS),
        ("own_text", OWN_TEXT),
        ("places", CHANGED_PLACES),
        ("places_lent", LENT_PLACES),
        ("through_lists", THROUGH_LISTS),
        ("walks", WALKS),
    ];
    for (name, source) in programs {
        matches_the_interpreter_on_workers(&dir, name, source)?;
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut covered = Vec::new();
    for entry in std::fs::read_dir(examples)? {
        let path = entry?.path();
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        // A server's example needs a network, and is covered by what it calls.
        if source.contains("parallel(") && !source.contains("Net") {
            matches_the_interpreter_on_workers(&dir, &name, &source)?;
            covered.push(name);
        }
    }
    std::fs::remove_dir_all(&dir)?;
    assert!(covered.len() >= 3, "only {covered:?} use `parallel`");
    Ok(())
}

/// Strings of one size with their characters in different places, let go of between calls, so a
/// later one can have the address of an earlier one. A worker reading the caller's string keeps
/// where it got to, and has to forget that when the range ends.
const CHARACTER_PLACES: &str = include_str!("fixtures/workers/character_places.fen");

#[test]
fn a_worker_forgets_where_it_got_to_in_a_string_when_its_range_ends() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-places-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let places = build_on_threads(&dir, CHARACTER_PLACES, "places")?;
    let ran = Command::new(&places).env("FENNEC_THREADS", "4").output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "373332\n");
    Ok(())
}

/// A constant read on workers is worked out once, on the calling thread, before any of them starts.
/// It holds a string too long to fit in a header and a list, read in a body and passed to a runtime
/// call, so counting on a worker has blocks of the caller's to count.
#[test]
fn workers_read_a_constant_the_calling_thread_worked_out() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("fennec-parallel-constants-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let source = "LONG = f\"{1 + 1} and then a string too long to fit in a header\"\n\
                  WORDS = [\"a\", \"b\", LONG]\n\n\
                  def main(stdout: Stdout):\n    mut counts = [at for at in range(2000)]\n    for ref count in parallel(counts):\n        \
                  count = LONG.len() + WORDS.len() + WORDS[2].upper().len()\n    \
                  stdout.print(sum(counts), LONG, WORDS.len())\n    mut mine = WORDS\n    mine.append(\"mine\")\n    \
                  stdout.print(mine.len(), WORDS.len())\n";
    let executable = build_on_threads(&dir, source, "constants")?;
    let ran = Command::new(&executable).env("FENNEC_THREADS", "4").output_within()?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    let interpreted = crate::executables::fennec_command()
        .args(["run", "--interp"])
        .arg(dir.join("constants.fen"))
        .env("FENNEC_HOME", &dir)
        .output_before(FENNEC_DEADLINE)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    assert!(ran.stdout.starts_with(b"1"), "{}", String::from_utf8_lossy(&ran.stdout));
    Ok(())
}
