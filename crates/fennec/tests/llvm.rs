//! `fennec build --llvm` against `fennec run`, on programs that backend takes so far. Skipped
//! where LLVM's tools aren't installed, as `fennec build --llvm` itself would say, unless
//! `FENNEC_EXPECT_LLVM` says they have to be.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::executables::runtime_built;

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

/// What the program printed, its errors and its exit code on the interpreter.
fn interpreted(name: &str, source: &str) -> std::io::Result<Outcome> {
    let dir = folder(&format!("{name}_interpreted"))?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let done = outcome(&fennec(&["run", "--interp", &program.to_string_lossy()], &dir)?);
    std::fs::remove_dir_all(&dir)?;
    Ok(done)
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

/// `stdout.write` shows what `print` shows, with no newline, in compiled code.
#[test]
fn writing_to_stdout_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "stdout_write",
        r#"struct Point:
    x: int
    label: str

def maybe(present: bool) -> str?:
    if present:
        return "here"
    None

def main(stdout: Stdout):
    stdout.write("no newline, ")
    stdout.write("then one\n")
    stdout.write(1, 2.5, True, "words", [1, 2], Point(x=1, label="p"), maybe(True), maybe(False))
    stdout.write()
    stdout.write("")
    stdout.print(" and print after")
    stdout.write(f"{1 + 1}\n")
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
            case .Empty:
                stdout.print("empty")
            case .Full(inner):
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

/// A text growing in a field: short then long, from each kind of text, and with a copy held
/// elsewhere that has to keep what it had.
#[test]
fn a_text_growing_in_a_field_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "text_in_a_field",
        r#"struct Log:
    text = ""
    hp = 0

    def add(ref self, more: str):
        self.text += more

struct World:
    log = Log()

def main(stdout: Stdout):
    mut log = Log()
    log.add("short")
    stdout.print(log.text, log.text.len())
    held = log.text
    log.add(" and a good deal longer than a header can hold")
    stdout.print(held, log.text)
    kept = log.text
    log.text += " été"
    log.text += log.text
    stdout.print(kept, log.text, log.text.len())
    mut world = World()
    for at in range(100):
        world.log.text += str(at)
    world.log.hp += 1
    stdout.print(world.log.text, world.log.hp)
    copy = world
    world.log.text += "!"
    stdout.print(copy.log.text.len(), world.log.text.len())
    stdout.print(str(-12), str(0), str(9007199254740993), str(1.5), str(2.0), str(-0.0), str(1e300), str(-1.7976931348623157e308))
"#,
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

def by_fail(ref units: list[Unit], at: int) -> int:
    for ref unit in units:
        unit.tags.append(at)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    0

def by_try(ref units: list[Unit]) -> int:
    for ref unit in units:
        unit.tags.append(8)
        unit.hp += maybe(unit.hp)?
    0

def main(stdout: Stdout):
    mut units = [Unit([1], 0), Unit([2], 1), Unit([3], 5)]
    stdout.print(by_fail(ref units, 9) else -1, units)
    mut others = [Unit([1], 0), Unit([2], 1), Unit([3], 5)]
    stdout.print(by_try(ref others) else -2, others)
"#;
    same_as_the_jit("failing_out_of_for_mut", shared)?;
    // Out of two loops at once. The JIT stops here with "expected an int", so this one's held to
    // what the interpreter prints.
    let nested = "error Bad:\n    why: str\n\ndef nested(ref grid: list[list[int]]) -> int:\n    for ref row in grid:\n        for ref cell in row:\n            cell += 1\n            if cell == 5:\n                fail Bad(\"inner\")\n    0\n\ndef main(stdout: Stdout):\n    mut grid = [[1, 2], [4, 7], [9]]\n    stdout.print(nested(ref grid) else -3, grid)\n";
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
/// What a program printed, how it ended and its first error line, built with LLVM and in dev
/// and `--release` both, is what the interpreter does. What the backend remembers about a local
/// has to end when something changes it, and only optimizing shows some of that.
fn same_as_the_interpreter_in_both_builds(name: &str, source: &str) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let output = executable.to_string_lossy().into_owned();
    let (wanted_out, wanted_err, wanted_code) = outcome(&fennec(&["run", "--interp", &program], &dir)?);
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let (out, err, code) = outcome(&Command::new(&executable).output()?);
        assert_eq!((&out, code), (&wanted_out, wanted_code), "{how:?}");
        assert_eq!(err.lines().next(), wanted_err.lines().next(), "{how:?}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Each kind of local passed by `mut` and used again, with what could be remembered about it
/// before the call: where an index points, a length, a read held across the change, a dict's
/// entry, how far a string's been read, what a loop or a closure holds, a number known to fit.
const GIVEN_MUT: &str = r#"struct Box:
    items: list[int]
    at: int
    name: str

    def next(ref self):
        self.at += 1

    def more(ref self, value: int):
        self.items.append(value)

    def fewer(ref self):
        self.items.pop()

    def renamed(ref self, name: str):
        self.name = name

    def swapped(ref self):
        self.items = [100, 200]

struct Pair:
    left: Box
    right: Box

enum Shape:
    Dot
    Line(size: int, tags: list[int])

def bump(ref at: int):
    at += 1

def grown(ref xs: list[int]):
    xs.append(99)

def shrunk(ref xs: list[int]):
    xs.pop()

def replaced(ref xs: list[int]):
    xs = [7]

def emptied(ref xs: list[int]):
    while xs.len() > 0:
        xs.pop()

def retext(ref text: str, by: str):
    text = by

def longer(ref text: str):
    text += "é"

def added(ref d: dict[str, int], key: str):
    d[key] = 1

def removed(ref d: dict[str, int], key: str):
    d.pop(key)

def rekeyed(ref key: str):
    key = key + "x"

def cleared(ref value: int?):
    value = None

def some(value: int) -> int?:
    value

def cleared_text(ref value: str?):
    value = None

def reshaped(ref shape: Shape):
    shape = .Dot

def newbox(ref box: Box):
    box = Box([5], 0, "new")

def param_index(ref at: int, ref xs: list[int]) -> int:
    xs[at] = 10
    bump(ref at)
    xs[at] = 20
    bump(ref at)
    xs[at]

def main(stdout: Stdout):
    # An index, as a position and as a length.
    mut i = 0
    mut xs = [1, 2, 3, 4]
    stdout.print(xs[i])
    bump(ref i)
    stdout.print(xs[i])
    mut n = xs.len()
    bump(ref n)
    stdout.print(n, xs[n - 2])

    mut p = 0
    mut qs = [1, 2, 3]
    stdout.print(param_index(ref p, ref qs), p, qs)

    # A list's length, kept across a change.
    mut ys = [1, 2, 3]
    size = ys.len()
    grown(ref ys)
    stdout.print(size, ys.len(), ys[size])
    shrunk(ref ys)
    shrunk(ref ys)
    stdout.print(ys.len(), ys[-1])
    replaced(ref ys)
    stdout.print(ys.len(), ys)

    # A loop's list changing in its body, by index up to a length read once.
    mut zs = [1, 2, 3, 4]
    for at in range(zs.len()):
        if at < zs.len():
            zs[at] += 1
        shrunk(ref zs)
    stdout.print(zs)

    # A read held across a change.
    mut lists = [[1], [2]]
    first = lists[0]
    grown(ref lists[0])
    stdout.print(first, lists[0])
    inner = lists[1]
    emptied(ref lists[1])
    stdout.print(inner, inner.len(), lists[1].len())

    # A struct's list and index, through `mut self` and a `mut` argument.
    mut box = Box([1, 2, 3], 0, "old")
    held = box.items
    box.more(4)
    stdout.print(held.len(), box.items.len(), box.items[box.at])
    box.next()
    stdout.print(box.items[box.at], box.at)
    box.fewer()
    box.fewer()
    stdout.print(box.items.len(), box.items[-1])
    box.swapped()
    stdout.print(box.items, held)
    name = box.name
    box.renamed("newer")
    stdout.print(name, box.name, box.name.len())
    newbox(ref box)
    stdout.print(box.items, box.at, box.name)

    # In places: through a list's item, and a field of a field.
    mut boxes = [Box([1], 0, "a"), Box([2], 0, "b")]
    other = boxes[0]
    boxes[0].more(5)
    boxes[0].next()
    stdout.print(boxes[0].items[boxes[0].at], other.items, other.at)
    newbox(ref boxes[1])
    stdout.print(boxes[1].items, boxes[1].name, boxes.len())
    mut pair = Pair(Box([1], 0, "l"), right=Box([2], 0, "r"))
    pair.left.more(6)
    pair.left.next()
    newbox(ref pair.right)
    stdout.print(pair.left.items[pair.left.at], pair.right.items, pair.right.name)

    # A string: its length, its characters, and where reading it had got to.
    mut text = "héllo wörld, with some more words"
    before = text.len()
    c = text[6]
    stdout.print(before, c, text[7])
    retext(ref text, "日本語のテキストです")
    stdout.print(text.len(), text[1], text[7])
    longer(ref text)
    stdout.print(text.len(), text[-1], text[3])
    retext(ref text, "ascii now")
    stdout.print(text.len(), text[2], text)
    mut chars = 0
    for at in range(text.len()):
        chars += 1 if text[at] == "a" else 0
        if at == 2:
            retext(ref text, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    stdout.print(chars)

    # A dict and its key, between a lookup and a write.
    mut d = {"a": 1, "b": 2}
    mut key = "a"
    if key in d:
        rekeyed(ref key)
        d[key] = 50
    stdout.print(key, d)
    if "b" in d:
        for extra in range(50):
            added(ref d, f"k{extra}")
        d["b"] = 60
    stdout.print(d["b"], d.len())
    if "b" in d:
        removed(ref d, "b")
        d["b"] = 70
    stdout.print(d["b"], d.len())

    # An optional and a case, known and then changed.
    # An optional known to hold something, given to what can make it `None`.
    mut maybe: int? = 3
    if maybe is not None:
        before_clear = maybe + 1
        cleared(ref maybe)
        stdout.print(before_clear, maybe is None)
    mut text_maybe: str? = "held"
    if text_maybe is not None:
        size_before = text_maybe.len()
        cleared_text(ref text_maybe)
        stdout.print(size_before, text_maybe is None)
    mut places: list[int?] = []
    places.append(some(1))
    places.append(some(2))
    if places[0] is not None:
        cleared(ref places[0])
        stdout.print(places)
    mut shape = Shape.Line(2, [1, 2])
    match shape:
        case .Line(size, tags):
            reshaped(ref shape)
            stdout.print(size, tags, tags.len())
        case .Dot:
            stdout.print("dot")
    match shape:
        case .Line(size, tags):
            stdout.print(size)
        case .Dot:
            stdout.print("dot")

    # An arithmetic result, known to fit, from something changed.
    mut small = 1
    bump(ref small)
    doubled = small * 2
    bump(ref small)
    stdout.print(doubled, small, 100 // small)
    mut count = 0
    while count < 3:
        bump(ref count)
    stdout.print(count)
    mut zero = 0
    bump(ref zero)
    stdout.print(10 // zero)
    more_facts(stdout)

struct Store:
    lists: list[list[int]]
    texts: list[str]

    def keep(ref self, xs: list[int]):
        self.lists.append(xs)

def stash(ref xs: list[int], ref store: Store):
    store.lists.append(xs)
    xs.append(-1)

def via(ref xs: list[int]):
    grown(ref xs)
    grown(ref xs)

def zeroed(ref value: float):
    value = 0.0

def huge(ref value: int):
    value = 9223372036854775806

def appended_twice(ref text: str):
    longer(ref text)
    longer(ref text)

def more_facts(stdout: Stdout):
    # A local's value kept by what it's given to, then changed here.
    mut store = Store([], [])
    mut mine = [1, 2]
    stash(ref mine, ref store)
    mine[0] = 9
    mine.append(3)
    stdout.print(mine, store.lists)
    store.keep(mine)
    mine[1] = 8
    stdout.print(mine, store.lists)
    mut grid = [[1], [2]]
    stash(ref grid[0], ref store)
    grid[0][0] = 7
    stdout.print(grid, store.lists)

    # Through a `mut` parameter passed on.
    mut chain = [1]
    chain[0] = 2
    via(ref chain)
    chain[2] = 5
    stdout.print(chain, chain.len(), chain[-1])

    # A list being filled, given away part of the way.
    mut filled = []
    for at in range(4):
        filled.append(0)
        if at == 1:
            grown(ref filled)
    stdout.print(filled)
    mut none_yet = [0 for _ in range(3)]
    grown(ref none_yet)
    stdout.print(none_yet)

    # A loop's item and count given to a call.
    mut items = [1, 2, 3]
    for ref item in items:
        bump(ref item)
        item *= 2
    stdout.print(items)
    mut lists = [[1], [2]]
    for ref one in lists:
        grown(ref one)
        one[0] = one.len()
    stdout.print(lists)
    mut limit = 3
    mut ran = 0
    for at in range(limit):
        bump(ref limit)
        ran += at
    stdout.print(limit, ran)
    for at, item in enumerate(items):
        mut here = copy(at)
        bump(ref here)
        stdout.print(at, here, item)

    # What a closure copied, and a copy made first.
    mut caught = 1
    read = () => caught
    bump(ref caught)
    stdout.print(read(), caught)
    mut base = [1, 2]
    copied = base
    gen = sum(x for x in copied)
    grown(ref base)
    stdout.print(gen, copied, base)

    # A string growing where it is: its length, its characters, where reading got to.
    mut long = "é" * 40
    stdout.print(long.len(), long[35], long[20])
    longer(ref long)
    stdout.print(long.len(), long[40], long[39])
    appended_twice(ref long)
    stdout.print(long.len(), long[42], long[41], long[0])
    mut plain = "a" * 40
    stdout.print(plain.len(), plain[39])
    longer(ref plain)
    stdout.print(plain.len(), plain[40], plain[39], plain[20])
    mut short = "ab"
    stdout.print(short.len(), short[1])
    longer(ref short)
    stdout.print(short.len(), short[2], short)
    for round in range(30):
        longer(ref short)
    stdout.print(short.len(), short[31], short[1])

    # In places, read before and after.
    mut names = ["é" * 30, "b"]
    stdout.print(names[0].len(), names[0][29])
    longer(ref names[0])
    stdout.print(names[0].len(), names[0][30], names[1])
    mut held = Box([1], 0, "é" * 30)
    stdout.print(held.name.len(), held.name[25])
    longer(ref held.name)
    held.renamed(held.name + "z")
    stdout.print(held.name.len(), held.name[31], held.name[25])

    # Numbers known to fit or not to be zero, until they're changed.
    mut one_half = 2.0
    half = 1.0 / one_half
    zeroed(ref one_half)
    stdout.print(half, one_half, 1.0 / one_half)
"#;

#[test]
fn what_a_mut_argument_changes_is_read_again() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("given_mut", GIVEN_MUT)
}

#[test]
fn what_a_mut_argument_changes_fails_as_it_should() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, source) in [
        (
            "given_mut_fails_1",
            r"def shrunk(ref xs: list[int]):
    xs.pop()

def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    for at in range(xs.len()):
        stdout.print(xs[at])
        shrunk(ref xs)
",
        ),
        (
            "given_mut_fails_2",
            r"struct Box:
    items: list[int]

    def fewer(ref self):
        self.items.pop()

def main(stdout: Stdout):
    mut box = Box([1, 2, 3])
    for at in range(box.items.len()):
        stdout.print(box.items[at])
        box.fewer()
",
        ),
        (
            "given_mut_fails_3",
            r"def huge(ref value: int):
    value = 9223372036854775807

def main(stdout: Stdout):
    mut big = 1
    small = big + 1
    huge(ref big)
    stdout.print(small)
    stdout.print(big + 1)
",
        ),
        (
            "given_mut_fails_4",
            r"def zero(ref value: int):
    value = 0

def main(stdout: Stdout):
    mut n = 5
    stdout.print(10 // n)
    zero(ref n)
    stdout.print(10 // n)
",
        ),
        (
            "given_mut_fails_5",
            r"def bumped(ref at: int):
    at += 100

def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    for at in range(xs.len()):
        mut i = at
        stdout.print(xs[i])
        bumped(ref i)
        stdout.print(xs[i])
",
        ),
    ] {
        same_as_the_interpreter_in_both_builds(name, source)?;
    }
    Ok(())
}

/// What's read as an argument keeps the value it had, in order, while a later `mut` argument
/// changes it and what's known of it ends: both the order arguments run in and an index that
/// changes have to hold at once.
const READ_THEN_CHANGED: &str = r#"def bump(ref at: int) -> int:
    at += 1
    at

def pair(a: int, b: int) -> str:
    f"{a}/{b}"

def three(a: int, b: int, c: int) -> str:
    f"{a}/{b}/{c}"

def listed(a: list[int], b: int) -> str:
    f"{a}/{b}"

def put(a: int, ref item: int, ref at: int) -> str:
    at += 1
    item += 100
    f"{a}"

def maybe_cleared(a: int, ref value: int?) -> int:
    value = None
    a

def main(stdout: Stdout):
    # An index read as an argument, then changed by a later one.
    mut i = 0
    stdout.print(pair(i, b=bump(ref i)), i)
    mut xs = [10, 20, 30, 40]
    stdout.print(pair(xs[i], b=bump(ref i)), i, xs[i])
    stdout.print(three(i, b=xs[i], c=bump(ref i)), i)
    i = 0
    stdout.print(i, xs[i], bump(ref i), xs[i], i)

    # What it remembered about the index ends, in order, with the old read kept.
    mut j = 0
    xs[j] = 11
    held = pair(xs[j], b=bump(ref j))
    xs[j] = 22
    stdout.print(held, j, xs)

    # A place and an index the call changes, held apart, since one can't pick the other.
    mut k = 0
    mut ys = [1, 2, 3]
    first = k
    stdout.print(put(first, ref ys[first], ref k), k, ys)
    second = k
    stdout.print(put(second, ref ys[second], ref k), k, ys)

    # A list read as an argument, then changed by a later one, and its length.
    mut zs = [1, 2, 3]
    stdout.print(listed(zs, zs.pop() else 0), zs)
    stdout.print(zs, zs.pop(), zs.len())

    # An optional given to what can clear it, its value held in a name first.
    mut maybe: int? = 3
    if maybe is not None:
        read = maybe
        stdout.print(maybe_cleared(read, ref maybe), maybe is None)
"#;

#[test]
fn what_an_argument_read_keeps_its_value_when_a_later_one_changes_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("read_then_changed", READ_THEN_CHANGED)
}

/// Each shape of place a call changes: one owner, shared at each level, given what came from
/// the place, kept by the call, in a loop, under a rule and through a failure.
const CHANGED_PLACES: &str = r#"error Bad:
    why: str

struct Inner:
    tags: list[int]

    def grow(ref self, value: int):
        self.tags.append(value)

struct Unit:
    hp: int
    tags: list[int]
    inner: Inner
    name: str

    def grow(ref self, value: int):
        self.tags.append(value)
        self.hp += 1

    def grown_by(ref self, values: list[int]):
        self.tags.append(values.len())
        for value in values:
            self.tags.append(value)

    def absorb(ref self, other: Unit):
        self.hp += other.hp
        for tag in other.tags:
            self.tags.append(tag)

    def renamed(ref self, name: str):
        self.name = self.name + name
        self.inner.grow(name.len())

    def replaced(ref self):
        self = unit(self.hp * 2)

    def kept_in(ref self, ref into: list[list[int]]):
        into.append(self.tags)
        self.tags.append(-1)

    def checked(ref self, value: int) -> int fails Bad:
        if value < 0:
            fail Bad("negative")
        self.tags.append(value)
        value

    def tried(ref self, value: int) -> int fails Bad:
        self.hp += 100
        self.checked(value)?

struct Band:
    units: list[Unit]
    leader: Unit
    rows: list[list[Unit]]

    def grow_at(ref self, at: int, value: int):
        self.units[at].grow(value)
        self.units[at].inner.grow(value)
        self.leader.grow(value)
        self.rows[at][at].grow(value)

struct Ruled:
    size: int where size >= 0
    tags: list[int]

    def shrink(ref self):
        self.size -= 1
        self.tags.append(self.size)

struct Holder:
    ruled: list[Ruled]

def unit(hp: int) -> Unit:
    Unit(hp, [hp], Inner([hp]), f"u{hp}")

def band() -> Band:
    Band([unit(1), unit(2)], unit(3), [[unit(4), unit(5)], [unit(6), unit(7)]])

def show(stdout: Stdout, units: list[Unit]):
    for one in units:
        stdout.print(one.hp, one.tags, one.inner.tags, one.name)

def ruled_at(size: int) -> Ruled:
    Ruled(size, []).expect("a size that's allowed")

def swap_ends(ref a: list[int], ref b: list[int]):
    a.append(b.len())
    b.append(a.len())

def try_all(ref units: list[Unit]) -> int fails Bad:
    units[0].tried(1)?
    units[1].tried(-1)?
    units[0].tried(2)?

def main(stdout: Stdout):
    # One owner, and each shape of place.
    mut units = [unit(1), unit(2)]
    units[0].grow(10)
    units[-1].grow(20)
    units[1].inner.grow(30)
    units[0].renamed("abc")
    units[1].replaced()
    show(stdout, units)
    mut grid = [[unit(8), unit(9)], [unit(10), unit(11)]]
    grid[1][0].grow(40)
    grid[1][0].inner.grow(50)
    grid[-1][-1].inner.grow(60)
    show(stdout, grid[1])
    show(stdout, grid[0])
    mut held = band()
    held.grow_at(1, value=70)
    held.rows[0][1].inner.grow(80)
    show(stdout, held.units)
    show(stdout, held.rows[1])
    show(stdout, held.rows[0])
    show(stdout, [held.leader])

    # Everything that shared what's changed keeps what it had, at every level.
    mut shared = [unit(1), unit(2)]
    other = shared
    item = shared[0]
    tags = shared[0].tags
    inner = shared[0].inner
    inner_tags = shared[0].inner.tags
    shared[0].grow(1)
    shared[0].inner.grow(2)
    show(stdout, shared)
    show(stdout, other)
    show(stdout, [item])
    stdout.print(tags, inner.tags, inner_tags)

    mut nested = band()
    copied = nested
    row = nested.rows[1]
    nested.grow_at(1, value=5)
    nested.rows[1][1].inner.grow(6)
    show(stdout, nested.rows[1])
    show(stdout, copied.rows[1])
    show(stdout, row)
    show(stdout, copied.units)

    # What's given to the call comes from what's changed.
    mut own = [unit(1), unit(2), unit(3)]
    own[0].grow(own[1].hp)
    own[0].grown_by(own[0].tags)
    own[1].grown_by(own[0].tags)
    own[2].absorb(own[2])
    own[2].absorb(own[0])
    first = own[1]
    own[1].grown_by(first.tags)
    show(stdout, own)
    show(stdout, [first])

    # Built-ins changing a place, given what comes from what's changed.
    mut lists = [unit(1), unit(2), unit(3)]
    lists[0].tags.append(lists[1].hp)
    lists[0].tags.append(lists[0].tags.len())
    lists[1].tags.extend(lists[0].tags)
    lists[1].tags.extend(lists[1].tags)
    lists[2].inner.tags.append(lists[1].tags.pop() else 0)
    lists[2].tags.insert(0, lists[0].tags[0])
    lists[0].tags.pop()
    show(stdout, lists)
    mut pick = 0
    mut chosen = [[1], [2]]
    chosen[pick].append(pick)
    chosen[pick + 1].append(chosen[pick][0])
    copied_chosen = chosen
    chosen[0].append(chosen[1].pop() else 0)
    stdout.print(chosen, copied_chosen)
    mut tables = [{"a": 1}, {"b": 2}]
    tables[0]["c"] = tables[1]["b"]
    tables[1].pop("b")
    stdout.print(tables)

    # What a call keeps stays what it was, and the place goes on from there.
    mut keeps = [unit(1)]
    mut kept: list[list[int]] = []
    keeps[0].kept_in(ref kept)
    keeps[0].grow(2)
    keeps[0].kept_in(ref kept)
    show(stdout, keeps)
    stdout.print(kept)

    # Two places at once, apart and in one root.
    mut left = [[1], [2]]
    mut right = [[3], [4]]
    swap_ends(ref left[0], ref right[1])
    swap_ends(ref left[1], ref left[0])
    stdout.print(left, right)

    # In a loop, through a loop's item, and with the rule checked after.
    mut looped = [unit(1), unit(2)]
    for round in range(3):
        for at_now in range(looped.len()):
            looped[at_now].grow(round)
    for ref one in looped:
        one.inner.grow(9)
        one.grow(8)
    show(stdout, looped)
    mut ruled = Holder([ruled_at(2)])
    ruled.ruled[0].shrink()
    ruled.ruled[0].shrink()
    stdout.print(ruled.ruled[0].size, ruled.ruled[0].tags)

    # A failure leaves what was done before it.
    mut failing = [unit(1), unit(2)]
    match try_all(ref failing):
        case Ok(got):
            stdout.print(got)
        case Err(bad):
            stdout.print(bad.why)
    show(stdout, failing)
"#;

#[test]
fn changing_through_a_place_changes_only_the_place() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("changed_places", CHANGED_PLACES)
}

/// A list in a `shared` value, changed in place under its lock: a queue would otherwise copy the whole
/// list on every call that changes it, and take time in proportion to its length squared.
#[test]
fn changing_a_list_in_a_shared_value_copies_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let types = "struct Row:\n    tags: list[int]\n\nshared Queue:\n    items = list[int]()\n    \
                 rows = [Row(tags=[1])]\n\n    def push(ref self, item: int):\n        self.items.append(item)\n\n    \
                 def pop(ref self) -> int:\n        self.items.pop() else 0\n\n    \
                 def first(ref self, item: int):\n        self.items.insert(0, item)\n\n    \
                 def more(ref self, others: list[int]):\n        self.items.extend(others)\n\n    \
                 def nested(ref self, item: int):\n        self.rows[0].tags.append(item)\n        self.rows[0].tags.pop()\n\n    \
                 def called(ref self, item: int):\n        grow(ref self.items, item)\n\n\
                 def grow(ref numbers: list[int], item: int):\n    numbers.append(item)\n\n\
                 def main(stdout: Stdout):\n    queue = Queue()\n    mut seen = 0\n";
    for (name, body) in [
        (
            "push_pop",
            "    for n in range(2000):\n        queue.push(n)\n    for n in range(2000):\n        seen += queue.pop()\n",
        ),
        ("insert", "    for n in range(500):\n        queue.first(n)\n"),
        ("extend", "    for n in range(2000):\n        queue.more([n])\n"),
        ("nested", "    for n in range(2000):\n        queue.nested(n)\n"),
        ("called", "    for n in range(2000):\n        queue.called(n)\n"),
        (
            "with",
            "    with queue as ref held:\n        for n in range(2000):\n            held.items.append(n)\n        for n in range(2000):\n            seen += held.items.pop() else 0\n",
        ),
    ] {
        let counts =
            counts_of(&format!("shared_place_{name}"), &format!("{types}{body}    stdout.print(seen)\n"))?;
        assert!(counts.starts_with("copies 0 "), "{name}: {counts}");
    }
    Ok(())
}

/// What's changed in place stays right when it's read while changing, and when something still holds
/// the list as it was.
#[test]
fn a_list_changed_in_a_shared_value_is_what_the_interpreter_says() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("shared_places", include_str!("fixtures/shared_places.fen"))
}

#[test]
fn changing_through_a_place_copies_nothing_when_nothing_else_holds_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let types = "struct Inner:\n    tags: list[int]\n\n    def grow(ref self, value: int):\n        self.tags.append(value)\n\nstruct Unit:\n    hp: int\n    tags: list[int]\n    inner: Inner\n\n    def grow(ref self, value: int):\n        self.tags.append(value)\n\nstruct Band:\n    units: list[Unit]\n\n    def grow(ref self, at: int, value: int):\n        self.units[at].grow(value)\n        self.units[at].inner.grow(value)\n\ndef unit() -> Unit:\n    Unit(0, [], Inner([]))\n\ndef main(stdout: Stdout):\n";
    for (name, body) in [
        (
            "item",
            "    mut xs = [unit(), unit()]\n    for n in range(2000):\n        xs[n % 2].grow(n)\n    stdout.print(xs[0].tags.len())\n",
        ),
        (
            "deep",
            "    mut xs = [unit(), unit()]\n    for n in range(2000):\n        xs[n % 2].inner.grow(n)\n    stdout.print(xs[0].inner.tags.len())\n",
        ),
        (
            "grid",
            "    mut grid = [[unit()], [unit()]]\n    for n in range(2000):\n        grid[n % 2][0].grow(n)\n        grid[n % 2][-1].inner.grow(n)\n    stdout.print(grid[0][0].tags.len())\n",
        ),
        (
            "method",
            "    mut band = Band([unit(), unit()])\n    for n in range(2000):\n        band.grow(n % 2, n)\n    stdout.print(band.units[0].tags.len())\n",
        ),
    ] {
        let counts = counts_of(&format!("place_{name}"), &format!("{types}{body}"))?;
        assert!(counts.starts_with("copies 0 "), "{name}: {counts}");
    }
    Ok(())
}

#[test]
fn an_index_a_call_changes_is_read_again() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "index_given_mut",
        r"struct Unit:
    tags: list[int]

def bump(ref at: int):
    at += 1

def bump_twice(ref at: int):
    bump(ref at)
    bump(ref at)

def main(stdout: Stdout):
    mut at = 0
    mut lists = [[1], [2], [3]]
    lists[at].append(10)
    bump(ref at)
    lists[at].append(20)
    bump_twice(ref at)
    lists[at - 1].append(30)
    stdout.print(at, lists)

    mut i = 0
    mut numbers = [5, 6, 7]
    numbers[i] = 50
    bump(ref i)
    numbers[i] = 60
    stdout.print(i, numbers, numbers[i])

    mut j = 0
    mut units = [Unit([1]), Unit([2])]
    units[j].tags.append(10)
    bump(ref j)
    units[j].tags.append(20)
    stdout.print(j, units[0].tags, units[1].tags)

    mut items = [1, 2, 3, 4]
    for round in range(3):
        mut k = round
        items[k] = items[k] + 10
        bump(ref k)
        items[k] = items[k] + 100
    stdout.print(items)

    mut w = 0
    while w < items.len():
        items[w] = 0
        bump(ref w)
        if w < items.len():
            items[w] = 7
        bump(ref w)
    stdout.print(items)
",
    )
}

/// Natively, a place's index can be known from the line before, so a call that changes the index
/// has to forget it. A call that changes the index of a place it's also given doesn't compile.
#[test]
fn a_place_is_found_again_after_a_call_changes_its_index() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("place_index_given_mut")?;
    let program = dir.join("place.fen");
    let parts = "struct Unit:\n    hp: int\n\n    def stepped(ref self, ref at: int):\n        at += 1\n        self.hp += at\n\n\
                 def step(ref at: int):\n    at += 1\n\n";
    std::fs::write(
        &program,
        format!(
            "{parts}def main(stdout: Stdout):\n    mut at = 0\n    mut units = [Unit(1), Unit(2), Unit(3)]\n    \
             units[at].hp += 10\n    step(ref at)\n    units[at].hp += 10\n    held = at\n    units[held].stepped(ref at)\n    \
             stdout.print(at, units[0].hp, units[1].hp, units[2].hp)\n"
        ),
    )?;
    let refused = dir.join("refused.fen");
    std::fs::write(
        &refused,
        format!(
            "{parts}def main(stdout: Stdout):\n    mut at = 0\n    mut units = [Unit(1)]\n    units[at].stepped(ref at)\n    \
             stdout.print(at, units[0].hp)\n"
        ),
    )?;
    let executable = dir.join("place");
    let (program, executable_name) = (program.to_string_lossy().into_owned(), executable.to_string_lossy());
    let built = fennec(&["build", "--llvm", &program, "-o", &executable_name], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    let stopped = fennec(
        &["build", "--llvm", &refused.to_string_lossy(), "-o", &dir.join("refused").to_string_lossy()],
        &dir,
    )?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "2 11 14 3\n");
    assert!(!stopped.status.success());
    let said = String::from_utf8_lossy(&stopped.stderr);
    assert!(said.contains("`ref at` changes what picks `units[at]`"), "{said}");
    Ok(())
}

/// Two `mut` arguments that go to different fields are two places, and a built program changes
/// both. The same place given twice doesn't build.
#[test]
fn mut_arguments_to_different_fields_both_change_and_overlapping_ones_dont_build() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let parts = "struct Spot:\n    x = 0\n\nstruct Unit:\n    position = Spot()\n    home = Spot()\n\n\
                 def bump(ref first: Spot, ref second: Spot):\n    first.x += 1\n    second.x += 10\n\n";
    let dir = folder("overlapping_mut")?;
    let (apart, together) = (dir.join("apart.fen"), dir.join("together.fen"));
    std::fs::write(
        &apart,
        format!(
            "{parts}def main(stdout: Stdout):\n    mut unit = Unit()\n    bump(ref unit.position, ref unit.home)\n    \
             stdout.print(unit.position.x, unit.home.x)\n"
        ),
    )?;
    std::fs::write(
        &together,
        format!(
            "{parts}def main(stdout: Stdout):\n    mut unit = Unit()\n    bump(ref unit.position, ref unit.position)\n    \
             stdout.print(unit.position.x)\n"
        ),
    )?;
    let executable = dir.join("apart");
    let built =
        fennec(&["build", "--llvm", &apart.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    let refused = fennec(
        &["build", "--llvm", &together.to_string_lossy(), "-o", &dir.join("together").to_string_lossy()],
        &dir,
    )?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours.0, "1 10\n");
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("`ref unit.position` and `ref unit.position` overlap"), "{said}");
    Ok(())
}

/// A comprehension replacing the list it goes through, which is done in place when it can be:
/// the same answers whatever else holds the list, and whatever its items hold.
const REPLACING_ITS_SOURCE: &str = r#"struct Unit:
    name: str
    hp: int
    tags: list[int]

def hurt(unit: Unit, by: int) -> Unit:
    Unit(unit.name, unit.hp - by, unit.tags)

def renamed(unit: Unit) -> Unit:
    Unit(unit.name + "!", unit.hp, [unit.hp, unit.tags.len()])

struct Crowd:
    numbers: list[int]
    by: int
    units: list[Unit]

    def size(self) -> int:
        self.numbers.len()

    def step(ref self):
        self.numbers = [number - self.by for number in self.numbers]

struct Outer:
    crowd: Crowd

struct Ruled:
    numbers: list[int] where numbers.len() > 0

def doubled(ref numbers: list[int]):
    numbers = [number * 2 for number in numbers]

def main(stdout: Stdout):
    mut numbers = [1, 2, 3]
    numbers = [number * 2 for number in numbers]
    stdout.print(numbers)
    other = numbers
    numbers = [number + 1 for number in numbers]
    stdout.print(numbers, other)
    doubled(ref numbers)
    stdout.print(numbers, other)

    mut words = ["a", "é", "a much longer word than fits in a header"]
    held = words
    first = words[0]
    words = [word + "!" for word in words]
    stdout.print(words, held, first)
    words = [word for word in words]
    words = ["same" for word in words]
    stdout.print(words, held)

    mut units = [Unit("a", 1, [1]), Unit("b", 2, [2, 3])]
    kept = units
    tags = units[1].tags
    units = [hurt(unit, 1) for unit in units]
    stdout.print(units[0].hp, units[1].hp, kept[0].hp, kept[1].hp, tags)
    units = [renamed(unit) for unit in units]
    stdout.print(units[0].name, units[1].tags, kept[1].tags, tags)

    mut grids = [[1, 2], [3]]
    row = grids[0]
    grids = [[cell * 2 for cell in cells] for cells in grids]
    stdout.print(grids, row)

    # What's worked out reads the list, so it sees it as it was before.
    mut seen = [1, 2, 3]
    seen = [number + seen[0] + seen.len() for number in seen]
    stdout.print(seen)
    mut first_last = [1, 2, 3]
    first_last = [first_last[-1] for number in first_last]
    stdout.print(first_last)
    reader = [10, 20]
    mut reading = [1, 2]
    reading = [number + reader[0] for number in reading]
    stdout.print(reading)

    # A field's list, reading the struct's other fields, and not this one.
    mut crowd = Crowd([1, 2, 3], 10, [Unit("a", 1, [1])])
    before = crowd.numbers
    crowd.numbers = [number * crowd.by for number in crowd.numbers]
    stdout.print(crowd.numbers, before)
    crowd.numbers = [number + crowd.numbers[0] for number in crowd.numbers]
    stdout.print(crowd.numbers)
    crowd.numbers = [number + crowd.size() for number in crowd.numbers]
    stdout.print(crowd.numbers)
    crowd.step()
    stdout.print(crowd.numbers)
    crowd.units = [hurt(unit, crowd.by) for unit in crowd.units]
    stdout.print(crowd.units[0].hp)
    mut outer = Outer(crowd)
    outer.crowd.numbers = [number + 1 for number in outer.crowd.numbers]
    stdout.print(outer.crowd.numbers, crowd.numbers)
    mut ruled = Ruled([3, 1, 2]).expect("a list with something in it")
    ruled.numbers = [number + 10 for number in ruled.numbers]
    stdout.print(ruled.numbers)

    # Not the same size, or another type: a new list.
    mut some = [1, 2, 3, 4]
    some = [number for number in some if number % 2 == 0]
    stdout.print(some)
    mut pairs = [1, 2]
    mut texts = [str(number) for number in pairs]
    texts = [text + "x" for text in texts]
    stdout.print(texts, pairs)

    # None to go through, and one that fails part of the way along.
    mut empty: list[int] = []
    empty = [number + 1 for number in empty]
    stdout.print(empty)
    mut failing = [1, 0, 3]
    stdout.print("before")
    failing = [10 // number for number in failing]
    stdout.print(failing)
"#;

#[test]
fn a_comprehension_replacing_its_source_says_what_a_new_list_would() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    // The JIT reads the same lowering, so only the interpreter can say this is right.
    same_as_the_interpreter_in_both_builds("replacing_its_source", REPLACING_ITS_SOURCE)
}

#[test]
fn a_comprehension_replacing_its_source_makes_no_second_list() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def main(stdout: Stdout):\n    mut numbers = [n for n in range(1000)]\n    for _ in range(100):\n        numbers = [(number * 3 + 1) % 1000 for number in numbers]\n    stdout.print(numbers[10])\n";
    let counts = counts_of("replacing_alone", source)?;
    assert_eq!(count(&counts, "allocations"), Some(1), "{counts}");
    let shared = "def main(stdout: Stdout):\n    mut numbers = [n for n in range(1000)]\n    other = numbers\n    numbers = [number + 1 for number in numbers]\n    stdout.print(numbers[10], other[10])\n";
    let counts = counts_of("replacing_shared", shared)?;
    assert!(count(&counts, "allocations") <= Some(2), "{counts}");
    Ok(())
}

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

def grow(ref units: list[Unit], by: int):
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
    grow(ref units, 5)
    stdout.print(units, snapshot)
    # A caller's copy stays as it was.
    mut mine = copy(units)
    kept = mine
    grow(ref mine, 6)
    stdout.print(mine[0].tags, kept[0].tags)
    # Items that are blocks of their own.
    mut boxes = [Box([1], []), Box([2], [])]
    other = boxes[0]
    boxes[0].items.append(3)
    stdout.print(boxes, other)
    # Outside the list's length, it fails as it should.
    units[9].tags.append(1)
    stdout.print(units)
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

/// A string's lengths needn't be read again when a struct's field changes, as a field is never
/// a block's header. They do when the string itself changes: here it grows in place, moves to a
/// new block and gets replaced, while a field changes beside it in the same loop.
#[test]
fn a_strings_length_is_read_again_when_it_changes_beside_a_field() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Reader:
    text: str
    at = 0
    seen = 0

    def grow(ref self, more: str):
        self.text += more
        self.at += 1

    def read(ref self) -> int:
        mut total = 0
        while self.at < self.text.len():
            if self.text[self.at] == "x":
                total += 1
            self.at += 1
            self.seen = self.text.len()
        total

    def read_growing(ref self) -> int:
        mut total = 0
        while self.at < self.text.len() and self.text.len() < 400:
            if self.text[self.at] == "a":
                self.text += "ax"
            self.at += 1
            total += self.text.len()
        total

    def read_replacing(ref self) -> int:
        mut total = 0
        while self.at < self.text.len():
            if self.at == 2:
                self.text = "a replacement long enough to have its own block of text"
            total += self.text.len()
            self.at += 1
        total

def main(stdout: Stdout):
    mut one = Reader(text="xaxax long enough to have a block of its own")
    stdout.print(one.read(), one.at, one.seen)
    mut two = Reader(text="a string long enough to have a block of its own ")
    stdout.print(two.read_growing(), two.at, two.text.len())
    mut three = Reader(text="short")
    stdout.print(three.read_replacing(), three.at, three.text)
    mut four = Reader(text="short")
    for round in range(40):
        four.grow("é" if round % 7 == 0 else "ab")
        stdout.print(four.text.len(), four.text[four.at - 1], four.at)
    mut readers = [Reader(text="xx long enough to have a block of its own"), Reader(text="x")]
    for ref reader in readers:
        reader.grow("x")
        stdout.print(reader.read(), reader.at, reader.seen)
"#;
    // Whether a read moves past a write is the optimizer's choice, so with it, too.
    for (name, how) in [
        ("lengths_beside_fields", &["--llvm"][..]),
        ("lengths_beside_fields_release", &["--llvm", "--release"][..]),
    ] {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

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

/// Reading a string that isn't ASCII by position, as a parser does: the cursor's character again,
/// the next, characters of each width, back and from the end, positions outside it, an empty
/// string, and slices, after which reading carries on from where it was.
#[test]
fn reading_characters_by_position_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "characters_by_position",
        r#"def words(text: str) -> list[str]:
    mut found = []
    mut at = 0
    while at < text.len():
        while at < text.len() and text[at] == " ":
            at += 1
        start = at
        while at < text.len() and text[at] != " ":
            at += 1
        if at > start:
            found.append(text[start:at])
    found

def main(stdout: Stdout):
    text = "fóx ünïcode 日本語 🦊🦊 and plain ascii past an inline header é"
    stdout.print(words(text))
    stdout.print(words(""), words("é"), words(" é 🦊 "))
    mut seen = []
    for at in [0, 0, 1, 1, 2, 5, 4, 30, 29, 31, -1, -2, -60, 2, 3, 13, 12, 14, 15, 16, 17]:
        if -text.len() <= at and at < text.len():
            seen.append(f"{at}={text[at]}")
    stdout.print("".join(seen))
    stdout.print(text[4:11], text[12:15], text[16:18], text[-1:], text[:3], text[5])
    mut total = 0
    for at in range(text.len()):
        total += text[at].code()
        if at % 7 == 0:
            total += text[at // 2].code()
    stdout.print(total)
"#,
    )
}

/// A loop over a string's characters: a text all ASCII takes a byte at a time with nothing to
/// test, and any other its characters' bytes, whether it's inline or a block's, and the body
/// breaking, going on, or looping over text of its own.
#[test]
fn looping_over_characters_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "character_loops",
        r##"def kinds(text: str) -> str:
    mut out = []
    for c in text:
        if c == " ":
            out.append("_")
        elif c.isdigit():
            out.append("#")
        elif c == "é" or c == "🦊":
            out.append("*")
        else:
            out.append(c)
    "".join(out)

def until(text: str, stop: str) -> int:
    mut seen = 0
    for c in text:
        if c == stop:
            break
        if c == "a":
            continue
        seen += 1
    seen

def main(stdout: Stdout):
    for text in ["", "a", "short ascii", "héllo wörld 🦊 and 日本", "a much longer ASCII text than any inline header can hold",
                 "a much longer text than a header holds, ending in é", "🦊🦊🦊🦊🦊🦊🦊🦊🦊", "x1 y2 z3 " * 20]:
        stdout.print(kinds(text), until(text, stop="z"), until(text, stop="🦊"), text.len())
    mut joined = []
    for c in "nested é":
        for d in "ab":
            joined.append(c + d)
    stdout.print("".join(joined))
    mut changed = 0
    for c in "rename ü inside":
        more = c + "!"
        changed += more.len()
    stdout.print(changed)
    words = ["plain", "wörd", "日本語", "long enough ascii word to need a block of its own"]
    stdout.print([sum(1 for c in w if c != "o") for w in words])
"##,
    )
}

/// Indexing and slicing text that isn't ASCII, in and out of order, from either end, clamped
/// and empty, as native code finds characters from a block's cursor.
#[test]
fn indexing_and_slicing_a_string_that_isnt_ascii_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "wide_slices",
        r#"def main(stdout: Stdout):
    text = "héllo wörld, ünïcode ✓ and 🦊 fox; then plain ascii after all that"
    n = text.len()
    mut parts = []
    for a in [0, 1, 5, 21, 26, 27, 28, 40, 64, 65, -1, -5, -30, -70, 70]:
        for b in [0, 2, 6, 22, 27, 28, 29, 45, 64, 65, -1, -3, -40, 100]:
            parts.append(text[a:b])
    stdout.print(parts)
    stdout.print(text[:5], text[60:], text[:], text[-6:], text[:-60], text[3:3], text[27:28])
    mut walked = []
    for i in range(n - 2):
        walked.append(text[i:i + 2] + text[i] + text[-1 - i])
    stdout.print("|".join(walked))
    mut jumps = []
    for i in [3, 50, 7, 60, 0, 61, 28, 27, 26, -1, -64]:
        jumps.append(text[i])
    stdout.print(jumps, text[0:40], text[5:n])
    stdout.print(text[n])
"#,
    )
}

/// Reading characters and short parts of text that isn't ASCII, in order, makes nothing.
#[test]
fn reading_text_that_isnt_ascii_makes_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let made =
        "def main(stdout: Stdout):\n    text = \"é\" + \"abc日\" * 1000\n    stdout.print(text.len())\n";
    let read = "def main(stdout: Stdout):\n    text = \"é\" + \"abc日\" * 1000\n    mut found = 0\n    mut size = 0\n    for at in range(text.len() - 2):\n        if text[at] == \"日\":\n            found += 1\n        size += text[at:at + 2].len()\n    stdout.print(found, size)\n";
    let (made, read) = (counts_of("text_made", made)?, counts_of("text_read", read)?);
    assert_eq!(count(&read, "allocations"), count(&made, "allocations"), "{made} / {read}");
    Ok(())
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

def by_fail(ref units: list[Unit]) -> int:
    for ref unit in units:
        unit.tags.append(9)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    0

def by_return(ref units: list[Unit]) -> int:
    for ref unit in units:
        unit.tags.append(8)
        if unit.hp == 1:
            return unit.tags.len()
    -1

def grow(ref units: list[Unit], by: int):
    for ref unit in units:
        unit.tags.append(by)
        unit.name += "!"

def step(ref world: World):
    for ref unit in world.units:
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
    for ref unit in units:
        unit.tags.append(unit.hp)
    stdout.print(units, shared, kept, item, tags)
    # Ways out.
    mut a = make()
    stdout.print(by_fail(ref a) else -1, a)
    mut b = make()
    stdout.print(by_return(ref b), b)
    mut c = make()
    for ref unit in c:
        if unit.hp == 0:
            continue
        unit.tags.append(4)
        if unit.hp == 1:
            break
        unit.hp = 100
    stdout.print(c)
    # The list read in the body sees what was written back so far.
    mut d = make()
    for ref unit in d:
        unit.tags.append(d[0].tags.len())
        unit.hp += d.len() + total(d)
    stdout.print(d)
    # A copy of the item, or of what it holds, made inside.
    mut e = make()
    mut seen: list[Unit] = []
    mut held: list[list[int]] = []
    for ref unit in e:
        seen.append(unit)
        held.append(unit.tags)
        unit.hp += 1
    stdout.print(e, seen, held)
    # Replaced whole, and given away.
    mut f = make()
    for ref unit in f:
        unit = Unit([7], "whole new value long enough not to be inline", unit.hp)
    stdout.print(f)
    # Through a function, and a field.
    mut g = make()
    grow(ref g, 6)
    grow(ref g, 7)
    stdout.print(g)
    mut w = World(make(), log=[], turn=3)
    step(ref w)
    other = w
    step(ref w)
    stdout.print(w, other)
    # Nested.
    mut grid = [[1, 2], [3], []]
    for ref row in grid:
        row.append(row.len())
        for ref cell in row:
            cell *= 2
    stdout.print(grid)
    # Strings and enums as items.
    mut words = ["a", "a string long enough not to be held inline"]
    for ref word in words:
        word += "!"
    stdout.print(words)
    mut lists = [[1], [2, 3]]
    before = lists[1]
    for ref one in lists:
        one.append(9)
    stdout.print(lists, before)
    # Emptied in the body, then grown.
    mut h = make()
    for ref unit in h:
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
        "struct Unit:\n    tags: list[int]\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units = [Unit([1, 2, 3], 0), Unit([1, 2, 3], 1)]\n    for round in range(1000):\n        for ref unit in units:\n            unit.tags.append(round)\n            if round % 2 == 0:\n                unit.hp += 1\n    stdout.print(units[0].tags.len())\n",
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

/// `//` and `%` by a power of two are a shift and a mask, which round toward negative infinity
/// as Fennec's do: for every sign, from the lowest int to the highest, and the divisors from 1 up.
#[test]
fn dividing_by_a_power_of_two_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("power_of_two_division", POWER_OF_TWO_DIVISION)
}

const POWER_OF_TWO_DIVISION: &str = r#"def main(stdout: Stdout):
    lowest = -9223372036854775807 - 1
    highest = 9223372036854775807
    values = [0, 1, -1, 2, -2, 3, -3, 7, -7, 8, -8, 9, -9, 1023, -1023, 1024, -1024, 123456789, -123456789, lowest, lowest + 1, highest, highest - 1]
    mut lines = []
    for v in values:
        lines.append(f"{v}: {v // 1} {v % 1} {v // 2} {v % 2} {v // 4} {v % 4} {v // 8} {v % 8} {v // 1024} {v % 1024} {v // 16384} {v % 16384} {v // 4611686018427387904} {v % 4611686018427387904}")
    for line in lines:
        stdout.print(line)
    mut total = 0
    for n in range(-50, 50):
        total += n // 2 + n % 2 * 3 + n // 16 - n % 32
        if n % 2 == 0:
            total += n // 2
    stdout.print(total)
"#;

/// A product past the check is known not to have overflowed, which lets LLVM drop later checks:
/// one that does overflow still fails, from either sign, and one that doesn't goes on to its sums.
#[test]
fn a_product_that_overflows_still_fails_as_on_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let program = |a: &str, b: &str| {
        format!(
            "def scaled(i: int, j: int) -> float:\n    1.0 / float((i + j) * (i + j + 1) // 2 + i + 1)\n\ndef main(stdout: Stdout):\n    mut total = 0.0\n    for i in range(50):\n        for j in range(50):\n            total += scaled(i, j=j)\n    stdout.print(f\"{{total:.6f}}\")\n    big = {a}\n    stdout.print(big * {b})\n"
        )
    };
    for (name, a, b) in [
        ("product_fits", "3037000499", "3037000499"),
        ("product_too_big", "3037000500", "3037000500"),
        ("product_too_small", "-3037000500", "3037000500"),
    ] {
        same_as_the_interpreter_in_both_builds(name, &program(a, b))?;
    }
    Ok(())
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

def grown(ref items: list[int]):
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
    grown(ref e)
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

/// Fixed-size arrays: a copy is a copy of every item, changing an item or what it holds changes
/// only that copy, and items are read and written by position, from the end too. Strings, lists,
/// structs and optionals as items; an array in a struct, in a list and in an array; one passed by
/// `ref`, looped over by `ref`, compared, searched and made a list.
const ARRAYS: &str = r#"struct Named:
    label = ""
    tags: array[str, 3] = ["a", "b", "c"]
    lists: array[list[int], 2] = [[], [1]]

struct Holder:
    slots: array[dict[str, int], 2] = [{}, {}]

def make(first: str) -> array[str, 3]:
    [first, first + "!", first + "?"]

def bump(ref values: array[float, 4]):
    for ref value in values:
        value += 0.5

def grow(ref items: list[int]):
    items.append(9)

def main(stdout: Stdout):
    mut words = make("hi")
    copied = words
    words[0] = "changed"
    words[-1] = "last"
    stdout.print(words, copied, words == copied, "hi!" in words, "nope" not in words, words.len())
    stdout.print(f"{words} and {copied[1]}", str(copied))

    mut floats = array[float, 4]()
    bump(ref floats)
    floats[2] = 3.25
    stdout.print(floats, sum(floats), max(floats), sorted(floats), list(floats) + [1.0])

    mut named = Named(label="x")
    other = named
    named.tags[1] += "x"
    named.lists[0].append(5)
    grow(ref named.lists[1])
    stdout.print(named, other)

    mut items = [Named(label="one"), Named(label="two")]
    more = items
    items[1].tags[0] = "zero"
    stdout.print(items[1].tags, more[1].tags)

    mut grid = array[array[int, 3], 2]()
    mut row = grid[1]
    row[2] = 7
    grid[1] = row
    grid[0][1] = 4
    stdout.print(grid, [sum(each) for each in grid])

    mut holder = Holder()
    kept = holder
    holder.slots[1]["k"] = 3
    stdout.print(holder.slots, kept.slots)

    mut maybe: array[str?, 3] = [None, "a", None]
    before = maybe
    maybe[1] = "there"
    stdout.print(maybe, before, maybe[0] is None)

    mut texts: array[str, 2] = ["p", "q"]
    keep = texts
    for ref text in texts:
        text += "!"
    stdout.print(texts, keep)
"#;

#[test]
fn arrays_keep_value_semantics_like_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("arrays", ARRAYS)
}

/// An array too big to keep in place is held as a list, which nothing in the program can tell:
/// here one of ints, one of strings and one in a struct. One just small enough is in place.
#[test]
fn a_big_array_behaves_as_a_small_one() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "big_arrays",
        "struct Big:
    values = array[int, 10000]()

def fill(ref values: array[int, 10000]):
    for index in range(10000):
        values[index] = index * 3

def main(stdout: Stdout):
    mut big = array[int, 10000]()
    fill(ref big)
    copy = big
    big[0] = -1
    big[-1] = 99
    stdout.print(sum(big), sum(copy), big.len(), big[9998], copy[0], big == copy)
    mut holder = Big()
    holder.values[5] = 5
    stdout.print(sum(holder.values), holder.values[5], holder.values[6])
    names = array[str, 5000]()
    mut more = names
    more[4999] = \"end\"
    stdout.print(names[4999] == \"\", more[-1])
    mut in_place = array[int, 8192]()
    in_place[8191] = 1
    mut other = in_place
    other[0] = 2
    stdout.print(sum(in_place), sum(other), in_place == other)
",
    )
}

/// An index outside an array fails as one outside a list does, counting from the end too.
#[test]
fn an_index_outside_an_array_fails_like_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, at) in [("array_past_the_end", "3"), ("array_before_the_start", "-4")] {
        let source = format!(
            "def main(stdout: Stdout):\n    values = array[int, 3]()\n    mut at = {at}\n    stdout.print(values[at + 0])\n"
        );
        same_as_the_interpreter_in_both_builds(name, &source)?;
    }
    Ok(())
}

/// An array of numbers is kept where it is, so math on one in a loop allocates nothing.
#[test]
fn an_array_of_numbers_allocates_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "array_math",
        "struct Mat2:
    m: array[float, 4] = [1.0, 0.0, 0.0, 1.0]

    def times(self, other: Mat2) -> Mat2:
        mut out = Mat2()
        for row in range(2):
            for col in range(2):
                out.m[row * 2 + col] = self.m[row * 2] * other.m[col] + self.m[row * 2 + 1] * other.m[2 + col]
        out

def main(stdout: Stdout):
    mut product = Mat2()
    step = Mat2(m=[1.0, 0.001, 0.0, 1.0])
    mut counts = array[int, 8]()
    for i in range(1000):
        product = product.times(step)
        counts[i % 8] += 1
    stdout.print(product.m[1], counts[7])
",
    )?;
    assert_eq!(count(&counts, "allocations"), Some(0), "{counts}");
    Ok(())
}

/// Every name for a `shared` value sees it change, however it was reached, and what's plain
/// around it stays a copy.
const SHARED_NAMES: &str = "\
shared Log:
    lines = list[str]()
    count = 0
    note = \"\"

    def add(ref self, line: str):
        self.lines.append(line)
        self.count += 1
        self.note = line + \" was last\"

    def seen(self) -> int:
        self.lines.len()

struct Holder:
    log: Log
    tag = 0

def add_through(log: Log, line: str):
    log.add(line)

def changed(holder: Holder) -> Holder:
    mut mine = holder
    mine.tag += 1
    mine.log.add(\"from a copy\")
    mine

def main(stdout: Stdout):
    first = Log()
    second = first
    first.add(\"one\")
    second.add(\"two\")
    add_through(second, \"three\")
    stdout.print(first.seen(), second.seen(), first.count, first.note)
    holder = Holder(log=first)
    after = changed(holder)
    stdout.print(holder.tag, after.tag, holder.log.seen(), after.log.seen(), first.seen())
    logs = [first, Log(), first]
    logs[0].add(\"through a list\")
    copied = logs
    copied[2].add(\"through a copied list\")
    stdout.print(first.seen(), logs[1].seen(), copied[0].seen())
    for log in logs:
        log.add(\"in a loop\")
    stdout.print(first.seen(), logs[1].seen())
    mut inside = 0
    with first, logs[1] as ref a, b:
        a.count = 100
        a.lines.append(\"inside with\")
        inside = b.count
    stdout.print(inside, first.count, first.lines)
    stdout.print([first.count], f\"{holder.tag}\")
    stdout.print(logs[1])
";

/// Two cells in a `with`, each changed through its own name.
const TWO_CELLS: &str = r"shared Account:
    total = 0

    def add(ref self, amount: int):
        self.total += amount

def transfer(source: Account, target: Account, amount: int) -> bool:
    with source, target as ref x, ref y:
        if x.total < amount: return False
        x.total -= amount
        y.add(amount)
    True

def main(stdout: Stdout):
    two = Account(total=10)
    three = Account(total=20)
    stdout.print(transfer(two, target=three, amount=4), two.total, three.total)
    stdout.print(transfer(three, target=two, amount=40), two.total, three.total)
";

/// Each way a `with` can come to take one cell twice, all after printing something.
const TAKEN_TWICE: [(&str, &str); 5] = [
    ("spelled", "    with one, one as ref x, ref y:\n        x.total = y.total + 1\n"),
    (
        "reading",
        "    mut seen = 0\n    with one, one as x, y:\n        seen = x.total + y.total\n    stdout.print(seen)\n",
    ),
    ("named", "    other = one\n    with one, other as ref x, y:\n        x.total = y.total + 1\n"),
    ("passed", "    both(one, b=one)\n"),
    ("held", "    with Holder(box=one).box, one as ref x, y:\n        x.total = y.total + 1\n"),
];

const TAKING: &str = r#"shared Account:
    total = 0

struct Holder:
    box: Account

def both(a: Account, b: Account):
    with a, b as ref x, ref y:
        x.total = y.total + 1

def main(stdout: Stdout):
    one = Account(total=10)
    two = Account(total=20)
    with one, two as ref x, y:
        x.total = y.total + 1
    stdout.print("before", one.total)
"#;

/// A way a function can still reach a cell its caller holds the lock of, past what the checker sees:
/// a closure capturing the handle, called out of a list. The checker refuses the rest.
const TAKEN_AGAIN: [(&str, &str); 1] = [(
    "closure_in_list",
    "    def inner() -> int:\n        one.total\n    readers = [inner]\n    with one as ref x:\n        x.total = readers[0]()\n",
)];

const LOCK_HELD: &str = r#"shared Box:
    total = 1
    items = [1, 2, 3]

struct Holder:
    box: Box

def through_holder(holder: Holder) -> int:
    holder.box.total

def main(stdout: Stdout):
    one = Box()
    holder = Holder(box=one)
    stdout.print("before", through_holder(holder))
"#;

/// A lock this thread holds, taken again, ends the program, as it would wait forever otherwise.
#[test]
fn taking_a_lock_already_held_ends_the_program() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("taken-again")?;
    let path = |file: &Path| file.display().to_string();
    for (name, shape) in TAKEN_AGAIN {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, format!("{LOCK_HELD}{shape}    stdout.print(\"not here\")\n"))?;
        let executable = dir.join(name);
        let interpreted = outcome(&fennec(&["run", "--interp", &path(&program)], &dir)?);
        assert_eq!((interpreted.0.as_str(), interpreted.2), ("before 1\n", Some(1)), "{name}");
        assert!(interpreted.1.contains("that's already held"), "{name}: {}", interpreted.1);
        // Native code doesn't take a nested function kept in a list yet. Once it does, it has to
        // stop the same way.
        let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
        if !built.status.success() {
            continue;
        }
        let ran = outcome(&Command::new(&executable).output()?);
        assert_eq!((&ran.0, ran.2), (&interpreted.0, interpreted.2), "{name}");
        assert_eq!(ran.1.lines().next(), interpreted.1.lines().next(), "{name}");
        assert_eq!(ran.2, Some(1), "{name}");
        assert_eq!(ran.0, "before 1\n", "{name}");
        assert!(ran.1.contains("that's already held"), "{name}: {}", ran.1);
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A comprehension replacing a list in a cell changes it where it is, as nothing else can read
/// the list while the lock's held.
#[test]
fn a_comprehension_replacing_a_list_in_a_cell_makes_no_second_list() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "cell_list_replaced",
        "shared Box:\n    items = [1, 2, 3]\n\n    def doubled(ref self):\n        self.items = [n * 2 for n in self.items]\n\ndef main(stdout: Stdout):\n    one = Box()\n    one.doubled()\n    with one as ref x:\n        x.items = [n + 1 for n in x.items]\n    stdout.print(one.items)\n",
    )
}

#[test]
fn two_cells_in_a_with_each_change_through_their_own_name() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("two_cells", TWO_CELLS)
}

/// One cell taken twice ends the program, after what it printed, however it came about.
#[test]
fn a_with_taking_one_cell_twice_ends_the_program() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("taken-twice")?;
    let path = |file: &Path| file.display().to_string();
    for (name, shape) in TAKEN_TWICE {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, format!("{TAKING}{shape}    stdout.print(\"not here\")\n"))?;
        let executable = dir.join(name);
        let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
        assert!(built.status.success(), "{name}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = outcome(&Command::new(&executable).output()?);
        assert_eq!(ran, outcome(&fennec(&["run", "--interp", &path(&program)], &dir)?), "{name}");
        assert_eq!(ran.2, Some(1), "{name}");
        assert_eq!(ran.0, "before 21\n", "{name}");
        assert!(ran.1.contains("takes the same `shared` value twice"), "{name}: {}", ran.1);
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn walking_a_map_in_a_cell_while_another_name_changes_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "shared_map_walked",
        include_str!("fixtures/shared_map_walked.fen"),
    )
}

/// `==` on `shared` values, alone and inside other values: the same only if they're one value, as
/// comparing what two hold would take a second lock while one is held.
const SHARED_SAME: &str = "shared Account:
    total = 0

struct Holder:
    account: Account
    n: int

enum Either:
    One(account: Account)
    Nothing

def main(stdout: Stdout):
    a = Account(total=1)
    b = a
    c = Account(total=1)
    stdout.print(a == a, a == b, a == c, a != a, a != c)
    stdout.print(Holder(a, 1) == Holder(b, 1), Holder(a, 1) == Holder(c, 1))
    stdout.print([a] == [b], [a] == [c], a in [c], a in [b], [a, c].index(c))
    stdout.print(Either.One(a) == .One(b), Either.One(a) == .One(c))
    maybe: Account? = a
    stdout.print(maybe == b, maybe == c)
    with b as ref held:
        held.total = 2
    stdout.print(a == b, a == c)
";

#[test]
fn shared_values_are_the_same_only_if_they_are_one_value() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let (printed, ..) = interpreted("shared_same_said", SHARED_SAME)?;
    let wanted = "True True False False True\nTrue False\nTrue False False True 1\nTrue False\nTrue False\nTrue False\n";
    assert_eq!(printed, wanted);
    same_as_the_interpreter_in_both_builds("shared_same", SHARED_SAME)?;
    // A type's own `def ==` still decides, wherever it's written.
    let own = "\
shared Account:
    total = 0

    def ==(self, other: Account) -> bool:
        self.total > 100

def main(stdout: Stdout):
    a = Account(total=1)
    c = Account(total=200)
    stdout.print(a == a, a == c, c == a, c == c, a != c, c != a)
";
    same_as_the_interpreter_in_both_builds("shared_own_same", own)
}

#[test]
fn every_name_for_a_shared_value_sees_it_change() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("shared-names")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, SHARED_NAMES)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    let interpreted = fennec(&["run", "--interp", &path(&program)], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(outcome(&ran), outcome(&interpreted));
    Ok(())
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

def bump(ref holder: Holder, at: int):
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
    bump(ref holder, 1)
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
    for ref item in walked:
        item = item * 2
    stdout.print(walked[0], walked[2], before[0], before[2])
    mut grows = [1, 2]
    for item in copy(grows):
        grows[0] = item + 10
        grows[1] = 40
    stdout.print(grows[0], grows[1])
    mut points = [Packet(id=1, data=[1]), Packet(id=2, data=[2])]
    seen = points
    for ref point in points:
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

def nudge(ref p: Point):
    p.y = p.y + 1.0
    p.on = not p.on

def pair(n: int) -> (int, float):
    (n + 1, float(n) / 2.0)

def main(stdout: Stdout):
    a = Point(x=1.0, y=2.0, on=True)
    b = moved(a, 0.5)
    stdout.print(a.x, b.x, a.on)
    mut c = copy(b)
    nudge(ref c)
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

/// Values whose types write their own text, printed inside others, in every place they can be.
const OWN_TEXT_INSIDE: &str = "\
struct Tag:
    name: str

    def str(self, ref out: StrWriter):
        out.write(\"<\" + self.name + \">\")

struct Pair:
    x = 0
    y = 0

    def str(self, ref out: StrWriter):
        out.write(f\"{self.x}/{self.y}\")

enum Light:
    Red
    Green(level: int)

    def str(self, ref out: StrWriter):
        out.write(\"light\")

struct Holder:
    tag: Tag
    pair: Pair
    count = 1

def main(stdout: Stdout):
    stdout.print([Tag(name=\"a\"), Tag(name=\"b\" * 40)])
    stdout.print([Pair(x=1), Pair(y=2)])
    stdout.print([Light.Red, Light.Green(level=2)])
    stdout.print([(Tag(name=\"t\"), 1)])
    maybe: list[Pair?] = [Pair(), None]
    stdout.print(maybe)
    stdout.print([Holder(tag=Tag(name=\"h\"), pair=Pair(x=9))])
    stdout.print(f\"before {[Pair(x=5)]} after\")
    stdout.print(Tag(name=\"top\"), Pair(y=3))
    stdout.print([[Pair(x=7)], []])
";

#[test]
fn own_text_inside_other_values_is_written_by_native_code() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("own-text-inside")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, OWN_TEXT_INSIDE)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    let interpreted = fennec(&["run", "--interp", &path(&program)], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(outcome(&ran), outcome(&interpreted));
    Ok(())
}

/// Every shape of value printing reads from its block, nested, with text that needs quoting.
const SHOWN_VALUES: &str = "\
from std.collections import Table

struct Point:
    x = 0
    y = 0.5

struct Named:
    name: str
    tags: list[str]
    near: Point
    next: str?

struct Empty:
    pass

enum Shape:
    Dot
    Circle(radius: float, label: str)
    Box(corner: Point)

def main(stdout: Stdout):
    stdout.print([1, -2, 3])
    stdout.print([1.5, 2.0, 1e30, 0.1])
    stdout.print([True, False])
    stdout.print([\"plain\", \"with \\\"quotes\\\"\", \"line\\nbreak\", \"été\", \"\"])
    stdout.print([[1], [], [2, 3]])
    stdout.print([(1, \"a\"), (2, \"b\")])
    stdout.print([(1,)])
    stdout.print([Point(x=1), Point()])
    named = Named(name=\"n\", tags=[\"a\", \"b\"], near=Point(x=3), next=None)
    stdout.print([named, Named(name=\"m\" * 40, tags=[], near=Point(), next=\"s\" * 40)])
    stdout.print(f\"{[named]}\")
    stdout.print([Empty()])
    stdout.print([Shape.Dot, Shape.Circle(radius=1.5, label=\"c\"), Shape.Box(corner=Point())])
    stdout.print({\"a\": 1, \"long key that isn't inline, at all, in any way\": 2})
    empty: dict[str, int] = {}
    stdout.print(empty)
    stdout.print({1, 2})
    stdout.print({\"a\", \"b\"})
    none: set[int] = set()
    stdout.print(none)
    maybe: list[int?] = [1, None]
    stdout.print(maybe)
    words: list[str?] = [\"a\", None]
    stdout.print(words)
    points: list[Point?] = [Point(), None]
    stdout.print(points)
    mut map = Table[str]()
    first = map.add(\"a\")
    second = map.add(\"b\")
    map.add(\"c\")
    map.remove(second)
    stdout.print(map)
    stdout.print([first])
";

#[test]
fn values_read_from_their_blocks_show_as_they_do_in_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("shown-values")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, SHOWN_VALUES)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    let interpreted = fennec(&["run", "--interp", &path(&program)], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(outcome(&ran), outcome(&interpreted));
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

    def grow(ref self) -> int:
        self.items.append(9)
        self.count = self.count + 1
        self.count

struct Pair:
    first: list[int]
    second: int

def show(a: list[int], b: int) -> str:
    f"{a} {b}"

def take(ref a: list[int]) -> int:
    a.pop() else 0

def pair(a: int, b: int) -> str:
    f"{a} {b}"

def bump(ref a: int) -> int:
    a = a + 1
    a

def through(called: (list[int], int) -> str, ref items: list[int]) -> str:
    called(items, items.pop() else 0)

def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    stdout.print(xs, xs.pop())
    stdout.print(xs)
    mut ys = [1, 2, 3]
    stdout.print(show(ys, ys.pop() else 0), ys)
    mut zs = [1, 2, 3]
    stdout.print(show(zs, take(ref zs)), zs)
    mut d = {1: 2, 3: 4}
    stdout.print(d, d.pop(1), d)
    mut e = {1: 2, 3: 4}
    stdout.print(e.len(), e.pop(1), e.len())
    mut n = 1
    stdout.print(pair(n, b=bump(ref n)), n)
    stdout.print(n, bump(ref n), n)
    mut b = Box(items=[1], count=0)
    stdout.print(b.count, b.grow(), b.count)
    stdout.print(b.items, b.grow(), b.items)
    stdout.print(b, b.grow(), b)
    mut q = [1]
    stdout.print([q, [q.pop() else 0]], q)
    mut r = [1, 2]
    stdout.print((r, r.pop()), r)
    mut s = [1, 2]
    stdout.print(f"{s} {s.pop()} {s}")
    mut u = [1, 2]
    stdout.print(show(u, show(u, u.pop() else 0).len()), u)
    mut v = [1, 2]
    stdout.print(v[0], v.pop(0), v[0])
    mut w = [1, 2, 3]
    stdout.print(w[(w.pop() else 0) - 3], w)
    mut x = [1, 2, 3]
    stdout.print(x[:x.pop() else 0], x)
    mut f = 2
    stdout.print(f + bump(ref f), f)
    mut i = [1]
    stdout.print(Pair(first=i, second=i.pop() else 0), i)
    mut j = [1]
    stdout.print(Pair(second=j.pop() else 0, first=j), j)
    mut m = [1, 2]
    stdout.print([m.pop(), m.len(), m.pop(), m.len()], m)
    mut o = [1, 2]
    stdout.print(through((a, b) => show(a, b), ref o), o)
    mut t = {1: 1, 2: 2}
    stdout.print(t.get(t.pop(2) - 1), t.get(t.pop(1)), t)
    mut aa = "a"
    mut ab = [aa]
    stdout.print(ab, ab.pop(), aa)
"#,
    )
}

/// Every example builds through LLVM and prints exactly what it should.
#[test]
fn the_examples_it_takes_print_what_they_should() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("examples")?;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let (mut refused, mut wrong) = (Vec::new(), Vec::new());
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
            refused.push(format!("{}: {}", path.display(), String::from_utf8_lossy(&made.stderr).trim()));
            continue;
        }
        let ran = Command::new(&executable).output()?;
        if String::from_utf8_lossy(&ran.stdout) != std::fs::read_to_string(&wanted)? {
            wrong.push(path);
        }
    }
    std::fs::remove_dir_all(&dir)?;
    assert!(wrong.is_empty(), "printed the wrong thing: {wrong:?}");
    assert!(refused.is_empty(), "these don't build through LLVM:\n{}", refused.join("\n"));
    Ok(())
}

/// Every program in the repository builds as native code, with nothing left to the JIT or the
/// interpreter: each example, kit and benchmark, `examples/proof` and `selfhost`. The examples
/// that print something are run by the test above, and the rest by their own suites.
#[test]
fn every_program_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut programs = Vec::new();
    for entry in std::fs::read_dir(repo.join("examples"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            programs.push(path);
        }
    }
    for entry in std::fs::read_dir(repo.join("examples/projects"))? {
        let path = entry?.path();
        if path.join("kit.toml").is_file() {
            programs.push(path);
        }
    }
    programs.extend([repo.join("examples/proof"), repo.join("selfhost")]);
    let mut folders = vec![repo.join("bench")];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|ext| ext == "fen") {
                programs.push(path);
            }
        }
    }
    programs.sort();
    let dir = folder("every_program")?;
    let mut not_native = Vec::new();
    for program in &programs {
        let said = fennec(&["dev", "fallbacks", "--llvm", &program.to_string_lossy()], &dir)?;
        let told = String::from_utf8_lossy(&said.stdout);
        if !said.status.success() || told.lines().last() != Some("everything compiles to native code") {
            let why = format!("{told}{}", String::from_utf8_lossy(&said.stderr));
            not_native.push(format!("{}:\n  {}", program.display(), why.trim().replace('\n', "\n  ")));
        }
    }
    std::fs::remove_dir_all(&dir)?;
    assert!(programs.len() > 180, "found only {} programs", programs.len());
    assert!(not_native.is_empty(), "these aren't all native code:\n{}", not_native.join("\n"));
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
        r#"from std.collections import Table

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
    lights["stop"] = .Red
    stdout.print(lights, lights["wait"], lights["go"] == .Green)
    for name, light in lights.items():
        match light:
            case .Red:
                stdout.print(name, "red")
            case .Green:
                stdout.print(name, "green")
            case .Amber(seconds):
                stdout.print(name, seconds)
    mut notes = {1: Note.Said(text="a long note that sits in its own block"), 2: Note.Empty}
    notes[3] = .Said(text="short")
    notes[1] = .Said(text="replaced")
    stdout.print(notes, notes[3])
    stdout.print(notes.pop(1), notes.get(3), notes.values())
    mut people = {"me": Named(name="me", tags=["a"])}
    people["you"] = Named(name="a long name that sits in its own block", tags=[])
    kept = people["you"]
    people["you"] = Named(name="other", tags=["b", "c"])
    stdout.print(people, kept, people["you"].tags[1], people == people)
    mut ids = Table[Home]()
    first = ids.add(Home(id=1))
    second = ids.add(Home(id=2))
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

/// A generator made for a loop goes when the loop does, letting go of what it's holding right
/// then, while one with a name lasts as long as the name, and returning out of the loop lets go
/// of it before the caller goes on. A `shared` type's `del` doesn't run yet, so nothing prints
/// when the door goes; when it does, this shows each door closing at the right moment.
#[test]
fn a_generator_left_partway_closes_as_the_interpreter_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter(
        "generator_left",
        r#"shared Door:
    out: Stdout
    name: str

    def del(self):
        self.out.print(f"closing {self.name}")

def holding(out: Stdout, name: str) -> Iterator[int]:
    door = Door(out=out, name=name)
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
    stdout.print(first_over(100, over=50), first_over(5, over=50))
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

/// A field changed in place, by a builtin, a method on `ref self` or a `ref` argument, with an
/// argument worked out with a branch, a loop or a call: the field is read once the arguments
/// are, so it has one owner while it changes. Each way gives what the interpreter does, copies
/// and failures included.
#[test]
fn a_field_changed_with_any_argument_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("changed_with_any_argument", CHANGED_WITH_ANY_ARGUMENT)
}

const CHANGED_WITH_ANY_ARGUMENT: &str = r#"struct Bag:
    items = list[int]()

    def add(ref self, value: int):
        self.items.append(value)

def push(ref items: list[int], value: int):
    items.append(value)

def twice(x: int) -> int:
    x * 2

struct Holder:
    bag = Bag()
    items = list[int]()
    counts = dict[int, int]()
    text = ""

    def put(ref self, value: int, flag: bool):
        self.items.append(1 if flag else 0)
        self.items.append(sum([value for _ in range(2)]))
        self.items.append(self.items.len() if flag else twice(value))
        self.bag.add(value if flag else -value)
        push(ref self.items, value * 3 if value > 1 else 7)
        self.items.extend([value for _ in range(1) if flag])
        self.counts[value % 5] = 1 if flag else 0
        self.text += "a" if flag else "b"

def main(stdout: Stdout):
    mut holder = Holder()
    for i in range(6):
        holder.put(i, i % 2 == 0)
    copy = holder
    holder.put(9, flag=True)
    stdout.print(holder.items, holder.bag.items, holder.counts, holder.text)
    stdout.print(copy.items, copy.bag.items, copy.text)
    zero = holder.items.len() - holder.items.len()
    holder.items.append(10 // zero if zero > 0 else 3)
    stdout.print(holder.items.len())
    holder.items.append(10 // zero)
    stdout.print(holder.items.len())
"#;

/// The same changes, each in a loop: none of them copies the field, whatever its argument is.
#[test]
fn a_field_changed_with_any_argument_is_never_copied() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let shapes = [
        "self.items.append(1 if flag else 0)",
        "self.items.append(sum([value for _ in range(2)]))",
        "self.bag.add(value if flag else -value)",
        "push(ref self.items, value * 3 if value > 1 else 7)",
        "self.items.extend([value for _ in range(1) if flag])",
    ];
    for shape in shapes {
        let source = format!(
            "struct Bag:\n    items = list[int]()\n\n    def add(ref self, value: int):\n        self.items.append(value)\n\ndef push(ref items: list[int], value: int):\n    items.append(value)\n\nstruct Holder:\n    bag = Bag()\n    items = list[int]()\n\n    def put(ref self, value: int, flag: bool):\n        {shape}\n\n    def size(self) -> int:\n        self.items.len() + self.bag.items.len()\n\ndef main(stdout: Stdout):\n    mut holder = Holder()\n    for i in range(2000):\n        holder.put(i, i % 2 == 0)\n    stdout.print(holder.size())\n"
        );
        let counts = counts_of("changed_field", &source)?;
        assert_eq!(count(&counts, "copies"), Some(0), "{shape}: {counts}");
    }
    Ok(())
}

#[test]
fn what_a_field_or_an_items_field_holds_changes_without_being_copied() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "field",
        "struct Counter:\n    counts: dict[int, int]\n\n    def bump(ref self, key: int):\n        self.counts[key] = (self.counts.get(key) else 0) + 1\n\ndef main(stdout: Stdout):\n    mut counter = Counter(counts={})\n    for i in range(1000):\n        counter.bump(i % 64)\n    stdout.print(counter.counts[0])\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    let counts = counts_of(
        "items",
        "struct Var:\n    value: int\n    links: list[int]\n\nstruct Holder:\n    vars: list[Var]\n\n    def add(ref self, value: int):\n        self.vars.append(Var(value=value, links=[]))\n\n    def link(ref self, at: int, other: int):\n        self.vars[at].links.append(other)\n\ndef main(stdout: Stdout):\n    mut holder = Holder(vars=[])\n    for i in range(100):\n        holder.add(i)\n    for i in range(1000):\n        holder.link(i % 100, other=i)\n    stdout.print(holder.vars[5].links.len())\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    // A method written in Fennec changing a field: `self.inner.add(x)` and std's `extend`.
    let counts = counts_of(
        "methods",
        "struct Inner:\n    items: list[int]\n\n    def add(ref self, item: int):\n        self.items.append(item)\n\nstruct Outer:\n    inner: Inner\n    items: list[int]\n\n    def push(ref self, item: int):\n        self.inner.add(item)\n\n    def grow(ref self, more: list[int]):\n        self.items.extend(more)\n\ndef main(stdout: Stdout):\n    mut outer = Outer(inner=Inner(items=[]), items=[])\n    for i in range(100):\n        outer.push(i)\n    for i in range(100):\n        outer.grow([i])\n    stdout.print(outer.inner.items.len(), outer.items.len())\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// One number from a `counts_of` line, like `copies`.
fn count(counts: &str, name: &str) -> Option<u64> {
    counts.split_whitespace().skip_while(|word| *word != name).nth(1).and_then(|number| number.parse().ok())
}

/// `fennec dev trace` says each place that allocated or copied in a run, with how often and why,
/// and agrees with what the run counted altogether.
#[test]
fn a_trace_names_each_line_that_cost_something_and_why() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "\
def main(stdout: Stdout):
    mut total = 0
    for i in range(50):
        pair = [i, i + 1]
        total += pair[0]
    mut first = [1, 2, 3]
    second = first
    first.append(4)
    stdout.print(total, first.len(), second.len())
";
    let dir = folder("trace")?;
    let program = dir.join("trace.fen");
    std::fs::write(&program, source)?;
    let path = program.to_string_lossy().into_owned();
    let traced = fennec(&["dev", "trace", &path], &dir)?;
    let (said, errors) = (String::from_utf8_lossy(&traced.stdout), String::from_utf8_lossy(&traced.stderr));
    assert!(traced.status.success(), "{errors}");
    let list_line = said.lines().find(|line| line.contains(".fen:4:")).unwrap_or_default();
    assert!(list_line.contains("50 allocations") && list_line.contains("allocated"), "{said}");
    assert!(list_line.contains("(in a loop)"), "{said}");
    let copy_line = said.lines().find(|line| line.contains(".fen:8:")).unwrap_or_default();
    assert!(copy_line.contains("1 copy") && copy_line.contains("`first` is copied here"), "{said}");
    assert!(!said.contains(".fen:2:") && !said.contains(".fen:5:"), "{said}");

    // The hottest place comes first, and what it printed is left out.
    assert!(said.lines().next().is_some_and(|line| line.contains(".fen:4:")), "{said}");
    assert!(!said.lines().any(|line| line.starts_with("1225 ")), "{said}");

    let json = fennec(&["dev", "trace", "--json", &path], &dir)?;
    let json = String::from_utf8_lossy(&json.stdout);
    assert!(json.contains("\"allocations\":50"), "{json}");
    assert!(json.contains("\"failed\":false"), "{json}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What a trace found matches what the same program counts without it: tracing changes nothing
/// about what a run costs.
#[test]
fn a_trace_counts_what_a_plain_build_counts() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def main(stdout: Stdout):\n    mut items = [1]\n    for i in range(200):\n        items.append(i)\n    other = items\n    items.append(1)\n    stdout.print(other.len(), f\"{items.len()} long\")\n";
    let plain = counts_of("trace_plain", source)?;
    let dir = folder("trace_counts")?;
    let program = dir.join("counts.fen");
    std::fs::write(&program, source)?;
    let path = program.to_string_lossy().into_owned();
    let traced = fennec(&["dev", "trace", &path], &dir)?;
    let said = String::from_utf8_lossy(&traced.stdout);
    std::fs::remove_dir_all(&dir)?;
    assert!(traced.status.success());
    let allocations = count(&plain, "allocations").unwrap_or(0);
    let copies = count(&plain, "copies").unwrap_or(0);
    let last = said.lines().last().unwrap_or_default();
    assert!(last.contains(&format!("{allocations} allocation")), "{plain} / {said}");
    assert!(last.contains(&format!("{copies} cop")), "{plain} / {said}");
    Ok(())
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

    def drop_last(ref self):
        self.items.pop()

    def swap_in(ref self):
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
    let source = r#"def add(n: int, ref out: list[str]):
    if n > 0:
        out.append(f"<{n}:")
        add(n - 1, ref out)
        out.append(str(n))
        out.append(">")

def shown(items: list[str]) -> str:
    "-".join(items)

def put(ref out: list[str], more: list[str]):
    for item in more:
        out.append(item)

# Its list is given back, not joined, so `put` gets a list from here too.
def gathered(rows: list[list[str]]) -> list[str]:
    mut cells: list[str] = []
    for row in rows:
        put(ref cells, row)
    cells

def main(stdout: Stdout):
    mut out = ["start "]
    add(3, ref out)
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

#[test]
fn arguments_and_the_clock_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("args-and-clock")?;
    let program = dir.join("program.fen");
    std::fs::write(
        &program,
        "\
from std.time import Clock, Duration

def main(stdout: Stdout, args: Args, clock: Clock):
    stdout.print(args.len(), args.program != \"\")
    for arg in args:
        stdout.print(arg)
    stdout.print(args[0], args[-1])
    started = clock.now()
    clock.sleep(Duration.millis(2))
    stdout.print((clock.now() - started).nanos >= 2000000)
    stdout.print(args[5])
",
    )?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).args(["one", "two"]).output()?;
    let interpreted = fennec(&["run", "--interp", &path(&program), "one", "two"], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "2 True\none\ntwo\none two\nTrue\n");
    assert_eq!(outcome(&ran), outcome(&interpreted));
    Ok(())
}

/// What compiled code does through capabilities without the interpreter, failing as well as not.
const NATIVE_CAPABILITIES: &str = "\
from std.path import Path

def main(stdout: Stdout, fs: Fs, process: Process, stdin: Stdin, entropy: Entropy):
    folder = \"native-capabilities\"
    stdout.print(fs.exists(folder))
    fs.make_dir(folder).expect(\"it works\")
    file = folder + \"/a.txt\"
    fs.write_text(file, \"one\\ntwo\\n\").expect(\"it works\")
    stdout.print(fs.read_text(file).expect(\"it works\"), fs.exists(Path(text=file)))
    stdout.print(fs.metadata(file).expect(\"it works\").size, fs.metadata(file).expect(\"it works\").is_file)
    stdout.print(fs.list_dir(folder).expect(\"it works\"))
    only = fs.only(folder)
    stdout.print(only.read_text(file).expect(\"it works\"))
    stdout.print(only.exists(\"/\"))
    match only.read_text(\"/etc/hosts\"):
        case Ok(_):
            stdout.print(\"read it\")
        case Err(error):
            stdout.print(error.reason.endswith(\"native-capabilities\"))
    match fs.read_text(\"no such file\"):
        case Ok(_):
            stdout.print(\"read it\")
        case Err(error):
            stdout.print(error.reason != \"\")
    fs.remove(file).expect(\"it works\")
    fs.remove(folder).expect(\"it works\")
    done = process.run(\"echo\", [\"hello\"]).expect(\"it works\")
    stdout.print(done.status, done.stdout, done.stderr == \"\")
    match process.run(\"no-such-program-at-all\", []):
        case Ok(_):
            stdout.print(\"ran it\")
        case Err(error):
            stdout.print(error.program)
    echo = process.only(\"echo\")
    stdout.print(echo.run(\"echo\", [\"again\"]).expect(\"it works\").stdout)
    match echo.run(\"ls\", []):
        case Ok(_):
            stdout.print(\"ran it\")
        case Err(error):
            stdout.print(error.message)
    stdout.print(stdin.lines())
    stdout.print(entropy.seed() != entropy.seed())
";

#[test]
fn capabilities_without_the_interpreter_match_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("native-capabilities")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, NATIVE_CAPABILITIES)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let with_lines = |command: &mut Command| -> std::io::Result<Output> {
        use std::io::Write as _;
        let mut running = command
            .current_dir(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        running
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no input"))?
            .write_all(b"first\r\nsecond\n")?;
        running.wait_with_output()
    };
    let ran = with_lines(&mut Command::new(&executable))?;
    let interpreted = with_lines(
        crate::executables::fennec_command()
            .args(["run", "--interp", &path(&program)])
            .env("FENNEC_HOME", &dir),
    )?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(outcome(&ran), outcome(&interpreted));
    Ok(())
}

/// The real network, with no interpreter behind it: a server and a client on this machine, each
/// way a call can come out, and looping over each.
const REAL_NETWORK: &str = "from std.net import NetError, Connection

def reason[T](found: Result[T, NetError]) -> str:
    match found:
        case Err(error):
            error.message()
        case _:
            \"it worked\"

def lines_of(connection: Connection) -> int fails NetError:
    mut count = 0
    for line in connection.lines():
        count += line.len()
    count

def talk(net: Net, stdout: Stdout) -> int fails NetError:
    with net.listen(0)? as server:
        stdout.print(server.port > 0)
        with net.connect(\"localhost\", server.port)? as client:
            stdout.print(client.peer.endswith(f\":{server.port}\"))
            client.write(\"hello\\nfrom a client\\r\\nand more\")?
            with server.accept()? as conn:
                stdout.print(conn.read_line()?)
                stdout.print(conn.read_line()?)
                stdout.print(reason(conn.read_line(timeout=20ms)))
                stdout.print(reason(server.accept(timeout=20ms)))
                client.write(\"\\n\")?
                stdout.print(conn.read_line()?)
                client.write(\"one\\ntwo\\n\")?
                client.close()
                stdout.print(lines_of(conn)?)
                stdout.print(conn.read_line()?)
                conn.close()
                stdout.print(reason(conn.read_line()))
                stdout.print(reason(conn.write(\"gone\")))
    7

def main(net: Net, stdout: Stdout):
    stdout.print(reason(talk(net, stdout)))
    server = net.listen(0).expect(\"it listens\")
    client = net.connect(\"127.0.0.1\", server.port).expect(\"it connects\")
    client.write(\"looped\\n\").expect(\"it writes\")
    client.close()
    for conn in server:
        for line in conn.lines():
            stdout.print(line)
        break
    port = server.port
    server.close()
    stdout.print(reason(server.accept()))
    stdout.print(reason(net.connect(\"127.0.0.1\", port)) == f\"`127.0.0.1` refused a connection on port {port}\")
";

#[test]
fn the_real_network_works_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("real_network", REAL_NETWORK)
}

/// `bytes` over TCP and UDP natively, against the interpreter: what isn't text, reads of none and
/// at the end, a packet's sender, timing out, a bad address and port, and looping over a socket.
const NETWORK_BYTES: &str = "\
from std.net import NetError

def talk(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        with net.connect(\"127.0.0.1\", server.port, timeout=2s)? as client:
            with server.accept(timeout=2s)? as conn:
                client.write(bytes([0, 255, 10, 65]))?
                client.write(\"x\".encode())?
                data = conn.read(4, timeout=2s)?
                stdout.print(data, data.len(), data[1])
                stdout.print(conn.read(1, timeout=2s)?.decode() else \"\")
                stdout.print(conn.read(0)?.len())
                client.close()
                stdout.print(conn.read(4, timeout=2s)?.len())
    with net.udp(port=0)? as first:
        with net.udp()? as second:
            first.send_to(\"ping\", f\"127.0.0.1:{second.port}\")?
            packet = second.receive(timeout=2s)?
            stdout.print(packet.data.decode() else \"\", packet.sender.endswith(str(first.port)))
            second.send_to(bytes([1, 2, 3]), packet.sender)?
            answer = first.receive(timeout=2s)?
            stdout.print(answer.data, answer.data[0])
            match first.receive(timeout=20ms):
                case Err(error):
                    stdout.print(error.message())
                case Ok(_):
                    stdout.print(\"a packet from nowhere\")
            match first.send_to(\"x\", \"no-such-host.invalid:9\"):
                case Err(error):
                    stdout.print(error.message())
                case Ok(_):
                    stdout.print(\"sent to nothing\")
            first.send_to(\"last\", f\"127.0.0.1:{second.port}\")?
            for packet in second:
                stdout.print(packet.data.decode() else \"\")
                second.close()

def main(net: Net, stdout: Stdout):
    match talk(net, stdout):
        case Err(error):
            stdout.print(error.message())
        case Ok(_):
            stdout.print(\"done\")
    _ = net.udp(port=99999)
";

#[test]
fn bytes_and_packets_cross_the_real_network_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("network_bytes", NETWORK_BYTES)
}

/// An HTTP server natively: a handler answers each request, a connection stays open for more
/// until it asks to close, and what's too big or isn't HTTP is answered without the handler.
#[test]
fn an_http_server_answers_natively() -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Read, Write};
    if !ready() {
        return Ok(());
    }
    let source = r#"from std.net import Request, Response

def respond(request: Request) -> Response:
    match request.path:
        case "/hello":
            name = request.query.get("name") else "you"
            Response(status=200, body=f"hello, {name}".encode())
        case "/echo":
            Response(status=201, headers={"x-method": request.method}, body=request.body)
        case _:
            Response(status=404)

def main(net: Net, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=10s)
"#;
    let dir = folder("http_server")?;
    let program = dir.join("serve.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("serve"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?;
    let mut printed = BufReader::new(server.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?);
    let mut first = String::new();
    printed.read_line(&mut first)?;
    let port: u16 = first.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    // Everything the server says on one connection, once it closes it.
    let exchange = |asked: &[u8]| -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        stream.write_all(asked)?;
        let mut heard = String::new();
        stream.read_to_string(&mut heard)?;
        Ok(heard)
    };
    let kept = exchange(
        b"GET /hello?name=red%20fox HTTP/1.1\r\nHost: x\r\n\r\n\
          POST /echo HTTP/1.1\r\nContent-Length: 3\r\n\r\nden\
          GET /missing HTTP/1.1\r\nConnection: close\r\n\r\n",
    );
    let chunked = exchange(b"POST /echo HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nden\r\n0\r\n\r\n");
    let not_http = exchange(b"HELLO THERE\r\n\r\n");
    let too_big = exchange(b"POST /echo HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n");
    let many = format!("GET / HTTP/1.1\r\n{}\r\n", "X-A: b\r\n".repeat(101));
    let many_headers = exchange(many.as_bytes());
    server.kill()?;
    server.wait()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        kept?,
        "HTTP/1.1 200 OK\r\ncontent-length: 14\r\n\r\nhello, red fox\
         HTTP/1.1 201 Created\r\nx-method: POST\r\ncontent-length: 3\r\n\r\nden\
         HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    );
    assert!(chunked?.starts_with("HTTP/1.1 501 Not Implemented\r\n"));
    assert!(not_http?.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    assert!(too_big?.starts_with("HTTP/1.1 413 Content Too Large\r\n"));
    assert!(many_headers?.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
    Ok(())
}

/// HTTP natively against a server of the test's own: params, headers and a body go out, a status
/// that isn't a 200 is an answer, and what can't be reached fails as a `NetError`.
#[test]
fn http_requests_go_through_native_code() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        for _ in 0..3 {
            let (mut conn, _) = listener.accept()?;
            let mut asked = Vec::new();
            let mut chunk = [0; 4096];
            while !String::from_utf8_lossy(&asked).contains("\r\n\r\n") {
                let read = std::io::Read::read(&mut conn, &mut chunk)?;
                asked.extend_from_slice(chunk.get(..read).unwrap_or_default());
            }
            let asked = String::from_utf8_lossy(&asked).into_owned();
            let first = asked.lines().next().unwrap_or_default();
            let status = if first.contains("/a") { "200 OK" } else { "500 Oops" };
            let why = if asked.contains("X-Mine: yes") { "mine" } else { "plain" };
            let answer = format!(
                "HTTP/1.1 {status}\r\nX-Why: {why}\r\nContent-Length: {}\r\n\r\n{first}",
                first.len()
            );
            std::io::Write::write_all(&mut conn, answer.as_bytes())?;
        }
        Ok(())
    });
    let source = format!(
        r#"from std.net import NetError

def ask(net: Net, stdout: Stdout) fails NetError:
    mut answer = net.get("http://127.0.0.1:{port}/a", params={{"q": "x y"}}, headers={{"X-Mine": "yes"}}, timeout=5s)?
    stdout.print(answer.status, answer.ok(), answer.text(), answer.headers["x-why"], answer.body.len())
    answer = net.post("http://127.0.0.1:{port}/b", body="data")?
    stdout.print(answer.status, answer.ok(), answer.text(), answer.headers["x-why"])
    answer = net.request("put", "http://127.0.0.1:{port}/c", body="bytes".encode())?
    stdout.print(answer.status, answer.text())

def main(net: Net, stdout: Stdout):
    match ask(net, stdout):
        case Err(error):
            stdout.print(error.message())
        case Ok(_):
            pass
    match net.get("http://127.0.0.1:1/"):
        case Err(error):
            stdout.print(error.message())
        case Ok(_):
            stdout.print("an answer from nothing")
    match net.get("not a url"):
        case Err(error):
            stdout.print(error.message())
        case Ok(_):
            stdout.print("an answer from nothing")
"#
    );
    let dir = folder("http_native")?;
    let program = dir.join("http.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("http"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    std::fs::remove_dir_all(&dir)?;
    server.join().map_err(|_| std::io::Error::other("the test's server panicked"))??;
    assert_eq!(
        ours.0,
        "200 True GET /a?q=x%20y HTTP/1.1 mine 23\n\
         500 False POST /b HTTP/1.1 plain\n\
         500 PUT /c HTTP/1.1\n\
         `127.0.0.1` refused a connection on port 1\n\
         `not a url` isn't a URL like `https://example.com/path`\n"
    );
    Ok(())
}

/// TLS costs a program that uses it: its size, and loading what checks certificates on every
/// run. One that makes no requests has none of it.
#[test]
fn a_program_that_makes_no_requests_links_no_tls() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("no_tls")?;
    let program = dir.join("plain.fen");
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(\"hi\")\n")?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("plain"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let linked = std::fs::read(&executable)?;
    std::fs::remove_dir_all(&dir)?;
    let has = |word: &[u8]| linked.windows(word.len()).any(|at| at == word);
    // Not `rustls` itself, which a debug build's shared generics can name with no TLS linked.
    assert!(!has(b"ClientConnection"));
    assert!(!has(b"Security.framework"));
    Ok(())
}

/// A program built with no interpreter in its runtime prints what it should and has none of the
/// interpreter in it, and one still needing the interpreter fails to link rather than to run.
#[test]
fn a_lean_runtime_leaves_the_interpreter_out() -> std::io::Result<()> {
    if !ready() || !crate::executables::lean_runtime_built() {
        return Ok(());
    }
    let dir = folder("lean-runtime")?;
    let path = |file: &Path| file.display().to_string();
    let lean = |source: &Path, output: &Path| {
        crate::executables::fennec_command()
            .args(["build", "--llvm", &path(source), "-o", &path(output)])
            .env("FENNEC_HOME", &dir)
            .env("FENNEC_LEAN_RUNTIME", "1")
            .output()
    };
    let plain = dir.join("plain.fen");
    std::fs::write(
        &plain,
        "\
struct Tag:
    name: str

    def str(self, ref out: StrWriter):
        out.write(\"<\" + self.name + \">\")

def main(stdout: Stdout, args: Args, process: Process):
    stdout.print([Tag(name=\"a\")], {\"key\": 1}, args.len())
    stdout.print(process.run(\"echo\", [\"hello\"]).expect(\"it runs\").stdout)
",
    )?;
    let executable = dir.join("plain");
    let built = lean(&plain, &executable)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "[<a>] {\"key\": 1} 0\nhello\n\n");
    // A Windows executable has no symbols to look through. There, what follows is the check: what
    // needs the interpreter doesn't link.
    if !cfg!(windows) {
        let symbols = Command::new("nm").arg(&executable).output()?;
        assert!(symbols.status.success(), "{}", String::from_utf8_lossy(&symbols.stderr));
        let symbols = String::from_utf8_lossy(&symbols.stdout);
        assert!(symbols.contains("fennec_clock_now") || symbols.contains("fennec_print_block"), "{symbols}");
        // The interpreter's constructor, which the full runtime always calls when the program
        // starts. Rust writes each name as its length and the name. A looser match finds
        // `Interp` in the name of a small closure the linker folded together with the native code's own
        // copy, in a release build.
        assert!(!symbols.contains("6Interp11with_output"), "the interpreter is still linked");
    }

    let needs = dir.join("needs.fen");
    std::fs::write(
        &needs,
        "\
from std import json
from std.collections import Packed

def main(stdout: Stdout):
    stdout.print(json.dumps(Packed.of([[1, 2]])))
",
    )?;
    let refused = lean(&needs, &dir.join("needs"))?;
    assert!(!refused.status.success(), "it linked without the interpreter");
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("fennec_json"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// With nothing asked, a program whose code doesn't cross into the interpreter is built with the
/// runtime that leaves it out, and one that does is built with the runtime that has it, and runs.
#[test]
fn a_build_leaves_the_interpreter_out_unless_the_program_needs_it() -> std::io::Result<()> {
    if !ready() || !crate::executables::lean_runtime_built() {
        return Ok(());
    }
    let dir = folder("lean-default")?;
    let built = |source: &str, name: &str| -> std::io::Result<PathBuf> {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        let executable = dir.join(name);
        let built = crate::executables::fennec_command()
            .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
            .env("FENNEC_HOME", &dir)
            .env_remove("FENNEC_LEAN_RUNTIME")
            .output()?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        Ok(executable)
    };
    let has_interpreter = |executable: &Path| -> std::io::Result<bool> {
        let symbols = Command::new("nm").arg(executable).output()?;
        // The interpreter's constructor, as `a_lean_runtime_leaves_the_interpreter_out` looks for it.
        Ok(String::from_utf8_lossy(&symbols.stdout).contains("6Interp11with_output"))
    };
    let lean = built("def main(stdout: Stdout):\n    stdout.print([1, 2], {\"key\": 3})\n", "lean")?;
    assert_eq!(String::from_utf8_lossy(&Command::new(&lean).output()?.stdout), "[1, 2] {\"key\": 3}\n");
    let needs = built(
        "from std import json\nfrom std.collections import Packed\n\ndef main(stdout: Stdout):\n    stdout.print(json.dumps(Packed.of([[1, 2]])))\n",
        "needs",
    )?;
    assert!(Command::new(&needs).output()?.status.success());
    // A Windows executable has no symbols to look through.
    if !cfg!(windows) {
        assert!(!has_interpreter(&lean)?, "the interpreter is still linked");
        assert!(has_interpreter(&needs)?, "a program that needs the interpreter lost it");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// With nothing asked, a program that uses `parallel` is built with the runtime that runs it on
/// every core, and prints what it should; `FENNEC_THREADS_RUNTIME=0` keeps it on one thread.
#[test]
fn a_build_runs_parallel_on_every_core_unless_asked_not_to() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("threads-default")?;
    let program = dir.join("squares.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    xs = [i for i in range(100_000)]\n    squares = [x * x % 7 for x in parallel(xs)]\n    stdout.print(sum(n for n in parallel(squares)), squares[99_999])\n",
    )?;
    let built = |threads: Option<&str>, name: &str| -> std::io::Result<PathBuf> {
        let executable = dir.join(name);
        let mut command = crate::executables::fennec_command();
        command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
        command.env("FENNEC_HOME", &dir).env_remove("FENNEC_THREADS_RUNTIME");
        if let Some(threads) = threads {
            command.env("FENNEC_THREADS_RUNTIME", threads);
        }
        let built = command.output()?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        Ok(executable)
    };
    let has_pool = |executable: &Path| -> std::io::Result<bool> {
        let symbols = Command::new("nm").arg(executable).output()?;
        Ok(String::from_utf8_lossy(&symbols.stdout).contains("rayon_core"))
    };
    let wanted = "199999 2\n";
    let every_core = built(None, "every-core")?;
    let ran = Command::new(&every_core).env("FENNEC_THREADS", "4").output()?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), wanted);
    let one_thread = built(Some("0"), "one-thread")?;
    assert_eq!(String::from_utf8_lossy(&Command::new(&one_thread).output()?.stdout), wanted);
    // A Windows executable has no symbols to look through.
    if !cfg!(windows) {
        assert!(has_pool(&every_core)?, "the program has no pool to run on");
        assert!(!has_pool(&one_thread)?, "asked for one thread, the program has a pool");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// JSON and TOML of types that save themselves go through std's readers and writers in Fennec,
/// std's collections among them, so a program reading and writing them links with no interpreter
/// in it.
#[test]
fn json_and_toml_of_plain_types_need_no_interpreter() -> std::io::Result<()> {
    if !ready() || !crate::executables::lean_runtime_built() {
        return Ok(());
    }
    let dir = folder("formats-lean")?;
    let program = dir.join("formats.fen");
    std::fs::write(
        &program,
        "\
from std import json, toml
from std.collections import Deque, Grid, SortedMap, SortedSet

struct Spot:
    x: int
    tags: list[str]

struct Board:
    cells: Grid[int]
    scores: SortedMap[str, int]
    seen: SortedSet[int]
    queue: Deque[str]

def main(stdout: Stdout):
    text = json.dumps([Spot(x=1, tags=[\"a\"])])
    stdout.print(text, json.loads[list[Spot]](text) else [])
    stdout.print(toml.loads[Spot](toml.dumps(Spot(x=2, tags=[]))) else Spot(x=0, tags=[]))
    board = Board(cells=Grid.of([[1, 2]]), scores=SortedMap.of([(\"b\", 2), (\"a\", 1)]), seen=SortedSet.of([3, 1]), queue=Deque.of([\"go\"]))
    saved = json.dumps(board)
    stdout.print(saved, json.dumps(json.loads[Board](saved) else board) == saved)
",
    )?;
    let executable = dir.join("formats");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .env("FENNEC_LEAN_RUNTIME", "1")
        .output()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "[{\"x\": 1, \"tags\": [\"a\"]}] [Spot(x=1, tags=[\"a\"])]\nSpot(x=2, tags=[])\n\
         {\"cells\": [[1, 2]], \"scores\": {\"a\": 1, \"b\": 2}, \"seen\": [1, 3], \"queue\": [\"go\"]} True\n"
    );
    Ok(())
}

/// Native JSON and TOML write and read what the JIT does, and fail with the same messages: a
/// field left out, a value of the wrong type, a key no field has, inside and outside, and text
/// that isn't JSON.
#[test]
fn native_json_and_toml_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "native_formats",
        r#"from std import json, toml

enum Kind:
    Fox
    Owl(wise: bool)

struct Spot:
    x: int
    y = 2.5

struct Unit:
    name: str
    level: int
    kind: Kind
    pet: str?
    at: Spot
    tags: list[str]
    stats: dict[str, int]

def said[T](found: Result[T, ParseError]) -> str:
    match found:
        case Err(error):
            error.message
        case _:
            "read"

def main(stdout: Stdout):
    ash = Unit(name="Ash \"the\" fox\n", level=3, kind=.Owl(wise=True), pet=None, at=Spot(x=1), tags=["a", "é"], stats={"hp": 10})
    text = json.dumps(ash)
    stdout.print(text)
    stdout.print(json.dumps([ash, ash], indent=2))
    back = json.loads[Unit](text) else ash
    stdout.print(back == ash, json.dumps(back) == text)
    stdout.print(said(json.loads[Unit]("{\"name\": \"a\"}")))
    stdout.print(said(json.loads[Unit]("{\"name\": 1, \"level\": 2}")))
    stdout.print(said(json.loads[Unit]("{\"name\": \"a\", \"at\": {\"x\": 1, \"z\": 2}, \"extra\": 1}")))
    stdout.print(said(json.loads[Unit]("[1, 2")))
    stdout.print(said(json.loads[list[int]]("[1, \"two\"]")))
    stdout.print(json.loads[Spot]("{\"x\": 1, \"x\": 2}") else Spot(x=0))
    spot_text = toml.dumps(Spot(x=4))
    stdout.print(spot_text)
    stdout.print(toml.loads[Spot](spot_text) else Spot(x=0))
    stdout.print(said(toml.loads[Spot]("x = \"a\"")))
"#,
    )
}

/// Two known differences between native JSON and the JIT's, both only in what a failed read says:
/// a key given twice whose first value doesn't fit fails natively, where the JIT keeps the last
/// value; and with two fields of the wrong type, native code names the first in the text, where
/// the JIT names the first the type declares.
#[test]
fn native_json_differs_from_the_jit_in_two_known_ways() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let ((ours, _, _), (jit, _, _)) = both(
        "native_json_differences",
        r#"from std import json

struct Spot:
    x: int
    y = 2.5

def said(found: Result[Spot, ParseError]) -> str:
    match found:
        case Err(error):
            error.message
        case _:
            "read"

def main(stdout: Stdout):
    stdout.print(said(json.loads[Spot]("{\"x\": \"one\", \"x\": 2}")))
    stdout.print(said(json.loads[Spot]("{\"y\": \"no\", \"x\": \"one\"}")))
"#,
    )?;
    assert_eq!(ours, "`x` should be an int, but it's a str\n`y` should be a float, but it's a str\n");
    assert_eq!(jit, "read\n`x` should be an int, but it's a str\n");
    Ok(())
}

/// `bytes` are written as standard base64 natively, padded with `=`, and read back; text that
/// isn't base64 says so.
#[test]
fn native_json_and_toml_write_bytes_as_base64() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("bytes_base64")?;
    let program = dir.join("blob.fen");
    std::fs::write(
        &program,
        r#"from std import json, toml

struct Blob:
    data: bytes

def main(stdout: Stdout):
    for size in range(5):
        mut items = list[int]()
        for at in range(size):
            items.append(250 + at)
        blob = Blob(data=bytes(items))
        text = json.dumps(blob)
        back = json.loads[Blob](text) else Blob(data=bytes([]))
        stdout.print(text, back.data == blob.data)
    stdout.print(toml.dumps(Blob(data="hello".encode())))
    match json.loads[Blob]("{\"data\": \"ab=c\"}"):
        case Err(error):
            stdout.print(error.message)
        case Ok(_):
            stdout.print("read")
"#,
    )?;
    let executable = dir.join("blob");
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "{\"data\": \"\"} True\n{\"data\": \"+g==\"} True\n{\"data\": \"+vs=\"} True\n\
         {\"data\": \"+vv8\"} True\n{\"data\": \"+vv8/Q==\"} True\ndata = \"aGVsbG8=\"\n\n\
         `data` should be bytes, written in base64\n"
    );
    Ok(())
}

/// Builds and runs a program through LLVM, giving back what it printed.
fn printed_by(name: &str, source: &str) -> std::io::Result<String> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let executable = dir.join(name);
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
}

/// What the next test builds: a round trip, then bytes `std.pack` should refuse.
const PACK_MESSAGES: &str = r#"from std import pack
from std.pack import Limits

enum Kind:
    Soldier
    Archer(range: int)
    Medic

struct Item:
    name: str
    weight = 1.5

struct Place:
    x: int
    y: int

struct Unit:
    name: str
    health: int
    kind: Kind
    at: Place
    items: list[Item]
    target: int? = None
    tags = dict[str, int]()
    alive = True

struct Guarded:
    points: int where points >= 0

struct Link:
    next: Link? = None

def said[T](found: Result[T, ParseError]) -> str:
    match found:
        case Ok(_):
            return "loaded"
        case Err(error):
            return error.message

def changed(data: bytes, at: int, to: int) -> bytes:
    mut items = list[int]()
    for b in range(data.len()):
        items.append(to if b == at else data[b])
    bytes(items)

def main(stdout: Stdout):
    units = [
        Unit(name="ash", health=10, kind=.Soldier, at=Place(x=0, y=-3), items=[Item(name="axe", weight=2.25)]),
        Unit(
            name="vëx",
            health=9223372036854775807,
            kind=.Archer(range=7),
            at=Place(x=-9223372036854775807 - 1, y=1),
            items=[],
            target=3,
            tags={"a": 1, "b": -1},
        ),
        Unit(name="", health=0, kind=.Medic, at=Place(x=1, y=2), items=[Item(name="x"), Item(name="y")], alive=False),
    ]
    data = pack.dumps(units)
    back = pack.loads[list[Unit]](data) else []
    stdout.print(data.len(), back == units, back[1].name, back[1].at.x, back[1].tags, back[0].items[0].weight)
    stdout.print(pack.loads[float](pack.dumps(-0.1)) else 0.0, pack.loads[str](pack.dumps("")) else "x")
    stdout.print(said(pack.loads[list[Item]](data)))
    stdout.print(said(pack.loads[list[Unit]](data[0:data.len() - 1])))
    stdout.print(said(pack.loads[int](bytes([1, 2, 3]))))
    stdout.print(said(pack.loads[int](pack.dumps(5) + bytes([0]))))
    stdout.print(said(pack.loads[str](pack.dumps("hello world"), limits=Limits(length=4))))
    mut deep = Link()
    for _ in range(10):
        deep = Link(next=deep)
    stdout.print(said(pack.loads[Link](pack.dumps(deep), limits=Limits(depth=4))), said(pack.loads[Link](pack.dumps(deep))))
    medic = pack.dumps(Kind.Medic)
    stdout.print(said(pack.loads[Kind](changed(medic, at=12, to=9))))
    stdout.print(said(pack.loads[bool](changed(pack.dumps(True), at=12, to=2))))
    one = pack.dumps(Guarded(points=1))
    # The points, 1, are the zigzag byte 2, and 1 is -1.
    stdout.print(said(pack.loads[Guarded](changed(one, at=12, to=1))))
"#;

/// `std.pack` reads back what it wrote, ints at both ends, a name that isn't ASCII, a case with
/// data and a dict among them, and says why bytes don't load: a type of another shape, bytes
/// that end too soon or go on past it, ones it didn't write, a length or a depth over the
/// limit, a case or a bool that doesn't exist, and a `where` rule broken.
#[test]
fn pack_reads_back_what_it_wrote_and_says_why_other_bytes_do_not_load() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("pack_messages", PACK_MESSAGES)?;
    assert_eq!(
        printed,
        "105 True vëx -9223372036854775808 {\"a\": 1, \"b\": -1} 2.25\n\
         -0.1 \n\
         this was packed from a different `list[Item]`, at byte 4\n\
         the data ends too soon, at byte 104\n\
         these aren't bytes `std.pack` wrote, at byte 0\n\
         there's more after the value, at byte 13\n\
         a length is over the limit of 4, at byte 12\n\
         it nests deeper than the limit of 4, at byte 16 loaded\n\
         `Kind` has no case 9, at byte 12\n\
         a bool should be 0 or 1, at byte 12\n\
         Guarded.points breaks its `where` rule, at byte 13\n"
    );
    Ok(())
}

/// What the next test builds: loads of good bytes truncated, changed and replaced.
const PACK_FUZZ: &str = r#"from std import pack
from std.pack import Limits

enum Shape:
    Dot
    Circle(radius: float)
    Box(width: int, height: int)

struct Node:
    name: str
    shape: Shape
    data: bytes
    weights: list[float]
    names: dict[str, int]
    next: Node? = None

## A number from the last, the same every run.
def step(seed: int) -> int:
    (seed * 1103515245 + 12345) % 2147483648

def tried(data: bytes, limits: Limits) -> int:
    match pack.loads[Node](data, limits=limits):
        case Ok(_):
            return 1
        case Err(_):
            return 0

def main(stdout: Stdout):
    tail = Node(name="tail", shape=.Dot, data=bytes([0, 255]), weights=[], names={})
    node = Node(
        name="héad", shape=.Box(width=3, height=-4), data=bytes([1, 2, 3]), weights=[0.5, -2.0], names={"a": 1}, next=tail
    )
    good = pack.dumps(node)
    small = Limits(depth=8, length=64)
    stdout.print(good.len(), tried(good, Limits()), tried(good, small))
    # Every truncation fails.
    mut loaded = 0
    for size in range(good.len()):
        loaded += tried(good[0:size], Limits())
    stdout.print("truncated", loaded)
    mut items = list[int]()
    for at in range(good.len()):
        items.append(good[at])
    # Every byte changed, one at a time, to bytes from a seed.
    mut seed = 7
    loaded = 0
    mut runs = 0
    for at in range(good.len()):
        for _ in range(40):
            seed = step(seed)
            mut changed = copy(items)
            changed[at] = (seed >> 16) & 255
            loaded += tried(bytes(changed), Limits())
            loaded += tried(bytes(changed), small)
            runs += 2
    stdout.print("changed", runs, loaded > 0)
    # Bytes from a seed after a good header.
    loaded = 0
    for round in range(3000):
        seed = step(seed)
        size = (seed >> 8) % 64
        mut random = list[int]()
        for at in range(12):
            random.append(items[at])
        for _ in range(size):
            seed = step(seed)
            random.append((seed >> 16) & 255)
        loaded += tried(bytes(random), Limits())
    stdout.print("random", loaded >= 0)
"#;

/// Bytes that aren't what `std.pack` wrote never crash a load or take more than its limits:
/// every truncation of good bytes fails, and so do most with one byte changed, at the default
/// limits and small ones, and random bytes after a good header.
#[test]
fn pack_loads_of_changed_and_random_bytes_fail_without_crashing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("pack_fuzz", PACK_FUZZ)?;
    assert_eq!(printed, "59 1 1\ntruncated 0\nchanged 4720 True\nrandom True\n");
    Ok(())
}

/// What the next test builds: a world of `Table`s and `Id`s, saved both ways, and bytes changed.
const PACK_TABLES: &str = r#"from std import pack
from std.collections import Id, Table

struct Unit:
    name: str
    target: Id[Unit]? = None

struct World:
    units = Table[Unit]()
    selected = list[Id[Unit]]()
    last: Id[Unit]? = None

def said[T](found: Result[T, ParseError]) -> str:
    match found:
        case Ok(_):
            return "loaded"
        case Err(error):
            return error.message

def changed(data: bytes, at: int, to: int) -> bytes:
    mut items = list[int]()
    for b in range(data.len()):
        items.append(to if b == at else data[b])
    bytes(items)

def main(stdout: Stdout):
    mut world = World()
    ash = world.units.add(Unit(name="ash"))
    gone = world.units.add(Unit(name="gone"))
    vex = world.units.add(Unit(name="vex", target=ash))
    world.units.remove(gone)
    kit = world.units.add(Unit(name="kit", target=vex))
    world.units.remove(ash)
    world.selected = [vex, kit]
    world.last = ash
    # By position: the items in order, and the Id whose item has gone saved as missing.
    data = pack.dumps(world)
    back = pack.loads[World](data) else World()
    stdout.print(data.len(), back.units.len(), back.last is None, pack.dumps(back) == data)
    for id in back.selected:
        unit = back.units[id]
        target = unit.target
        stdout.print(unit.name, back.units[target].name if target is not None else "-")
    # Slot for slot: the same slots and generations, and the Id whose item has gone still gone.
    exact = pack.dumps(world, exact=True)
    mut same = pack.loads[World](exact) else World()
    stdout.print(exact.len(), exact[3], same.units.len(), same.units[same.selected[1]].name)
    stdout.print(same.last is not None, (same.last else same.selected[0]) in same.units)
    # The next add takes the same slot, at the same generation, in both.
    _ = world.units.add(Unit(name="fox"))
    _ = same.units.add(Unit(name="fox"))
    stdout.print(pack.dumps(same, exact=True) == pack.dumps(world, exact=True))
    # What doesn't load.
    stdout.print(said(pack.loads[World](changed(data, at=18, to=9))))
    stdout.print(said(pack.loads[World](changed(exact, at=12, to=2))))
    stdout.print(said(pack.loads[World](changed(exact, at=32, to=20))))
    stdout.print(said(pack.loads[World](changed(exact, at=13, to=200))))
    # Every truncation fails, and every byte changed loads or fails, in both modes.
    mut seed = 11
    for good in [data, exact]:
        mut cut = 0
        for size in range(good.len()):
            if said(pack.loads[World](good[0:size])) == "loaded":
                cut += 1
        mut changes = 0
        for at in range(good.len()):
            for _ in range(30):
                seed = (seed * 1103515245 + 12345) % 2147483648
                found = pack.loads[World](changed(good, at=at, to=(seed >> 16) & 255))
                match found:
                    case Ok(loaded):
                        # What loads has every Id somewhere in its Table, or gone.
                        for id in loaded.selected:
                            _ = loaded.units.get(id)
                    case Err(_):
                        pass
                changes += 1
        stdout.print("truncated", cut, "changed", changes)
"#;

/// `std.pack` saves a `Table` as JSON does, by position, with an `Id` whose item has gone saved
/// as missing, and with `exact=True` slot for slot: the gone `Id` still gone, and the next `add`
/// in the same slot. An `Id` past its `Table`, or slots that don't fit the items, don't load, and
/// no truncation or changed byte crashes a load in either mode.
#[test]
fn pack_saves_tables_by_position_and_slot_for_slot() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("pack_tables", PACK_TABLES)?;
    assert_eq!(
        printed,
        "28 2 True True\nvex -\nkit vex\n39 69 2 kit\nTrue False\nTrue\n`units[0].target` is out of range, at byte 19\n`units` has 1 slot in use but 0 items, at byte 16\n`selected[0]` is item 10 of `units`, which has only 3\nthe data ends too soon, at byte 19\ntruncated 0 changed 840\ntruncated 0 changed 1170\n"
    );
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
        "def main(stdout: Stdout):\n    mut kept = Buffer[str]()\n    kept.insert(0, \"fox\")\n    kept.insert(1, \"kit\")\n    stdout.print(kept[0], kept[1])\n    kept.insert(5, \"den\")\n    stdout.print(kept[0])\n",
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

    def names(ref self, ref into: list[str]):
        for at in range(self.items.len()):
            item = self.items[at]
            if self.weight_of(at) > 1:
                for tag in item.tags:
                    into.append(tag)
            into.append(item.name)

    def first_tags(self) -> list[str]:
        self.items[0].tags

    def relabel(ref self):
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
    shelf.names(ref seen)
    kept = shelf.first_tags()
    shelf.relabel()
    stdout.print(seen, kept, shelf.items[0].name, shelf.log)
    tree = Tree.Node(left=.Node(left=.Leaf, right=.Leaf), right=.Leaf)
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

/// Floats have one equality and hash everywhere: `0.0` is `-0.0`, and NaN is the same as any other
/// NaN, whatever its sign. So this is held to what that decision says, on LLVM and the interpreter
/// both. The JIT's `==` and keys don't follow it yet.
#[test]
fn floats_are_equal_as_decided() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Pair:
    x: float
    y: float

enum Light:
    Red
    Green(seconds: float)

struct Tagged:
    x: float
    name: str

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
    stdout.print(same(zero, b=minus), same(nan, b=nan), same(nan, b=other), same(nan, b=one), same(one, b=one))
    stdout.print(different(zero, b=minus), different(nan, b=other), different(nan, b=one), different(one, b=2.0))
    stdout.print(zero == -0.0, nan == 1.0, 1.0 == nan, nan != 1.0, 1.0 != nan, -0.0 == zero)
    stdout.print(Pair(x=zero, y=nan) == Pair(x=minus, y=other), Pair(x=1.0, y=nan) == Pair(x=1.0, y=2.0))
    stdout.print(Pair(x=zero, y=nan) != Pair(x=minus, y=other))
    stdout.print((zero, nan) == (minus, other), (nan, 1.0) == (1.0, 1.0))
    stdout.print(zero in [minus], other in [nan], 1.0 in [nan])
    stdout.print(Light.Green(zero) == .Green(minus), Light.Green(nan) == .Green(other))
    stdout.print(Tagged(x=zero, name="a") == Tagged(x=minus, name="a"), Tagged(x=nan, name="a") == Tagged(x=other, name="a"))
    stdout.print(Tagged(x=1.0, name="a") == Tagged(x=2.0, name="a"), Tagged(x=1.0, name="a") == Tagged(x=1.0, name="b"))
"#;
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
True True
True True
False False
"
    );
    Ok(())
}

/// Floats as keys have one equality: `-0.0` is `0.0`, and every NaN is the same, with the key
/// kept as it was first written.
#[test]
fn float_keys_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
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

/// Keys that hold strings or floats go by what they hold, not by their words.
#[test]
fn keys_holding_strings_and_floats_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter(
        "keys_by_value",
        r#"struct Named:
    id: int
    name: str

enum Tag:
    Plain
    Label(text: str)

def joined(a: str, b: str) -> str:
    a + b

def main(stdout: Stdout):
    mut pairs = {(1, "one"): [1], (2, "a long string in its own block"): [2]}
    long = joined("a long string ", b="in its own block")
    stdout.print(pairs[(1, "one")], pairs[(2, long)], (2, long) in pairs, (3, long) in pairs)
    pairs[(2, long)] = [9]
    pairs[(3, joined("th", b="ree"))] = [3]
    kept = pairs
    pairs[(4, "four")] = []
    stdout.print(pairs, pairs.len(), kept.len(), pairs.get((3, "three")), pairs.get((9, "no")))
    stdout.print(pairs.pop((1, "one")), pairs.keys(), kept == pairs, kept == kept)
    for key, value in pairs.items():
        stdout.print(key, value)
    mut people = {Named(id=1, name="ann"): 1}
    people[Named(id=2, name=joined("b", b="ob"))] = 2
    stdout.print(people[Named(id=2, name="bob")], Named(id=1, name="ann") in people, people.len())
    mut tags = {Tag.Plain, Tag.Label(text="x"), Tag.Label(text="x")}
    tags.add(Tag.Label(text=joined("y", b="z")))
    tags.discard(Tag.Label(text="x"))
    stdout.print(tags, tags.len(), Tag.Label(text="yz") in tags, Tag.Plain in tags)
    zero = 0.0
    mut floats = {(zero, 1): "a"}
    floats[(-zero, 1)] = "b"
    floats[(1.5, 2)] = "c"
    stdout.print(floats, floats.len(), floats[(-zero, 1)])
    mut grown: dict[(int, str), int] = {}
    for at in range(200):
        grown[(at % 20, f"key {at % 20}")] = at
    stdout.print(grown.len(), grown[(7, "key 7")], grown[(19, "key 19")])
"#,
    )
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

/// A `Table` uses its first free slot first, whatever order slots came free in, and where it
/// looks from never changes which that is: the `Id`s it gives are the interpreter's.
#[test]
fn an_id_maps_new_items_go_in_its_first_free_slot() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "id_map_free_slots",
        r#"from std.collections import Id, Table

struct Named:
    name: str

def main(stdout: Stdout):
    mut names = Table[Named]()
    mut ids: list[Id[Named]] = []
    for at in range(8):
        ids.append(names.add(Named(f"name {at} long enough not to be held inline")))
    stdout.print(ids)
    # Free from high to low, low to high, and in between.
    names.remove(ids[6])
    names.remove(ids[2])
    names.remove(ids[4])
    first = names.add(Named("a"))
    second = names.add(Named("b"))
    names.remove(ids[0])
    third = names.add(Named("c"))
    fourth = names.add(Named("d"))
    fifth = names.add(Named("e"))
    stdout.print(first, second, third, fourth, fifth, names.len())
    stdout.print(names.get(ids[2]) is None, names[first], names[fifth])
    # A stale `Id` frees nothing, so it can't move where to look.
    names.remove(ids[2])
    sixth = names.add(Named("f"))
    stdout.print(sixth, names.values())
    # A copy keeps looking from where it was, apart from the original.
    mut copied = copy(names)
    copied.remove(ids[1])
    names.remove(ids[3])
    stdout.print(copied.add(Named("g")), names.add(Named("h")), copied.values(), names.values())
"#,
    )
}

/// `reversed` of text and tuples, and `remove_where` and `clear` on a `Table`. An item added
/// after either goes in the first slot it left free.
#[test]
fn reversing_and_removing_from_tables_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "reversed_removed",
        r#"from std.collections import Table
from std.math import sqrt

struct Unit:
    name: str
    hp: int

def main(stdout: Stdout):
    four = 16
    stdout.print(sqrt(16), sqrt(four), sqrt(2.25))
    stdout.print(reversed("fennec"), reversed("héllo"), reversed(""), reversed([1, 2, 3]))
    stdout.print(reversed((1, 2)), reversed((1, 2, 3, 4)), reversed(("a", "a long string in a block")))
    mut names = Table[str]()
    first = names.add("ash")
    names.add("kit")
    third = names.add("a long name that sits in its own block")
    copied = names
    names.remove_where(name => name.startswith("k"))
    stdout.print(names.values(), names.len(), copied.values(), copied.len(), names[first], names[third])
    fourth = names.add("vex")
    stdout.print(names.values(), names.len(), names[fourth])
    names.remove_where(name => False)
    stdout.print(names.len())
    names.remove_where(name => True)
    stdout.print(names.values(), names.len(), names.get(first))
    mut units = Table[Unit]()
    weak = units.add(Unit(name="weak", hp=1))
    strong = units.add(Unit(name="strong", hp=9))
    units.remove_where(unit => unit.hp < 5)
    stdout.print(units.values(), units.get(weak), units[strong].name)
    mut numbers = Table[int]()
    numbers.add(1)
    numbers.add(2)
    numbers.remove_where(n => n % 2 == 0)
    stdout.print(numbers.values(), numbers.len())
    numbers.clear()
    stdout.print(numbers.values(), numbers.len())
    gone = numbers.add(5)
    stdout.print(numbers.values(), numbers.len(), numbers[gone])
    mut cleared = copy(names)
    cleared.add("again")
    cleared.clear()
    stdout.print(cleared.len(), names.len(), cleared.values())
    ignore(numbers.remove(gone), "it's fine if it's gone")
    stdout.print(numbers.len())
"#,
    )
}

/// `Table.modify` changes an item where it is through a closure with a `ref` parameter, and a
/// `Table`'s items change their fields as a list's do.
#[test]
fn changing_what_a_table_holds_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "table_changes",
        r#"from std.collections import Id, Table

struct Enemy:
    name: str
    health: int
    tags: list[str]

    def take_damage(ref self, amount: int) -> int:
        self.health = self.health - amount
        self.tags.append("hit")
        self.health

struct Node:
    value: int
    children: list[Id[Node]]

struct World:
    enemies: Table[Enemy]

def main(stdout: Stdout):
    mut world = World(enemies=Table[Enemy]())
    near = world.enemies.add(Enemy(name="near", health=100, tags=[]))
    far = world.enemies.add(Enemy(name="a long name that sits in its own block", health=50, tags=["x"]))
    kept = world
    stdout.print(world.enemies.modify(far, foe => foe.take_damage(25)))
    stdout.print(world.enemies[far], kept.enemies[far])
    world.enemies.remove(near)
    stdout.print(world.enemies.modify(near, foe => foe.take_damage(1)), world.enemies.len())
    mut numbers = Table[int]()
    one = numbers.add(1)
    numbers.modify(one, n => n * 5)
    stdout.print(numbers[one])
    mut nodes = Table[Node]()
    root = nodes.add(Node(value=1, children=[]))
    leaf = nodes.add(Node(value=2, children=[]))
    nodes[root].children.append(leaf)
    nodes[root].value = 9
    copied = nodes
    nodes[root].children.append(root)
    stdout.print(nodes[root].value, nodes[root].children.len(), copied[root].children.len())
"#,
    )
}

/// `items()` and `values()` gone through in order, with slots empty, at the start and the end,
/// and everything an item can be, which the loop walks with no list made.
const WALKING_A_MAP: &str = r#"from std.collections import Table

struct Unit:
    name: str
    tags: list[int]

struct Point:
    x: int
    y: float

def total(numbers: Table[int]) -> int:
    mut sum_of = 0
    for id, number in numbers.items():
        sum_of += number + numbers[id]
    for number in numbers.values():
        sum_of += number
    sum_of

def named(units: Table[Unit]) -> str:
    mut parts = []
    for id, unit in units.items():
        parts.append(f"{unit.name}:{unit.tags.len()}:{units[id].name}")
    for unit in units.values():
        parts.append(unit.name)
    ",".join(parts)

def main(stdout: Stdout):
    mut numbers = Table[int]()
    ids = [numbers.add(n) for n in range(8)]
    stdout.print(total(numbers))
    numbers.remove(ids[0])
    numbers.remove(ids[3])
    numbers.remove(ids[4])
    numbers.remove(ids[7])
    stdout.print(total(numbers), numbers.len())
    numbers.add(100)
    stdout.print(total(numbers), numbers.len())

    mut units = Table[Unit]()
    first = units.add(Unit("first", [1]))
    units.add(Unit("second is a much longer name than fits", [2, 3]))
    last = units.add(Unit("é", []))
    stdout.print(named(units))
    units.remove(first)
    units.remove(last)
    stdout.print(named(units))

    mut points = Table[Point]()
    gone = points.add(Point(0, 0.5))
    points.add(Point(1, 1.5))
    points.add(Point(2, 2.5))
    points.remove(gone)
    for id, point in points.items():
        stdout.print(point.x, point.y, points[id].x)

    mut words = Table[str]()
    words.add("a")
    words.add("a word too long to fit in a header")
    for word in words.values():
        stdout.print(word)

    # Nothing in it, then nothing left in it.
    mut empty = Table[int]()
    for id, number in empty.items():
        stdout.print(number)
    only = empty.add(1)
    empty.remove(only)
    for number in empty.values():
        stdout.print(number)
    stdout.print(total(empty))

    # Inside another, and gone through more than once.
    held = [1, 2]
    for round in held:
        for number in numbers.values():
            for other in numbers.values():
                if number == other and round == 2:
                    stdout.print(number)
    pairs = numbers.items()
    for again in range(2):
        for id, number in pairs:
            stdout.print(again, number, numbers[id])
    stdout.print(pairs.len())

    # Changed after it's asked for and before it's gone through, which sees it as it was.
    mut changing = Table[int]()
    kept = [changing.add(n) for n in range(3)]
    before = changing.items()
    changing.remove(kept[1])
    for id, number in before:
        stdout.print(number)
    # Changed between rounds of an outer loop.
    for round in range(3):
        for number in changing.values():
            stdout.print(round, number)
        changing.add(round * 100)
"#;

#[test]
fn walking_a_map_says_what_a_list_of_its_items_would() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("walking_a_map", WALKING_A_MAP)
}

#[test]
fn walking_a_map_makes_no_list() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut numbers = Table[int]()\n    for n in range(1000):\n        numbers.add(n)\n    mut found = 0\n    for round in range(100):\n        for id, number in numbers.items():\n            found += number\n        for number in numbers.values():\n            found += number\n    stdout.print(found)\n";
    let counts = counts_of("walking_no_list", source)?;
    assert!(count(&counts, "allocations") < Some(40), "{counts}");
    Ok(())
}

#[test]
fn id_maps_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "idmap",
        r#"from std.collections import Id, Table

struct Unit:
    name: str
    health: int

def main(stdout: Stdout):
    mut units = Table[Unit]()
    a = units.add(Unit(name="fox", health=10))
    b = units.add(Unit(name="kit", health=5))
    units[a].health -= 3
    stdout.print(units[a].health, units.len(), units.get(b) is None)
    gone = units.remove(b)
    stdout.print(units.get(b) is None, units.len(), a == a, a == b)
    stdout.print(a in units, b in units, a not in units, b not in units)
    c = units.add(Unit(name="den", health=7))
    stdout.print(b in units, c in units)
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
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut names = Table[str]()\n    fox = names.add(\"fox\")\n    names.remove(fox)\n    stdout.print(names[fox])\n",
    )
}

/// An `Id` finds nothing in a `Table` other than the one that gave it out, though a copy of that
/// one, holding the same items, is the same `Table`.
#[test]
fn ids_from_another_table_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "other_table",
        r#"from std.collections import Id, Table

def main(stdout: Stdout):
    mut ours = Table[str]()
    mut theirs = Table[str]()
    fox = ours.add("fox")
    kit = theirs.add("kit")
    stdout.print(fox == kit, ours.get(kit) is None, kit in ours, fox in theirs)
    stdout.print(theirs.remove(fox) is None, theirs.len(), ours[fox])
    mut copied = ours
    copied[fox] = "den"
    stdout.print(copied[fox], ours[fox], fox in copied)
    held: list[Id[str]] = ours.ids()
    for id, name in theirs.items():
        stdout.print(name, id in theirs, id in ours, held[0] in ours)
"#,
    )?;
    same_message_as_the_jit(
        "other_table_stop",
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut ours = Table[str]()\n    mut theirs = Table[str]()\n    ours.add(\"fox\")\n    kit = theirs.add(\"kit\")\n    stdout.print(ours[kit])\n",
    )?;
    same_message_as_the_jit(
        "other_table_store",
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut ours = Table[str]()\n    mut theirs = Table[str]()\n    ours.add(\"fox\")\n    kit = theirs.add(\"kit\")\n    ours[kit] = \"den\"\n    stdout.print(ours.len())\n",
    )?;
    // Saved, an `Id` has to be from the `Table` saved with it.
    same_message_as_the_jit(
        "other_table_saved",
        r#"from std import json
from std.collections import Id, Table

struct World:
    units = Table[str]()
    selected = list[Id[str]]()

def main(stdout: Stdout):
    mut world = World()
    mut elsewhere = Table[str]()
    world.units.add("ash")
    world.selected = [elsewhere.add("vex")]
    stdout.print(json.dumps(world))
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

/// All of `std.crypto`, of text and of bytes, which needs no interpreter.
#[test]
fn crypto_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "crypto_all",
        r#"from std import crypto

def main(stdout: Stdout):
    stdout.print(crypto.sha256(""), crypto.blake3(""))
    stdout.print(crypto.hmac_sha256("key", "message"))
    key = crypto.derive_key("a context", "a secret")
    stdout.print(key, crypto.keyed_blake3(key, "message"))
    stdout.print(crypto.equal(key, key), crypto.equal("short", "longer"))
"#,
    )
}

/// A BLAKE3 key that isn't one says so, as it does on the JIT.
#[test]
fn a_bad_crypto_key_fails_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "crypto_bad_key",
        r#"from std import crypto

def main(stdout: Stdout):
    stdout.print(1)
    stdout.print(crypto.keyed_blake3("not hex", "message"))
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
    total = 0
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
    one = Spot(x=1, y=2.5, on=True, team=.Blue)
    flag = 3 > 2
    stdout.print(f"at {at} {one} {flag} {not flag} {Team.Red}")
    mut items = [5, 6, 7, 8]
    gone = items.pop(1)
    last = items.pop(-1)
    stdout.print(gone, last, items)
    mut spots = [Spot(x=9, y=0.5, on=False, team=.Red), one]
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
        total += dice.randint(1, high=6)
    stdout.print(total, dice.randint(-5, high=5), dice.drawn)
    stdout.print(f"{dice.random():.6f}", f"{dice.uniform(2.0, high=3.0):.6f}")
    mut wide = Random(seed=-7)
    stdout.print(wide.randint(-9223372036854775807, high=9223372036854775807))
    stdout.print(dice.randint(3, high=1))
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

    def bump(ref self):
        self.n += 1
        self.log.append(self.n)
        self.n += 1

    def keep(ref self, ref into: list[Counter]):
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
    a.keep(ref kept)
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

    def keep(ref self):
        self.saved = self.items

    def poke(ref self):
        self.items[0] = 1
        self.keep()
        self.items[0] = 99

    def touch(ref self, at: int):
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

/// New string keys a dict nothing else holds gets, from slices and whole strings: added in
/// native code while short and the dict has room, and by the runtime when long, the dict is
/// full or shared, or the text isn't ASCII. Every way gives what the interpreter does.
#[test]
fn new_string_keys_go_in_as_on_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "new_string_keys",
        r#"def main(stdout: Stdout):
    dna = "acgtacgtttgacca" * 300
    for size in [1, 3, 12, 23, 24, 30]:
        mut counts: dict[str, int] = {}
        for at in range(dna.len() - size + 1):
            piece = dna[at:at + size]
            counts[piece] = (counts.get(piece) else 0) + 1
        mut best = 0
        for key, value in counts.items():
            best = max(best, value)
        stdout.print(size, counts.len(), best, counts.get(dna[:size]) else -1)
    mut words: dict[str, float] = {}
    for at in range(200):
        words[f"w{at % 37}"] = float(at) * 0.5
        words[f"a much longer key than a header holds {at % 5}"] = 1.0
    stdout.print(words.len(), words["w3"], sorted(words.keys())[:4])
    mut seen: set[str] = set()
    for at in range(100):
        seen.add(dna[at:at + 4])
    stdout.print(seen.len(), "acgt" in seen)
    mut shared: dict[str, int] = {"x": 1}
    copy = shared
    for at in range(50):
        shared[dna[at:at + 2]] = at
    stdout.print(shared.len(), copy.len(), copy)
    mut flags: dict[str, bool] = {}
    for at in range(40):
        flags[f"{at % 9}"] = at % 2 == 0
    stdout.print(flags)
    uni = "éacgtü" * 20
    mut by: dict[str, int] = {}
    for at in range(uni.len() - 3):
        by[uni[at:at + 3]] = at
    stdout.print(by.len(), by["acg"])
"#,
    )
}

/// String keys of every length up to past what a header holds, from slices at every place in a
/// block's text and from whole strings: a short one is compared by its header's words, its last
/// part word read as the bytes ending where it ends, and both ways find the same entries.
#[test]
fn string_keys_of_every_length_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "string_key_lengths",
        r#"def main(stdout: Stdout):
    text = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ"
    mut by_slice: dict[str, int] = {}
    mut by_word: dict[str, int] = {}
    for size in range(31):
        for at in range(text.len() - size + 1):
            piece = text[at:at + size]
            by_slice[piece] = (by_slice.get(piece) else 0) + 1
            word = "".join([text[i] for i in range(at, at + size)])
            by_word[word] = (by_word.get(word) else 0) + 1
    mut agree = 0
    for key, count in by_slice.items():
        if (by_word.get(key) else -1) == count:
            agree += 1
    stdout.print(by_slice.len(), by_word.len(), agree, by_slice.get("") else -1, by_slice.get(text[3:20]) else -1)
    mut again = 0
    for size in [0, 1, 7, 8, 9, 15, 16, 17, 22, 23, 24, 29]:
        again += by_slice.get(text[5:5 + size]) else 0
        again += by_word.get(text[5:5 + size]) else 0
        again += by_slice.get(text[0:size] + "") else 0
    stdout.print(again, by_slice.get("nothere") else -1, by_slice.get(text[:23] + "!") else -1)
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
    .Word(text)

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

def replace(ref token: Token, text: str):
    token = .Word(text)

def pick(at: int) -> Token?:
    if at == 0:
        return None
    if at == 1:
        return .Word("a string long enough that it can't be held inline")
    if at == 2:
        return .Word("short")
    .Many([at], "label")

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
    a = .Num(1)
    stdout.print(describe(a), describe(b))
    mut c = copy(b)
    replace(ref c, "changed")
    stdout.print(describe(b), describe(c))
    # Reassigning in a loop lets go of what it held.
    mut last = Token.Blank
    for at in range(1000):
        last = word(f"a long string number {at} that doesn't fit inline")
    stdout.print(describe(last))
    # Equality.
    same = [word(long) == word(long), word("x") == word("x"), word("x") == word("y"),
        word("x") != .Num(1), Token.Blank == .Blank, Token.Num(1) == .Num(1),
        Token.Many([1], "a") == .Many([1], "a"), Token.Many([1], "a") == .Many([2], "a"),
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
    list_of[0] = .Num(9)
    popped = list_of.pop() else return
    stdout.print(list_of.len(), describe(list_of[0]), describe(list_of[50]), describe(popped))
    # First case holding a string, moved out and let go of at the end.
    mut first = First.Named(long)
    moved = first
    first = .Named("other")
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
    d = Doc(.Word(long), [Token.Blank, Token.Num(2), Token.Word("x")], long)
    text = json.dumps(d)
    stdout.print(text)
    back = json.loads[Doc](text) else Doc(.Blank, [], "")
    stdout.print(back == d, back)
    token = json.loads[Token](json.dumps(Token.Word(long))) else Token.Blank
    stdout.print(token)
    tokens = json.loads[list[Token]](json.dumps(d.tokens)) else []
    stdout.print(tokens)
    # An expected result whose error is one.
    stdout.print(get(5).expect("five is fine"))
    # Enums that hold numbers only, printed.
    stdout.print(Shape.Circle(1.5), Shape.Empty, [Shape.Rect(1.0, h=2.0)])
    stdout.print(f"{Shape.Rect(1.0, h=2.0)}")
"#;

/// Everything that holds, copies, compares or shows an enum in place, in every case.
/// Types holding themselves through a list, which live in place: an enum and a struct, on their
/// own and with others between. Moved, copied, changed, compared and printed.
const THROUGH_LISTS: &str = r#"enum Json:
    Null
    Flag(value: bool)
    Number(value: int)
    Text(value: str)
    Items(values: list[Json])
    Fields(names: list[str], values: list[Json])

struct Tree:
    label: str
    children: list[Tree]

    def add(ref self, label: str):
        self.children.append(Tree(label, []))

struct Folder:
    name: str
    files: list[File]

struct File:
    name: str
    inside: Holder

struct Holder:
    size: int
    folders: list[Folder]

enum Expr:
    Number(value: int)
    Add(parts: list[Expr])
    Named(name: str, value: Wrapped)

struct Wrapped:
    parts: list[Expr]

def printed(value: Json) -> str:
    match value:
        case .Null:
            "null"
        case .Flag(flag):
            "true" if flag else "false"
        case .Number(number):
            str(number)
        case .Text(text):
            f"'{text}'"
        case .Items(values):
            ",".join([printed(item) for item in values])
        case .Fields(names, values):
            mut parts = []
            for at, name in enumerate(names):
                parts.append(f"{name}={printed(values[at])}")
            ";".join(parts)

def document(depth: int) -> Json:
    if depth == 0:
        return .Number(7)
    .Fields(
        ["a much longer name than fits in a header", "b"],
        [Json.Items([document(depth - 1), Json.Null, Json.Flag(True)]), Json.Text("é")],
    )

def count(tree: Tree) -> int:
    mut total = 1
    for child in tree.children:
        total += count(child)
    total

def deep(depth: int) -> Tree:
    mut tree = Tree("leaf", [])
    for at in range(depth):
        tree = Tree("node", [tree])
    tree

def depth_of(tree: Tree) -> int:
    mut held = tree
    mut seen = 0
    while held.children.len() > 0:
        held = held.children[0]
        seen += 1
    seen

def total(expr: Expr) -> int:
    match expr:
        case .Number(value):
            value
        case .Add(parts):
            sum(total(part) for part in parts)
        case .Named(name, value):
            name.len() + sum(total(part) for part in value.parts)

def changed(value: Json) -> Json:
    match value:
        case .Items(values):
            mut more = copy(values)
            more.append(Json.Null)
            .Items(more)
        case _:
            value

def main(stdout: Stdout):
    made = document(3)
    stdout.print(printed(made))
    held = made
    other = changed(made)
    stdout.print(printed(held) == printed(made), printed(other).len())
    stdout.print(printed(changed(.Items([made, made]))).len())
    stdout.print(made == held, made == .Null, Json.Number(1) == .Number(1))
    stdout.print(Json.Text("a") == .Text("a"), Json.Items([Json.Null]) == .Items([Json.Null]))

    mut items = [Json.Null, Json.Text("in a list"), Json.Items([Json.Number(1)])]
    copied = items
    items.append(document(1))
    items[0] = .Text("replaced")
    last = items.pop() else return
    stdout.print(items.len(), copied.len(), printed(items[0]), printed(copied[0]), printed(last))
    stdout.print(items)

    mut tree = Tree("root", [])
    tree.add("left")
    tree.add("right")
    tree.children[0].add("under")
    kept = tree
    tree.children[1].add("more")
    tree.children[1].children[0].add("deeper")
    stdout.print(count(tree), count(kept), tree.children[1].children[0].children[0].label)
    stdout.print(tree == kept, kept.children[1].children.len())
    stdout.print(depth_of(deep(20000)))

    root = Folder("root", [File("f", Holder(1, [Folder("sub", [File("g", Holder(2, []))])]))])
    stdout.print(root.files[0].inside.folders[0].files[0].inside.size, root.files[0].name)

    expr = Expr.Add([Expr.Number(1), Expr.Named("two", Wrapped([Expr.Number(2)])), Expr.Add([])])
    stdout.print(total(expr))
"#;

#[test]
fn types_holding_themselves_through_a_list_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("through_lists", THROUGH_LISTS)
}

/// What's in place goes with the block holding it, so freeing a deep one needs no stack of its
/// own and leaves nothing behind.
#[test]
fn a_type_holding_itself_through_a_list_allocates_no_block_of_its_own() -> std::io::Result<()> {
    // Told to box what holds blocks, it does.
    if !ready() || std::env::var_os("FENNEC_INLINE_STRUCTS").is_some_and(|set| set == "0") {
        return Ok(());
    }
    let source = "enum Json:\n    Number(value: int)\n    Items(values: list[Json])\n\ndef main(stdout: Stdout):\n    mut items = []\n    for n in range(1000):\n        items.append(Json.Number(n))\n    stdout.print(Json.Items(items) == .Number(0))\n";
    let counts = counts_of("through_lists_blocks", source)?;
    assert!(count(&counts, "allocations") < Some(30), "{counts}");
    Ok(())
}

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
    for ref item in parallel(tokens):
        item = .Word("changed, and long enough that it can't be held inline")
    stdout.print(tokens)
    stdout.print([describe(item) for item in parallel(tokens)])
    mut more = [Token.Word(long + "!") for at in range(200)]
    for ref item in parallel(more):
        if describe(item).len() > 10:
            item = .Blank
    stdout.print(more[0], more[199], sum(describe(item).len() for item in parallel(more)))
"#,
    )
}

/// A value passed `mut`, with a copy or another name for it passed without: that one keeps what it held.
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

def both(items: list[int], ref others: list[int]) -> int:
    others.append(9)
    others[0] = 7
    items.len() * 10 + items[0]

def bags(bag: Bag, ref other: Bag) -> str:
    other.count = 5
    other.label = "a label long enough to be held in a block"
    f"{bag.label} {bag.count}"

def main(stdout: Stdout):
    mut twice = [1, 2]
    stdout.print(both(copy(twice), ref twice), twice)
    mut bag = Bag(label="short", count=1)
    stdout.print(bags(copy(bag), ref bag), bag)
    kept = twice
    stdout.print(both(kept, ref twice), twice, kept)
    held = bag
    stdout.print(bags(held, ref bag), bag, held)
"#,
    )
}

/// `copy` with fields changes only what it names and runs the `where` rule of each field it
/// changes, so a copy that breaks one stops the program as a write would.
#[test]
fn copy_with_fields_runs_their_rules() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_the_jit(
        "copy_with_fields",
        r#"struct Player:
    name: str
    level: int where level >= 1

def main(stdout: Stdout):
    player = Player(name="Ash", level=5)
    raised = copy(player, level=player.level + 1)
    stdout.print(player.level, raised.level, raised.name)
    stdout.print(copy(player, name="Kit", level=player.level - 5).level)
"#,
    )
}

/// `list.pop()` gives `None` once the list is empty, and the item as an optional before then:
/// for numbers, text and structs, from a name, a field, an item of a list, and an item picked
/// by a call, which is read once.
#[test]
fn popping_an_empty_list_gives_none() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "pop_or_none",
        r#"struct Unit:
    name: str

struct Squad:
    units: list[Unit]
    rows: list[list[int]]

def picked(at: int) -> int:
    at % 2

def drained(ref items: list[str]) -> int:
    mut count = 0
    while True:
        item = items.pop() else break
        count += item.len()
    count

def main(stdout: Stdout):
    mut numbers = [1, 2]
    stdout.print(numbers.pop(), numbers.pop(), numbers.pop(), numbers)
    mut words = ["a long word that isn't held inline", "fox"]
    stdout.print(drained(ref words), words, words.pop() else "none")
    mut squad = Squad(units=[Unit(name="ash")], rows=[[1], [2, 3]])
    first = squad.units.pop()
    stdout.print(first.name if first is not None else "none", squad.units.pop() is None)
    stdout.print(squad.rows[1].pop(), squad.rows[picked(1)].pop(), squad.rows[picked(1)].pop())
    squad.rows[0].pop()
    squad.rows[0].pop()
    stdout.print(squad.rows)
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

def rename(ref person: Person, name: str):
    person = Person(name, 2)

def set_person(ref person: Person, other: Person):
    person = other

def set_token(ref token: Token, other: Token):
    token = other

def retext(ref token: Token, text: str):
    token = .Word(text)

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    mut total = 0
    for at in range(2000):
        mut person = Person(long, 1)
        rename(ref person, long + "!")
        keep = Person(long + "?", 3)
        total += person.name.len()
        set_person(ref person, keep)
        total += person.name.len() + keep.name.len()
        mut token = Token.Word(long)
        retext(ref token, long + "!")
        other = Token.Word(long + "?")
        same = token == other
        set_token(ref token, other)
        mut people = [Person(long + "a", 1), Person(long + "b", 2), Person(long + "c", 3)]
        gone = people.pop() else return
        first = people.pop(0)
        people.pop()
        mut tokens = [Token.Word(long + "a"), Token.Blank, Token.Word(long + "b"), Token.Word(long + "c")]
        last = tokens.pop() else return
        early = tokens.pop(0)
        tokens.pop(0)
        tokens.remove_where((item) => item == .Blank)
        total += gone.name.len() + first.name.len() + people.len() + tokens.len()
        if same or last == early:
            total += 1
    stdout.print(total)
"#,
    )
}

/// A key holding a string is hashed and compared by what it holds, not by its words, as two equal
/// strings can be two blocks. So native code takes those dicts.
#[test]
fn keys_holding_strings_build_natively() -> std::io::Result<()> {
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
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&output).output()?;
    assert!(ran.status.success());
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "1\n");
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

/// Programs whose `main` fails, each with what comes first in it, in every way it can: an error
/// of one type, one of several, one held in a block of its own, and only after printing.
const FAILING_MAINS: [(&str, &str); 4] = [
    (
        "declared",
        "def main(stdout: Stdout) fails Short:\n    stdout.print(short(1)?)\n    stdout.print(short(2)?)\n    stdout.print(\"not here\")\n",
    ),
    (
        "undeclared",
        "def main(stdout: Stdout):\n    stdout.print(short(1)?)\n    check(4)?\n    stdout.print(\"not here\")\n",
    ),
    (
        "several",
        "def main(stdout: Stdout):\n    stdout.print(short(1)?)\n    stdout.print(coded(2)?)\n    stdout.print(short(2)?)\n",
    ),
    (
        "held",
        "def main(stdout: Stdout):\n    stdout.print(coded(1)?)\n    stdout.print(held(2)?)\n    stdout.print(coded(3)?)\n",
    ),
];

const WHAT_MAINS_CALL: &str = r#"error Short:
    why: str

error Coded:
    code: int

error Held:
    items: list[int]
    table: dict[str, int]

def short(n: int) -> int fails Short:
    if n > 1: fail Short(why="too big")
    n

def coded(n: int) -> int fails Coded:
    if n > 1: fail Coded(code=n)
    n

def held(n: int) -> int fails Held:
    if n > 1: fail Held(items=[n, n], table={"k": n})
    n

def check(n: int) fails Short:
    if n > 3: fail Short(why="the end")

"#;

/// A failing `main` says what the interpreter does, at the same place, after what it printed, and
/// the program ends with the same code.
#[test]
fn a_failing_main_ends_as_on_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("failing-main")?;
    let path = |file: &Path| file.display().to_string();
    for (name, main) in FAILING_MAINS {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, format!("{WHAT_MAINS_CALL}{main}"))?;
        let executable = dir.join(name);
        let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
        assert!(built.status.success(), "{name}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output()?;
        let interpreted = fennec(&["run", "--interp", &path(&program)], &dir)?;
        assert_eq!(outcome(&ran), outcome(&interpreted), "{name}");
        assert_eq!(ran.status.code(), Some(1), "{name}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn errors_of_several_types_go_up_and_show_as_on_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "errors_of_several_types",
        include_str!("fixtures/errors_of_several_types.fen"),
    )
}

/// A function that says `fails A | B`, with errors that have cases among them.
#[test]
fn failing_several_ways_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "fails_several_ways",
        include_str!("fixtures/fails_several_ways.fen"),
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

    def str(self, ref out: StrWriter):
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
        "\"q\" in [\"a\"]",
        "Plain(x=1, y=2.0) == Plain(x=2, y=2.0)",
        "Point(x=1, label=\"p\") == Point(x=2, label=\"q\")",
        "Shown(x=1) != Shown(x=1)",
        "Light.Red == .Green",
        "seen(True) is None",
        "seen(False) is not None",
        "named(True) is None",
        "named(False) is not None",
        "seen(True) == seen(False)",
        "[1, 2] == [1, 3]",
        "[\"a\"] == [\"b\"]",
        "{\"a\": 1} == {\"a\": 2}",
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
/// A loop sees what its body gave a name, on every engine: the checker used to say what the name
/// held before the loop, and LLVM folded `is None` on that.
#[test]
fn a_loop_tests_what_its_body_gave_a_name() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Box:
    v: int

def main(stdout: Stdout):
    mut x: int? = None
    for n in range(3):
        if x is None:
            stdout.print("none", n)
            x = n
        else:
            stdout.print("some", n)
    stdout.print(x)
    mut inferred = None
    for n in range(3):
        if inferred is None:
            stdout.print("no box", n)
            inferred = Box(v=n)
        else:
            stdout.print("box", n)
    stdout.print(inferred)
    mut text = None
    mut i = 0
    while text is None:
        stdout.print("no text", i)
        i += 1
        if i == 3:
            text = "a"
    stdout.print(text)
    mut nested: int? = None
    for a in range(2):
        for b in range(2):
            stdout.print(nested is None, a, b)
        nested = a
    stdout.print(nested)
"#;
    let (ours, jit) = both("a_loop_tests_what_its_body_gave_a_name", source)?;
    let held = interpreted("a_loop_tests_what_its_body_gave_a_name", source)?;
    assert_eq!(ours, held);
    assert_eq!(jit, held);
    Ok(())
}

/// The same when the body gives it a value only some of the time, or leaves early.
#[test]
fn a_loop_tests_what_a_path_through_its_body_gave_a_name() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def fill(ref slot: int?):
    slot = 7

def main(stdout: Stdout):
    mut inner: int? = None
    for a in range(2):
        stdout.print(inner is None, a)
        for b in range(2):
            inner = b
    mut branch: int? = None
    for at in range(3):
        if branch is not None:
            stdout.print(branch + 1, at)
        if at == 0:
            branch = 5
    mut left: int? = None
    for at in range(4):
        if left is not None:
            stdout.print("left", left, at)
            break
        if at == 1:
            left = at
    mut skipped: int? = None
    for at in range(4):
        if skipped is None:
            if at == 1:
                skipped = at
            continue
        stdout.print("skipped", skipped, at)
    mut matched: int? = None
    for at in range(3):
        match at:
            case 0:
                stdout.print(matched is None)
            case _:
                stdout.print(matched is None)
                matched = at
    mut passed: int? = None
    for at in range(2):
        stdout.print(passed is None, at)
        fill(ref passed)
    mut shown: int? = None
    for at in range(2):
        def show(stdout: Stdout):
            stdout.print("in", at)
        show(stdout)
        stdout.print(shown is None)
        shown = at
"#;
    let (ours, jit) = both("a_loop_tests_what_a_path_through_its_body_gave_a_name", source)?;
    let held = interpreted("a_loop_tests_what_a_path_through_its_body_gave_a_name", source)?;
    assert_eq!(ours, held);
    assert_eq!(jit, held);
    Ok(())
}

/// The other way round, a name that starts with a value and is given None. Not against the JIT,
/// which stops when it's given None: #572.
#[test]
fn a_loop_tests_a_name_its_body_gave_none() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Held:
    x: int?

    def clear(ref self):
        self.x = None

def clear(ref slot: int?):
    slot = None

def main(stdout: Stdout):
    mut direct: int? = 1
    for at in range(3):
        stdout.print(direct is None, at)
        direct = None
    stdout.print(direct)
    mut changed: int? = 1
    mut count = 0
    while count < 3:
        if changed is None:
            stdout.print("none", count)
        else:
            stdout.print("some", count)
        changed = None
        count += 1
    mut outer: int? = 1
    for a in range(2):
        for b in range(2):
            stdout.print(outer is None, a, b)
        outer = None
    mut branch: int? = 1
    for at in range(3):
        stdout.print(branch is None, at)
        if at == 1:
            branch = None
    mut left: int? = 1
    for at in range(3):
        stdout.print(left is None, at)
        left = None
        if at == 1:
            break
        left = 2
    stdout.print(left)
    mut skipped: int? = 1
    for at in range(3):
        stdout.print(skipped is None, at)
        skipped = None
        if at == 0:
            continue
        skipped = 2
    mut matched: int? = 1
    for at in range(3):
        match at:
            case 0:
                stdout.print(matched is None)
            case _:
                stdout.print(matched is None)
                matched = None
    mut passed: int? = 1
    for at in range(2):
        stdout.print(passed is None, at)
        clear(ref passed)
    mut field = Held(x=1)
    for at in range(2):
        stdout.print(field.x is None, at)
        field.x = None
    mut method = Held(x=1)
    for at in range(2):
        stdout.print(method.x is None, at)
        method.clear()
"#;
    let (ours, _) = both("a_loop_tests_a_name_its_body_gave_none", source)?;
    assert_eq!(ours, interpreted("a_loop_tests_a_name_its_body_gave_none", source)?);
    Ok(())
}

/// Items given where a list of something wider goes, each made to fit.
#[test]
fn numbers_and_strings_go_in_lists_of_optionals() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "optional_items",
        r#"struct Holder:
    values: list[int?]

def counted(values: list[int?]) -> int:
    mut found = 0
    for value in values:
        if value is not None:
            found += value
    found

def made() -> list[str?]:
    ["given back", None]

def main(stdout: Stdout):
    mut numbers: list[int?] = [1, 2]
    numbers.append(3)
    numbers.append(None)
    numbers.insert(0, 4)
    numbers[1] = 5
    stdout.print(numbers, counted(numbers), counted([6, None, 7]), counted([8, 9]))
    mut gaps: list[int?] = []
    gaps.append(1)
    gaps.append(None)
    gaps.insert(0, 2)
    gaps[1] = None
    stdout.print(gaps, counted(gaps))
    mut words: list[str?] = ["a", "é"]
    words.append("b")
    words.insert(1, "a word too long to fit in a header")
    stdout.print(words, made())
    mut floats: list[float?] = [1.5]
    floats.append(2.5)
    stdout.print(floats)
    holder = Holder([8, None])
    stdout.print(holder.values)
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
        match three("", level=1, code=code):
            case Ok(text):
                stdout.print(text)
            case Plain(code):
                stdout.print("plain", code)
            case NotFound(name):
                stdout.print("not found", name.len())
            case Err(error):
                stdout.print("other", error)
    match three("kit", level=1, code=0):
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

const WORDS_FORMAT: &str = r#"
struct Words:
    parts: list[str]
    at = 0
    place = 0

    def next(ref self) -> str fails ParseError:
        if self.at >= self.parts.len():
            fail ParseError(message="out of words", line=0, column=self.at)
        self.at += 1
        self.parts[self.at - 1]

    def int(ref self) -> int fails ParseError:
        word = self.next()?
        match int(word):
            case Ok(n):
                n
            case Err(_):
                fail ParseError(message=f"`{word}` should be an int", line=0, column=self.at)

    def float(ref self) -> float fails ParseError:
        word = self.next()?
        match float(word):
            case Ok(x):
                x
            case Err(_):
                fail ParseError(message=f"`{word}` should be a float", line=0, column=self.at)

    def bool(ref self) -> bool fails ParseError:
        self.next()? == "yes"

    def text(ref self) -> str fails ParseError:
        self.next()?

    def bytes(ref self) -> bytes fails ParseError:
        self.next()?.encode()

    def at_item(self, at: int, message: str) -> ParseError:
        ParseError(message=f"item {at} {message}", line=0, column=0)

    def here(self, message: str) -> ParseError:
        ParseError(message=message, line=0, column=0)

    def saw_id(ref self, kind: str, position: int):
        pass

    def saw_table(ref self, kind: str, count: int):
        pass

    def tag(ref self, kind: str) -> int:
        0

    def missing_as_none(self) -> bool:
        True

    def exact(self) -> bool:
        False

    def ordered(ref self, name: str) -> bool fails ParseError:
        False

    def end_ordered(ref self):
        pass

    def none(ref self) -> bool fails ParseError:
        if self.at < self.parts.len() and self.parts[self.at] == "-":
            self.at += 1
            return True
        False

    def value[T: Deserializable](ref self) -> T fails ParseError:
        T.deserialize(ref self)?

    def sequence[T: Deserializable](ref self) -> list[T] fails ParseError:
        mut found = list[T]()
        for _ in range(self.int()?):
            found.append(T.deserialize(ref self)?)
        found

    def map[K: Deserializable, V: Deserializable](ref self) -> list[(K, V)] fails ParseError:
        mut found = list[(K, V)]()
        for _ in range(self.int()?):
            key = K.deserialize(ref self)?
            found.append((key, V.deserialize(ref self)?))
        found

    def record(ref self, name: str) fails ParseError:
        word = self.next()?
        if word != name:
            fail ParseError(message=f"`{word}` should be a {name}", line=0, column=self.at)

    def next_field(ref self, fields: list[(str, str)]) -> bool fails ParseError:
        word = self.next()?
        if word == "end":
            return False
        mut at = 0
        for name, _ in fields:
            if name == word:
                self.place = at
                return True
            at += 1
        fail ParseError(message=f"no field `{word}`", line=0, column=self.at)

    def field_at(self) -> int:
        self.place

    def missing(self, name: str) -> ParseError:
        ParseError(message=f"`{name}` is missing", line=0, column=self.at)

    def rule(self, message: str) -> ParseError:
        ParseError(message=f"in the text, {message}", line=0, column=self.at)

    def case(ref self, name: str, cases: list[(str, bool)]) -> int fails ParseError:
        word = self.next()?
        mut at = 0
        for case, _ in cases:
            if case == word:
                return at
            at += 1
        fail ParseError(message=f"no case `{word}` in {name}", line=0, column=self.at)

    def end_case(ref self) fails ParseError:
        _ = self.next()?

enum Order:
    Hold
    Move(x: int)

    def str(self, ref out: StrWriter):
        match self:
            case .Hold:
                out.write("Hold")
            case .Move(x):
                out.write(f"Move({x})")

struct Pair[T]:
    left: T
    right: T

struct Unit:
    name: str
    @serialize(name="hp")
    health = 10 where health >= 0
    pet: str?
    pace = 0.0
    ready = False
    tags = list[str]()
    seen = set[int]()
    scores = dict[str, int]()
    orders = list[Order]()
    @skip
    visits = 7

def read(text: str) -> Unit fails ParseError:
    mut src = Words(parts=text.split(" "))
    src.value[Unit]()?

def shown(text: str) -> str:
    match read(text):
        case Ok(unit):
            orders = " ".join([str(order) for order in unit.orders])
            pet = unit.pet else "no pet"
            f"{unit.name} {unit.health} {pet} {unit.pace} {unit.ready} {unit.tags} {unit.seen.len()} {unit.scores} {orders}"
        case Err(error):
            f"failed: {error.message} at {error.column}"

def main(stdout: Stdout):
    stdout.print(shown("Unit name ash hp 3 pet - pace 0.5 ready yes tags 2 a b seen 2 7 7 scores 1 hits 9 orders 2 Hold Move Move x 4 end end end"))
    stdout.print(shown("Unit name ash pet fox end"))
    stdout.print(shown("Unit end"))
    stdout.print(shown("Unit hp x end"))
    stdout.print(shown("Unit size 1 end"))
    stdout.print(shown("Spot end"))
    stdout.print(shown("Unit orders 1 Wander end"))
    stdout.print(shown("Unit tags 2 a"))
    stdout.print(shown("Unit name ash hp -1 end"))
    mut src = Words(parts="Pair left 1 right 2 end".split(" "))
    pair = src.value[Pair[int]]() else Pair(left=0, right=0)
    skipped = read("Unit name ash end") else Unit(name="none", pet=None)
    stdout.print(pair.left, pair.right, skipped.visits)
"#;

/// A format written by hand on `Deserializer`, reading types whose `deserialize` is generated: every
/// kind of value reads through one `T.deserialize`, a field left out takes its default or says
/// it's missing, and each failure stops the read and says why.
#[test]
fn a_format_reads_a_type_through_its_deserialize() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("deserializer_format")?;
    let program = dir.join("words.fen");
    std::fs::write(&program, WORDS_FORMAT)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("words"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        ours.0,
        "ash 3 no pet 0.5 True [\"a\", \"b\"] 1 {\"hits\": 9} Hold Move(4)\n\
         ash 10 fox 0.0 False [] 0 {} \n\
         failed: `name` is missing at 2\n\
         failed: `x` should be an int at 3\n\
         failed: no field `size` at 2\n\
         failed: `Spot` should be a Unit at 1\n\
         failed: no case `Wander` in Order at 4\n\
         failed: out of words at 4\n\
         failed: in the text, Unit.health breaks its `where` rule at 6\n\
         1 2 7\n"
    );
    Ok(())
}

/// A generic function's type parameter can be used only in its body, when a call writes it in
/// brackets: each copy is made from that, not only from the function's signature.
#[test]
fn a_type_parameter_used_only_in_a_body_is_given_by_the_call() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "interface Made:\n    def make() -> Self\n\n    def measure(self) -> int\n\n\
                  struct Part:\n    size = 3\n\n    def make() -> Self:\n        Self(size=4)\n\n    \
                  def measure(self) -> int:\n        self.size\n\n\
                  struct Other:\n    size = 8\n\n    def make() -> Self:\n        Self(size=9)\n\n    \
                  def measure(self) -> int:\n        self.size\n\n\
                  def build[T: Made]() -> T:\n    T.make()\n\n\
                  def named[T: Made](text: str) -> str:\n    made = build[T]()\n    f\"{text} {made.measure()}\"\n\n\
                  def main(stdout: Stdout):\n    stdout.print(named[Part](\"part\"), named[Other](\"other\"))\n";
    let dir = folder("body_only_type_param")?;
    let program = dir.join("told.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("told"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = outcome(&Command::new(&executable).output()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours.0, "part 4 other 9\n");
    Ok(())
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

/// A function whose only way to fail is one of these still says it can, so it builds.
#[test]
fn a_function_failing_only_in_max_min_or_remove_builds() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "fails_only_in_max",
        r#"def top(values: list[int]) -> int:
    max(values)

def smallest(values: list[int]) -> int:
    min(values)

def longest(words: list[str]) -> str:
    max(words, key=word => word.len())

def shortest(words: list[str]) -> str:
    min(words, key=word => word.len())

def gone(ref values: list[int]):
    values.remove(9)

def main(stdout: Stdout):
    mut values = [1, 9, 2]
    stdout.print(top(values), smallest(values))
    stdout.print(longest(["a", "bcd"]), shortest(["a", "bcd"]))
    gone(ref values)
    stdout.print(values)
    gone(ref values)
    stdout.print("never")
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
    stdout.print(add("1", b="2") else -1, add("1", b="two") else -1)
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

/// A test that names capabilities gets fakes through LLVM, in a world of its own, whether its body
/// passes failures up or not. A failure of `net`, which is an error with cases, comes back as its
/// case.
#[test]
fn tests_that_take_capabilities_run_through_llvm() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("test_capabilities")?;
    let program = dir.join("fakes.fen");
    std::fs::write(
        &program,
        "\
test \"files are its own\"(fs: Fs):
    assert not fs.exists(\"a\")
    fs.write_text(\"a\", \"one\")?
    assert fs.read_text(\"a\")? == \"one\"

test \"files are its own, again\"(fs: Fs):
    assert not fs.exists(\"a\")
    fs.write_text(\"a\", \"two\")?

test \"what it prints is kept\"(stdout: Stdout, clock: Clock):
    stdout.print(\"at\", clock.now().nanos)
    assert stdout.captured() == \"at 0\\n\"

test \"a failure comes back as its case\"(net: Net):
    match net.connect(\"localhost\", 9001):
        case Err(Refused(host, port)):
            assert host == \"localhost\" and port == 9001
        case _:
            assert False
",
    )?;
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["test", "--all", "--verbose", &path], &dir)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(ran.status.success(), "{said}");
    assert!(said.contains("4 of 4 tests passed through LLVM"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What a compiled program does without the interpreter, a compiled test does with its fakes: a
/// sleep passes no time, and nothing runs.
#[test]
fn a_tests_clock_and_process_stay_fakes_through_llvm() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("test_fakes_stay")?;
    let program = dir.join("fakes.fen");
    std::fs::write(
        &program,
        "\
from std.time import Duration

test \"an hour goes by at once\"(clock: Clock):
    clock.sleep(Duration.millis(3600000))
    assert clock.now().nanos == 3600000000000

test \"nothing runs\"(process: Process):
    match process.run(\"echo\", [\"hello\"]):
        case Ok(_):
            assert False
        case Err(error):
            assert error.program == \"echo\"
    assert process.ran().len() == 1
",
    )?;
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["test", "--all", "--verbose", &path], &dir)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(ran.status.success(), "{said}");
    assert!(said.contains("2 of 2 tests passed through LLVM"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A property test runs through LLVM with each input, and one that fails there runs again in the
/// interpreter, on the same inputs, which shrinks what it failed for.
#[test]
fn property_tests_run_through_llvm() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("test_properties")?;
    let program = dir.join("properties.fen");
    std::fs::write(
        &program,
        "\
struct Point:
    x: int
    y: float

test \"ints stay small\" for value: int:
    assert value >= -1000 and value <= 1000

test \"points too\" for point: Point, count: int:
    assert point.x <= 1000 and count <= 1000

test \"this one goes too far\" for value: int:
    assert value < 500
",
    )?;
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["test", "--all", "--verbose", &path], &dir)?;
    let (out, said) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
    assert!(!ran.status.success(), "{said}");
    assert!(said.contains("2 of 3 tests passed through LLVM"), "{said}");
    assert!(out.contains("ints stay small (100 inputs)"), "{out}");
    assert!(said.contains("it failed for value = 500"), "{said}");
    assert!(!said.contains("through LLVM and passes"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A failure a test's body passes up with `?` fails the test through LLVM, and running it again
/// says what the failure was.
#[test]
fn a_failure_passed_up_fails_a_test_through_llvm() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("test_passed_up")?;
    let program = dir.join("up.fen");
    for (name, body) in [
        ("passed_up", "(fs: Fs):\n    fs.read_text(\"gone\")?\n"),
        ("fails", ":\n    fail Oops(\"broken\")\n"),
        ("fails_inside", ":\n    for _ in range(1):\n        if True:\n            fail Oops(\"broken\")\n"),
        // The function it makes of this gives back a result it can't see from the statements.
        (
            "fails_in_a_match",
            ":\n    match 1:\n        case 1:\n            fail Oops(\"broken\")\n        case _:\n            pass\n",
        ),
    ] {
        let source = format!("error Oops:\n    reason: str\n\ntest \"{name}\"{body}");
        std::fs::write(&program, source)?;
        let path = program.to_string_lossy().into_owned();
        let ran = fennec(&["test", "--all", &path], &dir)?;
        let said = String::from_utf8_lossy(&ran.stderr);
        assert!(!ran.status.success(), "{name}: {said}");
        assert!(said.contains("the test failed with"), "{name}: {said}");
        assert!(!said.contains("through LLVM"), "{name}: {said}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
