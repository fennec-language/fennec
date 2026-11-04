//! `parallel` built natively through LLVM: its loops, comprehensions and sums run through the
//! runtime's parallel calls, as `FENNEC_COUNTS` shows, and print what the interpreter prints.
//! Skipped where LLVM's tools aren't installed, as `llvm.rs` is.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

use crate::executables::{runtime_built, threads_runtime_built};

fn fennec(args: &[&str], home: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command().args(args).env("FENNEC_HOME", home).output()
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

def step(mut units: list[Unit], by: float):
    for mut unit in parallel(units):
        unit.x += by
        if unit.x > 5.0:
            unit.health -= 1.0

def take(mut state: list[int]) -> list[int]:
    state.append(9)
    [1, 2]

def show(stdout: Stdout, mut state: list[int]):
    for n in parallel(take(mut state)):
        stdout.print(n, state.len())

def main(stdout: Stdout):
    mut units = [Unit(float(n), 10.0) for n in range(8)]
    step(mut units, 2.0)
    stdout.print([unit.x for unit in parallel(units)])
    stdout.print(sum(unit.health for unit in parallel(units)))
    mut state = [0]
    show(stdout, mut state)
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
    let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output()?;
    let interpreted = fennec(&["run", "--interp", &source], &dir)?;
    // `fennec run` takes LLVM's dev build where it can, so it runs the same calls.
    let run = fennec(&["run", &source], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    assert_eq!(String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&interpreted.stdout));
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
    let ran = Command::new(&executable).output()?;
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
            .output()?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output()?;
        assert!(ran.status.success(), "inline text {inline}: {}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Enough units to be split among workers: a `for mut` and a sum over them.
const ON_WORKERS: &str = include_str!("fixtures/workers/on_workers.fen");

/// A range on a worker that recurses deeper than the worker's stack allows.
const TOO_DEEP: &str = "\
def deep(n: int) -> int:
    if n == 0:
        return 0
    1 + deep(n - 1)

def main(stdout: Stdout):
    xs = [n for n in range(50_000)]
    stdout.print(sum(deep(x * 1_000) for x in parallel(xs)))
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
        .output()?;
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
    let on = |executable: &Path| Command::new(executable).env("FENNEC_THREADS", "4").output();
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
    let (ran_first, ran_second) = (Command::new(&first).output()?, Command::new(&second).output()?);
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
        let ran = Command::new(&lent).env("FENNEC_THREADS", threads).output()?;
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
    let ran = Command::new(&prints).env("FENNEC_THREADS", "8").output()?;
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
    let (worker, main) =
        (Command::new(&on_worker).env("FENNEC_THREADS", "4").output()?, Command::new(&on_main).output()?);
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
    let ran = Command::new(&reads).env("FENNEC_THREADS", "4").output()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "6000 6000\n6000 6000\n");
    Ok(())
}

/// `parallel` bodies that read the program's arguments and its standard input.
const INPUTS: &str = "\
def main(stdout: Stdout, args: Args, stdin: Stdin):
    mut counts = [0, 0, 0, 0, 0, 0, 0, 0]
    for mut count in parallel(counts):
        count = args.len()
    stdout.print(counts)
    mut lines = [0, 0, 0, 0]
    for mut count in parallel(lines):
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
    let mut running = Command::new(&inputs)
        .args(["one", "two"])
        .env("FENNEC_PARALLEL_STRESS", "1")
        .env("FENNEC_THREADS", "4")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let given = "a line\n".repeat(1000);
    running
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("no standard input"))?
        .write_all(given.as_bytes())?;
    let ran = running.wait_with_output()?;
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
            .output()?;
        assert_eq!(ran.status.code(), interpreted.status.code(), "{name} on {threads} threads");
        assert_eq!(
            sorted_lines(&ran.stdout),
            sorted_lines(&interpreted.stdout),
            "{name} on {threads} threads"
        );
    }
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
    let ran = Command::new(&places).env("FENNEC_THREADS", "4").output()?;
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
                  def main(stdout: Stdout):\n    mut counts = [at for at in range(2000)]\n    for mut count in parallel(counts):\n        \
                  count = LONG.len() + WORDS.len() + WORDS[2].upper().len()\n    \
                  stdout.print(sum(counts), LONG, WORDS.len())\n    mut mine = WORDS\n    mine.append(\"mine\")\n    \
                  stdout.print(mine.len(), WORDS.len())\n";
    let executable = build_on_threads(&dir, source, "constants")?;
    let ran = Command::new(&executable).env("FENNEC_THREADS", "4").output()?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    let interpreted = crate::executables::fennec_command()
        .args(["run", "--interp"])
        .arg(dir.join("constants.fen"))
        .env("FENNEC_HOME", &dir)
        .output()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    assert!(ran.stdout.starts_with(b"1"), "{}", String::from_utf8_lossy(&ran.stdout));
    Ok(())
}
