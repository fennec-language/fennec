//! `parallel` built natively through LLVM: its loops, comprehensions and sums run through the
//! runtime's parallel calls, as `FENNEC_COUNTS` shows, and print what the interpreter prints.
//! Skipped where LLVM's tools aren't installed, as `llvm.rs` is.

use std::path::Path;
use std::process::{Command, Output};

use crate::executables::{runtime_built, threads_runtime_built};

fn fennec(args: &[&str], home: &Path) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec")).args(args).env("FENNEC_HOME", home).output()
}

fn ready() -> bool {
    !cfg!(windows)
        && std::env::var_os("FENNEC_UNDER_MUTANTS").is_none()
        && fennec_llvm::Tools::find().is_ok()
        && runtime_built()
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
    let built = Command::new(env!("CARGO_BIN_EXE_fennec"))
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
