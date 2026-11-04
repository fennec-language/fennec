//! `fennec build --llvm` against `fennec run`, on programs that backend takes so far. Skipped
//! where LLVM's tools aren't installed, as `fennec build --llvm` itself would say, unless
//! `FENNEC_EXPECT_LLVM` says they have to be.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::executables::runtime_built;

/// How many examples this backend takes, at least.
const BUILDING_EXAMPLES: usize = 100;

fn fennec(args: &[&str], home: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command().args(args).env("FENNEC_HOME", home).output()
}

/// Whether this can run: this isn't Windows, where `fennec build` doesn't link, LLVM's tools and
/// the runtime library are there, and mutation testing isn't under way, which would go past its
/// time limit, as in `executables.rs`. Where `FENNEC_EXPECT_LLVM` is set, as CI's Linux test
/// jobs set it, LLVM is the backend that has to work, so what's missing fails the test rather
/// than skipping it.
fn ready() -> bool {
    let tools = fennec_llvm::Tools::find();
    let runtime = runtime_built();
    if std::env::var_os("FENNEC_EXPECT_LLVM").is_some_and(|expected| !expected.is_empty()) {
        let missing = tools.as_ref().err().map_or("", String::as_str);
        assert!(tools.is_ok(), "FENNEC_EXPECT_LLVM is set, but {missing}");
        assert!(runtime, "FENNEC_EXPECT_LLVM is set, but the runtime library didn't build");
    }
    tools.is_ok() && runtime
}

/// A folder of its own, emptied when the test is done.
fn folder(name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-llvm-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn same_as_the_jit(name: &str, source: &str) -> std::io::Result<()> {
    let (ours, jit) = both(name, source)?;
    assert_eq!(ours, jit);
    Ok(())
}

/// A known difference: the IR gives an index being written, and a value being formatted, the span
/// of what they're in, where the JIT underlines the part that failed. Same message and outcome.
fn same_message_as_the_jit(name: &str, source: &str) -> std::io::Result<()> {
    let ((ours_out, ours_err, ours_code), (jit_out, jit_err, jit_code)) = both(name, source)?;
    assert_eq!((ours_out, ours_code), (jit_out, jit_code));
    assert_eq!(ours_err.lines().next(), jit_err.lines().next());
    Ok(())
}

/// What the program printed, its errors and its exit code, built with LLVM and run by the JIT.
fn both(name: &str, source: &str) -> std::io::Result<(Outcome, Outcome)> {
    both_built(name, source, &["--llvm"])
}

/// The same, built with these options.
fn both_built(name: &str, source: &str, how: &[&str]) -> std::io::Result<(Outcome, Outcome)> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let output = executable.to_string_lossy().into_owned();
    let args: Vec<&str> =
        ["build"].into_iter().chain(how.iter().copied()).chain([program.as_str(), "-o", &output]).collect();
    let built = fennec(&args, &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    let jit = outcome(&fennec(&["run", "--jit", &program], &dir)?);
    std::fs::remove_dir_all(&dir)?;
    Ok((ours, jit))
}

type Outcome = (String, String, Option<i32>);

fn outcome(output: &Output) -> Outcome {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

/// `str` of what's text already. The linter takes these out of the examples, since they change
/// nothing, so it's here they're run. This backend doesn't take `int`, or `float` of a float,
/// yet, so `cli.rs` runs those on the other engines.
#[test]
fn str_of_text_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "str_of_text",
        "def main(stdout: Stdout):\n    name = \"fox\"\n    stdout.print(str(name), str(\"\"))\n",
    )
}

/// `fail` and `?` leave a `for mut` loop as `return` does: the item it's on is written back first.
#[test]
fn failing_out_of_a_for_mut_loop_writes_the_item_back() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let shared = r#"error Bad:
    why: str

struct Unit:
    tags: list[int]
    hp: int

def maybe(at: int) -> int:
    if at == 1:
        fail Bad("from a call")
    at

def by_fail(mut units: list[Unit], at: int) -> int:
    for mut unit in units:
        unit.tags.append(at)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    0

def by_try(mut units: list[Unit]) -> int:
    for mut unit in units:
        unit.tags.append(8)
        unit.hp += maybe(unit.hp)?
    0

def main(stdout: Stdout):
    mut units = [Unit([1], 0), Unit([2], 1), Unit([3], 5)]
    stdout.print(by_fail(mut units, 9) else -1, units)
    mut others = [Unit([1], 0), Unit([2], 1), Unit([3], 5)]
    stdout.print(by_try(mut others) else -2, others)
"#;
    same_as_the_jit("failing_out_of_for_mut", shared)?;
    // Out of two loops at once. The JIT stops here with "expected an int", so this one's held to
    // what the interpreter prints.
    let nested = "error Bad:\n    why: str\n\ndef nested(mut grid: list[list[int]]) -> int:\n    for mut row in grid:\n        for mut cell in row:\n            cell += 1\n            if cell == 5:\n                fail Bad(\"inner\")\n    0\n\ndef main(stdout: Stdout):\n    mut grid = [[1, 2], [4, 7], [9]]\n    stdout.print(nested(mut grid) else -3, grid)\n";
    let dir = folder("failing_out_of_two_loops")?;
    let program = dir.join("two_loops.fen");
    std::fs::write(&program, nested)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("two_loops"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    let interpreted = outcome(&fennec(&["run", "--interp", &program], &dir)?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, interpreted);
    assert_eq!(ours.0, "-3 [[2, 3], [5, 7], [9]]\n");
    Ok(())
}

/// `units[at].tags.append(x)` with the list in a local: the inner list goes on growing in place,
/// and what's shared with it, at any level, doesn't change.
#[test]
fn appending_to_a_field_of_a_local_lists_item_copies_only_what_is_shared() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "local_item_field_appends",
        r"struct Unit:
    tags: list[int]
    hp: int

struct Box:
    items: list[int]
    next: list[Box]

def grow(mut units: list[Unit], by: int):
    for at in range(units.len()):
        units[at].tags.append(by)

def main(stdout: Stdout):
    mut units = [Unit([1, 2, 3], 0), Unit([4], 1)]
    # A copy of the list, of an item, and of the field, each kept apart from what comes after.
    snapshot = copy(units)
    shared = units
    item = units[0]
    tags = units[0].tags
    units[0].tags.append(10)
    stdout.print(units, snapshot, shared, item, tags)
    units[-1].tags.append(11)
    stdout.print(units, snapshot, shared, item, tags)
    # An argument read from the same place, and from another item.
    units[0].tags.append(units[0].tags.len())
    units[0].tags.append(units[1].tags[0])
    stdout.print(units)
    # An index that changes between the read and the write isn't this at all.
    mut at = 0
    units[at].tags.append(at)
    at = 1
    units[at].tags.append(at)
    stdout.print(units)
    grow(mut units, 5)
    stdout.print(units, snapshot)
    # A caller's copy stays as it was.
    mut mine = copy(units)
    kept = mine
    grow(mut mine, 6)
    stdout.print(mine[0].tags, kept[0].tags)
    # Items that are blocks of their own.
    mut boxes = [Box([1], []), Box([2], [])]
    other = boxes[0]
    boxes[0].items.append(3)
    stdout.print(boxes, other)
    # Outside the list's length, it fails as it should.
    units[9].tags.append(1)
",
    )?;
    let counts = counts_of(
        "local_item_field_counts",
        "struct Unit:\n    tags: list[int]\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units = [Unit([1, 2, 3], 0), Unit([1, 2, 3], 1)]\n    for round in range(1000):\n        for at in range(units.len()):\n            units[at].tags.append(round)\n    stdout.print(units[0].tags.len())\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// Reading a string that isn't ASCII goes on from where the last read got to, so reading it in
/// order takes a step for each character. A place kept for a string that's since moved, grown or
/// been let go of, with its address used again, would give a wrong character and no failure.
const CHARACTER_PLACES: &str = r#"def long_text() -> str:
    mut parts = ["héllo wörld "]
    for i in range(40):
        parts.append(f"ça {i} 日本語 🦊 ")
    "".join(parts)

def show_all(text: str) -> str:
    mut out = []
    for at in range(text.len()):
        out.append(text[at])
    "".join(out)

def main(stdout: Stdout):
    text = long_text()
    stdout.print(text.len(), show_all(text) == text)
    # Backward, forward, jumps, ends, negative.
    mut seen = []
    mut back = text.len() - 1
    while back >= 0:
        seen.append(text[back])
        back -= 1
    stdout.print(seen[0], seen[1], seen[seen.len() - 1], seen.len())
    stdout.print(text[0], text[1], text[-1], text[-2], text[5], text[100], text[3], text[text.len() - 1])
    mut jumps = []
    for at in [0, 200, 1, 199, 50, 51, 49, -1, -200, 7]:
        jumps.append(text[at])
    stdout.print(jumps)
    # Slices: every way of bounding one, inside, past the ends, empty, backward.
    stdout.print(text[:5], text[5:], text[1:6], text[-5:], text[:-200], text[-3:-1], text[3:3], text[9:2])
    stdout.print(text[0:100000], text[-100000:3], text[200:205], text[205:200], text[:].len())
    mut pieces = []
    mut piece_at = 0
    while piece_at < text.len():
        pieces.append(text[piece_at:piece_at + 3])
        piece_at += 37
    stdout.print(pieces)
    # A one-character slice of each width.
    stdout.print(text[1:2], "日本語"[1:2], "🦊x"[0:1], "a🦊"[1:], "é"[0:1], "é"[1:])
    # Two strings in turn, each with its own place.
    other = "ñandú " * 30
    mut turns = []
    mut turn = 0
    while turn < 60:
        turns.append(text[turn])
        turns.append(other[turn])
        turn += 7
    stdout.print(turns)
    # A string growing in place while it's read.
    mut grown = "é"
    for i in range(200):
        grown += "ü" if i % 3 == 0 else "a"
        if grown[-1] != grown[grown.len() - 1]:
            stdout.print("wrong")
    stdout.print(grown.len(), grown[100], grown[-1], grown[150:153])
    # Strings let go of, their addresses free for new ones with other text.
    for round in range(200):
        made = f"é{round} ümläut ümläut ümläut ümläut ümläut"
        last = made[made.len() - 1]
        middle = made[10]
        if last != "t":
            stdout.print("wrong", round, last)
        if round % 50 == 0:
            stdout.print(middle, made[2:6])
    # A block moved by `+=`, read at the same place before and after, past and before the cursor.
    mut moved = "é" * 3
    mut before = moved[2]
    for i in range(300):
        moved += "日"
        before = moved[moved.len() - 2]
    stdout.print(moved.len(), before, moved[1], moved[2], moved[3], moved[299], moved[-1], moved[1:4])
    # The address of a string let go of, used by another with different text, at the same place.
    mut wrong = 0
    for round in range(400):
        first = "ab" * 20 + f"é{round}" + "日本" * 20
        at_first = first[42]
        second = "é" * 30 + f"x{round}" + "ü" * 30
        at_second = second[42]
        if at_first != "0" and at_first != "1" and at_first != "2" and at_first != "3" and at_first != "é" and at_first != "4" and at_first != "5" and at_first != "6" and at_first != "7" and at_first != "8" and at_first != "9" and at_first != "日" and at_first != "本":
            wrong += 1
        if at_second != "ü":
            wrong += 1
    stdout.print(wrong)
    # Inline strings, read directly, and the same text as a block.
    short = "日本é"
    stdout.print(short[0], short[1], short[2], short[-1], short[1:], short[:1], short.len())
    # Other things on non-ASCII text.
    stdout.print(text[:20].upper(), text[3:9].find("w"), text[:8] + "é", f"{text[:4]}!", text[:4] == "héll", text[1] == "é")
    stdout.print(text[100000])
"#;

#[test]
fn reading_a_string_that_isnt_ascii_finds_the_right_character() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit("character_places", CHARACTER_PLACES)
}

/// Reading and slicing a long string in order, one accented letter in it. How long this takes
/// is held by the `json_unicode` kernel; a test this size says only that it's right.
#[test]
fn reading_a_string_that_isnt_ascii_in_order_takes_a_step_for_each_character() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "character_steps",
        "def main(stdout: Stdout):\n    text = \"é\" + \"a\" * 3000\n    mut found = 0\n    for at in range(text.len()):\n        if text[at] == \"a\":\n            found += 1\n    mut pieces = 0\n    for at in range(text.len() - 3):\n        pieces += text[at:at + 3].len()\n    stdout.print(found, pieces)\n",
    )
}

/// A program changing `for mut` items in every way a body can, and leaving in every way.
const FOR_MUT_ITEMS: &str = r#"error Bad:
    why: str

struct Unit:
    tags: list[int]
    name: str
    hp: int

struct World:
    units: list[Unit]
    log: list[int]
    turn: int

enum Token:
    Blank
    Word(text: str)
    Many(items: list[int])

def total(units: list[Unit]) -> int:
    sum(unit.hp for unit in units)

def by_fail(mut units: list[Unit]) -> int:
    for mut unit in units:
        unit.tags.append(9)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    0

def by_return(mut units: list[Unit]) -> int:
    for mut unit in units:
        unit.tags.append(8)
        if unit.hp == 1:
            return unit.tags.len()
    -1

def grow(mut units: list[Unit], by: int):
    for mut unit in units:
        unit.tags.append(by)
        unit.name += "!"

def step(mut world: World):
    for mut unit in world.units:
        world.log.append(unit.hp)
        unit.tags.append(world.turn)
    world.turn += 1

def make() -> list[Unit]:
    [Unit([1], "first name long enough not to be inline", 0), Unit([2], "b", 1), Unit([3], "c", 5)]

def main(stdout: Stdout):
    # Copies taken before the loop keep what they had.
    mut units = make()
    shared = units
    kept = copy(units)
    item = units[0]
    tags = units[0].tags
    for mut unit in units:
        unit.tags.append(unit.hp)
    stdout.print(units, shared, kept, item, tags)
    # Ways out.
    mut a = make()
    stdout.print(by_fail(mut a) else -1, a)
    mut b = make()
    stdout.print(by_return(mut b), b)
    mut c = make()
    for mut unit in c:
        if unit.hp == 0:
            continue
        unit.tags.append(4)
        if unit.hp == 1:
            break
        unit.hp = 100
    stdout.print(c)
    # The list read in the body sees what was written back so far.
    mut d = make()
    for mut unit in d:
        unit.tags.append(d[0].tags.len())
        unit.hp += d.len() + total(d)
    stdout.print(d)
    # A copy of the item, or of what it holds, made inside.
    mut e = make()
    mut seen: list[Unit] = []
    mut held: list[list[int]] = []
    for mut unit in e:
        seen.append(unit)
        held.append(unit.tags)
        unit.hp += 1
    stdout.print(e, seen, held)
    # Replaced whole, and given away.
    mut f = make()
    for mut unit in f:
        unit = Unit([7], "whole new value long enough not to be inline", unit.hp)
    stdout.print(f)
    # Through a function, and a field.
    mut g = make()
    grow(mut g, 6)
    grow(mut g, 7)
    stdout.print(g)
    mut w = World(make(), [], 3)
    step(mut w)
    other = w
    step(mut w)
    stdout.print(w, other)
    # Nested.
    mut grid = [[1, 2], [3], []]
    for mut row in grid:
        row.append(row.len())
        for mut cell in row:
            cell *= 2
    stdout.print(grid)
    # Strings and enums as items.
    mut words = ["a", "a string long enough not to be held inline"]
    for mut word in words:
        word += "!"
    stdout.print(words)
    mut lists = [[1], [2, 3]]
    before = lists[1]
    for mut one in lists:
        one.append(9)
    stdout.print(lists, before)
    # Emptied in the body, then grown.
    mut h = make()
    for mut unit in h:
        unit.tags = []
        unit.tags.append(1)
    stdout.print(h)
"#;

/// A `for mut` item is taken out for the whole of the body, so what it holds changes in place.
/// What's shared, and a body that reads the list, see what they should.
#[test]
fn for_mut_changes_what_an_item_holds_in_place() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("for_mut_items", FOR_MUT_ITEMS)?;
    let counts = counts_of(
        "for_mut_counts",
        "struct Unit:\n    tags: list[int]\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units = [Unit([1, 2, 3], 0), Unit([1, 2, 3], 1)]\n    for round in range(1000):\n        for mut unit in units:\n            unit.tags.append(round)\n            if round % 2 == 0:\n                unit.hp += 1\n    stdout.print(units[0].tags.len())\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// `isdigit`, native on headers, says what std's says: inline and in a block, empty, digits
/// outside ASCII, and a digit beside something that isn't.
#[test]
fn isdigit_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "isdigit",
        r#"def main(stdout: Stdout):
    long = "1234567890" * 5
    for text in ["", "0", "9", "5", "/", ":", "a", "12", "1a", "a1", " 1", "1 ", "٣", "１", "1٣", "é", "🦊", long, long + "x", "x" + long, long[:23], long[:24]]:
        stdout.print(text, text.isdigit())
    mut found = 0
    for at in range(long.len()):
        if long[at].isdigit():
            found += 1
    stdout.print(found)
"#,
    )
}

#[test]
fn numbers_calls_and_printing_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "numbers",
        "from std.math import sqrt\n\ndef fib(n: int) -> int:\n    if n < 2: return n\n    fib(n - 1) + fib(n - 2)\n\ndef main(stdout: Stdout):\n    stdout.print(\"fib\", fib(20), sqrt(2.0), 1.5 > 1.0)\n    stdout.print(7 // -2, 7 % -2, -7 // 2, -7 % 2, -(3))\n    mut total = 0\n    for i in range(5):\n        total += i * i\n    stdout.print(total)\n",
    )
}

#[test]
fn lists_keep_value_semantics_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "sharing",
        r"def changed(items: list[int]) -> list[int]:
    mut copy = items
    copy[0] = 99
    copy

def grown(mut items: list[int]):
    items.append(7)
    items[0] = 5

def same(items: list[int]) -> list[int]:
    items

def show(items: list[int], stdout: Stdout):
    mut at = 0
    while at < items.len():
        stdout.print(items[at])
        at += 1

def main(stdout: Stdout):
    a = [1, 2, 3]
    b = changed(a)
    show(a, stdout)
    show(b, stdout)
    mut c = [10, 20]
    d = c
    c[1] = 21
    c.append(30)
    show(c, stdout)
    show(d, stdout)
    mut e = [1]
    f = e
    grown(mut e)
    show(e, stdout)
    show(f, stdout)
    mut g = same(c)
    g[0] = -1
    show(c, stdout)
    show(g, stdout)
    stdout.print(c[-1], c[-3], [i * i for i in range(4)][2])
    mut h = [1, 2]
    for _ in range(3):
        k = copy(h)
        h.append(k.len())
        stdout.print(k.len(), h.len())
",
    )
}

/// Changing an item in place is only safe once the list is alone: another copy of the list
/// mustn't see it, however the item's own count reads.
#[test]
fn changing_an_item_doesnt_show_through_a_copy() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "items",
        r"struct Packet:
    id: int
    data: list[int]

struct Holder:
    packets: list[Packet]

def bump(mut holder: Holder, at: int):
    holder.packets[at].id = holder.packets[at].id + 1
    holder.packets[at].data[0] = 50

def main(stdout: Stdout):
    a = Packet(id=1, data=[1, 2])
    mut one = [a, Packet(id=2, data=[3])]
    two = one
    one[0].id = 10
    stdout.print(one[0].id, two[0].id, a.id)
    mut holder = Holder(packets=one)
    keep = holder
    bump(mut holder, 1)
    stdout.print(holder.packets[1].id, keep.packets[1].id, one[1].id, two[1].id)
    stdout.print(holder.packets[1].data[0], keep.packets[1].data[0], two[1].data[0])
    mut list_only = copy(two)
    shown = list_only[0]
    list_only[0].id = 77
    stdout.print(shown.id, list_only[0].id, two[0].id)
    holder.packets.append(a)
    holder.packets[2].id = 5
    stdout.print(holder.packets.len(), keep.packets.len(), holder.packets[2].id, a.id)
    mut kept = holder
    copied = kept
    kept.packets.append(a)
    stdout.print(kept.packets.len(), copied.packets.len())
    mut nested = copy(kept)
    other = nested
    nested.packets[0].data[1] = 123
    stdout.print(nested.packets[0].data[1], other.packets[0].data[1], kept.packets[0].data[1])
    mut walked = [1, 2, 3]
    before = walked
    for mut item in walked:
        item = item * 2
    stdout.print(walked[0], walked[2], before[0], before[2])
    mut grows = [1, 2]
    for item in copy(grows):
        grows[0] = item + 10
        grows[1] = 40
    stdout.print(grows[0], grows[1])
    mut points = [Packet(id=1, data=[1]), Packet(id=2, data=[2])]
    seen = points
    for mut point in points:
        point.id = point.id + 1
    stdout.print(points[0].id, points[1].id, seen[0].id, seen[1].id)
",
    )
}

#[test]
fn structs_tuples_and_formats_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "values",
        r#"struct Point:
    x: float
    y: float
    on: bool

struct Line:
    start: Point
    end: Point
    weight: int

def moved(p: Point, by: float) -> Point:
    mut q = p
    q.x = q.x + by
    q

def nudge(mut p: Point):
    p.y = p.y + 1.0
    p.on = not p.on

def pair(n: int) -> (int, float):
    (n + 1, float(n) / 2.0)

def main(stdout: Stdout):
    a = Point(x=1.0, y=2.0, on=True)
    b = moved(a, 0.5)
    stdout.print(a.x, b.x, a.on)
    mut c = copy(b)
    nudge(mut c)
    stdout.print(b.y, c.y, b.on, c.on)
    mut line = Line(start=a, end=c, weight=3)
    line.end.x = 9.5
    stdout.print(line.start.x, line.end.x, c.x, line.weight, line.end.on)
    mut points = [a, b]
    points[1].x = 7.0
    points.append(c)
    stdout.print(points[1].x, b.x, points.len(), points[2].on)
    shown = points[0]
    points[0].y = -3.0
    stdout.print(shown.y, points[0].y)
    n, half = pair(5)
    stdout.print(n, half)
    flags = [True, False]
    stdout.print(flags[0], flags[-1])
    stdout.print(f"{half:.3f} and {n} at {a.x}")
"#,
    )
}

/// Every example this backend takes prints exactly what it should. Those it doesn't take yet
/// say so, and the count keeps that from shrinking unnoticed.
#[test]
fn the_examples_it_takes_print_what_they_should() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("examples")?;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let (mut built, mut wrong) = (0, Vec::new());
    for entry in std::fs::read_dir(examples)? {
        let path = entry?.path();
        let wanted = path.with_extension("out");
        if path.extension().is_none_or(|ext| ext != "fen") || !wanted.exists() {
            continue;
        }
        let executable = dir.join("example");
        let made =
            fennec(&["build", "--llvm", &path.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
        if !made.status.success() {
            let said = String::from_utf8_lossy(&made.stderr).into_owned();
            assert!(said.contains("doesn't take yet") || said.contains("error"), "{said}");
            continue;
        }
        built += 1;
        let ran = Command::new(&executable).output()?;
        if String::from_utf8_lossy(&ran.stdout) != std::fs::read_to_string(&wanted)? {
            wrong.push(path);
        }
    }
    std::fs::remove_dir_all(&dir)?;
    assert!(wrong.is_empty(), "printed the wrong thing: {wrong:?}");
    assert!(built >= BUILDING_EXAMPLES, "only {built} examples build");
    Ok(())
}

#[test]
fn dicts_sets_closures_and_tuples_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "collections",
        r#"struct Key:
    x: int
    y: int

def twice(n: int, change: (int) -> int) -> int:
    change(change(n))

def main(stdout: Stdout):
    mut counts: dict[int, int] = {}
    for i in range(50):
        counts[i % 7] = (counts.get(i % 7) else 0) + i
    stdout.print(counts[3], counts.len(), 3 in counts, 99 in counts, (counts.get(99) else -1))
    shared = counts
    counts[3] = 0
    stdout.print(counts[3], shared[3])
    mut words: dict[str, int] = {"a": 1}
    for word in ["fox", "fennec", "fox", "a"]:
        words[word] = (words.get(word) else 0) + 1
    for name, count in sorted(words.items()):
        stdout.print(name, count)
    mut keys: dict[Key, int] = {}
    keys[Key(x=1, y=2)] = 5
    keys[Key(x=1, y=2)] = 6
    keys[Key(x=2, y=2)] = 7
    stdout.print(keys[Key(x=1, y=2)], keys.len(), Key(x=9, y=9) in keys)
    mut seen = {1, 2}
    seen.add(2)
    seen.add(3)
    stdout.print(seen.len(), 3 in seen, 4 in seen)
    offset = 3
    stdout.print(twice(1, (n) => n * 2 + offset))
    pairs = [(1, "one"), (2, "two")]
    for number, name in pairs:
        stdout.print(number, name)
    for at, value in enumerate([5.5, 6.5]):
        stdout.print(at, value)
    for a, b in zip([1, 2, 3], ["x", "y"]):
        stdout.print(a, b)
    stdout.print(sum([1, 2, 3]), sum([0.5, 1.5]), all([True, False]), any([False, True]))
    mut stack = [1, 2, 3]
    stdout.print(stack.pop(), stack.len())
    stdout.print(sorted([3, 1, 2])[0], sorted(["b", "a"], key=(s) => s)[0])
    mut text = "abc"
    text += "def"
    stdout.print(text, text.len(), text[1:3], "bc" in text, text < "b")
    zeros = [0 for _ in range(5)]
    mut flags = [False for _ in range(3)]
    flags[1] = True
    mut none = [0 for _ in range(0 - 2)]
    none.append(7)
    stdout.print(zeros.len(), zeros[4], flags, none)
    first = "abc"
    stdout.print("a" < "b", "ab" < "a", "" < "a", first <= "abc", "é" > "z", "abcdefghij" < "abcdefghik", "b" >= "abc")
    maybe: int? = None
    other: int? = 5
    stdout.print(maybe is None, other is None)
    if other is not None:
        stdout.print(other + 1)
"#,
    )?;
    same_message_as_the_jit(
        "missing",
        "def main(stdout: Stdout):\n    d = {1: 2}\n    stdout.print(d[1])\n    stdout.print(d[3])\n",
    )
}

#[test]
fn generators_and_results_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "generators",
        r#"error Fault:
    reason: str

def count_up(limit: int) -> Iterator[int]:
    mut at = 0
    while at < limit:
        yield at
        at += 1

def words(count: int) -> Iterator[str]:
    for at in range(count):
        yield f"w{at}"

def odd_squares(numbers: Iterator[int]) -> Iterator[int]:
    for n in numbers:
        if n % 2 == 1:
            yield n * n

def running(numbers: Iterator[int]) -> Iterator[int]:
    mut total = 0
    for at, n in enumerate(numbers):
        total += n + at
        yield total

def first_over(limit: int, over: int) -> int:
    for n in odd_squares(count_up(limit)):
        if n > over: return n
    0

def half(n: int) -> int:
    if n % 2 == 1: fail Fault(f"{n} is odd")
    n // 2

def show(n: int) -> str:
    match half(n):
        case Ok(got):
            f"half is {got}"
        case Fault(reason):
            reason

def main(stdout: Stdout):
    stdout.print(sum(n for n in running(odd_squares(count_up(30)))))
    for word in words(3):
        stdout.print(word, word.len())
    stdout.print(first_over(100, 50), first_over(5, 50))
    kept = words(4)
    mut joined = ""
    for word in kept:
        joined += word
    stdout.print(joined)
    stdout.print(show(10), show(7), half(10).expect("ten is even"))
    names = ["a", "b"]
    stdout.print(f"{[1, 2]} and {names}")
"#,
    )?;
    same_message_as_the_jit(
        "twice",
        "def count_up(limit: int) -> Iterator[int]:\n    mut at = 0\n    while at < limit:\n        yield at\n        at += 1\n\ndef total(numbers: Iterator[int]) -> int:\n    mut sum = 0\n    for n in numbers:\n        sum += n\n    sum\n\ndef main(stdout: Stdout):\n    numbers = count_up(3)\n    stdout.print(total(numbers))\n    stdout.print(total(numbers))\n",
    )?;
    same_message_as_the_jit(
        "expect",
        "error Fault:\n    reason: str\n\ndef half(n: int) -> int:\n    if n % 2 == 1: fail Fault(\"odd\")\n    n // 2\n\ndef main(stdout: Stdout):\n    stdout.print(half(4).expect(\"four is even\"))\n    stdout.print(half(3).expect(\"three is even\"))\n",
    )
}

#[test]
fn regexes_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "regexes",
        r#"from std.re import Regex, is_regex

def main(stdout: Stdout):
    words = Regex(r"(\w)(\w*)")
    for found in words.find_all("fox and éclair"):
        stdout.print(found.text, found.start, found.end, found.groups.len(), found.groups[0], found.groups[1])
    optional = Regex(r"a(x)?b")
    for found in optional.find_all("ab axb"):
        stdout.print(found.text, found.groups[0] is None)
    stdout.print(words.replace("fox den", r"\2\1"), Regex(" +").split("a  b c").len())
    stdout.print(is_regex("("), is_regex("a+"))
    first = Regex(r"\d+").search("ab 42 7")
    if first is not None:
        stdout.print(first.text, first.start)
    stdout.print(Regex(r"\d+").match("ab 42") is None)
"#,
    )
}

/// What a built program says it copied and allocated, as `FENNEC_COUNTS` has it report.
fn counts_of(name: &str, source: &str) -> std::io::Result<String> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output()?;
    std::fs::remove_dir_all(&dir)?;
    let errors = String::from_utf8_lossy(&ran.stderr).into_owned();
    Ok(errors.lines().find(|line| line.starts_with("copies ")).unwrap_or_default().to_owned())
}

#[test]
fn what_a_field_or_an_items_field_holds_changes_without_being_copied() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "field",
        "struct Counter:\n    counts: dict[int, int]\n\n    def bump(mut self, key: int):\n        self.counts[key] = (self.counts.get(key) else 0) + 1\n\ndef main(stdout: Stdout):\n    mut counter = Counter(counts={})\n    for i in range(1000):\n        counter.bump(i % 64)\n    stdout.print(counter.counts[0])\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    let counts = counts_of(
        "items",
        "struct Var:\n    value: int\n    links: list[int]\n\nstruct Holder:\n    vars: list[Var]\n\n    def add(mut self, value: int):\n        self.vars.append(Var(value=value, links=[]))\n\n    def link(mut self, at: int, other: int):\n        self.vars[at].links.append(other)\n\ndef main(stdout: Stdout):\n    mut holder = Holder(vars=[])\n    for i in range(100):\n        holder.add(i)\n    for i in range(1000):\n        holder.link(i % 100, i)\n    stdout.print(holder.vars[5].links.len())\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    // A method written in Fennec changing a field: `self.inner.add(x)` and std's `extend`.
    let counts = counts_of(
        "methods",
        "struct Inner:\n    items: list[int]\n\n    def add(mut self, item: int):\n        self.items.append(item)\n\nstruct Outer:\n    inner: Inner\n    items: list[int]\n\n    def push(mut self, item: int):\n        self.inner.add(item)\n\n    def grow(mut self, more: list[int]):\n        self.items.extend(more)\n\ndef main(stdout: Stdout):\n    mut outer = Outer(inner=Inner(items=[]), items=[])\n    for i in range(100):\n        outer.push(i)\n    for i in range(100):\n        outer.grow([i])\n    stdout.print(outer.inner.items.len(), outer.items.len())\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// One number from a `counts_of` line, like `copies`.
fn count(counts: &str, name: &str) -> Option<u64> {
    counts.split_whitespace().skip_while(|word| *word != name).nth(1).and_then(|number| number.parse().ok())
}

#[test]
fn looping_one_list_of_a_packed_copies_and_allocates_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "from std.collections import Packed\n\ndef main(stdout: Stdout):\n    mut packed = Packed[int]()\n    for n in range(100):\n        packed.append([n, n + 1, n + 2])\n";
    let building = counts_of("packed_built", &format!("{source}    stdout.print(packed.len())\n"))?;
    let looped = counts_of(
        "packed_looped",
        &format!(
            "{source}    mut total = 0\n    for _ in range(100):\n        for at in range(packed.len()):\n            for item in packed[at]:\n                total += item\n    stdout.print(total)\n"
        ),
    )?;
    // Reading a list goes through its items in place, and the loop over `packed[at]` fuses
    // into a plain loop: 10,000 reads copy and allocate nothing more than building did.
    for what in ["copies", "copied_bytes", "allocations", "allocated_bytes"] {
        assert_eq!(count(&looped, what), count(&building, what), "{what}: {building} / {looped}");
    }
    Ok(())
}

/// A copy that's the last thing done with its source takes the source's owner over, so a
/// list made by a comprehension and then changed isn't copied. A source read again still
/// keeps its own value.
#[test]
fn the_last_copy_of_a_list_takes_it_over() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "last_copy",
        "def main(stdout: Stdout):\n    mut marks = [0 for _ in range(1000)]\n    for i in range(1000):\n        marks[i] = i\n    stdout.print(marks[999])\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    same_as_the_jit(
        "copies_kept",
        "def main(stdout: Stdout):\n    mut a = [1, 2, 3]\n    mut b = copy(a)\n    b[0] = 9\n    stdout.print(a, b)\n    mut c = [0 for _ in range(3)]\n    d = c\n    c[1] = 5\n    stdout.print(c, d)\n    mut keep: list[list[int]] = []\n    for i in range(3):\n        row = [i, i]\n        mut held = copy(row)\n        held[0] = 7\n        keep.append(held)\n        keep.append(row)\n    stdout.print(keep)\n",
    )
}

/// `==` with a literal of up to eight bytes reads the length and one word: empty, eight
/// bytes, nine, a zero byte and two-byte characters all compare as in the interpreter.
#[test]
fn strings_compare_with_short_literals_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "short_literals",
        r#"def main(stdout: Stdout):
    words = ["", "a", " ", "fox", "fennec ", "eightchr", "eightchrs", "é", "ab\x00", "ab"]
    for w in words:
        stdout.print(w == "", w == " ", w != "fox", "eightchr" == w, w == "é", w == "ab", w == "eightchrs")
    mut spaces = 0
    for c in "a b  c":
        if c == " ":
            spaces += 1
    stdout.print(spaces)
"#,
    )
}

/// An index checked once stays inside its list until something shortens or replaces the
/// list, or writes the index: a pop, a method popping or replacing a field, a new list, and
/// negative indexes are each checked again, and the last read fails as in the interpreter.
#[test]
fn an_index_is_checked_again_once_its_list_changes() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r"struct Bag:
    items: list[int]

    def drop_last(mut self):
        self.items.pop()

    def swap_in(mut self):
        self.items = [7]

def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    mut at = -1
    stdout.print(xs[at], xs[at])
    xs.pop()
    stdout.print(xs[at])
    mut bag = Bag(items=[1, 2, 3])
    at = 2
    stdout.print(bag.items[at])
    bag.items[at] = 9
    bag.swap_in()
    at = 0
    stdout.print(bag.items[at])
    bag.items = [4, 5, 6]
    at = 2
    stdout.print(bag.items[at])
    bag.drop_last()
    stdout.print(bag.items[at])
";
    for (name, how) in [("range", &["--llvm"][..]), ("range_release", &["--llvm", "--release"][..])] {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

/// Pieces only ever added to a `list[str]` and joined with `""` are kept as that string, f-strings
/// and `str` going straight on its end: added to in a function given it, joined before more is
/// added, and alongside lists looked at some other way, or joined with something between.
#[test]
fn pieces_joined_once_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def add(n: int, mut out: list[str]):
    if n > 0:
        out.append(f"<{n}:")
        add(n - 1, mut out)
        out.append(str(n))
        out.append(">")

def shown(items: list[str]) -> str:
    "-".join(items)

def put(mut out: list[str], more: list[str]):
    for item in more:
        out.append(item)

# Its list is given back, not joined, so `put` gets a list from here too.
def gathered(rows: list[list[str]]) -> list[str]:
    mut cells: list[str] = []
    for row in rows:
        put(mut cells, row)
    cells

def main(stdout: Stdout):
    mut out = ["start "]
    add(3, mut out)
    stdout.print("".join(out))
    mut empty: list[str] = []
    stdout.print("".join(empty).len())
    mut looked = ["a", "b"]
    looked.append("c")
    stdout.print(looked.len(), looked[2], shown(looked), "".join(looked))
    mut again = ["x"]
    first = "".join(again)
    again.append("y")
    stdout.print(first, "".join(again))
    mut words: list[str] = []
    for i in range(5):
        words.append(f"w{i}")
    stdout.print(", ".join(words))
    stdout.print(gathered([["x", "."], [".", "o"]]))
"#;
    for (name, how) in [("pieces", &["--llvm"][..]), ("pieces_release", &["--llvm", "--release"][..])] {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

/// `for at, item in enumerate(list)` makes no pairs when the list stays as it is, and still does
/// when the pairs are kept, looked at whole, or the list grows while it's gone through.
#[test]
fn enumerates_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def total(names: list[str]) -> int:
    mut sum = 0
    for at, name in enumerate(names):
        sum += at * name.len()
    sum

def main(stdout: Stdout):
    names = ["fox", "kit", "den"]
    stdout.print(total(names))
    for at, name in enumerate(names):
        if at > 0:
            stdout.print(at, name)
    pairs = enumerate(names)
    stdout.print(pairs.len(), pairs[1])
    mut grow = [1, 2, 3]
    for at, n in enumerate(copy(grow)):
        grow.append(n + at)
    stdout.print(grow)
    for at, word in enumerate(["a", "b"]):
        kept = (at, word)
        stdout.print(kept)
"#;
    for (name, how) in [("enumerates", &["--llvm"][..]), ("enumerates_release", &["--llvm", "--release"][..])]
    {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

/// `json.dumps` of plain structs, lists, strings and ints, and `json.loads` reading a struct, a
/// list of plain structs, and failing on bad text, as the interpreter does.
#[test]
fn json_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"from std import json

struct Item:
    name: str
    heals: int

struct Point:
    x: int
    y: int

struct Bag:
    items: list[Item]
    at: Point
    count = 0

def load(text: str) -> Bag:
    json.loads[Bag](text)?

def main(stdout: Stdout):
    b = Bag(items=[Item(name="a", heals=1)], at=Point(x=1, y=2))
    text = json.dumps(b)
    stdout.print(text, json.dumps(b.at), json.dumps([1, 2]), json.dumps("q"), json.dumps(3))
    back = load(text) else Bag(items=[], at=Point(x=0, y=0))
    stdout.print(back.items[0].name, back.at.y, back.count)
    points = json.loads[list[Point]]("[{\"x\": 3, \"y\": 4}]") else []
    stdout.print(points.len(), points[0].x)
    failed = load("{") else Bag(items=[], at=Point(x=9, y=9))
    stdout.print(failed.at.x)
"#;
    for (name, how) in [("json", &["--llvm"][..]), ("json_release", &["--llvm", "--release"][..])] {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

/// Skirmish built with LLVM prints its `.out` file, as `cli.rs` checks the interpreter and the
/// JIT do: a whole kit, saved to JSON and loaded back, cached and as a release.
#[test]
fn the_proof_program_built_with_llvm_prints_its_out_file() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let proof = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/proof");
    let expected = std::fs::read_to_string(proof.join("skirmish.out"))?;
    let dir = folder("skirmish")?;
    for how in [&["--llvm"][..], &["--llvm", "--release"]] {
        let executable = dir.join("skirmish");
        let output = executable.to_string_lossy().into_owned();
        let args: Vec<&str> =
            ["build"].into_iter().chain(how.iter().copied()).chain(["-o", &output]).collect();
        let built = crate::executables::fennec_command()
            .args(&args)
            .env("FENNEC_HOME", &dir)
            .current_dir(&proof)
            .output()?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output()?;
        assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), expected, "built with {how:?}");
    }
    std::fs::remove_dir_all(&dir)
}

/// A narrowed capability is a block owned like any other: made in a function, handed back,
/// copied and replaced each time round a loop, it still says what it allows when it's used.
#[test]
fn narrowed_capabilities_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"from std.process import Output

def narrowed(process: Process) -> Process:
    process.only("echo")

def main(stdout: Stdout, process: Process):
    mut kept = 0
    for i in range(2000):
        echo = narrowed(process)
        again = echo
        if i % 500 == 0:
            found = again.run("echo", [f"{i}"]) else Output()
            stdout.print(found.stdout.strip())
        kept += 1
    stdout.print(kept)
"#;
    for (name, how) in [("narrowed", &["--llvm"][..]), ("narrowed_release", &["--llvm", "--release"][..])] {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

/// A `Buffer` popped empty, or inserted into past its end, fails as the interpreter says, not
/// as a list does.
#[test]
fn an_empty_buffer_fails_to_pop_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "empty_buffer",
        "def main(stdout: Stdout):\n    mut kept = Buffer[str]()\n    kept.push(\"fox\")\n    stdout.print(kept.pop())\n    stdout.print(kept.pop())\n",
    )?;
    same_message_as_the_jit(
        "buffer_insert",
        "def main(stdout: Stdout):\n    mut kept = Buffer[str]()\n    kept.insert(0, \"fox\")\n    kept.insert(1, \"kit\")\n    stdout.print(kept[0], kept[1])\n    kept.insert(5, \"den\")\n",
    )
}

/// `Net` reaches native code as kind 7, and its methods run through the runtime by that kind,
/// so a wrong number would send `listen` to another capability.
#[test]
fn net_reaches_native_code_as_its_own_kind() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def main(stdout: Stdout, net: Net):\n    match net.listen(0):\n        case Ok(server):\n            stdout.print(\"listening\", server.port > 0)\n        case Err(_):\n            stdout.print(\"failed\")\n";
    same_as_the_jit("net_kind", source)
}

/// Reads of a field or item kept without an owner while nothing can change what they came
/// from: across calls, in loops, through a `match`'s fields. One returned, stored, kept past a
/// change, or copied out still takes an owner. `FENNEC_COUNTS` shows no copies.
#[test]
fn borrowed_reads_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"enum Tree:
    Leaf
    Node(left: Tree, right: Tree)

struct Item:
    name: str
    tags: list[str]
    weight: int

struct Shelf:
    items: list[Item]
    log: list[str]

    def weight_of(self, at: int) -> int:
        self.items[at].weight

    def names(mut self, mut into: list[str]):
        for at in range(self.items.len()):
            item = self.items[at]
            if self.weight_of(at) > 1:
                for tag in item.tags:
                    into.append(tag)
            into.append(item.name)

    def first_tags(self) -> list[str]:
        self.items[0].tags

    def relabel(mut self):
        tags = self.items[0].tags
        self.items[0] = Item(name="new", tags=["x"], weight=9)
        self.log.append(tags[0])

def size(tree: Tree) -> int:
    match tree:
        case .Leaf:
            1
        case .Node(left, right):
            1 + size(left) + size(right)

def main(stdout: Stdout):
    mut shelf = Shelf(items=[Item(name="fox", tags=["a", "b"], weight=2), Item(name="kit", tags=["c"], weight=1)], log=[])
    mut seen: list[str] = []
    shelf.names(mut seen)
    kept = shelf.first_tags()
    shelf.relabel()
    stdout.print(seen, kept, shelf.items[0].name, shelf.log)
    tree = Tree.Node(left=Tree.Node(left=Tree.Leaf, right=Tree.Leaf), right=Tree.Leaf)
    stdout.print(size(tree))
"#;
    same_as_the_jit("borrowed", source)?;
    let counts = counts_of("borrowed", source)?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// `list.insert` at the front, the middle and the end, of numbers and of strings, then past the
/// end, which fails as the interpreter says.
#[test]
fn a_list_insert_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "list_insert",
        "def main(stdout: Stdout):\n    mut ns = [2, 4]\n    ns.insert(0, 1)\n    ns.insert(2, 3)\n    ns.insert(4, 5)\n    mut words = [\"b\"]\n    words.insert(0, \"a\")\n    words.insert(2, \"c\")\n    stdout.print(ns, words)\n",
    )?;
    same_message_as_the_jit(
        "list_insert_past",
        "def main(stdout: Stdout):\n    mut ns = [1]\n    ns.insert(3, 2)\n    stdout.print(ns)\n",
    )
}

#[test]
fn an_optional_of_a_block_allocates_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def find(names: list[str], wanted: str) -> str?:\n    for name in names:\n        if name == wanted: return name\n    None\n\nstruct Seen:\n    last: str?\n\ndef main(stdout: Stdout):\n    names = [\"fox\", \"kit\", \"den\"]\n    mut seen = Seen(last=None)\n    mut hits = 0\n    for at in range(1000):\n        found = find(names, names[at % 3])\n        if found is not None:\n            hits += 1\n        seen.last = found\n    stdout.print(hits, seen.last, find(names, \"owl\"))\n";
    same_as_the_jit("optional", source)?;
    let counts = counts_of("optional", source)?;
    let allocations = counts
        .split_whitespace()
        .skip_while(|word| *word != "allocations")
        .nth(1)
        .and_then(|number| number.parse::<u64>().ok());
    assert!(allocations.is_some_and(|count| count < 100), "{counts}");
    Ok(())
}

#[test]
fn a_release_build_optimized_as_a_whole_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "struct Point:\n    x: int\n    y: int\n\n    def moved(self, by: int) -> Point:\n        Point(x=self.x + by, y=self.y - by)\n\ndef count_up(limit: int) -> Iterator[int]:\n    mut at = 0\n    while at < limit:\n        yield at\n        at += 1\n\ndef main(stdout: Stdout):\n    mut points = [Point(x=i, y=i * 2) for i in range(5)]\n    mut seen: dict[str, int] = {}\n    for point in points:\n        moved = point.moved(3)\n        seen[f\"{moved.x}\"] = moved.y\n    stdout.print(seen.len(), sum(n for n in count_up(10)), points[4].moved(1).y)\n    stdout.print([1, 2][5])\n";
    let (ours, jit) = both_built("release", source, &["--llvm", "--release"])?;
    assert_eq!((ours.0, ours.2), (jit.0, jit.2));
    assert_eq!(ours.1.lines().next(), jit.1.lines().next());
    Ok(())
}

#[test]
fn tuples_and_structs_compare_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "compare",
        r#"struct Point:
    x: int
    y: float
    name: str

def main(stdout: Stdout):
    a = (1, "b")
    b = (1, "c")
    c = (0, "z")
    stdout.print(a < b, b < a, c < a, a <= a, a == (1, "b"), a != b, (2, 1.5) > (2, 1.25))
    p = Point(x=1, y=2.0, name="fox")
    q = Point(x=1, y=2.0, name="fox")
    stdout.print(p == q, p != Point(x=1, y=2.5, name="fox"))
    mut items = [(3, "c"), (1, "z"), (1, "a"), (2, "b")]
    mut best = items[0]
    for item in items:
        if item < best:
            best = item
    first, second = best
    stdout.print(first, second)
"#,
    )
}

#[test]
fn id_maps_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "idmap",
        r#"from std.game import Id, IdMap

struct Unit:
    name: str
    health: int

def main(stdout: Stdout):
    mut units = IdMap[Unit]()
    a = units.insert(Unit(name="fox", health=10))
    b = units.insert(Unit(name="kit", health=5))
    units[a].health -= 3
    stdout.print(units[a].health, units.len(), units.get(b) is None)
    gone = units.remove(b)
    stdout.print(units.get(b) is None, units.len(), a == a, a == b)
    c = units.insert(Unit(name="den", health=7))
    ids: list[Id[Unit]] = units.ids()
    for id, unit in units.items():
        stdout.print(unit.name, unit.health)
    stdout.print(ids.len(), units.values().len(), c == b)
    if gone is not None:
        stdout.print(gone.name)
"#,
    )?;
    same_message_as_the_jit(
        "stale",
        "from std.game import IdMap\n\ndef main(stdout: Stdout):\n    mut names = IdMap[str]()\n    fox = names.insert(\"fox\")\n    names.remove(fox)\n    stdout.print(names[fox])\n",
    )
}

#[test]
fn slices_as_dict_keys_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "slices",
        r#"def main(stdout: Stdout):
    text = "abcabcabd"
    mut counts: dict[str, int] = {}
    for at in range(text.len() - 1):
        piece = text[at:at + 2]
        counts[piece] = (counts.get(piece) else 0) + 1
    for size in [1, 3]:
        for at in range(text.len()):
            part = text[at:at + size]
            counts[part] = (counts.get(part) else 0) + 1
    tail = text[-3:]
    counts[tail] = (counts.get(tail) else 0) + 10
    whole = text[:]
    counts[whole] = (counts.get(whole) else 0) + 1
    wide = "éaé"
    w = wide[0:2]
    counts[w] = (counts.get(w) else 0) + 1
    long_text = "the quick brown fox jumps over the lazy dog"
    for step in range(4):
        at = step * 5
        chunk = long_text[at:at + 12]
        counts[chunk] = (counts.get(chunk) else 0) + 1
    for key, value in sorted(counts.items()):
        stdout.print(key, value)
"#,
    )
}

#[test]
fn std_pieces_the_proof_program_uses_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    // Lists of lists and `dict.pop`.
    same_as_the_jit(
        "nested",
        r#"def main(stdout: Stdout):
    mut grid: list[list[int]] = [[], [1, 2]]
    grid[0].append(5)
    grid.append([7, 8, 9])
    copy_of = grid
    grid[1][0] = 42
    mut total = 0
    mut sum_of = 0
    for row in grid:
        for n in row:
            sum_of += n
    stdout.print(grid, copy_of, sum_of, grid[2].len())
    mut counts: dict[str, int] = {"a": 1, "b": 2}
    gone = counts.pop("a")
    stdout.print(gone, counts.len(), "a" in counts)
"#,
    )?;
    // std's `PriorityQueue`, which compares tuples and pops from a dict.
    same_as_the_jit(
        "queue",
        r#"from std.collections import PriorityQueue

def main(stdout: Stdout):
    mut queue: PriorityQueue[str, int] = PriorityQueue()
    for n in [5, 1, 4, 2, 3]:
        queue.push(f"job{n}", n)
    stdout.print(queue.peek() else "none", queue.len())
    while queue.len() > 0:
        stdout.print(queue.pop() else "none")
"#,
    )?;
    // f-strings of bools, tuples, plain structs and enums, and `list.pop(at)`.
    same_as_the_jit(
        "formats",
        r#"enum Team:
    Red
    Blue

struct Spot:
    x: int
    y: float
    on: bool
    team: Team

def main(stdout: Stdout):
    at = (3, 4)
    one = Spot(x=1, y=2.5, on=True, team=Team.Blue)
    flag = 3 > 2
    stdout.print(f"at {at} {one} {flag} {not flag} {Team.Red}")
    mut items = [5, 6, 7, 8]
    gone = items.pop(1)
    last = items.pop(-1)
    stdout.print(gone, last, items)
    mut spots = [Spot(x=9, y=0.5, on=False, team=Team.Red), one]
    first = spots.pop(0)
    stdout.print(f"{first}", spots.len())
"#,
    )?;
    // std's `Random`, drawn as the interpreter draws it, ending in its failure.
    same_message_as_the_jit(
        "random",
        r#"from std.random import Random

def main(stdout: Stdout):
    mut dice = Random(seed=42)
    mut total = 0
    for _ in range(1000):
        total += dice.randint(1, 6)
    stdout.print(total, dice.randint(-5, 5), dice.drawn)
    stdout.print(f"{dice.random():.6f}", f"{dice.uniform(2.0, 3.0):.6f}")
    mut wide = Random(seed=-7)
    stdout.print(wide.randint(-9223372036854775807, 9223372036854775807))
    stdout.print(dice.randint(3, 1))
"#,
    )
}

#[test]
fn a_method_checks_self_once_and_still_copies_a_shared_one() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "self_once",
        r"struct Counter:
    n: int
    log: list[int]

    def bump(mut self):
        self.n += 1
        self.log.append(self.n)
        self.n += 1

    def keep(mut self, mut into: list[Counter]):
        self.n += 10
        into.append(self)
        self.n += 10

def main(stdout: Stdout):
    mut a = Counter(n=0, log=[])
    b = a
    a.bump()
    a.bump()
    stdout.print(a.n, b.n, a.log, b.log)
    mut kept: list[Counter] = []
    a.keep(mut kept)
    stdout.print(a.n, kept[0].n)
",
    )
}

/// What's known to have one owner stops being known where something might give it another: a
/// method keeping a field, a function handing one back, a copy of an item, an item stored
/// twice, a new index, a loop keeping what it changes, and items moving along a list. Each
/// change after has to copy.
#[test]
fn a_field_something_keeps_is_checked_again() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r"struct Packet:
    a2: list[int]
    link: int

struct Box:
    items: list[int]
    saved: list[int]
    packets: list[Packet]

    def keep(mut self):
        self.saved = self.items

    def poke(mut self):
        self.items[0] = 1
        self.keep()
        self.items[0] = 99

    def touch(mut self, at: int):
        self.packets[at].a2[0] = 5
        self.packets[at].link = 6

def handed_back(b: Box) -> list[int]:
    b.items

def main(stdout: Stdout):
    mut b = Box(items=[0, 0], saved=[], packets=[Packet(a2=[0], link=0), Packet(a2=[0], link=0)])
    b.poke()
    stdout.print(b.items, b.saved)
    b.items[1] = 2
    held = handed_back(b)
    b.items[1] = 3
    stdout.print(b.items, held)
    mut at = 0
    b.packets[at].a2[0] = 1
    copied = b.packets[at]
    b.packets[at].a2[0] = 2
    stdout.print(copied.a2, b.packets[0].a2)
    b.packets[1] = b.packets[0]
    at = 1
    b.packets[at].a2[0] = 3
    stdout.print(b.packets[0].a2, b.packets[1].a2)
    mut seen: list[list[int]] = []
    for i in range(3):
        b.items[0] = i
        seen.append(b.items)
    stdout.print(seen)
    b.packets[0] = b.packets[1]
    b.touch(0)
    stdout.print(b.packets[0].a2, b.packets[1].a2, b.packets[0].link, b.packets[1].link)
";
    // Items move along when one before them goes, so the item now at an index is another.
    let shifted = r"struct Packet:
    a2: list[int]
    link: int

def main(stdout: Stdout):
    shared = Packet(a2=[0], link=0)
    mut xs = [Packet(a2=[0], link=0), Packet(a2=[0], link=0), shared]
    mut at = 1
    xs[at].a2[0] = 2
    xs.pop(0)
    xs[at].a2[0] = 3
    stdout.print(shared.a2, xs[0].a2, xs[1].a2)
";
    for (name, how) in [("kept", &["--llvm"][..]), ("kept_release", &["--llvm", "--release"][..])] {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
        let (ours, jit) = both_built(&format!("{name}_shifted"), shifted, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

#[test]
fn failures_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let print_after = "\n\ndef main(stdout: Stdout):\n    stdout.print(\"before\")\n";
    same_as_the_jit(
        "overflow",
        &format!("def f(n: int) -> int:\n    n * 9223372036854775807{print_after}    stdout.print(f(3))\n"),
    )?;
    same_as_the_jit(
        "zero",
        &format!("def f(n: int) -> int:\n    10 // n{print_after}    stdout.print(f(0))\n"),
    )?;
    same_as_the_jit(
        "outside",
        &format!("def f(n: int) -> int:\n    [1, 2][n]{print_after}    stdout.print(f(2))\n"),
    )?;
    same_message_as_the_jit(
        "before",
        &format!(
            "def f(n: int) -> int:\n    mut a = [1, 2]\n    a[n] = 3\n    a[0]{print_after}    stdout.print(f(-3))\n"
        ),
    )?;
    // A format Fennec doesn't have is caught when the program's checked, before anything runs.
    let dir = folder("spec")?;
    let program = dir.join("spec.fen");
    std::fs::write(
        &program,
        format!("def f(n: int) -> int:\n    n{print_after}    stdout.print(f\"{{f(1):z}}\")\n"),
    )?;
    let output = dir.join("spec").to_string_lossy().into_owned();
    let built = fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &output], &dir)?;
    assert!(!built.status.success());
    assert!(String::from_utf8_lossy(&built.stderr).contains("Fennec has no format `z`"));
    std::fs::remove_dir_all(&dir)?;
    same_as_the_jit(
        "deep",
        &format!("def f(n: int) -> int:\n    f(n + 1) + 1{print_after}    stdout.print(f(0))\n"),
    )
}

#[test]
fn an_edit_recompiles_only_the_function_it_touches() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("edit")?;
    let program = dir.join("edit.fen");
    let source = |line: &str| {
        format!(
            "{line}def big(n: int) -> int:\n    mut total = n\n    for i in range(n):\n        total = total + i * 2 + 1\n        total = total + big_two(i)\n    total\n\ndef big_two(n: int) -> int:\n    if n > 1: return big_two(n - 1) + 3\n    n * 3 + 5 + n * 7 + n * 9 + n * 11 + n * 13 + n * 15 + n * 17 + n * 19 + n * 21 + n * 23 + n * 25 + n * 27 + n * 29 + n * 31 + n * 33\n\ndef main(stdout: Stdout):\n    stdout.print(big(3))\n"
        )
    };
    let (path, out) =
        (program.to_string_lossy().into_owned(), dir.join("edit").to_string_lossy().into_owned());
    let build = |text: String| -> std::io::Result<String> {
        std::fs::write(&program, text)?;
        let built = fennec(&["build", "--llvm", &path, "-o", &out], &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        Ok(String::from_utf8_lossy(&built.stdout).into_owned())
    };
    assert!(build(source(""))?.contains("compiled 3 of 3"));
    assert!(build(source("## nothing here\n\n"))?.contains("compiled 0 of 3"));
    assert!(build(source("").replace("n * 33", "n * 34"))?.contains("compiled 1 of 3"));
    std::fs::remove_dir_all(&dir)
}

/// ASCII is stripped here, sharing a string with nothing to take off; other text goes to the
/// runtime.
#[test]
fn strips_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "strips",
        r#"def main(stdout: Stdout):
    for s in ["", "   ", "x", " x", "x ", "\t a b \n", "\x0b\x0cq\r", "héllo  ", "  ünï  ", "abc", "  long text with spaces inside   "]:
        stdout.print(f"[{s.strip()}] [{s.lstrip()}] [{s.rstrip()}] {s.strip().len()}")
    mut total = 0
    for i in range(1000):
        line = f"  {i} " if i % 2 == 0 else f"{i}"
        total += line.strip().len()
    stdout.print(total)
"#,
    )
}

/// `upper`, `lower`, `replace`, `find` and `split`, which run the interpreter's own code.
#[test]
fn string_methods_the_runtime_runs_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "string_methods",
        r#"def main(stdout: Stdout):
    words = ["Hello", "wörld", "", "MiXeD 123"]
    for w in words:
        stdout.print(w.upper(), w.lower(), w.replace("l", "L"), w.find("l"), w.find(""))
    stdout.print("a b  c".split(), "a,b,,c".split(","), "".split(), "xyx".split("y"))
"#,
    )
}

/// Strings let go of where nothing takes them, and a `str?` from a dict of strings.
#[test]
fn strings_let_go_of_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "strings_let_go_of",
        r#"def main(stdout: Stdout):
    mut parts = ["a", "a much longer string than a header holds", "b", "c"]
    parts.pop()
    parts.pop(0)
    parts.remove_where((s) => s.len() > 10)
    stdout.print(parts)
    names: dict[str, str] = {"one": "uno", "long": "a value longer than twenty-three bytes"}
    for key in ["one", "long", "none"]:
        found: str? = names.get(key)
        stdout.print(found else "missing")
"#,
    )
}

/// A result prints as the interpreter writes it, `Ok(...)` or `Err(...)`, alone or in a list.
#[test]
fn results_print_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "results_print",
        r#"def check(n: int) -> Result[int, str]:
    if n > 0:
        return Ok(n)
    Err("not positive")

def main(stdout: Stdout):
    stdout.print(check(3), check(-1))
    results = [check(1), check(0)]
    stdout.print(results)
"#,
    )
}

/// String keys held in a dict's entries as headers: every length from empty to past a header's
/// 23 bytes, made from literals, slices, `+` and the runtime, so inline and block keys of the
/// same text always meet as the same key.
#[test]
fn string_keys_in_place_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "string_keys",
        r#"def main(stdout: Stdout):
    letters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJ"
    mut counts: dict[str, int] = {}
    for size in range(31):
        counts[letters[:size]] = size
    held = letters[:30]
    for size in range(31):
        made = "".join([letters[at] for at in range(size)])
        counts[made] = (counts.get(made) else 0) + 100
    for word in "the quick brown fox jumps over the lazy dog and a much longer word than a header holds".split():
        counts[word] = (counts.get(word) else 0) + 1
    stdout.print(counts.len(), counts[""], counts["abc"], counts[held], counts[letters[:23]], counts[letters[:24]])
    stdout.print(counts.pop("the"), counts.pop(letters[:24]), counts.len())
    counts["the"] = 7
    counts[letters[:24]] = 8
    stdout.print(counts["the"], counts[letters[:24]], counts.len(), list(counts.keys())[-2:])
    mut order: list[str] = []
    for key in counts.keys():
        if key.len() < 3:
            order.append(key)
    stdout.print(order, counts.get("missing") else -1)
    mut seen: set[str] = {"dog"}
    for word in ["fox", "fox", "a much longer word than a header holds", "dog"]:
        seen.add(word)
    stdout.print(seen.len(), "fox" in seen, "cat" in seen)
"#,
    )
}

/// Reading a string key a dict doesn't have fails with the key in the message.
#[test]
fn a_missing_string_key_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "missing_string_key",
        r#"def main(stdout: Stdout):
    counts: dict[str, int] = {"fox": 1}
    stdout.print(counts["a key long enough to live in a block"])
"#,
    )
}

/// A float sum over a generator, whose start the checker writes in by name, as an int's isn't.
#[test]
fn float_sums_over_generators_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "float_sums",
        r"struct U:
    h: float

def main(stdout: Stdout):
    us = [U(1.5), U(2.0), U(-0.25)]
    stdout.print(sum(u.h for u in us), sum(u.h for u in us if u.h > 0.0))
    stdout.print(sum((u.h for u in us), start=1.0), sum(1 if u.h > 0.0 else 0 for u in us))
",
    )
}

/// Short strings made by every path that copies bytes into a header, at every length that fits
/// and just past it, in ASCII and not.
#[test]
fn short_copies_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "short_copies",
        r#"def main(stdout: Stdout):
    for letters in ["abcdefghijklmnopqrstuvwxyz0123456789", "héllo wörld ünï çà ñé ßø åæ"]:
        for size in range(27):
            part = letters[:size]
            other = letters[3:3 + size]
            stdout.print(size, part, other, part + "!", "!" + part, part + other, f"[{part}]", f"{size}:{part}")
            padded = f"  {part}  "
            stdout.print(padded.strip(), padded.lstrip(), padded.rstrip(), part.strip() == part)
    mut grown = "a block long enough that it isn't a header at all: "
    for size in range(27):
        grown += "x" * size
    stdout.print(grown, grown.len())
"#,
    )
}

/// A program holding enums with strings and lists in every way: each case's blocks are owned by
/// whatever holds the enum.
const ENUMS_HOLDING_BLOCKS: &str = r#"enum Token:
    Blank
    Word(text: str)
    Num(value: int)
    Many(items: list[int], label: str)

enum First:
    Named(name: str)
    Count(value: int)

struct Holder:
    token: Token
    tag: str
    size: int

struct Person:
    name: str
    age: int

enum Wrap:
    Empty
    Who(person: Person)
    Inner(token: Token)

def word(text: str) -> Token:
    Token.Word(text)

def describe(token: Token) -> str:
    match token:
        case .Blank:
            "blank"
        case .Word(text):
            f"word {text}"
        case .Num(value):
            f"num {value}"
        case .Many(items, label):
            f"{label}: {items.len()} items, first {items[0]}"

def replace(mut token: Token, text: str):
    token = Token.Word(text)

def pick(at: int) -> Token?:
    if at == 0:
        return None
    if at == 1:
        return Token.Word("a string long enough that it can't be held inline")
    if at == 2:
        return Token.Word("short")
    Token.Many([at], "label")

def someone(at: int) -> Person?:
    if at == 0:
        return None
    Person("somebody with a name long enough to be a block of its own", at)

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    tokens = [Token.Blank, word("short"), word(long), Token.Num(7), Token.Many([1, 2, 3], long)]
    for token in tokens:
        stdout.print(describe(token))
        stdout.print(token)
    # Copies, and changes after copying.
    mut a = word(long + "!")
    b = a
    a = Token.Num(1)
    stdout.print(describe(a), describe(b))
    mut c = copy(b)
    replace(mut c, "changed")
    stdout.print(describe(b), describe(c))
    # Reassigning in a loop lets go of what it held.
    mut last = Token.Blank
    for at in range(1000):
        last = word(f"a long string number {at} that doesn't fit inline")
    stdout.print(describe(last))
    # Equality.
    same = [word(long) == word(long), word("x") == word("x"), word("x") == word("y"),
        word("x") != Token.Num(1), Token.Blank == Token.Blank, Token.Num(1) == Token.Num(1),
        Token.Many([1], "a") == Token.Many([1], "a"), Token.Many([1], "a") == Token.Many([2], "a"),
        word(long[0:30]) == word(long[0:30])]
    stdout.print(same)
    # In structs and lists.
    held = Holder(word(long), "tag", 3)
    other = held
    stdout.print(describe(held.token), held == other, held.tag)
    stdout.print(held)
    mut list_of = [word("one")]
    for at in range(100):
        list_of.append(word(f"item number {at} long enough not to be held inline"))
    list_of[0] = Token.Num(9)
    popped = list_of.pop()
    stdout.print(list_of.len(), describe(list_of[0]), describe(list_of[50]), describe(popped))
    # First case holding a string, moved out and let go of at the end.
    mut first = First.Named(long)
    moved = first
    first = First.Named("other")
    stdout.print(first, moved)
    # A struct inside a case, and an enum inside a case.
    wrap = Wrap.Who(Person(long, 3))
    wrapped = [wrap, Wrap.Empty, Wrap.Inner(word(long))]
    for item in wrapped:
        stdout.print(item)
    stdout.print(wrapped[0] == wrap, wrapped[2] == wrap)
    # Optionals of them.
    for at in range(4):
        found = pick(at)
        stdout.print(found)
        if found is not None:
            stdout.print(describe(found))
    mut picked = pick(1)
    for at in range(100):
        picked = pick(at % 4)
    stdout.print(picked)
    who = someone(1)
    stdout.print(who)
"#;

/// The same enums read and written as JSON, and as what a result holds.
const ENUMS_IN_JSON_AND_RESULTS: &str = r#"from std import json


enum Token:
    Blank
    Word(text: str)
    Num(value: int)

enum Failure:
    Plain
    Why(reason: str)

enum Shape:
    Empty
    Circle(radius: float)
    Rect(w: float, h: float)

struct Person:
    name: str
    age: int

struct Doc:
    token: Token
    tokens: list[Token]
    name: str

def get(at: int) -> Result[int, Failure]:
    if at == 0:
        return Err(Failure.Why("a reason long enough to be held as a block of its own"))
    if at == 1:
        return Err(Failure.Plain)
    Ok(at)

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    # Results holding them.
    for at in range(3):
        match get(at):
            case Ok(value):
                stdout.print("ok", value)
            case Err(failure):
                stdout.print("err", failure)
    # JSON of them.
    d = Doc(Token.Word(long), [Token.Blank, Token.Num(2), Token.Word("x")], long)
    text = json.dumps(d)
    stdout.print(text)
    back = json.loads[Doc](text) else Doc(Token.Blank, [], "")
    stdout.print(back == d, back)
    token = json.loads[Token](json.dumps(Token.Word(long))) else Token.Blank
    stdout.print(token)
    tokens = json.loads[list[Token]](json.dumps(d.tokens)) else []
    stdout.print(tokens)
    # An expected result whose error is one.
    stdout.print(get(5).expect("five is fine"))
    # Enums that hold numbers only, printed.
    stdout.print(Shape.Circle(1.5), Shape.Empty, [Shape.Rect(1.0, 2.0)])
    stdout.print(f"{Shape.Rect(1.0, 2.0)}")
"#;

/// Everything that holds, copies, compares or shows an enum in place, in every case.
#[test]
fn enums_holding_blocks_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("enums_holding_blocks", ENUMS_HOLDING_BLOCKS)?;
    same_as_the_jit("enums_in_json_and_results", ENUMS_IN_JSON_AND_RESULTS)
}

/// `parallel` puts items back by their case, letting go of what each item held.
#[test]
fn parallel_loops_over_enums_holding_blocks_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "parallel_enums",
        r#"enum Token:
    Blank
    Word(text: str)
    Many(items: list[int], label: str)

def describe(token: Token) -> str:
    match token:
        case .Blank:
            "blank"
        case .Word(text):
            text
        case .Many(items, label):
            f"{label} {items.len()}"

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    mut tokens = [Token.Word(long), Token.Blank, Token.Many([1, 2], long), Token.Word("short")]
    for mut item in parallel(tokens):
        item = Token.Word("changed, and long enough that it can't be held inline")
    stdout.print(tokens)
    stdout.print([describe(item) for item in parallel(tokens)])
    mut more = [Token.Word(long + "!") for at in range(200)]
    for mut item in parallel(more):
        if describe(item).len() > 10:
            item = Token.Blank
    stdout.print(more[0], more[199], sum(describe(item).len() for item in parallel(more)))
"#,
    )
}

/// What's written whole through a `mut` parameter belongs to the caller's local, and what `pop`
/// and `remove_where` take out of a list keeps or loses the owners the list had. A string made
/// for the call has no other owner, so an owner too few frees it while it's still in use.
#[test]
fn in_place_values_keep_their_owners_through_mut_params_and_pops() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "in_place_owners",
        r#"struct Person:
    name: str
    age: int

enum Token:
    Blank
    Word(text: str)

def rename(mut person: Person, name: str):
    person = Person(name, 2)

def set_person(mut person: Person, other: Person):
    person = other

def set_token(mut token: Token, other: Token):
    token = other

def retext(mut token: Token, text: str):
    token = Token.Word(text)

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    mut total = 0
    for at in range(2000):
        mut person = Person(long, 1)
        rename(mut person, long + "!")
        keep = Person(long + "?", 3)
        total += person.name.len()
        set_person(mut person, keep)
        total += person.name.len() + keep.name.len()
        mut token = Token.Word(long)
        retext(mut token, long + "!")
        other = Token.Word(long + "?")
        same = token == other
        set_token(mut token, other)
        mut people = [Person(long + "a", 1), Person(long + "b", 2), Person(long + "c", 3)]
        gone = people.pop()
        first = people.pop(0)
        people.pop()
        mut tokens = [Token.Word(long + "a"), Token.Blank, Token.Word(long + "b"), Token.Word(long + "c")]
        last = tokens.pop()
        early = tokens.pop(0)
        tokens.pop(0)
        tokens.remove_where((item) => item == Token.Blank)
        total += gone.name.len() + first.name.len() + people.len() + tokens.len()
        if same or last == early:
            total += 1
    stdout.print(total)
"#,
    )
}

/// Keys are hashed and compared by their words, so one holding a string can't be: two equal
/// strings can be two blocks. Native code turns those dicts down, and the JIT takes them.
#[test]
fn keys_holding_strings_go_to_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Person:
    name: str
    age: int

def main(stdout: Stdout):
    mut by_person: dict[Person, int] = {}
    by_person[Person("a string long enough that it can't be held inline", 1)] = 1
    stdout.print(by_person.len())
"#;
    let dir = folder("keys_holding_strings")?;
    let program = dir.join("keys_holding_strings.fen");
    std::fs::write(&program, source)?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("out"));
    let built = fennec(&["build", "--llvm", &program, "-o", &output.to_string_lossy()], &dir)?;
    assert!(!built.status.success());
    assert!(String::from_utf8_lossy(&built.stderr).contains("Dict(Named(\"Person\"), Int)"));
    let ran = fennec(&["run", &program], &dir)?;
    assert!(ran.status.success());
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "1\n");
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();
    assert!(said.starts_with("note: running this with the JIT, since LLVM can't build it: `main`"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A constant that isn't written out is worked out once. Reading it this often would take minutes
/// if each read worked it out again.
#[test]
fn a_constant_is_worked_out_once() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def slow() -> int:\n    mut total = 0\n    for i in range(2000000):\n        total += i\n    total\n\n\
                  SLOW = slow()\n\ndef main(stdout: Stdout):\n    mut seen = 0\n    for i in range(300000):\n        \
                  seen += SLOW + i\n    stdout.print(seen)\n";
    let dir = folder("constant_once")?;
    let program = dir.join("once.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("once"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours.0, "599999744999850000\n");
    Ok(())
}

/// A constant keeps its value whatever a read does with what it was given.
#[test]
fn each_read_of_a_constant_has_its_own_value() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Point:
    x: int
    tags: list[str]

enum Shape:
    Dot
    Named(words: list[str])

WORDS = ["a", "b"]
LENGTHS = {word: word.len() for word in WORDS}
ORIGIN = Point(x=0, tags=["start"])
NAMED = Shape.Named(words=["n"])
TEXT = f"{WORDS[0]} and a long tail of text"

def changed() -> list[str]:
    mut mine = WORDS
    mine.append("c")
    mine[0] = "z"
    mine

def main(stdout: Stdout):
    stdout.print(changed(), changed(), WORDS)
    mut counts = LENGTHS
    counts["new"] = 3
    stdout.print(counts, LENGTHS)
    mut point = ORIGIN
    point.x = 5
    point.tags.append("moved")
    stdout.print(point, ORIGIN)
    match NAMED:
        case .Named(words):
            stdout.print(words.len())
        case .Dot:
            pass
    stdout.print(NAMED, TEXT, TEXT.len())
"#;
    same_as_the_jit("constant_reads", source)
}

/// Conversions and small builtins: floats to ints, `PI`, `range` and `reversed` as lists, and a
/// dict's or set's keys as a list.
#[test]
fn conversions_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "conversions",
        r#"from std.math import PI, ceil, floor

def main(stdout: Stdout):
    for x in [2.5, -2.5, 3.5, 0.49, -0.51, 7.0]:
        stdout.print(int(x), round(x), floor(x), ceil(x), x.abs())
    stdout.print(PI > 3.14, PI < 3.15)
    stdout.print(range(5), range(2, 6), range(10, 0, -3), range(0, 10, 4), range(3, 3), range(5, 0))
    stdout.print(sorted(range(4), key=(n) => -n), range(-3, 3, 2))
    stdout.print(reversed([1, 2, 3]), reversed(["a", "b long enough to be a block of its own", "c"]))
    counts = {"b": 2, "a": 1, "c": 3}
    stdout.print(list(counts), list({3, 1, 2}))
    mut seen: set[int] = set()
    seen.add(4)
    copied = set(seen)
    seen.add(5)
    stdout.print(seen, copied)
"#,
    )
}

/// A float outside an int's range doesn't become one.
#[test]
fn a_float_too_big_for_an_int_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "float_too_big",
        "def main(stdout: Stdout):
    huge = 1e300
    stdout.print(int(huge * huge / huge))
",
    )
}

/// Optional strings going into a list, whose items hold an optional as an enum.
#[test]
fn optional_strings_in_lists_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "optional_strings_in_lists",
        r#"def first(words: list[str]) -> str?:
    if words.len() > 0:
        return words[0]
    None

def main(stdout: Stdout):
    mut found = []
    found.append(first(["short"]))
    found.append(first([]))
    found.append(first(["a string long enough that it has a block of its own"]))
    stdout.print(found, found.len())
    for item in found:
        stdout.print(item else "missing")
"#,
    )
}

/// A failure in a function with nothing else in it that can fail still says what went wrong.
#[test]
fn failing_calls_alone_in_a_function_fail_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, call) in [
        ("alone_int", "int(1e300 * 1e300)"),
        ("alone_floor", "floor(1e300 * 1e300)"),
        ("alone_ceil", "ceil(1e300 * 1e300)"),
        ("alone_round", "round(1e300 * 1e300)"),
        ("alone_range", "range(0, 1, 0)"),
        ("alone_max", "max(empty)"),
        ("alone_min", "min(empty)"),
        ("alone_shift", "1 << 64"),
    ] {
        same_message_as_the_jit(
            name,
            &format!(
                "from std.math import ceil, floor

def main(stdout: Stdout):
    empty: list[int] = []
    stdout.print(empty.len())
    stdout.print({call})
"
            ),
        )?;
    }
    Ok(())
}

/// What a program prints through LLVM is what the interpreter prints.
fn same_as_the_interpreter(name: &str, source: &str) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    let interpreted = outcome(&fennec(&["run", "--interp", &program], &dir)?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, interpreted);
    Ok(())
}

/// Evaluation runs left to right: an operand keeps the value it had when it was read, even if one
/// after it changes what it names. The JIT doesn't on a plain call, so this is held to what the
/// interpreter prints.
#[test]
fn an_earlier_operand_keeps_its_value_when_a_later_one_changes_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter(
        "left_to_right",
        r#"struct Box:
    items: list[int]
    count: int

    def grow(mut self) -> int:
        self.items.append(9)
        self.count = self.count + 1
        self.count

struct Pair:
    first: list[int]
    second: int

def show(a: list[int], b: int) -> str:
    f"{a} {b}"

def take(mut a: list[int]) -> int:
    a.pop()

def pair(a: int, b: int) -> str:
    f"{a} {b}"

def bump(mut a: int) -> int:
    a = a + 1
    a

def through(called: (list[int], int) -> str, mut items: list[int]) -> str:
    called(items, items.pop())

def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    stdout.print(xs, xs.pop())
    stdout.print(xs)
    mut ys = [1, 2, 3]
    stdout.print(show(ys, ys.pop()), ys)
    mut zs = [1, 2, 3]
    stdout.print(show(zs, take(mut zs)), zs)
    mut d = {1: 2, 3: 4}
    stdout.print(d, d.pop(1), d)
    mut e = {1: 2, 3: 4}
    stdout.print(e.len(), e.pop(1), e.len())
    mut n = 1
    stdout.print(pair(n, bump(mut n)), n)
    stdout.print(n, bump(mut n), n)
    mut b = Box(items=[1], count=0)
    stdout.print(b.count, b.grow(), b.count)
    stdout.print(b.items, b.grow(), b.items)
    stdout.print(b, b.grow(), b)
    mut q = [1]
    stdout.print([q, [q.pop()]], q)
    mut r = [1, 2]
    stdout.print((r, r.pop()), r)
    mut s = [1, 2]
    stdout.print(f"{s} {s.pop()} {s}")
    mut u = [1, 2]
    stdout.print(show(u, show(u, u.pop()).len()), u)
    mut v = [1, 2]
    stdout.print(v[0], v.pop(0), v[0])
    mut w = [1, 2, 3]
    stdout.print(w[w.pop() - 3], w)
    mut x = [1, 2, 3]
    stdout.print(x[:x.pop()], x)
    mut f = 2
    stdout.print(f + bump(mut f), f)
    mut i = [1]
    stdout.print(Pair(first=i, second=i.pop()), i)
    mut j = [1]
    stdout.print(Pair(second=j.pop(), first=j), j)
    mut m = [1, 2]
    stdout.print([m.pop(), m.len(), m.pop(), m.len()], m)
    mut o = [1, 2]
    stdout.print(through((a, b) => show(a, b), mut o), o)
    mut t = {1: 1, 2: 2}
    stdout.print(t.get(t.pop(2) - 1), t.get(t.pop(1)), t)
    mut aa = "a"
    mut ab = [aa]
    stdout.print(ab, ab.pop(), aa)
"#,
    )
}

/// `T.make()` calls the function of the type each copy of a generic stands for. It's native only:
/// the interpreter has no types at run time to find it through.
#[test]
fn a_type_parameter_calls_its_types_function() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "interface Made:\n    def make() -> Self\n\n\
                  struct Part:\n    x = 1\n\n    def make() -> Self:\n        Self(x=7)\n\n\
                  struct Other:\n    name = \"\"\n\n    def make() -> Self:\n        Self(name=\"other\")\n\n\
                  def build[T: Made]() -> T:\n    T.make()\n\n\
                  def twice[T: Made]() -> list[T]:\n    [build[T](), T.make()]\n\n\
                  def main(stdout: Stdout):\n    stdout.print(build[Part]().x, build[Other]().name)\n    \
                  made: Part = build()\n    stdout.print(made.x, twice[Other]().len())\n";
    let dir = folder("type_param_function")?;
    let program = dir.join("made.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("made"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours.0, "7 other\n7 2\n");
    Ok(())
}

/// `max` and `min` of lists and of values, with keys and a type's own `<`, as the first of equals
/// wins; `in` and `remove` on lists of each kind of item; `sin`, `cos` and `str` of what isn't text.
#[test]
fn small_calls_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "small_calls",
        r#"from std.math import cos, sin

struct Money:
    cents: int

    def <(self, other: Money) -> bool:
        self.cents > other.cents

struct Spot:
    x: int
    y: int

def main(stdout: Stdout):
    stdout.print(max(1, 3, 2), min(1, 3, 2), max(1.5, -0.5), min(-0.0, 0.0), max("a", "b long enough for a block"))
    stdout.print(max([3, 1, 3]), min([3, 1, 1]), max([0.5, 2.5]), min(["pear", "apple"]), max([False, True]))
    stdout.print(max([(1, "b"), (2, "a"), (2, "b")]), min([(1, 2.5), (1, 1.5)]))
    stdout.print(max(["aa", "bbb", "ccc"], key=word => word.len()), min(["aa", "bbb", "c"], key=word => word.len()))
    stdout.print(max([1, 2, 3], key=n => -n), min([1.5, -2.5], key=n => n * n), max([1, 2], key=n => str(n)))
    wallet = [Money(cents=5), Money(cents=2), Money(cents=9)]
    stdout.print(max(wallet), min(wallet), max(wallet[0], wallet[1]), min(wallet[0], wallet[1]))
    stdout.print(2 in [1, 2], 3 in [1, 2], 2.5 in [2.5], True in [False], "b" in ["a", "b"], "long enough for a block of its own" in ["no"])
    stdout.print((1, 2) in [(1, 2)], Spot(x=1, y=2) in [Spot(x=1, y=2)], Spot(x=2, y=2) in [Spot(x=1, y=2)], (1, "a") in [(1, "a")], [1] in [[1], [2]])
    mut numbers = [1, 2, 3, 2]
    shared = numbers
    numbers.remove(2)
    stdout.print(numbers, shared)
    mut words = ["a", "a string long enough to be a block of its own", "c"]
    words.remove("a string long enough to be a block of its own")
    stdout.print(words)
    mut spots = [Spot(x=1, y=2), Spot(x=3, y=4)]
    spots.remove(Spot(x=1, y=2))
    stdout.print(spots)
    mut pairs = [(1, "a"), (2, "b")]
    pairs.remove((2, "b"))
    stdout.print(pairs)
    stdout.print(sin(0.5), cos(0.5), sin(1), cos(0))
    stdout.print(int(5), float(2.5), str(True), str(False), str([1, 2]), str(Spot(x=1, y=2)), str((1, "a")))
"#,
    )
}

/// `max` of nothing, and removing what isn't there, fail as the JIT says.
#[test]
fn small_calls_fail_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "max_of_nothing",
        "def main(stdout: Stdout):
    mut none = [1]
    none.pop()
    stdout.print(max(none))
",
    )?;
    same_message_as_the_jit(
        "remove_missing",
        "def main(stdout: Stdout):
    mut some = [1]
    some.remove(2)
    stdout.print(some)
",
    )
}

/// Numbers read from text, with what fails being a value the program holds; and lists and sets of
/// a string's characters and a list's items.
#[test]
fn numbers_and_sets_from_text_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "numbers_from_text",
        r#"from std.re import Regex

def add(a: str, b: str) -> int:
    int(a)? + int(b)?

def main(stdout: Stdout):
    for text in ["42", " -7 ", "4x2", "", "99999999999999999999"]:
        match int(text):
            case Ok(n):
                stdout.print(n)
            case Err(error):
                stdout.print(error.message)
    for text in ["2.5", " 1e3 ", "nope", "a \"quoted\" value long enough for a block"]:
        match float(text):
            case Ok(x):
                stdout.print(x)
            case Err(error):
                stdout.print(error.message)
    stdout.print(int("12") else 0, int("x") else -1, float("0.5") else 0.0, float("y") else -1.5)
    stdout.print(add("1", "2") else -1, add("1", "two") else -1)
    for pattern in ["[0-9]+", "[0-9+"]:
        match Regex(pattern):
            case Ok(regex):
                stdout.print(regex.pattern)
            case Err(error):
                stdout.print(error.message)
    stdout.print(list("héllo"), list(""), set("hello").len(), "l" in set("hello"), set(""))
    stdout.print(set([3, 1, 3, 2, 1]), set(["b", "a", "b", "a key long enough for a block", "a key long enough for a block"]))
    stdout.print(set([(1, 2), (1, 2), (3, 4)]).len())
"#,
    )
}

/// A failed `assert` says all the JIT does, each side as the interpreter shows it.
#[test]
fn a_failed_assert_says_what_the_jit_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let setup = "struct Point:
    x: int
    label: str

struct Plain:
    x: int
    y: float

struct Shown:
    x: int

    def str(self, mut out: StrWriter):
        out.write(\"its own text\")

enum Light:
    Red
    Green

def seen(present: bool) -> int?:
    if present:
        return 3
    None

def named(present: bool) -> str?:
    if present:
        return \"hi\"
    None
";
    for (number, condition) in [
        "1 + 1 == 3",
        "2 > 3",
        "2.5 <= 1.5",
        "True != True",
        "\"a\\nb\" == \"c\\\"d\"",
        "[1].len() >= 2",
        "[1, 2] == [1, 3]",
        "[\"a\"] == [\"b\"]",
        "{\"a\": 1} == {\"a\": 2}",
        "\"q\" in [\"a\"]",
        "Plain(x=1, y=2.0) == Plain(x=2, y=2.0)",
        "Point(x=1, label=\"p\") == Point(x=2, label=\"q\")",
        "Shown(x=1) != Shown(x=1)",
        "Light.Red == Light.Green",
        "seen(True) is None",
        "seen(False) is not None",
        "named(True) is None",
        "named(False) is not None",
        "seen(True) == seen(False)",
        "3 in [1, 2]",
        "1 not in [1, 2]",
        "\"z\" in {\"a\": 1}",
        "\"z\" in \"abc\"",
        "False",
        "1 == 1 and 1 == 2",
        "not True",
    ]
    .iter()
    .enumerate()
    {
        let source =
            format!("{setup}\ndef main(stdout: Stdout):\n    stdout.print(1)\n    assert {condition}\n");
        let ((ours_out, ours_err, ours_code), (jit_out, jit_err, jit_code)) =
            both(&format!("assert_{number}"), &source)?;
        assert_eq!((&ours_out, ours_code), (&jit_out, jit_code), "{condition}");
        assert_eq!(ours_err, jit_err, "{condition}");
    }
    Ok(())
}

/// Shifts, `~`, sorting a list where it is, a `match` on a bool and `random_choice`.
#[test]
fn bits_sorts_and_choices_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "bits_sorts_and_choices",
        r#"from std.random import Random

def name(flag: bool) -> str:
    match flag:
        case True:
            "yes"
        case False:
            "no"

struct Bag:
    numbers: list[int]

def main(stdout: Stdout):
    for count in [0, 1, 63]:
        stdout.print(1 << count, -8 >> count, 255 >> count, ~count)
    mut items = [3, 1, 2]
    shared = items
    items.sort()
    stdout.print(items, shared)
    items.sort(key=n => -n)
    stdout.print(items, shared)
    mut words = ["pear", "fig", "a word long enough to be held in a block"]
    words.sort(key=word => word.len())
    stdout.print(words)
    words.sort()
    stdout.print(words)
    mut bag = Bag(numbers=[2, 1])
    bag.numbers.sort()
    stdout.print(bag)
    stdout.print(name(True), name(False))
    mut random = Random(7)
    stdout.print(random.choice([1, 2, 3, 4]), random.choice(["a", "b", "a string long enough to be a block"]), random.choice([1.5, 2.5]))
    stdout.print(random.choice([(1, "one"), (2, "two")]))
"#,
    )
}

/// A shift by a count that isn't one fails as the JIT says.
#[test]
fn a_bad_shift_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "bad_shift",
        "def main(stdout: Stdout):
    for count in [1, 64]:
        stdout.print(1 << count)
",
    )
}

/// `==`, `+` and ordering on lists of each kind of item, and `==` on dicts and sets.
#[test]
fn operators_on_collections_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "operators_on_collections",
        r#"struct Spot:
    x: int
    y: str

def maybe(on: bool) -> Spot?:
    if on:
        return Spot(x=1, y="a")
    None

def main(stdout: Stdout):
    a = [1, 2]
    stdout.print(a == [1, 2], a != [1, 2], a == [1], a + [3], a, [] + a, [1.5] == [1.5], [True] != [False])
    words = ["a", "a string long enough to be held in a block"]
    stdout.print(words == ["a", "a string long enough to be held in a block"], words + ["c"], words != words)
    spots = [Spot(x=1, y="a")]
    stdout.print(spots == [Spot(x=1, y="a")], spots == [Spot(x=2, y="a")], spots + spots)
    stdout.print([[1], [2, 3]] == [[1], [2, 3]], [[1]] + [[2]], [(1, "a")] == [(1, "a")])
    stdout.print([1] < [1, 0], [2] < [1, 0], [1, 2] <= [1, 2], [1, 2] > [1], [1.5] >= [2.5], ["a"] < ["b"], ["b", "a"] > ["b"])
    stdout.print({1, 2} == {2, 1}, {1, 2} != {1, 3}, {"a": 1} == {"a": 1}, {"a": 1} != {"a": 1}, {1: 2} == {1: 3})
    stdout.print(maybe(True) == maybe(True), maybe(True) == maybe(False), maybe(False) == None)
    mut grown = a + [3]
    grown.append(4)
    stdout.print(grown, a)
"#,
    )
}

/// What std's runtime calls give back: hashes, TOML and JSON with its names respelled.
#[test]
fn std_calls_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "std_calls",
        r#"import std.crypto as crypto
import std.json as json
import std.toml as toml

struct Tool:
    main_weapon: str
    uses: int

error Wrapped:
    why: str

error Inner:
    what: str

def inner(n: int) -> int:
    if n > 1: fail Inner(what="too many")
    n

def outer(n: int) -> int:
    inner(n)? from Wrapped(why="inner failed")

def main(stdout: Stdout):
    stdout.print(crypto.sha256("abc"), crypto.blake3("abc"))
    stdout.print(crypto.keyed_blake3("0000000000000000000000000000000000000000000000000000000000000000", "abc") else "no")
    stdout.print(crypto.equal("a", "a"), crypto.equal("a", "b"))
    tool = Tool(main_weapon="bow", uses=3)
    stdout.print(json.dumps(tool, names=.camel_case), json.dumps(tool, names=.kebab_case))
    stdout.print(toml.dumps(tool) else "no")
    match toml.loads[Tool]("main_weapon = \"axe\"\nuses = 2\n"):
        case Ok(read):
            stdout.print(read)
        case Err(error):
            stdout.print(error.message)
    match toml.loads[Tool]("not toml"):
        case Ok(read):
            stdout.print(read)
        case Err(error):
            stdout.print(error.message)
    stdout.print(outer(1) else -1, outer(2) else -1)
"#,
    )
}

/// The same value passed `mut` and not: the one without `mut` keeps what it held.
#[test]
fn a_value_passed_both_ways_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "passed_both_ways",
        r#"struct Bag:
    label: str
    count: int

def both(items: list[int], mut others: list[int]) -> int:
    others.append(9)
    others[0] = 7
    items.len() * 10 + items[0]

def bags(bag: Bag, mut other: Bag) -> str:
    other.count = 5
    other.label = "a label long enough to be held in a block"
    f"{bag.label} {bag.count}"

def main(stdout: Stdout):
    mut twice = [1, 2]
    stdout.print(both(twice, mut twice), twice)
    mut bag = Bag(label="short", count=1)
    stdout.print(bags(bag, mut bag), bag)
"#,
    )
}

/// `?` in `main`, which then gives back a result: going on when it's `Ok`.
#[test]
fn a_main_giving_back_a_result_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "main_gives_back",
        r#"def number(text: str) -> int:
    int(text)?

def main(stdout: Stdout):
    stdout.print(number("12")?)
    stdout.print("still here")
"#,
    )
}

/// `?` in `main` on an error fails the program, saying so at `main`.
#[test]
fn a_main_giving_back_an_error_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "main_gives_back_an_error",
        r#"def number(text: str) -> int:
    int(text)?

def main(stdout: Stdout):
    stdout.print("first")
    stdout.print(number("twelve")?)
    stdout.print("never")
"#,
    )
}

/// Slices of tuples, with what they hold kept by both, and `enumerate` of a string.
#[test]
fn tuple_slices_and_enumerated_text_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "tuple_slices",
        r#"def main(stdout: Stdout):
    numbers = (1, 2, 3)
    stdout.print(numbers[1:], numbers[-2:-1], numbers[:2])
    held = (1, "a string long enough to be held in a block", [1, 2], 2.5)
    part = held[1:3]
    stdout.print(part, held)
    for at, letter in enumerate("héllo"):
        stdout.print(at, letter)
    stdout.print(enumerate("ab"))
"#,
    )
}

/// `bytes`, wherever it's held and whatever's done with it.
#[test]
fn bytes_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "bytes_all",
        r#"struct Packet:
    id: int
    data: bytes

enum Held:
    Empty
    Full(data: bytes)

def first(data: bytes) -> int:
    data[0]

def maybe(present: bool) -> bytes?:
    if present:
        return bytes([9, 8])
    None

def main(stdout: Stdout):
    empty = bytes()
    data = bytes([0, 1, 127, 128, 255])
    stdout.print(empty, empty.len(), data, data.len())
    stdout.print(data[0], data[-1], data[3], first(data))
    stdout.print(data[1:3], data[:2], data[3:], data[-2:], data[:-2], data[10:], data[3:1])
    stdout.print(data + bytes([7]), empty + empty, data == bytes([0, 1, 127, 128, 255]), data != data)
    stdout.print(data < bytes([1]), data <= data, data > bytes([0]), data >= bytes([255]))
    stdout.print(1 in data, 2 in data, 2 not in data, 300 in data, -1 in data)
    stdout.print(data.hex(), data.hex(" "), data.hex("--"), empty.hex())
    mut total = 0
    for byte in data:
        total = total + byte
    stdout.print(total, list(data), sum(list(data)))
    text = "héllo wörld"
    encoded = text.encode()
    stdout.print(encoded, encoded.len(), text.len())
    match encoded.decode():
        case Ok(back):
            stdout.print(back, back == text)
        case Err(error):
            stdout.print(error.message)
    match bytes([255, 254]).decode():
        case Ok(back):
            stdout.print(back)
        case Err(error):
            stdout.print(error.message)
    for hex in ["00ff", "00 FF", "", "abc", "zz", "a é"]:
        match bytes.fromhex(hex):
            case Ok(made):
                stdout.print(made)
            case Err(error):
                stdout.print(error.message)
    packet = Packet(id=1, data=data[:2])
    copied = packet
    stdout.print(packet, copied.data, packet.data == copied.data, packet == copied)
    packets = [packet, Packet(id=2, data=bytes())]
    stdout.print(packets[0].data, packets[1].data, packets)
    mut many = [data, empty, encoded]
    many.append(bytes([3]))
    stdout.print(many[0], many[-1], many.len(), many)
    for held in [Held.Empty, Held.Full(data=bytes([1]))]:
        match held:
            case Empty:
                stdout.print("empty")
            case Full(inner):
                stdout.print(inner, inner[0], held)
    stdout.print(maybe(True), maybe(False), maybe(True) is None, maybe(False) is None)
    got = maybe(True) else bytes()
    stdout.print(got, got.len())
    pair = (data[:1], 3)
    stdout.print(pair)
    stdout.print(f"{data[:1]} and {empty}", str(data[:2]))
    mut grown = bytes()
    for at in range(300):
        grown = grown + bytes([at % 256])
    stdout.print(grown.len(), grown[299], grown[255], grown[:3])
    named = {"a": 1}
    stdout.print(named["a"], grown[:9].hex())
"#,
    )
}

/// What fails with `bytes` says what the JIT says.
#[test]
fn bytes_fail_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, call) in [
        ("bytes_past_end", "bytes([1, 2])[2]"),
        ("bytes_before_start", "bytes([1, 2])[-3]"),
        ("bytes_too_big", "bytes([1, 256])"),
        ("bytes_negative", "bytes([-1])"),
    ] {
        same_message_as_the_jit(
            name,
            &format!(
                "def main(stdout: Stdout):
    stdout.print(1)
    stdout.print({call})
"
            ),
        )?;
    }
    Ok(())
}

/// `list` of a capability, `round` to places, the codes of text, and clearing and taking keys out
/// of dicts and sets.
#[test]
fn more_small_calls_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "more_small_calls",
        r#"struct Key:
    x: int
    y: int

def main(args: Args, stdout: Stdout):
    stdout.print(list(args).len())
    for arg in args:
        stdout.print(arg.len() > 0)
    stdout.print(round(3.14159, 2), round(2.5, 0), round(1234.5, -2), round(0.125, 2))
    stdout.print("héllo".codes(), "".codes(), "€".code())
    for code in [65, 233, 8364, -1, 55296, 1114112]:
        match str.from_code(code):
            case Ok(text):
                stdout.print(text, text.len())
            case Err(error):
                stdout.print(error.message)
    mut numbers = {1: 2, 3: 4}
    shared = numbers
    numbers.clear()
    stdout.print(numbers, shared, numbers.len())
    numbers[5] = 6
    stdout.print(numbers)
    mut names = {"a": "b", "a long string that is held in a block": "c"}
    names.clear()
    names["d"] = "e"
    stdout.print(names)
    mut ints = {1, 2, 3}
    kept = ints
    ints.discard(2)
    ints.discard(9)
    ints.remove(3)
    stdout.print(ints, kept)
    ints.clear()
    ints.add(7)
    stdout.print(ints, ints.len())
    mut words = {"a", "b", "a long string that is held in a block"}
    words.discard("a")
    words.discard("nope")
    words.remove("a long string that is held in a block")
    stdout.print(words)
    mut keys = {Key(x=1, y=2), Key(x=3, y=4)}
    keys.discard(Key(x=1, y=2))
    keys.discard(Key(x=9, y=9))
    keys.remove(Key(x=3, y=4))
    stdout.print(keys, keys.len())
"#,
    )
}

/// `set.remove` of a key that isn't there fails as the JIT does.
#[test]
fn removing_what_a_set_lacks_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, set, key) in [
        ("remove_int", "{1, 2}", "3"),
        ("remove_str", "{\"a\"}", "\"b\""),
        ("remove_long", "{\"a\"}", "\"a long string that is held in a block\""),
    ] {
        same_message_as_the_jit(
            name,
            &format!(
                "def main(stdout: Stdout):
    mut held = {set}
    held.remove({key})
    stdout.print(held)
"
            ),
        )?;
    }
    Ok(())
}

/// Dicts whose values are blocks, with what's done to them.
#[test]
fn dicts_of_blocks_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "dict_values",
        r#"struct Book:
    title: str
    pages: dict[str, list[int]]

def main(stdout: Stdout):
    mut lists = {"a": [1, 2], "b": []}
    lists["c"] = [3]
    copied = lists
    lists["a"] = [9]
    stdout.print(lists, copied, lists["a"], copied["a"][1], lists.len())
    lists["c"].append(4)
    stdout.print(lists["c"], copied["c"])
    stdout.print(lists.get("c"), lists.get("nope"), "a" in lists, "z" in lists)
    stdout.print(lists.keys(), lists.values(), lists.items())
    for key, value in lists.items():
        stdout.print(key, value, value.len())
    stdout.print(lists.pop("a"), lists)
    mut by_int = {1: ["one"], 2: ["two", "too"]}
    by_int[3] = ["three"]
    stdout.print(by_int, by_int[2][1], by_int == {1: ["one"]}, by_int == by_int)
    mut nested = {"x": {"y": [1]}}
    nested["z"] = {}
    nested["z"]["w"] = [2]
    stdout.print(nested, nested["x"]["y"], nested["z"]["w"][0])
    mut sets = {"s": {1, 2}}
    sets["t"] = {3}
    stdout.print(sets, 1 in sets["s"])
    mut raw = {"b": bytes([1, 2])}
    raw["c"] = "hi".encode()
    stdout.print(raw, raw["c"].len())
    book = Book(title="t", pages={"one": [1], "two": [2, 2]})
    stdout.print(book, book.pages["two"], book.pages.keys())
    mut empty: dict[str, list[int]] = {}
    empty.clear()
    stdout.print(empty, empty.get("a"), empty.len())
    mut grown: dict[int, list[int]] = {}
    for at in range(100):
        grown[at] = [at, at + 1]
    stdout.print(grown[99], grown.len(), sum(sum(items) for items in grown.values()))
"#,
    )
}

/// Dicts whose values are plain structs and enums, or `Id`s, with what's done to them.
#[test]
fn dicts_of_plain_values_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "dict_plain_values",
        r#"from std.game import IdMap

struct Point:
    x: int
    y: float

struct Named:
    name: str
    tags: list[str]

enum Light:
    Red
    Green
    Amber(seconds: int)

enum Note:
    Empty
    Said(text: str)

struct Home:
    id: int

def main(stdout: Stdout):
    mut points = {"p": Point(x=1, y=2.0)}
    points["q"] = Point(x=3, y=4.0)
    points["p"] = Point(x=5, y=6.0)
    other = points
    points["q"] = Point(x=7, y=8.0)
    stdout.print(points, other, points["p"].x, points == other, points.get("q"), points.get("r"))
    stdout.print(points.values(), points.items(), points.pop("q"), points)
    mut lights = {"go": Light.Green, "wait": Light.Amber(seconds=3)}
    lights["stop"] = Light.Red
    stdout.print(lights, lights["wait"], lights["go"] == Light.Green)
    for name, light in lights.items():
        match light:
            case Red:
                stdout.print(name, "red")
            case Green:
                stdout.print(name, "green")
            case Amber(seconds):
                stdout.print(name, seconds)
    mut notes = {1: Note.Said(text="a long note that sits in its own block"), 2: Note.Empty}
    notes[3] = Note.Said(text="short")
    notes[1] = Note.Said(text="replaced")
    stdout.print(notes, notes[3])
    stdout.print(notes.pop(1), notes.get(3), notes.values())
    mut people = {"me": Named(name="me", tags=["a"])}
    people["you"] = Named(name="a long name that sits in its own block", tags=[])
    kept = people["you"]
    people["you"] = Named(name="other", tags=["b", "c"])
    stdout.print(people, kept, people["you"].tags[1], people == people)
    mut ids = IdMap[Home]()
    first = ids.insert(Home(id=1))
    second = ids.insert(Home(id=2))
    mut links = {first: second}
    links[second] = first
    stdout.print(links[first] == second, links[second] == first, first in links, links.len())
    mut homes = {"first": first}
    homes["second"] = second
    stdout.print(homes["second"] == second, homes.get("nope") is None, homes.len())
"#,
    )
}

/// A key a dict of blocks doesn't have fails as the JIT's does.
#[test]
fn a_missing_key_in_a_dict_of_lists_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "missing_list",
        "def main(stdout: Stdout):
    held = {\"a\": [1]}
    stdout.print(held[\"b\"])
",
    )
}

/// `args[at]`, `reversed` of text and tuples, and `remove_where` on an `IdMap`.
#[test]
fn indexing_args_reversing_and_removing_from_id_maps_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "args_reversed_removed",
        r#"from std.game import IdMap
from std.math import sqrt

struct Unit:
    name: str
    hp: int

def main(args: Args, stdout: Stdout):
    stdout.print(args.len() >= 0)
    four = 16
    stdout.print(sqrt(16), sqrt(four), sqrt(2.25))
    stdout.print(reversed("fennec"), reversed("héllo"), reversed(""), reversed([1, 2, 3]))
    stdout.print(reversed((1, 2)), reversed((1, 2, 3, 4)), reversed(("a", "a long string in a block")))
    mut names = IdMap[str]()
    first = names.insert("ash")
    names.insert("kit")
    third = names.insert("a long name that sits in its own block")
    copied = names
    names.remove_where(name => name.startswith("k"))
    stdout.print(names.values(), names.len(), copied.values(), copied.len(), names[first], names[third])
    fourth = names.insert("vex")
    stdout.print(names.values(), names.len(), names[fourth])
    names.remove_where(name => False)
    stdout.print(names.len())
    names.remove_where(name => True)
    stdout.print(names.values(), names.len(), names.get(first))
    mut units = IdMap[Unit]()
    weak = units.insert(Unit(name="weak", hp=1))
    strong = units.insert(Unit(name="strong", hp=9))
    units.remove_where(unit => unit.hp < 5)
    stdout.print(units.values(), units.get(weak), units[strong].name)
    mut numbers = IdMap[int]()
    numbers.insert(1)
    numbers.insert(2)
    numbers.remove_where(n => n % 2 == 0)
    stdout.print(numbers.values(), numbers.len())
    numbers.clear()
    stdout.print(numbers.values(), numbers.len())
    gone = numbers.insert(5)
    stdout.print(numbers.values(), numbers.len(), numbers[gone])
    mut cleared = copy(names)
    cleared.insert("again")
    cleared.clear()
    stdout.print(cleared.len(), names.len(), cleared.values())
    ignore(numbers.remove(gone), "it's fine if it's gone")
    stdout.print(numbers.len())
"#,
    )
}

/// An `args` index that isn't there fails as the JIT's does.
#[test]
fn indexing_past_the_end_of_args_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "args_past_end",
        "def main(args: Args, stdout: Stdout):
    stdout.print(args[1000])
",
    )
}

/// `IdMap[str].get`, an optional string, however it's then used.
#[test]
fn getting_from_an_id_map_of_strings_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "id_map_of_strings",
        r#"from std.game import Id, IdMap

struct Holder:
    names: IdMap[str]

    def named(self, id: Id[str]) -> str:
        found = self.names.get(id)
        if found is None:
            return "nobody"
        found

def main(stdout: Stdout):
    mut names = IdMap[str]()
    first = names.insert("ash")
    second = names.insert("a long name that sits in its own block")
    found = names.get(first)
    stdout.print(found, found is None)
    named = names.get(first) else "nothing"
    stdout.print(named)
    if found is not None:
        stdout.print(found.len())
    held = Holder(names=names)
    stdout.print(held.named(second))
    kept: list[str?] = [names.get(first), names.get(second)]
    stdout.print(kept)
    names.remove(first)
    stdout.print(names.get(first), names.get(first) else "gone", names.get(first) is None)
    for at in [first, second]:
        got = names.get(at)
        stdout.print(got else "absent")
"#,
    )
}

/// A function that can fail in more than one way, however its error is then read.
#[test]
fn sets_of_errors_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "error_sets",
        r#"error NotFound:
    name: str

error TooWeak:
    level: int

error Plain:
    code: int

def weak(level: int) -> int:
    if level < 5: fail TooWeak(level)
    level

def named(name: str) -> str:
    if name == "": fail NotFound(name)
    name

def challenge(name: str, level: int) -> str:
    who = named(name)?
    power = weak(level)?
    f"{who} at {power}"

def three(name: str, level: int, code: int) -> str:
    if code > 0: fail Plain(code)
    challenge(name, level)?

def main(stdout: Stdout):
    for name, level in [("ash", 7), ("kit", 1), ("", 9)]:
        result = challenge(name, level)
        stdout.print(result)
        match result:
            case Ok(text):
                stdout.print(text)
            case TooWeak(level):
                stdout.print("weak", level)
            case Err(error):
                stdout.print("other", error, f"{error}")
    for code in [0, 3]:
        match three("", 1, code):
            case Ok(text):
                stdout.print(text)
            case Plain(code):
                stdout.print("plain", code)
            case NotFound(name):
                stdout.print("not found", name.len())
            case Err(error):
                stdout.print("other", error)
    match three("kit", 1, 0):
        case Err(error):
            stdout.print(error)
        case Ok(text):
            stdout.print(text)
    stdout.print(challenge("kit", 1) == Err(TooWeak(level=1)))
    stdout.print(challenge("kit", 1) == Err(TooWeak(level=2)))
    stdout.print(Err(TooWeak(level=1)) == challenge("kit", 1))
    stdout.print(challenge("kit", 1) != Err(TooWeak(level=1)))
    stdout.print(challenge("ash", 7) == Err(TooWeak(level=1)))
    stdout.print(challenge("", 1) == Err(NotFound(name="")))
    found = challenge("kit", 9) else "failed"
    missing = challenge("", 9) else "failed"
    stdout.print(found, missing)
"#,
    )
}

/// An error from a set that nothing handles ends the program as the JIT's does.
#[test]
fn an_unhandled_error_of_a_set_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "error_set_unhandled",
        "error NotFound:
    name: str

error TooWeak:
    level: int

def pick(level: int) -> int:
    if level < 0: fail NotFound(\"x\")
    if level < 5: fail TooWeak(level)
    level

def main(stdout: Stdout) -> int:
    stdout.print(pick(9)?)
    stdout.print(pick(1)?)
    0
",
    )
}

/// Sorting by a key that's a tuple, in place or into a new list, keeping equal keys in order.
#[test]
fn sorting_by_tuple_keys_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "tuple_keys",
        r#"struct Unit:
    name: str
    hp: int

def main(stdout: Stdout):
    words = ["bramble", "kit", "ash", "a long word that sits in a block", "kit"]
    stdout.print(sorted(words, key=(word) => (word.len(), word)))
    stdout.print(sorted([3, 1, 2], key=(n) => (n % 2, -n)))
    stdout.print(sorted([1.5, -2.5], key=(x) => (x, 1)))
    units = [Unit(name="b", hp=2), Unit(name="a", hp=2), Unit(name="c", hp=1), Unit(name="d", hp=2)]
    stdout.print(sorted(units, key=(unit) => (unit.hp, unit.name)))
    stdout.print(sorted(units, key=(unit) => (unit.hp, 0)))
    mut changing = [5, 3, 9, 1]
    kept = changing
    changing.sort(key=(n) => (n % 3, n))
    stdout.print(changing, kept)
    empty: list[int] = []
    stdout.print(sorted(empty, key=(n) => (n, n)))
    stdout.print(words)
"#,
    )
}

/// Floats as keys have one equality: `-0.0` is `0.0`, and every NaN is the same, with the key
/// kept as it was first written. The JIT's keys don't follow that yet.
#[test]
fn float_keys_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter(
        "float_keys",
        r#"from std.math import sqrt

def main(stdout: Stdout):
    mut floats = {1.5, 2.5}
    stdout.print(floats, 1.5 in floats, 3.5 in floats, floats.len(), sorted(floats), sorted({"b", "a"}))
    floats.add(1.5)
    floats.add(3.5)
    floats.discard(2.5)
    stdout.print(floats, floats.len(), list(floats), sum(x for x in floats))
    zero = 0.0
    mut zeros = {zero}
    negative = -zero
    stdout.print(negative in zeros, zero in zeros)
    zeros.add(negative)
    stdout.print(zeros, zeros.len())
    mut signed = {negative}
    signed.add(zero)
    stdout.print(signed, signed.len(), zero in signed)
    nan = 1e300 * 1e300 - 1e300 * 1e300
    mut nans = {nan}
    nans.add(nan)
    nans.add(-nan)
    stdout.print(nans.len(), nan in nans, -nan in nans, 1.0 in nans)
    mut named = {1.5: "a", 2.5: "b"}
    named[1.5] = "c"
    named[0.0] = "z"
    stdout.print(named, named[1.5], named[negative], named.get(9.5), named.keys(), named.values())
    stdout.print(named.pop(2.5), named, {1.5, 2.5} == {2.5, 1.5})
    mut counts: dict[float, int] = {}
    for at in range(100):
        key = float(at % 7) / 2.0
        counts[key] = (counts.get(key) else 0) + 1
    stdout.print(counts.len(), counts[0.0], counts[3.0])
"#,
    )
}

/// `None` itself inside a tuple.
#[test]
fn none_in_a_tuple_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "none_in_tuple",
        r#"def main(stdout: Stdout):
    pair = (1, None)
    stdout.print(pair, (None, "a"), (None, None))
    stdout.print(pair == (1, None), [(1, None), (2, None)])
"#,
    )
}

/// A list that's packed in native code, as it crosses to the JSON writer.
#[test]
fn json_of_packed_lists_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "json_packed",
        r#"from std import json

struct Scores:
    name: str
    points: list[int]
    rates: list[float]

def main(stdout: Stdout):
    stdout.print(json.dumps(Scores(name="a", points=[1, 2], rates=[0.5])))
    stdout.print(json.dumps([1, 2, 3]), json.dumps([[1], [2, 3], []]), json.dumps((1, [2])))
    stdout.print(json.dumps({"6": [6], "7": []}), json.dumps({"a": {"b": [1.5]}}))
"#,
    )
}
/// A generator that fails: what asks for its next item gets the failure, by whichever error.
#[test]
fn generators_that_fail_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "failing_generators",
        r#"error TooBig:
    at: int

error Named:
    who: str

def checked(limit: int) -> Iterator[int]:
    for n in range(5):
        if n >= limit: fail TooBig(at=n)
        yield n

def named(limit: int) -> Iterator[str]:
    for n in range(5):
        if n >= limit: fail Named(who=f"a long name for number {n} in a block")
        yield str(n)

def either(limit: int) -> Iterator[int]:
    if limit == 0: fail Named(who="none")
    for n in range(limit):
        if n == 2: fail TooBig(at=n)
        yield n

def total(limit: int) -> int:
    sum(n for n in checked(limit)?)

def joined(limit: int) -> str:
    mut out = ""
    for word in named(limit)?:
        out = out + word
    out

def looped(limit: int) -> int:
    mut count = 0
    for n in either(limit)?:
        count = count + n + 1
    count

def main(stdout: Stdout):
    stdout.print(total(9), total(2))
    match total(2):
        case Ok(found):
            stdout.print(found)
        case TooBig(at):
            stdout.print("too big", at)
    stdout.print(joined(3), joined(9))
    match joined(1):
        case Named(who):
            stdout.print(who)
        case Ok(text):
            stdout.print(text)
    for limit in [0, 1, 2, 3]:
        stdout.print(limit, looped(limit))
"#,
    )
}

/// Floats have one equality and hash everywhere: `0.0` is `-0.0`, and NaN is the same as any other
/// NaN, whatever its sign. So this is held to what that decision says, on LLVM and the interpreter
/// both. The JIT's `==` and keys don't follow it yet.
#[test]
fn floats_are_equal_as_decided() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r"struct Pair:
    x: float
    y: float

def same(a: float, b: float) -> bool:
    a == b

def different(a: float, b: float) -> bool:
    a != b

def main(stdout: Stdout):
    zero = 0.0
    minus = -zero
    nan = 1e300 * 1e300 - 1e300 * 1e300
    other = -nan
    one = 1.0
    stdout.print(same(zero, minus), same(nan, nan), same(nan, other), same(nan, one), same(one, one))
    stdout.print(different(zero, minus), different(nan, other), different(nan, one), different(one, 2.0))
    stdout.print(zero == -0.0, nan == 1.0, 1.0 == nan, nan != 1.0, 1.0 != nan, -0.0 == zero)
    stdout.print(Pair(x=zero, y=nan) == Pair(x=minus, y=other), Pair(x=1.0, y=nan) == Pair(x=1.0, y=2.0))
    stdout.print(Pair(x=zero, y=nan) != Pair(x=minus, y=other))
    stdout.print((zero, nan) == (minus, other), (nan, 1.0) == (1.0, 1.0))
    stdout.print(zero in [minus], other in [nan], 1.0 in [nan])
";
    let dir = folder("floats_as_decided")?;
    let program = dir.join("floats.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("floats"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    let interpreted = outcome(&fennec(&["run", "--interp", &program], &dir)?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, interpreted);
    assert_eq!(
        ours.0,
        "True True True False True
False False True True
True False False True True True
True False
False
True False
True True False
"
    );
    Ok(())
}

/// Floats as keys hash as they compare, on the interpreter here. LLVM's float keys are held to the
/// same answers where they're lowered.
#[test]
fn float_keys_hash_as_decided_on_the_interpreter() -> std::io::Result<()> {
    let source = "def main(stdout: Stdout):
    zero = 0.0
    minus = -zero
    nan = 1e300 * 1e300 - 1e300 * 1e300
    mut zeros = {zero}
    stdout.print(minus in zeros)
    zeros.add(minus)
    stdout.print(zeros.len(), {zero, 1.0} == {minus, 1.0})
    mut nans = {nan}
    nans.add(-nan)
    stdout.print(nans.len(), -nan in nans)
    named = {zero: 1}
    stdout.print(named.get(minus), named[minus])
    stdout.print([zero, nan] == [minus, -nan])
";
    let dir = folder("float_keys_interpreted")?;
    let program = dir.join("keys.fen");
    std::fs::write(&program, source)?;
    let interpreted = outcome(&fennec(&["run", "--interp", &program.to_string_lossy()], &dir)?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(interpreted.0, "True\n1 True\n1 True\n1 1\nTrue\n");
    Ok(())
}

/// A generator made for a loop goes when the loop does, closing what it's holding right then,
/// while one with a name lasts as long as the name.
#[test]
fn a_generator_left_partway_closes_as_the_jit_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "generator_left",
        r#"struct Door:
    out: Stdout
    name: str

    def del(self):
        self.out.print(f"closing {self.name}")

def holding(out: Stdout, name: str) -> Iterator[int]:
    with Door(out=out, name=name) as door:
        yield 1
        yield 2

def leaves_early(out: Stdout):
    for n in holding(out, "made for the loop"):
        out.print("got", n)
        break
    out.print("left the loop")

def named(out: Stdout):
    kept = holding(out, "with a name")
    for n in kept:
        out.print("got", n)
        break
    out.print("left the second loop")

def finishes(out: Stdout):
    for n in holding(out, "run to its end"):
        out.print("got", n)
    out.print("left the third loop")

def returns_from_it(out: Stdout) -> int:
    for n in holding(out, "returned from"):
        return n
    0

def main(stdout: Stdout):
    leaves_early(stdout)
    stdout.print("next")
    named(stdout)
    stdout.print("next")
    finishes(stdout)
    stdout.print(returns_from_it(stdout))
"#,
    )
}
