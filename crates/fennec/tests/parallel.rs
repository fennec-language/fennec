//! `parallel` built natively through LLVM: its loops, comprehensions and sums run through the
//! runtime's parallel calls, as `FENNEC_COUNTS` shows, and print what the interpreter prints.
//! Skipped where LLVM's tools aren't installed, as `llvm.rs` is.

use std::path::Path;
use std::process::{Command, Output};

use crate::executables::runtime_built;

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
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&interpreted.stdout));
    // The `for mut`, the comprehension, the sum and the loop over `take`'s list: four calls, not
    // ordinary loops.
    let counts = String::from_utf8_lossy(&ran.stderr);
    assert!(counts.contains("parallel_calls 4"), "{counts}");
    Ok(())
}
