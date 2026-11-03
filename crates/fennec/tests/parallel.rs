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
        let built = Command::new(env!("CARGO_BIN_EXE_fennec"))
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
