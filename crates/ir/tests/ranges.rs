//! What's known of ints' and floats' values: what's found in physics, what each way a fact is made
//! or lost does on a small program, and that every fact holds while every example and kernel runs
//! in the IR's evaluator.

use std::fs;
use std::path::{Path, PathBuf};

use fennec_ir::eval::check_ranges;
use fennec_ir::ranges::{Place, Ranges, Unsettled};
use fennec_ir::{InstKind, Program, lower, validate};
use fennec_syntax::BinOp;

fn lowered(source: &str) -> Result<Program, String> {
    let mut module = fennec_syntax::parse(source).map_err(|problem| format!("{problem:?}\n{source}"))?;
    let checked =
        fennec_check::check_and_resolve(&mut module).map_err(|problems| format!("{problems:?}\n{source}"))?;
    let program = lower(&module, &checked);
    validate(&program).map_err(|problems| format!("{problems:?}\n{source}"))?;
    Ok(program)
}

/// The places in a function of the instructions `wanted` picks.
fn places(
    program: &Program,
    function: &str,
    wanted: &dyn Fn(&InstKind) -> bool,
) -> Result<Vec<Place>, String> {
    let found = program.functions.iter().find(|found| found.name == function);
    let found = found.ok_or_else(|| format!("`{function}` wasn't lowered:\n{program}"))?;
    let mut places = Vec::new();
    for (at, block) in found.blocks.iter().enumerate() {
        for (place, inst) in block.insts.iter().enumerate() {
            if wanted(&inst.kind) {
                places.push((at, place));
            }
        }
    }
    Ok(places)
}

fn is_division(kind: &InstKind) -> bool {
    matches!(kind, InstKind::Binary { op: BinOp::FloorDiv | BinOp::Mod, .. })
}

fn is_product(kind: &InstKind) -> bool {
    matches!(kind, InstKind::Binary { op: BinOp::Mul, .. })
}

fn is_index(kind: &InstKind) -> bool {
    matches!(kind, InstKind::GetIndex { .. } | InstKind::SetIndex { .. })
}

fn is_int(kind: &InstKind) -> bool {
    matches!(kind, InstKind::Builtin { name, .. } if name == "int")
}

fn is_comparison(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Binary {
            op: BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq,
            ..
        }
    )
}

/// For each instruction `wanted` picks in a function, whether its ints are known not negative.
fn not_negative(
    program: &Program,
    ranges: &Ranges,
    function: &str,
    wanted: &dyn Fn(&InstKind) -> bool,
) -> Result<Vec<bool>, String> {
    let known = ranges.function(function).ok_or_else(|| format!("no facts for `{function}`"))?;
    Ok(places(program, function, wanted)?.into_iter().map(|place| known.not_negative(place)).collect())
}

/// For each `//` or `%` in a function, whether it's known unsigned: both sides not negative.
fn unsigned(program: &Program, ranges: &Ranges, function: &str) -> Result<Vec<bool>, String> {
    not_negative(program, ranges, function, &is_division)
}

/// For each `int(x)` in a function, whether its float is known to fit.
fn fits(program: &Program, ranges: &Ranges, function: &str) -> Result<Vec<bool>, String> {
    let known = ranges.function(function).ok_or_else(|| format!("no facts for `{function}`"))?;
    Ok(places(program, function, &is_int)?.into_iter().map(|place| known.fits(place)).collect())
}

/// Runs `main`, checking every fact, and says how many checks were made.
fn checked(program: &Program, ranges: &Ranges) -> u64 {
    let ran = check_ranges(program, ranges, 5_000_000);
    assert_eq!(ran.broken, None, "{program}");
    ran.checked
}

/// Physics's three conversions in `cell_of` and four divisions in `collide` are proven, from
/// `kept` keeping every coordinate from 0.25 to 23.75 and never NaN, `integrate` setting every
/// position from it, and the radii being 0.25 to 0.45. Its comparisons' NaN tests aren't: what
/// `collide` pushes and kicks has no bound.
#[test]
fn physics_proves_its_conversions_and_divisions_and_not_its_nan_tests() {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/kernels/physics.fen"))
            .unwrap();
    let program = lowered(&source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "cell_of").unwrap(), [true, true, true]);
    assert_eq!(unsigned(&program, &ranges, "collide").unwrap(), [true, true, true, true]);
    for function in ["kept", "collide"] {
        let known = ranges.function(function).unwrap();
        let proven: Vec<Place> = places(&program, function, &is_comparison)
            .unwrap()
            .into_iter()
            .filter(|place| known.not_nan(*place))
            .collect();
        assert_eq!(proven, [], "{function}");
    }
    assert!(checked(&program, &ranges) > 1000);
}

/// A floor `%` or `//` is unsigned when every call gives a side that isn't negative, and not when
/// one call can give a negative one. A `%` by a positive number is from 0 to one less than it
/// either way, so a `//` of that is unsigned.
#[test]
fn a_division_is_unsigned_when_every_call_gives_what_isnt_negative() {
    let source = "def part(n: int) -> int:\n    return n % 7\n\n\
                  def whole(n: int) -> int:\n    return (n % 7) // 2\n\n\
                  def main(stdout: Stdout):\n    stdout.print(part(3), part(10), whole(-4))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(unsigned(&program, &ranges, "part").unwrap(), [true]);
    assert_eq!(unsigned(&program, &ranges, "whole").unwrap(), [false, true]);
    let negative = source.replace("part(10)", "part(-10)");
    let program = lowered(&negative).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(unsigned(&program, &ranges, "part").unwrap(), [false]);
    checked(&program, &ranges);
}

/// A list's index known not negative, read or set straight into the list, and an int `*` of two
/// sides known not negative are noted, as the backend's index and product checks want them. An
/// index counting from the end isn't, and the `*` beside it still is.
#[test]
fn an_index_or_a_product_is_not_negative_when_its_ints_arent() {
    let source = "def weighted(values: list[int]) -> int:\n    mut running = 0\n    for place in range(len(values)):\n        running = running + values[place] * place\n    return running\n\n\
                  def doubled_tail(values: list[int]) -> int:\n    return values[-1] * 2\n\n\
                  def main(stdout: Stdout):\n    mut values = [1, 2, 3]\n    for place in range(len(values)):\n        values[place] = place\n    stdout.print(weighted(values), doubled_tail(values))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "weighted", &is_index).unwrap(), [true]);
    assert_eq!(not_negative(&program, &ranges, "weighted", &is_product).unwrap(), [true]);
    assert_eq!(not_negative(&program, &ranges, "doubled_tail", &is_index).unwrap(), [false]);
    assert_eq!(not_negative(&program, &ranges, "doubled_tail", &is_product).unwrap(), [true]);
    assert_eq!(not_negative(&program, &ranges, "main", &is_index).unwrap(), [true]);
    checked(&program, &ranges);
    let negative = source.replace("[1, 2, 3]", "[1, -2, 3]");
    let program = lowered(&negative).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "weighted", &is_product).unwrap(), [false]);
    assert_eq!(not_negative(&program, &ranges, "doubled_tail", &is_product).unwrap(), [false]);
}

/// A function whose walk doesn't settle in its rounds ends on facts still moving, which can't be
/// trusted, so nothing in the program is known. Here each level takes the one after it, which a
/// walk sees a round later, so the first level hears of the last one's -5 only on the thirtieth
/// round, though a run gets it there by the thirtieth time round the loop.
#[test]
fn a_function_that_doesnt_settle_leaves_nothing_known() {
    let mut source = String::from("def main(stdout: Stdout):\n");
    source.extend((1..=30).map(|level| format!("    mut level_{level} = 0\n")));
    source.push_str("    for _ in range(40):\n");
    source.extend((1..30).map(|level| format!("        level_{level} = level_{}\n", level + 1)));
    source.push_str("        level_30 = -5\n    stdout.print(level_1 // 2)\n");
    let program = lowered(&source).unwrap();
    let ranges = Ranges::of(&program);
    assert!(ranges.function("main").is_none(), "{:?}", unsigned(&program, &ranges, "main"));
    assert_eq!(ranges.unsettled(), Some(&Unsettled::Function("main".to_owned())));
    checked(&program, &ranges);
}

/// By floats' total order NaN sorts last: the false side of `at > high` can't be NaN, and the
/// false side of `at < low` can. So a float kept between two bounds by a `>` test fits an int
/// whatever it was given, NaN included, and one kept only by a `<` test doesn't.
#[test]
fn a_float_kept_by_a_greater_than_test_is_never_nan() {
    let kept = "from std.math import NAN\n\n\
                def clamp(at: float) -> float:\n    if at > 10.0:\n        return 10.0\n    if at < 0.0:\n        return 0.0\n    return at\n\n\
                def main(stdout: Stdout):\n    stdout.print(int(clamp(NAN)), int(clamp(4.5)))\n";
    let program = lowered(kept).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "main").unwrap(), [true, true]);
    checked(&program, &ranges);
    let half = "from std.math import NAN\n\n\
                def floor_at_zero(at: float) -> float:\n    if at < 0.0:\n        return 0.0\n    return at\n\n\
                def main(stdout: Stdout):\n    stdout.print(floor_at_zero(NAN), int(floor_at_zero(4.5)))\n";
    let program = lowered(half).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "main").unwrap(), [false]);
}

/// A list lent with `ref` to a function that sets every item, counting from 0 to its length, holds
/// only what that function put in after the call. One that sets all but the last leaves what was
/// there too.
#[test]
fn a_call_that_sets_every_item_leaves_only_what_it_put_in() {
    let every = "from std.math import NAN\n\n\
                 def fill(ref values: list[float]):\n    for at in range(len(values)):\n        values[at] = 2.5\n    return\n\n\
                 def main(stdout: Stdout):\n    mut values = [NAN, NAN]\n    fill(ref values)\n    stdout.print(int(values[0]))\n";
    let program = lowered(every).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "main").unwrap(), [true]);
    checked(&program, &ranges);
    let most = every
        .replace("range(len(values))", "range(len(values) - 1)")
        .replace("int(values[0])", "int(values[1])");
    let program = lowered(&most).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "main").unwrap(), [false]);
}

/// A local written more than once holds, after each write, only what that write gave it, and a
/// branch narrows it where it holds, even where it's written again later.
#[test]
fn a_local_holds_what_its_last_write_gave_it() {
    let source = "def scaled(start: int) -> int:\n    mut value = start - 10\n    value = value * 0\n    value = value + 3\n    return value * 2\n\n\
                  def main(stdout: Stdout):\n    stdout.print(scaled(4))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "scaled", &is_product).unwrap(), [false, true]);
    checked(&program, &ranges);
}

/// A test that a local isn't `-1`, the end of a list linked by index, moves the least it can be
/// from `-1` to `0` where it holds, so an index read with it after the test isn't negative.
#[test]
fn a_not_equals_test_moves_an_end_of_a_range() {
    let source = "def walk(links: list[int], first: int) -> int:\n    mut at = first\n    mut seen = 0\n    while at != -1:\n        seen += 1\n        at = links[at]\n    return seen\n\n\
                  def main(stdout: Stdout):\n    stdout.print(walk([1, 2, -1], 0))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "walk", &is_index).unwrap(), [true]);
    checked(&program, &ranges);
}

/// A struct's int field holds what every write of it in the program gives it, so a field read
/// from a struct held in a list, whose own value isn't followed, is known too.
#[test]
fn a_field_holds_what_every_write_of_it_gives() {
    let source = "struct Task:\n    next: int\n\n\
                  def follow(tasks: list[Task], links: list[int], at: int) -> int:\n    return links[tasks[at].next]\n\n\
                  def main(stdout: Stdout):\n    mut tasks = [Task(next=0)]\n    tasks.append(Task(next=1))\n    tasks[0].next = 1\n    stdout.print(follow(tasks, [5, 6], 1))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "follow", &is_index).unwrap(), [true, true]);
    checked(&program, &ranges);
    let negative = source.replace("tasks[0].next = 1", "tasks[0].next = -1");
    let program = lowered(&negative).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "follow", &is_index).unwrap(), [true, false]);
}

/// A closure's first parameters are what it captured where it was made.
#[test]
fn a_closure_knows_what_it_captured() {
    let source = "def apply(value: int, change: (int) -> int) -> int:\n    return change(value)\n\n\
                  def main(stdout: Stdout):\n    step = 3\n    stdout.print(apply(4, (count) => step * 2 + count))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    let closure = program
        .functions
        .iter()
        .find(|function| function.name.contains("arrow"))
        .map(|function| function.name.clone());
    let closure = closure.unwrap_or_else(|| panic!("no closure in:\n{program}"));
    assert_eq!(not_negative(&program, &ranges, &closure, &is_product).unwrap(), [true]);
    checked(&program, &ranges);
}

/// Values that go round several locals in turn, as `sum`'s eight running totals do, grow each
/// local only every few rounds. Widening is counted by where the loop starts, not by local, so
/// they settle.
#[test]
fn values_going_round_several_locals_settle() {
    let source = "def main(stdout: Stdout):\n    mut first = 0.0\n    mut second = 0.0\n    mut third = 0.0\n    mut fourth = 0.0\n    mut fifth = 0.0\n    mut sixth = 0.0\n    mut seventh = 0.0\n    mut eighth = 0.0\n    for round in range(100):\n        added = first + float(round)\n        first = second\n        second = third\n        third = fourth\n        fourth = fifth\n        fifth = sixth\n        sixth = seventh\n        seventh = eighth\n        eighth = added\n    stdout.print(first + second + third + fourth + fifth + sixth + seventh + eighth)\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(ranges.unsettled(), None);
    checked(&program, &ranges);
}

/// A `shared` value is a cell anything holding its handle can change, so nothing's known of
/// what it holds from where it was made: here its count is made as 0, and a lock makes it -3.
#[test]
fn a_shared_value_is_what_any_lock_leaves_it() {
    let source = "struct Tally:\n    count: int\n\n\
                  def main(stdout: Stdout):\n    shared tally = Tally(count=0)\n    lock tally as ref held:\n        held.count -= 3\n    lock tally as seen:\n        stdout.print([1, 2, 3][seen.count], seen.count // 2)\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    let unknown =
        |wanted: &dyn Fn(&InstKind) -> bool| not_negative(&program, &ranges, "main", wanted).unwrap();
    assert!(!unknown(&is_index).contains(&true), "{program}");
    assert!(!unknown(&is_division).contains(&true), "{program}");
}

/// A list parameter lent with `ref` that its function writes whole holds what was written after
/// the call, which its caller sees.
#[test]
fn a_list_lent_and_written_whole_holds_what_was_written() {
    let source = "def lowered_by_ten(values: list[int]) -> list[int]:\n    return [value - 10 for value in values]\n\n\
                  def shifted(ref values: list[int]):\n    values = lowered_by_ten(values)\n    return\n\n\
                  def main(stdout: Stdout):\n    mut values = [1, 2, 3]\n    shifted(ref values)\n    stdout.print(values[0] // 4)\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(unsigned(&program, &ranges, "main").unwrap(), [false]);
    checked(&program, &ranges);
}

/// Under floats' total order NaN is past every number, so `min` of a NaN and a number is the
/// number: a side that can be NaN lets `min` be as high as the other side goes.
#[test]
fn min_of_what_can_be_nan_can_be_the_other_side() {
    let source = "def main(stdout: Stdout):\n    for count in range(3):\n        big = 1e308 * float(count)\n        nothing = big * 0.0\n        stdout.print(int(min(nothing, 1e300)))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "main").unwrap(), [false]);
    checked(&program, &ranges);
}

/// An infinity divided by an infinity is NaN, even where the divisor can also be zero.
#[test]
fn an_infinity_by_an_infinity_can_be_nan() {
    let source = "def main(stdout: Stdout):\n    values = [1, 2]\n    top = 1e308 * 10.0\n    big = 1e308 * float(len(values))\n    ratio = top / big\n    if 1.0 < ratio:\n        stdout.print(\"more\")\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    let known = ranges.function("main").unwrap();
    let proven: Vec<Place> = places(&program, "main", &is_comparison)
        .unwrap()
        .into_iter()
        .filter(|place| known.not_nan(*place))
        .collect();
    assert_eq!(proven, []);
    checked(&program, &ranges);
}

/// A struct held in an error's field is written through the error too, and what that write
/// gives joins the field across the program.
#[test]
fn a_field_written_through_an_error_is_followed() {
    let source = "struct Point:\n    across: int\n\n\
                  error Missed:\n    at: Point\n\n\
                  def main(stdout: Stdout):\n    mut missed = Missed(at=Point(across=1))\n    missed.at.across = -5\n    point = missed.at\n    stdout.print([1, 2][point.across])\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(not_negative(&program, &ranges, "main", &is_index).unwrap(), [false]);
}

/// A function something other than a call by name may call, here a function passed as a value,
/// knows nothing of its parameters.
#[test]
fn a_function_called_as_a_value_knows_nothing_of_its_parameters() {
    let source = "def whole(x: float) -> int:\n    return int(x)\n\n\
                  def apply(convert: (float) -> int, x: float) -> int:\n    return convert(x)\n\n\
                  def main(stdout: Stdout):\n    stdout.print(whole(1.5), apply(whole, 2.5))\n";
    let program = lowered(source).unwrap();
    let ranges = Ranges::of(&program);
    assert_eq!(fits(&program, &ranges, "whole").unwrap(), [false]);
}

const STEPS: u64 = 3_000_000;

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

/// Every kernel's, example's and real program's facts settle within their rounds. One that
/// didn't would leave nothing known about it, which no other test would see.
#[test]
fn every_kernel_example_and_real_program_settles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = files_in(&root.join("bench/kernels")).unwrap();
    files.extend(files_in(&root.join("examples")).unwrap());
    files.extend(files_in(&root.join("bench/realworld")).unwrap());
    let mut programs = 0;
    for path in files {
        let source = fs::read_to_string(&path).unwrap();
        let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
        let Ok(checked) = fennec_check::check_and_resolve(&mut module) else { continue };
        let program = lower(&module, &checked);
        assert_eq!(Ranges::of(&program).unsettled(), None, "{}", path.display());
        programs += 1;
    }
    assert!(programs >= 100, "only {programs} programs settled");
}

/// Every fact holds while every kernel and example runs in the IR's evaluator, as far as it can
/// take each in its steps.
#[test]
fn every_fact_holds_while_the_kernels_and_examples_run() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = files_in(&root.join("bench/kernels")).unwrap();
    files.extend(files_in(&root.join("examples")).unwrap());
    let (mut programs, mut checks) = (0, 0);
    for path in files {
        let shown = path.display().to_string();
        let source = fs::read_to_string(&path).unwrap();
        let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
        let Ok(checked) = fennec_check::check_and_resolve(&mut module) else { continue };
        let program = lower(&module, &checked);
        let ranges = Ranges::of(&program);
        let ran = check_ranges(&program, &ranges, STEPS);
        assert_eq!(ran.broken, None, "{shown}");
        programs += 1;
        checks += ran.checked;
    }
    assert!(programs >= 20, "only {programs} programs ran");
    assert!(checks > 100_000, "only {checks} facts were checked");
}
