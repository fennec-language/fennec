//! What's known of list lengths across calls: what's found on the shapes the kernels have, that
//! each way a length can change drops what was known, and that every fact holds while every
//! example and kernel runs in the IR's evaluator.

use std::fs;
use std::path::{Path, PathBuf};

use fennec_ir::eval::check_lengths;
use fennec_ir::lengths::{Length, Lengths};
use fennec_ir::{Program, lower, validate};

fn lowered(source: &str) -> Result<Program, String> {
    let mut module = fennec_syntax::parse(source).map_err(|problem| format!("{problem:?}\n{source}"))?;
    let checked =
        fennec_check::check_and_resolve(&mut module).map_err(|problems| format!("{problems:?}\n{source}"))?;
    let program = lower(&module, &checked);
    validate(&program).map_err(|problems| format!("{problems:?}\n{source}"))?;
    Ok(program)
}

/// What's known of each parameter of a function.
fn known(program: &Program, function: &str) -> Result<Vec<Option<Length>>, String> {
    let lengths = Lengths::of(program);
    let found = program.functions.iter().find(|found| found.name == function);
    let found = found.ok_or_else(|| format!("`{function}` wasn't lowered:\n{program}"))?;
    Ok((0..found.params).map(|at| lengths.param(function, at)).collect())
}

/// Runs `main`, checking every fact, and says how many checks were made.
fn checked(program: &Program) -> u64 {
    let lengths = Lengths::of(program);
    let ran = check_lengths(program, &lengths, 5_000_000);
    assert_eq!(ran.broken, None, "{program}");
    ran.checked
}

/// Lists filled in one loop, as physics fills its positions, velocities and radii, of a length
/// only known when it runs.
const FILLED: &str = "struct V:
    x: float

def step(ref ps: list[V], vs: list[V], rs: list[float]):
    for i in range(len(ps)):
        ps[i] = V(x=ps[i].x + vs[i].x * rs[i])
    _ = reach(ps, rs)
    return

def reach(ps: list[V], rs: list[float]) -> float:
    mut far = 0.0
    for i in range(len(ps)):
        far = max(far, ps[i].x + rs[i])
    return far

def run(n: int) -> float:
    mut ps = list[V]()
    mut vs = list[V]()
    mut rs = list[float]()
    for b in range(n):
        ps.append(V(x=float(b)))
        if b % 2 == 0:
            vs.append(V(x=1.0))
        else:
            vs.append(V(x=-1.0))
        rs.append(0.5)
    for _ in range(3):
        step(ref ps, vs, rs)
    return reach(ps, rs)

def main(stdout: Stdout):
    stdout.print(run(40))
    return
";

#[test]
fn lists_filled_together_are_as_long_as_each_other_wherever_theyre_handed() {
    let program = lowered(FILLED).unwrap();
    assert_eq!(known(&program, "step").unwrap(), [None, Some(Length::Same(0)), Some(Length::Same(0))]);
    assert_eq!(known(&program, "reach").unwrap(), [None, Some(Length::Same(0))]);
    assert_eq!(known(&program, "run").unwrap(), [None]);
    assert_eq!(known(&program, "main").unwrap(), [None]);
    assert!(checked(&program) > 0);
}

#[test]
fn a_list_of_a_known_size_is_as_long_as_that_wherever_its_handed() {
    let source = "def grid() -> list[int]:
    return [0 for _ in range(64)]

def doubled(xs: list[int]) -> list[int]:
    return [x * 2 for x in xs]

def sum_at(cells: list[int], starts: list[int], twice: list[int], three: list[int]) -> int:
    mut total = 0
    for at in range(len(cells)):
        total += starts[at] + twice[at] + three[at]
    return total

def main(stdout: Stdout):
    cells = grid()
    mut starts = [1 for _ in range(SIDE * SIDE)]
    starts[3] = 7
    stdout.print(sum_at(cells, starts, doubled(cells), [1, 2, 3]), len(doubled([1, 2])))
    return

SIDE = 8
";
    let program = lowered(source).unwrap();
    let lengths = Lengths::of(&program);
    assert_eq!(lengths.returned("grid"), Some(Length::Fixed(64)));
    assert_eq!(lengths.returned("doubled"), Some(Length::Same(0)));
    assert_eq!(
        known(&program, "sum_at").unwrap(),
        [Some(Length::Fixed(64)), Some(Length::Fixed(64)), Some(Length::Fixed(64)), Some(Length::Fixed(3))]
    );
    // Handed lists of two lengths, `doubled` knows nothing of its own, but gives back one as long.
    assert_eq!(known(&program, "doubled").unwrap(), [None]);
    assert!(checked(&program) > 0);
}

/// Two lists as long as each other, then `change`, then both handed to `uses`.
fn after(change: &str) -> String {
    format!(
        "def uses(xs: list[int], ys: list[int]) -> int:
    mut total = 0
    for at in range(len(xs)):
        total += ys[at]
    return total

def grow(ref xs: list[int]):
    xs.append(1)
    return

def grows(ref xs: list[int]):
    grow(ref xs)
    return

def sorts(ref xs: list[int]):
    xs.sort()
    return

def run(n: int) -> int:
    mut xs = [0 for _ in range(n)]
    ys = [1 for _ in range(n)]
    mut extra = 0
    {change}
    return uses(xs, ys) + extra

def main(stdout: Stdout):
    stdout.print(run(6))
    return
"
    )
}

#[test]
fn each_way_a_length_can_change_drops_what_was_known() {
    for (kept, change) in [
        (true, "extra = 1"),
        (true, "xs[0] = 5"),
        (true, "xs.sort()"),
        (true, "sorts(ref xs)"),
        (false, "xs.append(7)"),
        (false, "extra = xs.pop() else 0"),
        (false, "xs.clear()"),
        (false, "xs.insert(0, 9)"),
        (false, "xs.remove(0)"),
        (false, "xs.remove_where((x) => x > 2)"),
        (false, "xs = xs + ys"),
        (false, "xs += [1]"),
        (false, "grow(ref xs)"),
        (false, "grows(ref xs)"),
        (false, "if extra == 0:\n        xs.append(1)"),
        (true, "for _ in range(extra):\n        xs.append(1)"),
        (false, "for _ in range(n - 2):\n        xs.append(1)"),
        (false, "while len(xs) < 9:\n        xs.append(1)"),
    ] {
        let program = lowered(&after(change)).unwrap();
        let should = if kept { Some(Length::Same(0)) } else { None };
        assert_eq!(known(&program, "uses").unwrap(), [None, should], "after `{change}`");
        checked(&program);
    }
}

#[test]
fn a_length_a_function_changes_isnt_known_inside_it() {
    let source = "def uses(xs: list[int], ref ys: list[int]) -> int:
    mut total = 0
    for at in range(len(xs)):
        total += xs[at]
    ys.append(total)
    return total

def keeps(ref xs: list[int], ys: list[int]) -> int:
    return len(xs) + len(ys)

def run(n: int) -> int:
    xs = [0 for _ in range(n)]
    mut ys = [1 for _ in range(n)]
    return uses(xs, ref ys) + keeps(ref ys, xs)

def main(stdout: Stdout):
    stdout.print(run(5))
    return
";
    let program = lowered(source).unwrap();
    assert_eq!(known(&program, "uses").unwrap(), [None, None]);
    // `ys` came back one longer from `uses`.
    assert_eq!(known(&program, "keeps").unwrap(), [None, None]);
    checked(&program);
}

#[test]
fn one_call_that_disagrees_drops_what_every_other_says() {
    let source = "def uses(xs: list[int], ys: list[int]) -> int:
    return len(xs) + len(ys)

def run(n: int) -> int:
    xs = [0 for _ in range(n)]
    ys = [1 for _ in range(n)]
    return uses(xs, ys) + uses(xs, ys) + uses(xs, xs + ys)

def main(stdout: Stdout):
    stdout.print(run(5))
    return
";
    let program = lowered(source).unwrap();
    assert_eq!(known(&program, "uses").unwrap(), [None, None]);
}

#[test]
fn what_a_closure_a_task_or_a_generator_is_handed_isnt_known() {
    let source = "from std.concurrent import Closed, Crashed

def uses(xs: list[int], ys: list[int]) -> int:
    mut total = 0
    for at in range(len(xs)):
        total += ys[at]
    return total

def handed(xs: list[int], ys: list[int]) -> Iterator[int]:
    yield uses(xs, ys)

def run(n: int) fails Closed | Crashed:
    xs = [0 for _ in range(n)]
    ys = [1 for _ in range(n)]
    later = () => uses(xs, ys)
    with launch(() => uses(xs, ys))?:
        pass
    for found in handed(xs, ys):
        assert found >= 0
    assert later() >= 0

def main(stdout: Stdout) fails Closed | Crashed:
    run(4)?
    stdout.print(1)
    return
";
    let program = lowered(source).unwrap();
    assert_eq!(known(&program, "uses").unwrap(), [None, None]);
    assert_eq!(known(&program, "handed").unwrap(), [None, None]);
}

/// Every `.fen` file directly in a folder.
fn files_in(folder: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "fen") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// How far each program runs: under a second or two each.
const STEPS: u64 = 30_000_000;

#[test]
fn every_fact_holds_while_the_kernels_and_examples_run() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = files_in(&root.join("bench/kernels")).unwrap();
    files.extend(files_in(&root.join("examples")).unwrap());
    let (mut with_facts, mut checks) = (0, 0);
    for path in files {
        let shown = path.display().to_string();
        let source = fs::read_to_string(&path).unwrap();
        let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
        let Ok(checked) = fennec_check::check_and_resolve(&mut module) else { continue };
        let program = lower(&module, &checked);
        let lengths = Lengths::of(&program);
        if lengths.params().next().is_none() {
            continue;
        }
        with_facts += 1;
        let ran = check_lengths(&program, &lengths, STEPS);
        assert_eq!(ran.broken, None, "{shown}");
        checks += ran.checked;
        if shown.contains("bench/kernels/physics") {
            assert!(ran.checked > 0, "physics checked no fact: {ran:?}");
        }
    }
    assert!(with_facts >= 10, "only {with_facts} programs had a fact");
    assert!(checks > 10_000, "only {checks} facts were checked");
}
