//! `parallel` built natively through LLVM: its loops, comprehensions and sums run through the
//! runtime's parallel calls, as `FENNEC_COUNTS` shows, and print what the interpreter prints.
//! Skipped where LLVM's tools aren't installed, as `llvm.rs` is.

use std::path::Path;
use std::process::{Command, Output};

use crate::executables::{runtime_built, threads_runtime_built};

fn fennec(args: &[&str], home: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command().args(args).env("FENNEC_HOME", home).output()
}

fn ready() -> bool {
    !cfg!(windows)
        && std::env::var_os("FENNEC_UNDER_MUTANTS").is_none()
        && fennec_llvm::Tools::find().is_ok()
        && runtime_built()
        // Coverage builds `fennec` in a folder of its own, where no runtime library is built.
        && Path::new(env!("CARGO_BIN_EXE_fennec")).with_file_name("libfennec_runtime.a").is_file()
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
const PUT_BACK: &str = "\
struct Tag:
    name: str
    n: int

def main(stdout: Stdout):
    mut tags = [Tag(f\"a name long enough to be held in a block of its own, {n}\", n) for n in range(5_000)]
    for mut tag in parallel(tags):
        if tag.n % 2 == 0:
            tag.n += 1
    stdout.print(tags[0].name, tags[1].name, tags[4_999].name, tags[2].n)
    mut words = [f\"a word long enough to be held in a block of its own, {n}\" for n in range(5_000)]
    for mut word in parallel(words):
        if word.len() % 2 == 0:
            word = word + \"!\"
    stdout.print(words[0], words[1], words[4_999])
    stdout.print(sum(word.len() for word in parallel(words)))
";

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
const ON_WORKERS: &str = "\
struct Unit:
    x: float
    health: float

def main(stdout: Stdout):
    mut units = [Unit(float(n % 100), 10.0) for n in range(50_000)]
    for mut unit in parallel(units):
        unit.x += 2.0
        if unit.x > 50.0:
            unit.health -= 1.0
    stdout.print(sum(unit.health for unit in parallel(units)))
";

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
const LENT_BLOCKS: &str = "\
struct Tag:
    name: str
    n: int

def shout(word: str) -> str:
    word + \"!\"

def over(f: (str) -> str, words: list[str]) -> list[str]:
    [f(word) for word in parallel(words)]

def grown(base: list[int], n: int) -> int:
    mut copy = base
    copy.append(n)
    copy.len()

def main(stdout: Stdout):
    words = [f\"a word long enough to be held in a block of its own, {n}\" for n in range(20_000)]
    longer = [word + \"!\" for word in parallel(words)]
    stdout.print(longer[0], longer[19_999], words[7])
    called = [shout(word) for word in parallel(words)]
    stdout.print(called[1], sum(word.len() for word in parallel(called)))
    same = [word for word in parallel(words)]
    stdout.print(same[3], same[19_998])
    counts = {word: n for n, word in enumerate(words)}
    stdout.print(sum(counts[word] for word in parallel(words)))
    suffix = \"? and long enough to be a block of its own\"
    asked = over(word => word + suffix, words)
    stdout.print(asked[11])
    pairs = [[word, word] for word in parallel(words)]
    stdout.print(pairs[5][1], pairs.len())
    base = [1, 2, 3]
    numbers = [n for n in range(20_000)]
    stdout.print(sum(grown(base, n) for n in parallel(numbers)), base.len())
    mut tags = [Tag(f\"a name long enough to be held in a block of its own, {n}\", n) for n in range(20_000)]
    for mut tag in parallel(tags):
        if tag.n % 2 == 0:
            tag.name = tag.name + \"*\"
        tag.n += 1
    stdout.print(tags[0].name, tags[1].name, tags[19_999].n)
    grids = [[n * m for m in range(100)] for n in range(2_000)]
    stdout.print(sum(sum(cell for cell in parallel(row)) for row in parallel(grids)))
";

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
const PRINTS: &str = "\
def main(stdout: Stdout):
    words = [f\"text long enough to be held in a block of its own, {n}\" for n in range(50_000)]
    stdout.print(\"before\")
    for word in parallel(words):
        stdout.print(\"first\", word, word.len())
        stdout.print(\"second\", word, word.len())
    stdout.print(\"after\")
";

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
