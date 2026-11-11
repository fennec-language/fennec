//! `fennec build --llvm` against `fennec run`, on programs that backend takes so far. Skipped
//! where LLVM's tools aren't installed, as `fennec build --llvm` itself would say, unless
//! `FENNEC_EXPECT_LLVM` says they have to be.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::executables::{FENNEC_DEADLINE, Running, Within, runtime_built};

fn fennec(args: &[&str], home: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command().args(args).env("FENNEC_HOME", home).output_before(FENNEC_DEADLINE)
}

/// Whether this can run: LLVM's tools and the runtime library are there, and mutation testing
/// isn't under way, which would go past its time limit, as in `executables.rs`. Where
/// `FENNEC_EXPECT_LLVM` is set, as CI's Linux and Windows test jobs set it, LLVM is the backend
/// that has to work, so what's missing fails the test rather than skipping it.
pub(crate) fn ready() -> bool {
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

/// What the program printed, its errors and its exit code on the interpreter, as pinned in
/// `tests/fixtures/interpreted` before the interpreter was removed.
fn interpreted(name: &str, source: &str) -> std::io::Result<Outcome> {
    pinned_in("interpreted", name, source)
}

/// What the interpreter gave for the program at `program`, pinned as `name` (see `interpreted`).
pub(crate) fn interpreted_by(program: &Path, name: &str) -> std::io::Result<Outcome> {
    pinned_in("interpreted", name, &std::fs::read_to_string(program)?)
}

/// What the program printed, its errors and its exit code, built with LLVM, and what the JIT gave.
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
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    let jit = pinned(name, source, || Ok(ours.clone()))?;
    std::fs::remove_dir_all(&dir)?;
    Ok((ours, jit))
}

/// The outcome pinned for program `name` in `tests/fixtures/pinned`, one file for each program,
/// named as the test names it. They were written from the JIT before it was removed.
fn pinned(
    name: &str,
    source: &str,
    reference: impl FnOnce() -> std::io::Result<Outcome>,
) -> std::io::Result<Outcome> {
    // With `FENNEC_PIN_OUTPUTS` set, it's written from `reference`, the build's own outcome, which
    // then has to be checked by reading it: a new test's expected outcome is decided before it's
    // built, not copied from it.
    if std::env::var_os("FENNEC_PIN_OUTPUTS").is_some() {
        let (out, err, code) = reference()?;
        let code = code.map_or_else(|| "none".to_owned(), |code| code.to_string());
        let hash = fennec_blake3::hash(source.as_bytes()).to_hex();
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pinned");
        std::fs::create_dir_all(&folder)?;
        let text =
            format!("source {hash}\ncode {code}\nstdout {}\n{out}stderr {}\n{err}", out.len(), err.len());
        std::fs::write(folder.join(format!("{name}.txt")), text)?;
    }
    pinned_in("pinned", name, source)
}

/// An outcome with the folder it ran in written as `<dir>`, so it reads the same wherever it ran.
/// A diagnostic gives a file's path as it was given, which on Windows goes on with `\`, so the
/// separator after `<dir>` is written as `/` there too.
pub(crate) fn apart_from(dir: &Path, (out, err, code): Outcome) -> Outcome {
    let mut folders = vec![dir.to_string_lossy().into_owned()];
    folders.extend(std::fs::canonicalize(dir).ok().map(|real| real.to_string_lossy().into_owned()));
    let plain = |text: String| {
        folders
            .iter()
            .fold(text, |text, folder| text.replace(folder.as_str(), "<dir>"))
            .replace("<dir>\\", "<dir>/")
    };
    (plain(out), plain(err), code)
}

/// The outcome pinned for program `name` in `tests/fixtures/{kind}`. One pinned for other source
/// than `source` is out of date, as is a missing one, and the test says so: what the program should
/// do is then checked by hand and written there.
fn pinned_in(kind: &str, name: &str, source: &str) -> std::io::Result<Outcome> {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(kind).join(format!("{name}.txt"));
    let hash = fennec_blake3::hash(source.as_bytes()).to_hex();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let read = read_pinned(&text).filter(|(pinned_for, _)| *pinned_for == hash);
    read.map(|(_, outcome)| outcome).ok_or_else(|| {
        std::io::Error::other(format!(
            "{} is missing or was pinned for other source: check what the program should do, and write it there, for source {hash}",
            file.display()
        ))
    })
}

/// A pinned file's source hash and outcome.
fn read_pinned(text: &str) -> Option<(String, Outcome)> {
    let (hash, rest) = text.strip_prefix("source ")?.split_once('\n')?;
    let (code, rest) = rest.strip_prefix("code ")?.split_once('\n')?;
    let (length, rest) = rest.strip_prefix("stdout ")?.split_once('\n')?;
    let (out, rest) = rest.split_at_checked(length.parse().ok()?)?;
    let (length, err) = rest.strip_prefix("stderr ")?.split_once('\n')?;
    if err.len() != length.parse::<usize>().ok()? {
        return None;
    }
    Some((hash.to_owned(), (out.to_owned(), err.to_owned(), code.parse().ok())))
}

pub(crate) type Outcome = (String, String, Option<i32>);

pub(crate) fn outcome(output: &Output) -> Outcome {
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
    back = encoded.decode() else as error:
        stdout.print(error.message)
        return
    stdout.print(back, back == text)
    bad = bytes([255, 254]).decode() else as error:
        error.message
    stdout.print(bad)
    for hex in ["00ff", "00 FF", "", "abc", "zz", "a é"]:
        made = bytes.fromhex(hex) else as error:
            stdout.print(error.message)
            continue
        stdout.print(made)
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

def maybe(at: int) -> int fails Bad:
    if at == 1:
        fail Bad("from a call")
    at

def by_fail(ref units: list[Unit], at: int) -> int fails Bad:
    for ref unit in units:
        unit.tags.append(at)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    0

def by_try(ref units: list[Unit]) -> int fails Bad:
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
    let nested = "error Bad:\n    why: str\n\ndef nested(ref grid: list[list[int]]) -> int fails Bad:\n    for ref row in grid:\n        for ref cell in row:\n            cell += 1\n            if cell == 5:\n                fail Bad(\"inner\")\n    0\n\ndef main(stdout: Stdout):\n    mut grid = [[1, 2], [4, 7], [9]]\n    stdout.print(nested(ref grid) else -3, grid)\n";
    let dir = folder("failing_out_of_two_loops")?;
    let program = dir.join("two_loops.fen");
    std::fs::write(&program, nested)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("two_loops"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    let interpreted = interpreted("two_loops", nested)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, interpreted);
    assert_eq!(ours.0, "-3 [[2, 3], [5, 7], [9]]\n");
    Ok(())
}

/// `else:` after a call that gives nothing back when it doesn't fail, as `conn.write(...)` does:
/// the block runs only when it fails, leaving by `continue`, `break` or `return`. It once wasn't
/// lowered, as "a value of type nothing".
#[test]
fn else_after_a_call_that_gives_nothing_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "else_on_nothing",
        "error Odd:
    n: int

def check(n: int) fails Odd:
    if n % 2 == 1:
        fail Odd(n=n)

def first_odd(limit: int) -> int:
    mut at = 0
    while at < limit:
        check(at) else:
            return at
        at += 1
    -1

def main(stdout: Stdout):
    mut kept = 0
    for n in range(6):
        check(n) else:
            continue
        kept += 1
    mut seen = 0
    for n in [2, 4, 5, 6]:
        check(n) else:
            break
        seen += 1
    stdout.print(kept, seen, first_odd(10), first_odd(0))
",
    )
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
    let (wanted_out, wanted_err, wanted_code) = interpreted(name, source)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let (out, err, code) = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
        assert_eq!((&out, code), (&wanted_out, wanted_code), "{how:?}");
        assert_eq!(err.lines().next(), wanted_err.lines().next(), "{how:?}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// An `else` block that gives a value: `else as why:` with what failed in it, a plain `else:`
/// after a failure or a `None`, a `match` on the failure before the value, and one that leaves.
#[test]
fn an_else_block_gives_the_value_on_its_last_line() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "else_block_value",
        ELSE_BLOCK_VALUE,
        "failed: big\n0\nno world\n1\n2\nnot there\n-1\n0\n4\n",
    )
}

const ELSE_BLOCK_VALUE: &str = r#"error Bad:
    why: str

struct World:
    size: int

def risky(n: int) -> int fails Bad:
    if n > 5:
        fail Bad("big")
    n

def load(n: int) -> World fails Bad:
    if n > 5:
        fail Bad("no world")
    World(size=n)

def find(names: list[str], name: str) -> int?:
    for at, held in enumerate(names):
        if held == name:
            return at
    None

def main(stdout: Stdout):
    small = risky(9) else as why:
        stdout.print(f"failed: {why.why}")
        0
    stdout.print(small)
    world = load(8) else as why:
        match why:
            case Bad(text):
                stdout.print(text)
        World(size=1)
    stdout.print(world.size)
    kept = load(2) else:
        World(size=-1)
    stdout.print(kept.size)
    where = find(["a", "b"], "c") else:
        stdout.print("not there")
        -1
    stdout.print(where)
    for turn in range(3):
        held = risky(turn * 4) else:
            continue
        stdout.print(held)
"#;

/// On what can fail as well as give `None`, `?` passes the failure up and `else` handles the
/// `None`, and `else as why:` handles only the failure, its block giving a value that may be `None`.
#[test]
fn a_failure_and_a_none_are_handled_apart() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("failure_and_none", FAILURE_AND_NONE, "1 -1 -9\nfailed: broken\nTrue\n1\n")
}

const FAILURE_AND_NONE: &str = r#"error Bad:
    why: str

struct Entry:
    size: int

def cached(n: int) -> Entry? fails Bad:
    if n > 5:
        fail Bad("broken")
    if n > 2:
        return None
    Entry(size=n)

def lookup(n: int) -> int fails Bad:
    entry = cached(n)? else Entry(size=-1)
    entry.size

def main(stdout: Stdout):
    stdout.print(lookup(1) else -9, lookup(3) else -9, lookup(7) else -9)
    held = cached(7) else as why:
        stdout.print(f"failed: {why.why}")
        None
    stdout.print(held is None)
    other = cached(1) else as why:
        Entry(size=0)
    found = other else Entry(size=-2)
    stdout.print(found.size)
"#;

/// `total = lock scores as current:` gives the block's last line, read or after a change, as
/// `mut` or not, with lines before it that do something.
#[test]
fn a_lock_block_gives_its_last_line() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("lock_block_value", LOCK_BLOCK_VALUE, "7 4\nlong\n1\n")
}

const LOCK_BLOCK_VALUE: &str = r#"def main(stdout: Stdout):
    shared scores = [1, 2]
    total = lock scores as ref current:
        current.append(4)
        sum(current)
    mut count = lock scores as current:
        current.len()
    count += 1
    stdout.print(total, count)
    kept = lock scores as ref current:
        current.append(9)
        if current.len() > 3:
            stdout.print("long")
        current[0]
    stdout.print(kept)
"#;

/// What a program prints, built in dev and `--release` both, is `wanted`.
fn prints_in_both_builds(name: &str, source: &str, wanted: &str) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join(name));
    let output = output.to_string_lossy().into_owned();
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&output).output_within()?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            wanted,
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A value stored into another keeps what it held when a name it came from changes after: in a
/// tuple, a dict, a list by `insert`, a closure, a struct, an optional and a comprehension.
#[test]
fn a_stored_value_keeps_what_it_held() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "stored_values_keep_theirs",
        include_str!("fixtures/stored_values_keep_theirs.fen"),
        "([1, 2], 1) [9, 2]\n{\"k\": [1, 2]} [9, 2]\n[[1, 2], [0]] [9, 2]\n3 [9, 2]\n{\"k\": [1, 2]} [9, 2]\n\
         Box(item=[1, 2]) [9, 2]\n[1, 2] [9, 2]\n{\"k\": [1, 2]} [1, 2] [9, 2]\n\
         (\"text that is long enough to live in a block\", 2) 44\n[[1, 2], [1, 2]] [9, 2]\n",
    )
}

/// An assignment goes left to right: the place written is the one named before the value runs,
/// and `+=` adds to what was there before it, whatever the value changes by `ref`.
#[test]
fn an_assignment_goes_left_to_right() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "assignment_order",
        "struct Total:
    v: int

def inc(ref i: int) -> int:
    i += 1
    i * 10

def text(ref k: str) -> int:
    k = \"b\"
    5

def bump(ref t: Total) -> int:
    t.v = 10
    2

def main(stdout: Stdout):
    mut xs = [0, 0, 0]
    mut i = 0
    xs[i] = inc(ref i)
    stdout.print(xs, i)

    mut m = {\"a\": 1, \"b\": 2}
    mut k = \"a\"
    m[k] = text(ref k)
    stdout.print(m, k)

    mut a = [[0, 0], [0, 0]]
    mut r = 0
    a[r][r] = inc(ref r)
    stdout.print(a, r)

    mut t = Total(v=0)
    t.v += bump(ref t)
    stdout.print(t)

    mut u = Total(v=0)
    u.v = u.v + bump(ref u)
    stdout.print(u)

    mut ys = [1, 2]
    mut j = 0
    ys[j] += inc(ref j)
    stdout.print(ys, j)

    mut n = 1
    n += inc(ref n)
    stdout.print(n)
",
        "[10, 0, 0] 1\n{\"a\": 5, \"b\": 2} b\n[[10, 0], [0, 0]] 1\nTotal(v=2)\nTotal(v=2)\n[11, 2] 1\n21\n",
    )
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
    _ = xs.pop()

def replaced(ref xs: list[int]):
    xs = [7]

def emptied(ref xs: list[int]):
    while xs.len() > 0:
        _ = xs.pop()

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
        mut here = at
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
    _ = xs.pop()

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
    _ = units[0].tried(1)?
    _ = units[1].tried(-1)?
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
    first_tags = own[0].tags
    own[0].grown_by(first_tags)
    own[1].grown_by(own[0].tags)
    third = own[2]
    own[2].absorb(third)
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
    _ = lists[0].tags.pop()
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
    _ = tables[1].pop("b")
    stdout.print(tables)

    # What a call keeps stays what it was, and the place goes on from there.
    mut keeps = [unit(1)]
    mut kept: list[list[int]] = []
    keeps[0].kept_in(ref kept)
    keeps[0].grow(2)
    keeps[0].kept_in(ref kept)
    show(stdout, keeps)
    stdout.print(kept)

    # Two places at once, each in its own collection.
    mut left = [[1], [2]]
    mut right = [[3], [4]]
    swap_ends(ref left[0], ref right[1])
    swap_ends(ref right[0], ref left[1])
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
    got = try_all(ref failing) else as bad:
        stdout.print(bad.why)
        show(stdout, failing)
        return
    stdout.print(got)
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
    let types = "struct Row:\n    tags: list[int]\n\nstruct Queue:\n    items = list[int]()\n    \
                 rows = [Row(tags=[1])]\n\n    def push(ref self, item: int):\n        self.items.append(item)\n\n    \
                 def pop(ref self) -> int:\n        self.items.pop() else 0\n\n    \
                 def first(ref self, item: int):\n        self.items.insert(0, item)\n\n    \
                 def more(ref self, others: list[int]):\n        self.items.extend(others)\n\n    \
                 def nested(ref self, item: int):\n        self.rows[0].tags.append(item)\n        self.rows[0].tags.pop()\n\n    \
                 def called(ref self, item: int):\n        grow(ref self.items, item)\n\n\
                 def grow(ref numbers: list[int], item: int):\n    numbers.append(item)\n\n\
                 def main(stdout: Stdout):\n    shared queue = Queue()\n    mut seen = 0\n";
    for (name, body) in [
        (
            "push_pop",
            "    for n in range(2000):\n        lock queue as ref held:\n            held.push(n)\n    \
             for n in range(2000):\n        lock queue as ref held:\n            seen += held.pop()\n",
        ),
        (
            "extend",
            "    for n in range(2000):\n        lock queue as ref held:\n            held.more([n])\n",
        ),
        (
            "called",
            "    for n in range(2000):\n        lock queue as ref held:\n            held.called(n)\n",
        ),
        (
            "lock",
            "    lock queue as ref held:\n        for n in range(2000):\n            held.items.append(n)\n        for n in range(2000):\n            seen += held.items.pop() else 0\n",
        ),
    ] {
        let counts =
            counts_of(&format!("shared_place_{name}"), &format!("{types}{body}    stdout.print(seen)\n"))?;
        assert!(counts.starts_with("copies 0 "), "{name}: {counts}");
    }
    Ok(())
}

/// A dict's put and pop, and a list's item set, in a `shared` value under its lock, made in place
/// with each change noted for a crash to undo: the lock costs what it changes, not a copy of the
/// whole dict or list each time it's taken. A dict still copies when it grows past its room, as one
/// with keys taken out does now and then, but no more often than that.
#[test]
fn changing_a_dict_or_an_item_in_a_shared_value_copies_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let types = "struct Table:\n    counts = dict[str, int]()\n    names = dict[int, str]()\n    \
                 items = [0, 0, 0, 0]\n\n    def put(ref self, n: int):\n        self.counts[f\"k{n % 50}\"] = n\n\n\
                 def main(stdout: Stdout):\n    shared table = Table()\n    mut seen = 0\n";
    for (name, body) in [
        (
            "put",
            "    for n in range(2000):\n        lock table as ref held:\n            held.counts[f\"k{n % 50}\"] = n\n            \
             held.names[n % 50] = f\"a name long enough for a block of its own {n}\"\n    \
             lock table as held:\n        seen = held.counts.len() + held.names.len()\n",
        ),
        (
            "pop",
            "    for n in range(2000):\n        lock table as ref held:\n            held.counts[f\"k{n}\"] = n\n        \
             lock table as ref held:\n            seen += held.counts.pop(f\"k{n}\")\n",
        ),
        ("method", "    for n in range(2000):\n        lock table as ref held:\n            held.put(n)\n"),
        (
            "item",
            "    for n in range(2000):\n        lock table as ref held:\n            held.items[n % 4] = n\n            \
             held.items[-1] += 1\n    lock table as held:\n        seen = held.items[3]\n",
        ),
    ] {
        let counts =
            counts_of(&format!("shared_dict_{name}"), &format!("{types}{body}    stdout.print(seen)\n"))?;
        let number = |what: &str| {
            let mut words = counts.split_whitespace();
            words.position(|word| word == what).and_then(|_| words.next()).and_then(|n| n.parse::<u64>().ok())
        };
        // One copy in each lock would be 2,000, of hundreds of bytes each.
        let (copies, bytes) = (number("copies"), number("copied_bytes"));
        assert!(copies.is_some_and(|copies| copies < 400), "{name}: {counts}");
        assert!(bytes.is_some_and(|bytes| bytes < 50_000), "{name}: {counts}");
    }
    Ok(())
}

/// The same in a program with tasks, where a lock's value is read through the lock's edge: 2,000
/// pushes under the lock in `main` and 2,000 pushes and pops in a launched task copy nothing a
/// round. What the program copies is only what starting its task copies, the same program with no
/// locks at all. A field read under the lock once held owners of what it read, as letting go at the
/// lock's edge counted as a use, and they made every change copy.
#[test]
fn changing_a_list_in_a_shared_value_in_a_task_copies_nothing() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let locked =
        counts_of("shared_queue_in_a_task", include_str!("fixtures/workers/shared_queue_in_a_task.fen"))?;
    let unlocked = counts_of(
        "shared_queue_no_locks",
        include_str!("fixtures/workers/shared_queue_in_a_task_no_locks.fen"),
    )?;
    let copies = |counts: &str| counts.split_whitespace().nth(1).map(str::to_owned);
    assert!(copies(&locked).is_some(), "{locked}");
    assert_eq!(copies(&locked), copies(&unlocked), "{locked} against {unlocked}");
    Ok(())
}

/// Changes to a list in a `shared` value that still copy it under its lock: an `insert`, and a change
/// inside one of its items, like a list in a struct in it. A crash puts back an append or a pop from
/// a note of each in the lock's rollback log, but these change what a note of one place can't put
/// back, so the log keeps the list as it was, and the change copies it. `insert(0, x)` takes time
/// in proportion to the list's length anyway.
#[test]
#[ignore = "a crash puts back only appends and pops from notes; an insert, or a change inside an item, under a `lock ... as ref` copies the list"]
fn changes_inside_a_list_in_a_shared_value_copy_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let types = "struct Row:\n    tags: list[int]\n\nstruct Queue:\n    items = list[int]()\n    rows = [Row(tags=[1])]\n\n    \
                 def first(ref self, item: int):\n        self.items.insert(0, item)\n\n    \
                 def nested(ref self, item: int):\n        self.rows[0].tags.append(item)\n        self.rows[0].tags.pop()\n\n\
                 def main(stdout: Stdout):\n    shared queue = Queue()\n";
    for (name, body) in [
        ("insert", "    for n in range(500):\n        lock queue as ref held:\n            held.first(n)\n"),
        (
            "nested",
            "    for n in range(2000):\n        lock queue as ref held:\n            held.nested(n)\n",
        ),
    ] {
        let counts = counts_of(
            &format!("shared_place_copies_{name}"),
            &format!("{types}{body}    stdout.print(0)\n"),
        )?;
        assert!(counts.starts_with("copies 0 "), "{name}: {counts}");
    }
    Ok(())
}

/// An append under `lock ... as ref` to a list in a struct with other fields beside it is made in
/// place, as it is for a struct with that list alone: the struct, a block of its own, is marked by
/// the lock's log and its fields saved, not held, so it isn't copied and the list isn't either. A
/// failure still puts the list and the string back. Held, each lock copied the struct and then all
/// of its list.
#[test]
fn an_append_under_a_lock_beside_other_fields_is_in_place() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of("lock_append_beside", LOCK_APPEND_BESIDE)?;
    assert!(count(&counts, "copies") < Some(5), "{counts}");
    prints_in_both_builds("lock_append_beside", LOCK_APPEND_BESIDE, "10100 0 10100 3\n")
}

const LOCK_APPEND_BESIDE: &str = "error Refused:
    code: int

struct Big:
    items: list[int]
    table: dict[int, int]
    name: str

def refused(big: shared Big) -> int fails Refused:
    lock big as ref held:
        held.items.append(0)
        held.name += \"!\"
        fail Refused(code=1)
    1

def main(stdout: Stdout):
    shared big = Big(items=[n for n in range(10_000)], table={1: 2}, name=\"big\")
    for i in range(100):
        lock big as ref held:
            held.items.append(i)
    gone = refused(big) else 0
    lock big as held:
        stdout.print(held.items.len(), gone, held.items[-1] + held.items.len() - 99, held.name.len())
";

/// Random changes to lists in a `shared` value under `lock ... as ref`, failing at a random step:
/// appends and pops made in place are undone from the lock's log, and the rest copy, and either way
/// a failure leaves the lists as they were, and an end leaves what plain lists get. Three thousand
/// cases, in dev and release.
#[test]
fn random_changes_under_a_lock_are_undone_by_a_failure() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_undo_by_failure",
        include_str!("fixtures/lock_undo_by_failure.fen"),
        "wrong 0 crashed True kept True\n",
    )
}

/// The same random changes in a task, crashing at a random step: a crash leaves the lists as they
/// were. Three thousand cases, in dev and release.
#[test]
fn random_changes_under_a_lock_are_undone_by_a_crash() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_undo_by_crash",
        include_str!("fixtures/workers/lock_undo_by_crash.fen"),
        "wrong 0 crashed True kept True\n",
    )
}

/// A lock on a struct with a list, a dict and a string, changed in place: kept, failed, crashed in
/// a launched task, and failing on every other parallel worker. Only what a normal end made stays.
#[test]
fn a_struct_of_three_fields_changed_in_place_keeps_only_what_ended_normally() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_rollback_three_fields",
        include_str!("fixtures/workers/lock_rollback_three_fields.fen"),
        include_str!("fixtures/workers/lock_rollback_three_fields.out"),
    )
}

/// Puts, pops and item sets made in place on a `shared` value's dicts and list, straight in the
/// lock's block, then a failure: the dicts and the list are as they were, keys in the same order. So
/// they are after a dict grown past its room, which copies it once the changes in place are made.
#[test]
fn changes_in_place_under_a_lock_are_undone_exactly() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_undo_in_place",
        include_str!("fixtures/lock_undo_in_place.fen"),
        include_str!("fixtures/lock_undo_in_place.out"),
    )
}

/// A put after a lookup of the same key under a lock writes where the lookup found it, unless what
/// came between grew the dict, popped the key or put another: each kept, and each put back by a
/// failure.
#[test]
fn a_put_after_a_lookup_under_a_lock_is_right_whatever_came_between() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("lock_found_entry", include_str!("fixtures/lock_found_entry.fen"))
}

/// A put after a lookup under a lock, of a number and of a string held in a block, takes and lets go
/// of the same owners whether it's kept or a failure puts the old value back: twice the rounds hold
/// no more at the end. The puts are made where the lookup found the key (`found_puts`), so the
/// rolled-back ones are undone from what that path noted.
#[test]
fn a_put_where_a_lookup_found_the_key_holds_nothing_more_kept_or_undone() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "error Boom:\n    step: int\n\nstruct Bins:\n    counts: dict[int, int]\n    names: dict[int, str]\n\ndef put(bins: shared Bins, key: int, name: str, failing: bool) fails Boom:\n    lock bins as ref held:\n        held.counts[key] = (held.counts.get(key) else 0) + 1\n        held.names[key] = held.names.get(key) else name\n        if failing:\n            fail Boom(step=key)\n\ndef main(stdout: Stdout, args: Args):\n    rounds = int(args[0]) else 0\n    shared bins = Bins(counts={}, names={})\n    mut failed = 0\n    for step in range(rounds):\n        name = f\"name {step}, long enough to be held in a block of its own\"\n        _ = put(bins, key=step % 2, name=name, failing=step % 3 == 0) else:\n            failed += 1\n    lock bins as held:\n        stdout.print(failed, held.counts[0], held.counts[1], held.names[0], held.names[1])\n";
    let dir = folder("lock_found_counts")?;
    let program = dir.join("found.fen");
    std::fs::write(&program, source)?;
    let executable = dir.join("found");
    let built = fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let run = |rounds: &str| -> std::io::Result<(String, String)> {
        let ran = Command::new(&executable).arg(rounds).env("FENNEC_COUNTS", "1").output_within()?;
        let said = String::from_utf8_lossy(&ran.stderr).into_owned();
        let counts = said.lines().find(|line| line.starts_with("copies ")).unwrap_or_default().to_owned();
        Ok((String::from_utf8_lossy(&ran.stdout).into_owned(), counts))
    };
    let (fewer, more) = (run("60")?, run("120")?);
    std::fs::remove_dir_all(&dir)?;
    let names = "name 2, long enough to be held in a block of its own name 1, long enough to be held in a block of its own";
    assert_eq!((fewer.0, more.0), (format!("20 20 20 {names}\n"), format!("40 40 40 {names}\n")));
    let field = |counts: &str, name: &str| {
        counts.split_whitespace().skip_while(|word| *word != name).nth(1).unwrap_or_default().to_owned()
    };
    // Both puts of every round, rolled back or kept, but for the six whose key wasn't there yet:
    // rounds 0, 1 and 2, as round 0's were put back.
    assert_eq!(field(&fewer.1, "found_puts"), "114", "{}", fewer.1);
    assert_eq!(field(&fewer.1, "live"), field(&more.1, "live"), "{} / {}", fewer.1, more.1);
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
    let ran = Command::new(&executable).output_within()?;
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
    let ours = outcome(&Command::new(&executable).output_within()?);
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
    // The JIT read the same lowering, so only the interpreter could say this was right.
    same_as_the_interpreter_in_both_builds("replacing_its_source", REPLACING_ITS_SOURCE)
}

/// A list a counted loop fills once a turn, with room made for every turn and nothing else in
/// the loop touching it, is filled with no room check. Every way that rule can fail keeps the
/// check: the list shared or read in the loop, a count starting below 0 or from a variable,
/// two appends a turn, and a loop inside another that makes the list each time round.
#[test]
fn lists_filled_within_their_room_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("filled_within_room", FILLED_WITHIN_ROOM)
}

const FILLED_WITHIN_ROOM: &str = r#"def main(stdout: Stdout):
    mut total = 0
    for round in range(3):
        numbers = [i * 2 + round for i in range(1000)]
        names = [f"n{i}" for i in range(50)]
        total += numbers[999] + numbers.len() + names[49].len()
    stdout.print(total)
    zeros = [0 for _ in range(4)]
    grid = [[r * 10 + c for c in range(3)] for r in range(3)]
    stdout.print(zeros, grid, [row.len() for row in grid])
    mut shared = []
    mut seen = []
    for i in range(6):
        shared.append(i)
        seen = shared
    stdout.print(shared, seen)
    below = [i for i in range(-3, 3)]
    stdout.print(below)
    start = 2
    from_variable = [i for i in range(start, 8)]
    stdout.print(from_variable)
    mut twice = []
    for i in range(4):
        twice.append(i)
        twice.append(i * 10)
    stdout.print(twice)
    mut read = []
    for i in range(5):
        read.append(read.len() + i)
    stdout.print(read)
"#;

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
    snapshot = units
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
    mut mine = units
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

/// Each character read twice, as a parser checks one and then takes it, and the few before it
/// sliced off, in text with characters of each width: a slice of ASCII is found as many bytes
/// back as it has characters, and one taking in part of a wider character is walked to instead.
/// Then jumps ahead, from the end, and the last character twice.
#[test]
fn reading_again_and_slicing_back_in_text_that_isnt_ascii() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "reading_again_slicing_back",
        r#"def main(stdout: Stdout):
    text = "ab日c🦊de é f" + "x" * 8 + "ñ"
    mut out = []
    for at in range(text.len()):
        first = text[at]
        again = text[at]
        out.append(f"{first}{again}")
        for back in range(1, 4):
            if at >= back:
                out.append(text[at - back:at])
    stdout.print("|".join(out))
    stdout.print(text[0], text[4], text[9], text[3], text[-1], text[-1], text[text.len() - 1], text[2], text[2])
"#,
        "aa|bb|a|日日|b|ab|cc|日|b日|ab日|🦊🦊|c|日c|b日c|dd|🦊|c🦊|日c🦊|ee|d|🦊d|c🦊d|  |e|de|🦊de|éé| |e |de |  |é| é|e é|ff| |é | é |xx|f| f|é f|xx|x|fx| fx|xx|x|xx|fxx|xx|x|xx|xxx|xx|x|xx|xxx|xx|x|xx|xxx|xx|x|xx|xxx|xx|x|xx|xxx|ññ|x|xx|xxx\na 🦊   c ñ ñ ñ 日 日\n",
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

def by_fail(ref units: list[Unit]) -> int fails Bad:
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
    kept = units
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

/// A `for ref` body that reads what it loops over sees its item as it is now, changes made
/// through the loop's name included: by position, through a field, with `enumerate`, and by `Id`.
#[test]
fn a_for_ref_body_reads_its_item_as_it_is_now() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "for_ref_reads_now",
        FOR_REF_READS_NOW,
        "7\n7\n27 24\n12 30\n0 106\n1 206\n2 306\n6\n6\n",
    )
}

const FOR_REF_READS_NOW: &str = r"from std.collections import Table

struct Unit:
    health: int
    seen: int

struct World:
    units: list[Unit]
    turn: int

def total(units: list[Unit]) -> int:
    sum(unit.health for unit in units)

def total_in(world: World) -> int:
    total(world.units)

def main(stdout: Stdout):
    mut units = [Unit(health=10, seen=0), Unit(health=20, seen=0)]
    for ref unit in units:
        unit.health -= 3
        unit.seen = total(units)
        stdout.print(units[0].health)
    stdout.print(units[0].seen, units[1].seen)
    mut world = World(units=[Unit(health=1, seen=0), Unit(health=2, seen=0)], turn=0)
    for ref unit in world.units:
        unit.health *= 10
        unit.seen = total_in(world)
    stdout.print(world.units[0].seen, world.units[1].seen)
    mut scores = [1, 2, 3]
    for at, ref score in enumerate(scores):
        score += 100
        stdout.print(at, sum(scores))
    mut table = Table[Unit]()
    first = table.add(Unit(health=5, seen=0))
    _ = table.add(Unit(health=6, seen=0))
    for ref unit in table:
        unit.health += 1
        unit.seen = table[first].health
    for unit in table:
        stdout.print(unit.seen)
";

/// A local `ref` inside `lock ... as ref held:` reads and writes the locked value, `slot = slot * 2`
/// and `x = slot + 1` as well as `+=`, and a failure leaving the lock undoes what was changed
/// through it, as it undoes a write made directly.
#[test]
fn a_local_ref_in_a_lock_reads_and_is_rolled_back() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "local_ref_in_lock",
        LOCAL_REF_IN_LOCK,
        "Bag(items=[2, 2], counts={})\n5\nfailed: stop\nBag(items=[4, 2], counts={})\n",
    )
}

const LOCAL_REF_IN_LOCK: &str = r#"error Nope:
    why: str

struct Bag:
    items: list[int]
    counts: dict[str, int]

def changed(box: shared Bag):
    lock box as ref held:
        ref slot = held.items[0]
        slot = slot * 2

def read_after(box: shared Bag) -> int:
    lock box as ref held:
        ref items = held.items
        items[0] = items[1] * 2
        ref slot = held.items[0]
        x = slot + 1
        x

def undone(box: shared Bag) fails Nope:
    lock box as ref held:
        ref slot = held.items[1]
        slot = slot * 10
        ref items = held.items
        items.append(9)
        fail Nope("stop")

def main(stdout: Stdout):
    shared b = Bag(items=[1, 2], counts=dict[str, int]())
    changed(b)
    lock b as held:
        stdout.print(held)
    stdout.print(read_after(b))
    undone(b) else as why:
        stdout.print("failed:", why.why)
    lock b as held:
        stdout.print(held)
"#;

/// `ref unit = world.units[actor]` is the item where it is: a change through it is a change there,
/// read back through the list and through the name, its index worked out once at the `ref`, and
/// passed on as `ref`. It's declared again each time round a loop.
#[test]
fn a_local_ref_changes_its_place_where_it_is() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("local_ref", LOCAL_REF, "25 15 0\n15 hit 110 1\n[[1, 2], [30, 4]]\n125\n17\n")
}

const LOCAL_REF: &str = r#"struct Unit:
    health: int
    status: str

struct World:
    units: list[Unit]
    score: int

def total(units: list[Unit]) -> int:
    sum(unit.health for unit in units)

def hit(ref unit: Unit, by: int):
    unit.health -= by

def main(stdout: Stdout):
    mut world = World(units=[Unit(health=10, status="ok"), Unit(health=20, status="ok")], score=0)
    mut actor = 1
    ref unit = world.units[actor]
    actor = 0
    unit.health -= 3
    unit.status = "hit"
    hit(ref unit, by=2)
    world.score += 1
    stdout.print(total(world.units), unit.health, actor)
    world.units[0].health += 100
    stdout.print(world.units[1].health, world.units[1].status, world.units[0].health, world.score)
    mut grid = [[1, 2], [3, 4]]
    ref cell = grid[1][0]
    cell *= 10
    stdout.print(grid)
    ref first = world.units[0]
    bonus = sum(other.health for other in world.units if other.health < first.health)
    first.health += bonus
    stdout.print(first.health)
    for turn in range(3):
        ref last = world.units[world.units.len() - 1]
        if turn == 1:
            continue
        last.health += turn
    stdout.print(world.units[1].health)
"#;

/// `case .Button(ref clicks):` changes the field inside the enum the `match` is over, natively as
/// in the interpreter: a case that finishes, one that leaves by `return`, and one that fails, each
/// putting the case back, and a `_` field kept as it was.
#[test]
fn ref_cases_change_fields_where_they_are_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("ref_cases", REF_CASES)
}

const REF_CASES: &str = r#"error TooMany:
    clicks: int

enum Kind:
    Button(clicks: int, label: str)
    Slider(value: float)
    Empty

struct Widget:
    kind: Kind

def click(ref widgets: list[Widget], widget_id: int, times: int) -> int:
    match widgets[widget_id].kind:
        case .Button(ref clicks, label):
            clicks += times
            if clicks > 5:
                return clicks
            clicks += 1
        case .Slider(ref value):
            value *= 2.0
        case _:
            pass
    0

def capped(ref kind: Kind) fails TooMany:
    match kind:
        case .Button(ref clicks, _):
            clicks += 10
            if clicks > 12: fail TooMany(clicks)
        case _:
            pass

def main(stdout: Stdout):
    mut widgets = [Widget(kind=.Button(clicks=0, label="ok")), Widget(kind=.Slider(value=1.5)), Widget(kind=.Empty)]
    stdout.print(click(ref widgets, 0, 2), click(ref widgets, 0, 3), click(ref widgets, 1, 0), click(ref widgets, 2, 0))
    stdout.print(widgets[0].kind, widgets[1].kind, widgets[2].kind)
    mut kind = Kind.Button(clicks=1, label="go")
    mut first = "fine"
    capped(ref kind) else as failure:
        first = f"{failure}"
    mut second = "fine"
    capped(ref kind) else as failure:
        second = f"{failure}"
    stdout.print(first, second)
    stdout.print(kind)
"#;

/// `for at, ref item in enumerate(items)` changes each item where it is, natively as in the
/// interpreter: numbers, structs and lists, a `continue` that still counts what it skips, and a
/// nested one over the items' own lists.
#[test]
fn enumerate_with_ref_changes_items_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("enumerate_ref", ENUMERATE_REF)
}

const ENUMERATE_REF: &str = r"struct Unit:
    health: int
    tags: list[int]

def main(stdout: Stdout):
    mut scores = [1, 2, 3]
    for at, ref score in enumerate(scores):
        score += at * 10
    mut units = [Unit(health=1, tags=[]), Unit(health=2, tags=[7])]
    for at, ref unit in enumerate(units):
        if at == 0:
            continue
        unit.health *= at + 10
        for place, ref tag in enumerate(unit.tags):
            tag += place + at
    stdout.print(scores, units[0].health, units[1].health, units[1].tags)
";

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

/// A remainder by a positive number is known to be from 0 up to it, which lets LLVM drop the work
/// for a sign it can't have: the remainders of either sign, by any positive or negative number,
/// still match, and so do ones fed back in, compared or cut again.
#[test]
fn a_remainder_kept_in_range_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("remainder_in_range", REMAINDER_IN_RANGE)
}

const REMAINDER_IN_RANGE: &str = r#"def stirred(n: int) -> int:
    mut x = n % 2_147_483_646 + 1
    for _ in range(100):
        x = x * 48_271 % 2_147_483_647
    x % 1_000

def main(stdout: Stdout):
    lowest = -9223372036854775807 - 1
    highest = 9223372036854775807
    values = [0, 1, -1, 6, -6, 7, -7, 8, -8, 999, -999, 1000, -1000, 123456789, -123456789, lowest, lowest + 1, highest, highest - 1]
    for v in values:
        stdout.print(f"{v}: {v % 3} {v // 3} {v % 7} {v // 7} {v % 1000} {v // 1000} {v % -7} {v // -7} {v % 2147483647} {v // 2147483647} {v % highest} {v // highest}")
        stdout.print(f"{v % 7 < 0} {v % 7 >= 7} {v % 7 == 6} {v % 7 * 3 % 5} {(v % 1000 - 500) % 9}")
    mut total = 0
    for n in range(-500, 500):
        total += stirred(n)
        mut x = n
        for _ in range(5):
            x = x * 31 % 1_009 - 504
        total += x
    stdout.print(total)
"#;

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

/// Steps proved not to overflow, from a comparison on the way to them or from every value a local
/// is given, run right up to the highest and lowest ints; and a step past what's proved, which
/// still fails as on the interpreter.
#[test]
fn steps_proved_safe_match_the_interpreter_at_the_ends_of_the_ints() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let program = |last: &str| {
        format!(
            "def closing(low: int, high: int) -> (int, int):\n    mut a = low\n    mut b = high\n    while a < b:\n        a += 1\n        b -= 1\n    (a, b)\n\ndef counted(limit: int, start: int) -> int:\n    mut count = start\n    while count < limit and count % 1000 != 999:\n        count += 1\n    count\n\ndef stirred(n: int) -> int:\n    mut seed = 42\n    mut total = 0\n    for _ in range(n):\n        seed = (seed * 3877 + 29573) % 139968\n        total += seed % 10\n    total\n\ndef main(stdout: Stdout):\n    highest = 9223372036854775807\n    lowest = -9223372036854775807 - 1\n    stdout.print(closing(highest - 9, high=highest), closing(lowest, high=lowest + 7), closing(5, high=1))\n    stdout.print(counted(highest, start=highest - 5), counted(10, start=3), stirred(1000))\n    mut x = highest - 1\n    if x < highest:\n        x += 1\n    stdout.print(x)\n    {last}\n"
        )
    };
    same_as_the_interpreter_in_both_builds("proved_steps", &program("stdout.print(\"done\")"))?;
    same_as_the_interpreter_in_both_builds("proved_steps_then_past", &program("x += 1\n    stdout.print(x)"))
}

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

/// Sized integers natively: arithmetic, floor division and remainder as `int`'s, `~` within an
/// unsigned size's width, shifts by an `int`, conversions written out, lists of them, and `/`
/// giving a float. The interpreter doesn't take sized numbers, so the outcomes are written here.
#[test]
fn sized_integers_work_out_as_their_sizes_say() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "sized_integers",
        "def mix(a: u8, b: u8) -> u8:
    u8((u16(a) * 7 + u16(b)) % 251)

def main(stdout: Stdout):
    mut pixel: u8 = 250
    pixel += 5
    low: i8 = -100
    small: i32 = 70000
    big = small * 3
    stdout.print(pixel, low, small, big, ~pixel, -low, pixel >> 2, low // 3, low % 7)
    mut acc: u8 = 0
    for k in range(10):
        acc = mix(acc, b=u8(k))
    stdout.print(acc, int(acc) + 1000, u16(pixel) * 200, small / 7)
    bytes: list[u8] = [1, 2, 255]
    stdout.print(bytes, bytes[2] == 255, i32(low) * 1000, 3 * i32(low) < 0)
",
    )?;
    assert_eq!(
        printed,
        "255 -100 70000 210000 0 100 63 -34 5\n22 1022 51000 10000.0\n[1, 2, 255] True -100000 True\n"
    );
    Ok(())
}

/// A `u64` natively is unsigned all the way up: arithmetic, comparisons, shifts and `~` on its
/// 64 bits. An `f32` is rounded to 32 bits after each operation, and prints as the shortest text
/// that reads back as the same `f32`. Both go in structs, lists, f-strings with a format, `str`,
/// and conversions either way.
#[test]
fn u64_and_f32_work_out_as_their_sizes_say() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "u64_and_f32",
        "struct Pixel:
    hash: u64
    level: f32

def mixed(text: str) -> u64:
    mut hash = u64(7347990519673328018) * 2 + 1
    for c in text:
        hash = (hash ^ u64(c.code())) >> 1
    hash

def scaled(by: f32) -> f32:
    by * 2.0

def main(stdout: Stdout):
    half: u64 = 9223372036854775807
    top = half + half + 1
    stdout.print(top, top // 10, top % 10, top > half, top >> 60, ~top, top / 2)
    one: f32 = 1.0
    third = one / 3.0
    stdout.print(third, float(third), scaled(third), third * 3.0 == 1.0, -third, f32(16777217))
    p = Pixel(hash=top - 5, level=third)
    stdout.print(p, f\"{p.hash} {p.level:.3f} {p.hash:,} {p.level:.0f}\", [p.hash, u64(5)], [third, f32(2)])
    stdout.print(str(top), str(third), int(f32(2.75)), float(top), u8(u64(200)), mixed(\"hi\"))
",
    )?;
    assert_eq!(
        printed,
        "18446744073709551615 1844674407370955161 5 True 15 0 9.223372036854776e+18\n\
         0.33333334 0.3333333432674408 0.6666667 True -0.33333334 16777216.0\n\
         Pixel(hash=18446744073709551610, level=0.33333334) 18446744073709551610 0.333 \
         18,446,744,073,709,551,610 0 [18446744073709551610, 5] [0.33333334, 2.0]\n\
         18446744073709551615 0.33333334 2 1.8446744073709552e+19 200 3673995259836664039\n"
    );
    Ok(())
}

/// `**` on `u64`s, worked out by squaring and checked: `0 ** 0` is 1, `2 ** 63` is past `int`'s top,
/// `3 ** 40` takes most of the 64 bits, and `2 ** 64` doesn't fit, which stops the program.
#[test]
fn a_u64_to_a_power() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r"def main(stdout: Stdout):
    zero: u64 = 0
    one: u64 = 1
    two: u64 = 2
    three: u64 = 3
    forty: u64 = 40
    sixty_three: u64 = 63
    sixty_four: u64 = 64
    stdout.print(zero ** zero, two ** zero, zero ** two, one ** sixty_four, two ** sixty_three, three ** forty)
    stdout.print(two ** sixty_four)
";
    let dir = folder("u64_power")?;
    let program = dir.join("u64_power.fen");
    std::fs::write(&program, source)?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("u64_power"));
    let output = output.to_string_lossy().into_owned();
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&output).output_within()?;
        let (out, err) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
        assert_eq!(out, "1 1 0 1 9223372036854775808 12157665459056928801\n", "{how:?}: {err}");
        assert!(!ran.status.success(), "{how:?}");
        assert!(err.contains("this calculation overflows a u64"), "{err}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// `u64`s sort as unsigned numbers natively, those past `int`'s top after the rest: by themselves,
/// in place, by a key, and as a tuple's first item. `f32`s sort as the floats they are, and both
/// can be what a key gives.
#[test]
fn u64s_and_f32s_sort_by_their_values() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def main(stdout: Stdout):
    top: u64 = 9223372036854775807
    big = top * 2 + 1
    half = top + 1
    xs = [u64(5), big, u64(0), half, u64(7)]
    stdout.print(sorted(xs))
    mut ys = xs
    ys.sort()
    stdout.print(ys, xs)
    stdout.print(sorted(xs, key=(x) => x))
    stdout.print(sorted([(half + 1, "b"), (u64(1), "a"), (big, "c")]))
    stdout.print(sorted([f32(2.5), f32(-1.0), f32(0.1), f32(3.0)]))
    named = ["ccc", "a", "bb"]
    stdout.print(sorted(named, key=(s) => u64(s.len())), sorted(named, key=(s) => f32(s.len())))
"#;
    let printed = printed_by("u64_sort", source)?;
    let (low, top) = ("0, 5, 7, 9223372036854775808, 18446744073709551615", "9223372036854775809");
    assert_eq!(
        printed,
        format!(
            "[{low}]\n[{low}] [5, 18446744073709551615, 0, 9223372036854775808, 7]\n[{low}]\n\
             [(1, \"a\"), ({top}, \"b\"), (18446744073709551615, \"c\")]\n[-1.0, 0.1, 2.5, 3.0]\n\
             [\"a\", \"bb\", \"ccc\"] [\"a\", \"bb\", \"ccc\"]\n"
        )
    );
    Ok(())
}

/// What a function does to a local through `ref` counts as a write. The proofs that take a bounds
/// or an overflow check away can't rest on the local's value from before the call.
const REF_WRITES: &str = "def neg(ref at: int):
    at = -3

def bump(ref x: int):
    x = 9223372036854775807

def drop(ref x: int):
    x = -9223372036854775807 - 1

def past_the_end(xs: list[int]) -> int:
    mut at = 0
    neg(ref at)
    mut total = 0
    while at + 2 <= xs.len():
        total += xs[at] + xs[at + 1]
        at += 2
    total

def after_a_guard(x0: int, b: int) -> int:
    mut x = x0
    if x < b:
        bump(ref x)
        return x + 1
    0

def down_after_a_guard(x0: int, b: int) -> int:
    mut x = x0
    if x > b:
        drop(ref x)
        return x - 1
    0

";

#[test]
fn a_write_through_ref_undoes_what_was_proved() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, call, wanted) in [
        ("ref_negative_index", "past_the_end([1, 2, 3, 4])", Ok("15\n")),
        ("ref_after_a_guard", "after_a_guard(1, b=5)", Err("error: this calculation overflows an int")),
        (
            "ref_down_after_a_guard",
            "down_after_a_guard(5, b=1)",
            Err("error: this calculation overflows an int"),
        ),
    ] {
        let source = format!("{REF_WRITES}def main(stdout: Stdout):\n    stdout.print({call})\n");
        for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
            let dir = folder(name)?;
            let program = dir.join(format!("{name}.fen"));
            std::fs::write(&program, &source)?;
            let output = dir.join(name);
            let (from, to) = (program.to_string_lossy().into_owned(), output.to_string_lossy().into_owned());
            let args: Vec<&str> =
                ["build"].into_iter().chain(how.iter().copied()).chain([from.as_str(), "-o", &to]).collect();
            let built = fennec(&args, &dir)?;
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            let ran = Command::new(&output).output_within()?;
            std::fs::remove_dir_all(&dir)?;
            match wanted {
                Ok(printed) => assert_eq!(String::from_utf8_lossy(&ran.stdout), printed, "{name} {how:?}"),
                Err(said) => {
                    assert!(!ran.status.success(), "{name} {how:?}");
                    assert_eq!(
                        String::from_utf8_lossy(&ran.stderr).lines().next(),
                        Some(said),
                        "{name} {how:?}"
                    );
                }
            }
        }
    }
    Ok(())
}

/// A division's check against 0 only goes when the divisor can't become 0 after what proved it.
#[test]
fn a_divisor_written_again_is_still_checked() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def n() -> int:\n    7\n\ndef main(stdout: Stdout):\n    v = (n() % 5) + 1\n    mut d = float(v)\n    d = 0.0\n    stdout.print(1.0 / d)\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("divisor_written_again")?;
        let program = dir.join("divides.fen");
        std::fs::write(&program, source)?;
        let output = dir.join("divides");
        let (from, to) = (program.to_string_lossy().into_owned(), output.to_string_lossy().into_owned());
        let args: Vec<&str> =
            ["build"].into_iter().chain(how.iter().copied()).chain([from.as_str(), "-o", &to]).collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&output).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        assert!(!ran.status.success(), "{how:?}: {}", String::from_utf8_lossy(&ran.stdout));
        assert!(
            String::from_utf8_lossy(&ran.stderr).contains("division by zero"),
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// A number written on the left of a `u64` or an `f32` is that size, so what's worked out is too:
/// unsigned on all 64 bits, and rounded to 32.
#[test]
fn a_number_on_the_left_takes_the_size_on_the_right() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = "literal_left_of_a_size";
    let source = "def big() -> u64:
    half: u64 = 9223372036854775807
    half + half

def large() -> f32:
    f: f32 = 16777216.0
    f

def main(stdout: Stdout):
    b = big()
    stdout.print(100 // b, 100 % b, 100 / b, 100 < b, 100 == b)
    f = large()
    stdout.print(1.0 + f, 0.1 * f, 1.0 < f, 2.0 / f)
";
    prints_in_both_builds(
        dir,
        source,
        "0 100 5.421010862427522e-18 True False\n16777216.0 1677721.6 True 1.1920929e-07\n",
    )
}

/// `%` on floats is exact, however much bigger the left side is, with the sign of the right.
#[test]
fn a_float_remainder_is_exact() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "float_remainder",
        "def number(x: float) -> float:\n    x\n\ndef main(stdout: Stdout):\n    stdout.print(number(1e17) % 3.0, number(5.5) % 0.1, number(-7.5) % 2.0, number(7.5) % -2.0)\n    stdout.print(number(6.0) % 3.0, number(1e300) % 7.0, number(0.3) % 0.1)\n",
        "1.0 0.0999999999999997 0.5 -0.5\n0.0 1.0 0.09999999999999998\n",
    )
}

/// `<<` drops the bits shifted past a size's top, as on `int` and `u64`: what's left is that size,
/// unsigned or with the sign of its top bit.
#[test]
fn a_shift_drops_bits_past_a_sizes_top() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "sized_shifts_drop_bits",
        "def main(stdout: Stdout):\n    a: u8 = 200\n    b: i8 = 100\n    c: u16 = 65535\n    d: i32 = 1\n    e: u32 = 3\n    stdout.print(a << 1, b << 1, c << 4, d << 31, e << 31, a << 7)\n",
        "144 -56 65520 -2147483648 2147483648 0\n",
    )
}

/// What doesn't fit a size stops the program, saying which size and what it holds: arithmetic
/// past either end, a negation, the one division that grows, and a conversion that doesn't fit,
/// a `u64` too big for an `int` among them.
#[test]
fn what_overflows_a_size_stops_the_program() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let overflow =
        |size: &str, range: &str| format!("error: this calculation overflows {size}, which holds {range}");
    for (at, (body, said)) in [
        ("x: i8 = -128\n    stdout.print(-x)", overflow("an i8", "-128 to 127")),
        ("big: i32 = 2147483647\n    stdout.print(big + 1)", overflow("an i32", "-2147483648 to 2147483647")),
        ("m: i32 = -2147483648\n    stdout.print(m // -1)", overflow("an i32", "-2147483648 to 2147483647")),
        ("p: u16 = 300\n    stdout.print(p * p)", overflow("a u16", "0 to 65535")),
        ("z: u8 = 0\n    stdout.print(z - 1)", overflow("a u8", "0 to 255")),
        (
            "n = 300\n    stdout.print(u8(n))",
            "error: it doesn't fit in u8, which holds 0 to 255: 300".to_owned(),
        ),
        (
            "n = -1\n    stdout.print(u32(n))",
            "error: it doesn't fit in u32, which holds 0 to 4294967295: -1".to_owned(),
        ),
        ("z: u64 = 0\n    stdout.print(z - 1)", overflow("a u64", "0 to 18446744073709551615")),
        ("stdout.print(5 - u64(6))", overflow("a u64", "0 to 18446744073709551615")),
        (
            "h: u64 = 9223372036854775807\n    stdout.print(4 * h)",
            overflow("a u64", "0 to 18446744073709551615"),
        ),
        (
            "x: u8 = 200\n    n = 9\n    stdout.print(x >> n)",
            "error: can't shift u8 by a count outside 0 to 7".to_owned(),
        ),
        (
            "x: u8 = 16\n    n = 60\n    stdout.print(x << n)",
            "error: can't shift u8 by a count outside 0 to 7".to_owned(),
        ),
        (
            "x: i32 = 2\n    n = 63\n    stdout.print(x << n)",
            "error: can't shift i32 by a count outside 0 to 31".to_owned(),
        ),
        (
            "h: u64 = 9223372036854775807\n    stdout.print(h * 3)",
            overflow("a u64", "0 to 18446744073709551615"),
        ),
        (
            "n = -1\n    stdout.print(u64(n))",
            "error: it doesn't fit in u64, which holds 0 to 18446744073709551615: -1".to_owned(),
        ),
        (
            "h: u64 = 9223372036854775807\n    stdout.print(i8(h))",
            "error: it doesn't fit in i8, which holds -128 to 127: 9223372036854775807".to_owned(),
        ),
        (
            "h: u64 = 9223372036854775807\n    stdout.print(int(h + 1))",
            "error: it doesn't fit in int, which holds -9223372036854775808 to 9223372036854775807: \
             9223372036854775808"
                .to_owned(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("sized_overflow_{at}");
        let dir = folder(&name)?;
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, format!("def main(stdout: Stdout):\n    {body}\n"))?;
        let executable = dir.join(&name);
        let built =
            fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        assert!(!ran.status.success(), "{body}");
        assert_eq!(String::from_utf8_lossy(&ran.stderr).lines().next(), Some(said.as_str()), "{body}");
    }
    Ok(())
}

/// What comes from outside becomes a sized integer through `u8.from(x)?` and `u8.parse(text)?`,
/// which fail cleanly with an `InvalidValue` when it doesn't fit or isn't a whole number, where
/// `u8(x)` would stop the program.
#[test]
fn sized_integers_from_outside_fail_cleanly() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "sized_from_outside",
        "def main(stdout: Stdout):
    stdout.print(u8.from(200) else u8(0), u8.parse(\"17\") else u8(0), i8.from(-5) else i8(0), u32.parse(\"4294967295\") else u32(0))
    for text in [\"300\", \"-1\", \"seven\"]:
        n = u8.parse(text) else as problem:
            stdout.print(\"failed:\", problem.message)
            continue
        stdout.print(\"fit\", n)
    small = u8.parse(\"250\") else u8(0)
    stdout.print(small + 5, i16.from(40000) else i16(-1))
",
    )?;
    assert_eq!(
        printed,
        "200 17 -5 4294967295\n\
         failed: 300 doesn't fit in u8, which holds 0 to 255\n\
         failed: -1 doesn't fit in u8, which holds 0 to 255\n\
         failed: \"seven\" isn't an int\n\
         255 -1\n"
    );
    Ok(())
}

/// A `u64` from outside: `u64.from(x)?` and `u64.parse(text)?`, which reads what's past the
/// largest `int` a digit at a time, failing cleanly past the top. And `u64`'s `wrapping_` and
/// `saturating_` methods, worked out on its 64 bits without going past them.
#[test]
fn a_u64_from_outside_and_its_wrapping_and_saturating_methods() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "u64_from_outside",
        "def said(run: () -> u64 fails InvalidValue) -> str:
    n = run() else as why:
        return f\"[{why.message}]\"
    f\"{n}\"

def main(stdout: Stdout):
    top = u64.parse(\"18446744073709551615\") else u64(0)
    half = u64.parse(\"9223372036854775808\") else u64(0)
    stdout.print(top, half, said(() => u64.parse(\"42\")?), said(() => u64.from(7)?), said(() => u64.from(-1)?), said(() => u64.parse(\"18446744073709551616\")?), said(() => u64.parse(\"-3\")?), said(() => u64.parse(\"seven\")?))
    stdout.print(top.wrapping_add(2), u64(1).wrapping_sub(3), top.wrapping_mul(top), half.wrapping_mul(2), u64(3037000500).wrapping_mul(3037000500))
    stdout.print(top.saturating_add(1), u64(1).saturating_sub(2), half.saturating_mul(2), u64(6).saturating_mul(7), top.saturating_mul(0))
    stdout.print(i8(u64(100)), u32(half >> 33), top << 4, u64(3) << 63)
",
    )?;
    let too_big = |text: &str| format!("[{text} doesn't fit in u64, which holds 0 to 18446744073709551615]");
    assert_eq!(
        printed,
        format!(
            "18446744073709551615 9223372036854775808 42 7 {} {} {} [\"seven\" isn't an int]\n\
             1 18446744073709551614 1 0 9223372037000250000\n\
             18446744073709551615 0 18446744073709551615 42 0\n\
             100 1073741824 18446744073709551600 9223372036854775808\n",
            too_big("-1"),
            too_big("18446744073709551616"),
            too_big("-3")
        )
    );
    Ok(())
}

/// `wrapping_` arithmetic goes round past either end, as the size's bits do, and `saturating_`
/// stops at the nearest end, for each size; a u32's product past what an int holds included.
#[test]
fn sized_integers_wrap_or_saturate_when_asked() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "sized_wrapping",
        "def main(stdout: Stdout):
    a: u8 = 250
    b: u8 = 10
    stdout.print(a.wrapping_add(b), a.saturating_add(b), b.wrapping_sub(a), b.saturating_sub(a), a.wrapping_mul(b))
    low: i8 = -128
    stdout.print(low.wrapping_sub(1), low.saturating_sub(1), low.wrapping_mul(-1), low.saturating_mul(-1))
    big: u32 = 4294967295
    stdout.print(big.wrapping_mul(big), big.saturating_mul(2), big.wrapping_add(1), u32(65536).wrapping_mul(65536))
    small: i32 = -2147483648
    stdout.print(small.wrapping_mul(small), small.saturating_mul(small), small.wrapping_sub(1))
    mut hash: u32 = 2166136261
    for byte in [104, 105]:
        hash = u32(int(hash) ^ byte).wrapping_mul(16777619)
    stdout.print(hash)
",
    )?;
    assert_eq!(
        printed,
        "4 255 16 0 196\n127 -128 -128 127\n1 4294967295 0 0\n0 2147483647 2147483647\n1748694682\n"
    );
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

/// An int's text is written natively, digit pairs from the last: every length of digits, both
/// ends of `int`, signs, and appended to a growing string, in `str` and in an f-string.
#[test]
fn an_ints_text_of_every_length_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "int_text",
        r#"def main(stdout: Stdout):
    mut power = 1
    mut out = ""
    for _ in range(19):
        stdout.print(str(power - 1), str(power), str(power + 1), str(-power), f"[{power * 3}]")
        out += str(power) + ","
        power *= 10
    biggest = 9223372036854775807
    stdout.print(str(biggest), str(-biggest - 1), f"{-biggest - 1}", str(0), str(-0))
    stdout.print(out, out.len())
"#,
    )
}

/// A builtin's value nothing keeps is still made and let go of natively, each on a line of its own,
/// with what's printed after the same.
#[test]
fn a_value_nothing_keeps_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "discarded",
        r#"def doubled(xs: list[int]) -> list[int]:
    [x * 2 for x in xs]

def main(stdout: Stdout):
    scores = [3, 1, 2]
    _ = sorted(scores)
    _ = str(42)
    _ = "a,b".split(",")
    _ = doubled(scores)
    stdout.print(scores, sorted(scores))
"#,
    )
}

/// A std module's exported constant, read through its module and imported by name, which
/// natively are the same text.
#[test]
fn a_std_constant_reads_both_ways_like_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "std_constants",
        "from std import string\nfrom std.string import DIGITS, PUNCTUATION\n\ndef main(stdout: Stdout):\n    stdout.print(string.DIGITS, DIGITS, string.DIGITS == DIGITS)\n    stdout.print(string.ASCII_LETTERS.len(), PUNCTUATION.len(), string.WHITESPACE.len())\n    stdout.print([c for c in \"a1b2\" if string.DIGITS.find(c) is not None])\n",
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
        text = str.from_code(code) else as error:
            stdout.print(error.message)
            continue
        stdout.print(text, text.len())
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

/// `sorted` of a type with its own `<` sorts by it as the JIT does: stable, so items `<` says are
/// neither before nor after keep their order, and the standard library's `Uuid` too.
#[test]
fn sorting_by_a_types_own_order_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "sorting_by_own_order",
        r#"from std.uuid import Uuid

struct Score:
    points: int
    name: str

    def <(self, other: Score) -> bool:
        self.points > other.points

def main(stdout: Stdout):
    scores = [Score(points=3, name="ash"), Score(points=7, name="vex"), Score(points=3, name="kit"), Score(points=9, name="bo")]
    stdout.print([score.name for score in sorted(scores)])
    empty: list[Score] = []
    stdout.print(sorted(empty).len(), [score.name for score in sorted([Score(points=1, name="one")])])
    later = Uuid.parse("018f2a6e-0000-7000-8000-000000000002") else Uuid()
    first = Uuid.parse("018f2a6e-0000-7000-8000-000000000001") else Uuid()
    stdout.print(sorted([later, first]) == [first, later])
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
        k = h
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

/// A failure inside the standard library's own code is said to be where the program called it,
/// not somewhere in the standard library, as the interpreter says it: here `swap`'s index.
#[test]
fn a_failure_inside_std_points_at_the_call_like_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "std_failure_at_the_call",
        "def shuffled(xs: list[int]) -> list[int]:\n    mut out = xs\n    out.swap(0, 7)\n    out\n\ndef main(stdout: Stdout):\n    stdout.print(shuffled([1, 2, 3]))\n",
    )
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
struct Log:
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
    log: shared Log
    tag = 0

def add_through(log: shared Log, line: str):
    lock log as ref held:
        held.add(line)

def seen(log: shared Log) -> int:
    lock log as held:
        return held.seen()

def changed(holder: Holder) -> Holder:
    mut mine = holder
    mine.tag += 1
    add_through(mine.log, \"from a copy\")
    mine

def main(stdout: Stdout):
    shared first = Log()
    second = first
    add_through(first, \"one\")
    add_through(second, \"two\")
    add_through(second, \"three\")
    mut count = 0
    mut note = \"\"
    lock first as log:
        count = log.count
        note = log.note
    stdout.print(seen(first), seen(second), count, note)
    holder = Holder(log=first)
    after = changed(holder)
    stdout.print(holder.tag, after.tag, seen(holder.log), seen(after.log), seen(first))
    shared other = Log()
    logs = [first, other, first]
    add_through(logs[0], \"through a list\")
    copied = logs
    add_through(copied[2], \"through a copied list\")
    stdout.print(seen(first), seen(logs[1]), seen(copied[0]))
    for log in logs:
        add_through(log, \"in a loop\")
    stdout.print(seen(first), seen(logs[1]))
    mut inside = 0
    lock first, logs[1] as ref a, b:
        a.count = 100
        a.lines.append(\"inside with\")
        inside = b.count
    lock first as log:
        stdout.print(inside, log.count, log.lines)
        stdout.print([log.count], f\"{holder.tag}\")
    lock logs[1] as log:
        stdout.print(log)
";

/// Two `shared` values in one `lock`, each changed through its own name.
const TWO_CELLS: &str = r"struct Account:
    total = 0

    def add(ref self, amount: int):
        self.total += amount

def transfer(source: shared Account, target: shared Account, amount: int) -> bool:
    lock source, target as ref x, ref y:
        if x.total < amount: return False
        x.total -= amount
        y.add(amount)
    True

def main(stdout: Stdout):
    shared two = Account(total=10)
    shared three = Account(total=20)
    moved = transfer(two, target=three, amount=4)
    lock two, three as x, y:
        stdout.print(moved, x.total, y.total)
    moved_back = transfer(three, target=two, amount=40)
    lock two, three as x, y:
        stdout.print(moved_back, x.total, y.total)
";

/// Each way a `lock` can come to take one value twice, all after printing something.
const TAKEN_TWICE: [(&str, &str); 5] = [
    ("spelled", "    lock one, one as ref x, ref y:\n        x.total = y.total + 1\n"),
    (
        "reading",
        "    mut seen = 0\n    lock one, one as x, y:\n        seen = x.total + y.total\n    stdout.print(seen)\n",
    ),
    ("named", "    other = one\n    lock one, other as ref x, y:\n        x.total = y.total + 1\n"),
    ("passed", "    both(one, b=one)\n"),
    ("held", "    lock Holder(box=one).box, one as ref x, y:\n        x.total = y.total + 1\n"),
];

const TAKING: &str = r#"struct Account:
    total = 0

struct Holder:
    box: shared Account

def both(a: shared Account, b: shared Account):
    lock a, b as ref x, ref y:
        x.total = y.total + 1

def main(stdout: Stdout):
    shared one = Account(total=10)
    shared two = Account(total=20)
    lock one, two as ref x, y:
        x.total = y.total + 1
    lock one as x:
        stdout.print("before", x.total)
"#;

/// The last way a function could take a lock its caller holds past what the checker saw: a
/// closure that takes it, passed to a call. The checker follows it now, so the program is refused
/// where it's built; the runtime's check, which ends a program that takes a held lock, stays as
/// the backstop.
const TAKEN_AGAIN: [(&str, &str); 1] = [(
    "closure_passed",
    "    reader = () => through_holder(holder)\n    lock one as ref x:\n        x.total = called(reader)\n",
)];

const LOCK_HELD: &str = r#"struct Box:
    total = 1
    items = [1, 2, 3]

struct Holder:
    box: shared Box

def through_holder(holder: Holder) -> int:
    lock holder.box as held:
        return held.total

def called(read: () -> int) -> int:
    read()

def main(stdout: Stdout):
    shared one = Box()
    holder = Holder(box=one)
    stdout.print("before", through_holder(holder))
"#;

/// A lock this thread holds, taken again through a closure passed to a call, is refused where the
/// program's built: the closure is followed to the lock it takes. It once built, and ended the
/// program where it ran, as it would wait forever otherwise.
#[test]
fn taking_a_lock_already_held_through_a_closure_is_refused_where_its_built() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("taken-again")?;
    let path = |file: &Path| file.display().to_string();
    for (name, shape) in TAKEN_AGAIN {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, format!("{LOCK_HELD}{shape}    stdout.print(\"not here\")\n"))?;
        let executable = dir.join(name);
        let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
        let said = String::from_utf8_lossy(&built.stderr);
        assert!(!built.status.success(), "{name} built");
        assert!(
            said.contains("`called` calls `read` while `x` is locked, and this takes `holder.box`'s lock"),
            "{name}: {said}"
        );
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
        "struct Box:\n    items = [1, 2, 3]\n\n    def doubled(ref self):\n        self.items = [n * 2 for n in self.items]\n\n\
         def main(stdout: Stdout):\n    shared one = Box()\n    lock one as ref x:\n        x.doubled()\n    \
         lock one as ref x:\n        x.items = [n + 1 for n in x.items]\n    lock one as x:\n        stdout.print(x.items)\n",
    )
}

#[test]
fn two_values_in_a_lock_each_change_through_their_own_name() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("two_cells", TWO_CELLS)
}

/// One value taken twice ends the program, after what it printed, however it came about.
#[test]
fn a_lock_taking_one_value_twice_ends_the_program() -> std::io::Result<()> {
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
        let ran = outcome(&Command::new(&executable).output_within()?);
        let interpreted = interpreted_by(&program, &format!("taken_twice_{name}"))?;
        assert_eq!(apart_from(&dir, ran.clone()), interpreted, "{name}");
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
const SHARED_SAME: &str = "struct Account:
    total = 0

struct Holder:
    account: shared Account
    n: int

enum Either:
    One(account: shared Account)
    Nothing

def main(stdout: Stdout):
    shared a = Account(total=1)
    b = a
    shared c = Account(total=1)
    stdout.print(a == a, a == b, a == c, a != a, a != c)
    stdout.print(Holder(a, 1) == Holder(b, 1), Holder(a, 1) == Holder(c, 1))
    stdout.print([a] == [b], [a] == [c], a in [c], a in [b], [a, c].index(c))
    stdout.print(Either.One(a) == .One(b), Either.One(a) == .One(c))
    maybe: (shared Account)? = a
    stdout.print(maybe == b, maybe == c)
    lock b as ref held:
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
struct Account:
    total = 0

    def ==(self, other: Account) -> bool:
        self.total > 100

def main(stdout: Stdout):
    shared a = Account(total=1)
    shared c = Account(total=200)
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
    let ran = Command::new(&executable).output_within()?;
    let interpreted = interpreted_by(&program, "shared_names")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
    Ok(())
}

/// `shared T` on a plain struct and a list, kept in a dict and a struct's fields, and reached only
/// in a `lock`: two locked together for a transfer, one locked to read. It builds natively.
#[test]
fn shared_values_reached_in_locks_build_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("shared_locks", SHARED_LOCKS)
}

/// A `lock ... as ref` block's changes are saved for a crash to put back, and kept when it ends
/// normally: an int, a string and a list changed, two values locked together, a `return` from
/// inside, and appends in a loop. What's printed after is the interpreter's.
#[test]
fn changes_in_a_lock_to_write_are_kept_as_the_interpreter_keeps_them() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "lock_saves",
        r#"struct Account:
    owner: str
    log: list[int]
    balance = 0

def pay(account: shared Account, amount: int, name: str):
    lock account as ref payer:
        payer.balance -= amount
        payer.owner = name
        payer.log.append(amount)

def swap(a: shared Account, b: shared Account) -> bool:
    lock a, b as ref first, ref second:
        if first.balance == second.balance:
            return False
        held = first.owner
        first.owner = second.owner
        second.owner = held
    True

def fill(account: shared Account, count: int):
    lock account as ref held:
        for n in range(count):
            held.log.append(n)
            held.balance += n

def main(stdout: Stdout):
    shared ana = Account(owner="ana", log=[], balance=10)
    shared bo = Account(owner="a name long enough to live in a block", log=[1])
    pay(ana, amount=4, name="bo")
    stdout.print(swap(ana, b=bo))
    fill(bo, count=100)
    lock ana, bo as first, second:
        stdout.print(first.balance, first.owner, first.log)
        stdout.print(second.balance, second.owner, second.log.len())
"#,
    )
}

const SHARED_LOCKS: &str = "struct Account:
    owner: str
    balance = 0

struct Bank:
    accounts: dict[str, shared Account]
    audit: shared list[str]

def transfer(bank: Bank, from_id: str, to_id: str, amount: int) -> bool:
    source = bank.accounts[from_id]
    target = bank.accounts[to_id]
    lock source, target as ref payer, ref payee:
        if payer.balance < amount:
            return False
        payer.balance -= amount
        payee.balance += amount
    lock bank.audit as ref log:
        log.append(f\"{from_id} -> {to_id}: {amount}\")
    True

def balance(account: shared Account) -> int:
    lock account as current:
        current.balance

def main(stdout: Stdout):
    shared ana = Account(owner=\"ana\", balance=10)
    shared bo = Account(owner=\"bo\")
    shared log = list[str]()
    bank = Bank(accounts={\"ana\": ana, \"bo\": bo}, audit=log)
    stdout.print(transfer(bank, from_id=\"ana\", to_id=\"bo\", amount=4), transfer(bank, from_id=\"bo\", to_id=\"ana\", amount=9))
    stdout.print(balance(ana), balance(bo))
    lock log as lines:
        stdout.print(lines)
";

/// An arrow failing fewer ways than the function type it's given for, as one failing with
/// `Refused` where `() -> str fails Refused | Shut` goes, is made as failing all of them, natively:
/// passed to a function, with a parameter, given to a name typed as one, and a function named as a
/// value, written in as an arrow calling it.
#[test]
fn an_arrow_failing_fewer_ways_than_its_type_says_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "arrow_widened",
        r#"error Refused:
    code: int

error Shut:
    why: str

def capped(n: int) -> int fails Refused:
    if n < 0:
        fail Refused(code=n)
    n

def closed(n: int) -> int fails Shut:
    if n > 5:
        fail Shut(why="big")
    n

def only_refused() -> str fails Refused:
    f"{capped(-3)?}"

def either(run: () -> str fails Refused | Shut) -> str:
    run() else as why:
        match why:
            case Refused(code): f"refused {code}"
            case Shut(why=reason): f"shut {reason}"

def each(step: (int) -> int fails Refused | Shut, n: int) -> str:
    stepped = step(n) else as why:
        match why:
            case Refused(code): return f"refused {code}"
            case Shut(why=reason): return f"shut {reason}"
    f"{stepped}"

def main(stdout: Stdout):
    stdout.print(either(only_refused), each(n => closed(n)? + 1, 9), each(n => closed(n)? + 1, 2))
    stdout.print(either(() => f"{capped(-1)?}"), either(() => f"{capped(2)?}"))
    run: () -> str fails Refused | Shut = () => f"{capped(-4)?}"
    stdout.print(either(run))
"#,
        "refused -3 shut big 3\nrefused -1 2\nrefused -4\n",
    )
}

/// An arrow function's `?` passes its failure up as the arrow's own, natively: a function taking
/// one that fails runs it twice, passing its failure on.
#[test]
fn an_arrow_functions_question_mark_passes_its_failure_up_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "arrow_fails",
        r#"error Bad:
    why: str

def half(n: int) -> int fails Bad:
    if n < 0:
        fail Bad(why="neg")
    n // 2

def twice(run: () -> int fails Bad) -> int fails Bad:
    run()? + run()?

def main(stdout: Stdout) fails Bad:
    stdout.print(twice(() => half(8)? + 1)?)
    stdout.print(twice(() => half(-2)?) else 99)
"#,
        "10\n99\n",
    )
}

/// `else as name:` gives the block what failed, and it still has to leave: by `return`, `continue`
/// or `fail`, here with a failure that's counted, after a call that gives nothing and one that
/// gives a value.
#[test]
fn else_as_names_what_failed() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "else_as",
        "error Boom:
    reason: str

def risky(flag: int) -> str fails Boom:
    if flag == 0:
        fail Boom(reason=\"zero \" + str(flag))
    return \"fine \" + str(flag)

def nothing(flag: int) fails Boom:
    if flag == 0:
        fail Boom(reason=\"nothing \" + str(flag))

def run(flag: int, stdout: Stdout) -> str:
    text = risky(flag) else as problem:
        stdout.print(\"saw\", problem.reason)
        return \"left\"
    nothing(flag) else as trouble:
        stdout.print(\"also\", trouble.reason)
        return \"unreached\"
    return text

def again(stdout: Stdout) fails Boom:
    for flag in [1, 0, 2]:
        made = risky(flag) else as problem:
            stdout.print(\"skip\", problem.reason)
            continue
        stdout.print(made)
    made = risky(0) else as problem:
        fail problem
    stdout.print(made)

def main(stdout: Stdout):
    stdout.print(run(0, stdout))
    stdout.print(run(1, stdout))
    again(stdout) else as problem:
        stdout.print(\"failed\", problem.reason)
        return
    stdout.print(\"ok\")
",
    )?;
    assert_eq!(printed, "saw zero 0\nleft\nfine 1\nfine 1\nskip zero 0\nfine 2\nfailed zero 0\n");
    Ok(())
}

/// A source's `else:` whose `x = f()?` fails: the block's own failure is the one that leaves,
/// carrying the cleanup's as a note, and the rest of the cleanup is skipped.
#[test]
fn a_cleanups_failing_assignment_is_a_note_on_the_blocks_failure() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "cleanup_assignment_noted",
        r#"error Shut:
    name: str

error Other:
    why: str

def closed(name: str) -> int fails Shut:
    fail Shut(name=name)

def opened(name: str, stdout: Stdout) -> with str fails Shut:
    lend name else:
        n = closed(name)?
        stdout.print("not here", n)

def used(stdout: Stdout) fails Shut | Other:
    with opened("door", stdout)? as door:
        fail Other(why="in the block")

def main(stdout: Stdout):
    used(stdout) else as why:
        stdout.print("failed", why, why.notes())
        return
    stdout.print("worked")
"#,
    )?;
    assert_eq!(printed, "failed Other(why=\"in the block\") [\"Shut(name=\\\"door\\\")\"]\n");
    Ok(())
}

/// A failure can leave a `with` block from inside what it calls: from a generator, whether it's
/// read through a name, looped over or summed, or from a round of a `parallel` loop. Each runs the
/// source's `else:` once, a `ref` case still puts back what it changed, and what a failing
/// cleanup says goes on the failure as a note.
#[test]
fn a_failure_from_inside_what_a_block_calls_runs_its_cleanup() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("failure_inside_a_block", FAILURE_INSIDE_A_BLOCK)?;
    assert_eq!(
        printed,
        "open\nopen\nclosed\nclosed\n7\n\
         open\n7\n8\ncleanup ran\nfailed 2 []\n\
         open\n7\n8\ncleanup ran\nfailed 2 []\n\
         open\ncleanup ran\nfailed 2 []\n\
         open\nopen\n14\n15\ncleanup ran\ncleanup ran\nfailed 2 []\n\
         open\ncleanup ran\nfailed 3 []\n\
         failed 2 [\"Boom(at=99)\"]\n\
         failed 2 [] 2\n"
    );
    Ok(())
}

const FAILURE_INSIDE_A_BLOCK: &str = "error Boom:
    at: int

def numbers(limit: int) -> Iterator[int] fails Boom:
    for at in range(limit):
        if at == 2:
            fail Boom(at=at)
        yield at

def opened(stdout: Stdout) -> with int fails Boom:
    stdout.print(\"open\")
    lend 7 else:
        stdout.print(\"cleanup ran\")
    stdout.print(\"closed\")

def through_a_name(stdout: Stdout) fails Boom:
    with opened(stdout)? as held:
        items = numbers(5)?
        for each in items:
            stdout.print(each + held)

def looped(stdout: Stdout) fails Boom:
    with opened(stdout)? as held:
        for each in numbers(5)?:
            stdout.print(each + held)

def summed(stdout: Stdout) fails Boom:
    with opened(stdout)? as held:
        stdout.print(sum(numbers(5)?) + held)

def nested(stdout: Stdout) fails Boom:
    with opened(stdout)? as first:
        with opened(stdout)? as second:
            for each in numbers(5)?:
                stdout.print(each + first + second)

def check(item: int) -> int fails Boom:
    if item == 3:
        fail Boom(at=item)
    return item

def rounds(stdout: Stdout) fails Boom:
    items = [1, 2, 3, 4]
    with opened(stdout)? as held:
        for item in parallel(items):
            _ = check(item)?
        stdout.print(held)

enum Kind:
    Button(clicks: int)
    Label

def clicks_of(kind: Kind) -> int:
    return match kind:
        case .Button(clicks):
            clicks
        case .Label:
            0

def put_back(ref kind: Kind) fails Boom:
    match kind:
        case .Button(ref clicks):
            clicks += 1
            _ = sum(numbers(5)?)
        case .Label:
            pass

def closing_fails() -> with int fails Boom:
    lend 1 else:
        fail Boom(at=99)

def noted(stdout: Stdout) fails Boom:
    with closing_fails()? as held:
        stdout.print(sum(numbers(5)?) + held)

def inner_returns(stdout: Stdout) -> int fails Boom:
    with opened(stdout)? as first:
        with opened(stdout)? as second:
            if first == second:
                return first
    return 0

## The run numbered `run`, passing its failure up.
def ran(stdout: Stdout, run: int) fails Boom:
    match run:
        case 0:
            through_a_name(stdout)?
        case 1:
            looped(stdout)?
        case 2:
            summed(stdout)?
        case 3:
            nested(stdout)?
        case 4:
            rounds(stdout)?
        case _:
            noted(stdout)?

def main(stdout: Stdout):
    value = inner_returns(stdout) else as problem:
        problem.at
    stdout.print(value)
    for run in [0, 1, 2, 3, 4, 6]:
        ran(stdout, run) else as problem:
            stdout.print(\"failed\", problem.at, problem.notes())
            continue
        stdout.print(\"no failure\")
    mut kind = Kind.Button(clicks=1)
    put_back(ref kind) else as problem:
        stdout.print(\"failed\", problem.at, problem.notes(), clicks_of(kind))
        return
    stdout.print(\"no failure\")
";

/// A crash in a `with` block runs its sources' `lend x else:` cleanups on the way out, innermost
/// first, and only those whose blocks haven't ended. A cleanup's own calls run as normal while the
/// crash waits, and a cleanup that crashes too is said, with the first crash the one that ends
/// the program.
#[test]
fn a_crash_runs_the_cleanups_of_the_blocks_it_leaves() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def lent(stdout: Stdout, name: str) -> with int:
    lend 1 else:
        stdout.print(f"cleanup {name}")
    stdout.print(f"done {name}")

def breaks(stdout: Stdout) -> with int:
    lend 0 else:
        stdout.print("cleanup that crashes")
        xs = [0]
        stdout.print(xs[7])

def main(stdout: Stdout):
    with lent(stdout, "ended") as first:
        stdout.print("first", first)
    with lent(stdout, "outer") as a:
        with breaks(stdout) as broken:
            with lent(stdout, "inner") as b:
                stdout.print("in")
                xs = [1]
                stdout.print(xs[a + b + broken])
"#;
    let dir = folder("crash_cleanups")?;
    let program = dir.join("crash_cleanups.fen");
    std::fs::write(&program, source)?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("crash_cleanups"));
    let output = output.to_string_lossy().into_owned();
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&output).output_within()?;
        let (out, err) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
        assert_eq!(
            out, "first 1\ndone ended\nin\ncleanup inner\ncleanup that crashes\ncleanup outer\n",
            "{how:?}: {err}"
        );
        assert!(!ran.status.success(), "{how:?}");
        assert!(
            err.contains("index 7 is out of range for length 1 (while cleaning up after a crash)"),
            "{err}"
        );
        assert!(err.contains("index 2 is out of range for length 1"), "{err}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A task that crashes inside a `with` runs the block's `lend x else:` before its crash ends it,
/// and a task that doesn't runs the rest of the source instead.
#[test]
fn a_crashing_task_runs_its_blocks_cleanup() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "task_crash_cleanup",
        r#"from std.concurrent import Crashed, Sender, channel

## Says `text` on `done`, whether or not anything's there to hear it.
def tell(done: Sender[str], text: str):
    done.send(text) else:
        pass

def lent(done: Sender[str]) -> with int:
    lend 7 else:
        tell(done, text="crashed")
    tell(done, text="finished")

def job(done: Sender[str], at: int):
    with lent(done) as held:
        values = [1, 2, 3]
        print_nothing(values[at] + held)

def print_nothing(n: int):
    pass

## Runs a job in a task of its own, and says how its task ended.
def ran(done: Sender[str], at: int) -> str:
    _ = launched(done, at=at) else:
        return "crashed"
    return "ended"

def launched(done: Sender[str], at: int) fails Crashed:
    with launch(() => job(done, at)) as task:
        pass

def main(stdout: Stdout):
    with channel[str](size=4) as said:
        stdout.print(ran(said.sender(), at=1), said.receive() else "none")
        stdout.print(ran(said.sender(), at=9), said.receive(timeout=1s) else "none")
"#,
        "ended finished\ncrashed crashed\n",
    )
}

/// A launched task runs only on the runtime's threads, which have no way to run it in order the way
/// `parallel`'s loops do, so a program that launches one and is built for the plain runtime is
/// refused when it's built, saying why, rather than failing at line 1 when it's run.
#[test]
fn a_program_that_launches_is_refused_without_the_threads_runtime() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("launch-needs-threads")?;
    let program = dir.join("launching.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    with launch(() => stdout.print(\"in a task\")):\n        pass\n",
    )?;
    let built = crate::executables::fennec_command()
        .args(["build", &program.to_string_lossy(), "-o", &dir.join("launching").to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .env("FENNEC_THREADS_RUNTIME", "0")
        .output_before(FENNEC_DEADLINE)?;
    let said = String::from_utf8_lossy(&built.stderr);
    assert!(!built.status.success(), "{said}");
    assert!(said.contains("`launch` runs its task on the runtime's threads"), "{said}");
    assert!(!dir.join("launching").exists(), "nothing's built");
    std::fs::remove_dir_all(&dir)
}

/// A type with an operator for each of several types on its right calls the one for the type
/// that's there, in its own methods, from an extension, and in a function.
#[test]
fn an_operator_is_chosen_by_the_type_on_its_right_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "operators_by_right",
        "struct Vec2:
    x: float
    y: float

struct Mat2:
    a: float
    b: float
    c: float
    d: float

    def *(self, other: Mat2) -> Mat2:
        Mat2(
            a=self.a * other.a + self.b * other.c,
            b=self.a * other.b + self.b * other.d,
            c=self.c * other.a + self.d * other.c,
            d=self.c * other.b + self.d * other.d,
        )

extend Mat2:
    def *(self, other: Vec2) -> Vec2:
        Vec2(x=self.a * other.x + self.b * other.y, y=self.c * other.x + self.d * other.y)

    def *(self, other: float) -> Mat2:
        Mat2(a=self.a * other, b=self.b * other, c=self.c * other, d=self.d * other)

def main(stdout: Stdout):
    turn = Mat2(a=0.0, b=-1.0, c=1.0, d=0.0)
    twice = turn * turn
    moved = turn * Vec2(x=1.0, y=2.0)
    big = turn * 3.0
    stdout.print(twice.a, twice.d, moved.x, moved.y, big.b)
",
    )?;
    assert_eq!(printed, "-1.0 -1.0 -2.0 1.0 -3.0\n");
    Ok(())
}

/// A `sum` of sized numbers is one of their size, starting at its zero: of `f32`s, of `u8`s, of
/// none, and of a generator's.
#[test]
fn a_sum_of_sized_numbers_is_of_their_size_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "sized_sums",
        "def main(stdout: Stdout):
    weights: list[f32] = [1.5, 2.25]
    counts: list[u8] = [200, 50]
    small: list[i16] = []
    total: u8 = sum(counts)
    stdout.print(sum(weights), total, sum(small), sum(count for count in counts), sum(counts, start=5))
",
    )?;
    assert_eq!(printed, "3.75 250 0 250 255\n");
    Ok(())
}

/// A function that takes a `shared` value and isn't called stays in the build when a local in
/// `main` has its name, and so does the declaration its `shared Point` means, which nothing
/// names: the program builds, rather than failing to check once trimmed.
#[test]
fn a_local_named_like_a_locking_function_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "local_named_like_locking",
        r"struct Point:
    x: int

def change(box: shared Point, by: int):
    lock box as ref held:
        held.x += by

def main(stdout: Stdout):
    for change in [1, 2]:
        stdout.print(change)
",
        "1\n2\n",
    )
}

/// A `shared` value of a program's own type named like a standard library type, as `Log` is, is
/// locked in a function of its own. A `ref self` method called under a write lock in a loop
/// changes the cell where it is, which copies nothing for the handle.
#[test]
fn a_lock_on_a_type_named_like_stds_and_in_a_loop_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "lock_named_like_std",
        "struct Log:
    count = 0

    def bump(ref self):
        self.count += 1

def bumped(log: shared Log, times: int):
    for _ in range(times):
        lock log as ref held:
            held.bump()

def length(log: shared Log) -> int:
    lock log as held:
        return held.count

def main(stdout: Stdout):
    shared log = Log()
    bumped(log, times=3)
    stdout.print(length(log))
",
    )?;
    assert_eq!(printed, "3\n");
    Ok(())
}

/// A program's own type named like one the library keeps to itself, `std.collections`' `Reached`,
/// is checked under a name no one can write, but nothing the program does depends on that: it
/// saves as a dict's key, which goes through functions written in Fennec for its tuples, and what
/// `pack` writes for it is what it would be for any type of that name and shape, the first word
/// after `FPK2` the hash of `Reached{x: int, y: int}`.
#[test]
fn a_type_named_like_a_private_std_type_saves_as_written() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let collections = Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/src/stdlib/collections.fen");
    assert!(
        std::fs::read_to_string(collections)?.contains("\nstruct Reached:"),
        "std no longer has a private `Reached`: name a type here after one it has"
    );
    // FNV-1a, as `shape_hash` takes it, of the shape as written.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in "Reached{x: int, y: int}".bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let shape: Vec<String> = hash.to_le_bytes().iter().map(u8::to_string).collect();
    let wanted = format!("[[{{\"x\": 1, \"y\": 2}}, \"tree\"]]\nTrue\nTrue\nbytes([{}])\n", shape.join(", "));
    prints_in_both_builds(
        "named_like_private_std",
        r#"from std import json, pack

struct Reached:
    x: int
    y: int

def main(stdout: Stdout):
    spots = {Reached(x=1, y=2): "tree"}
    text = json.dumps(spots)
    stdout.print(text)
    stdout.print((json.loads[dict[Reached, str]](text) else {}) == spots)
    stdout.print((pack.loads[dict[Reached, str]](pack.dumps(spots)) else {}) == spots)
    stdout.print(pack.dumps(Reached(x=1, y=2))[4:12])
"#,
        &wanted,
    )
}

/// A `JsonNames` case written with its enum, as `JsonNames.CamelCase`, in a `json.dumps` or a
/// `json.loads` builds natively as a leading dot does. The calls rewritten to std's code in
/// Fennec, whose `names` has a type the builtin's doesn't, aren't held again to the rule that
/// leaves the enum out where it's known: that's for the program as written.
#[test]
fn a_json_names_case_written_with_its_enum_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "json_names_with_enum",
        r#"from std import json
from std.json import JsonNames

struct Player:
    display_name: str

def main(stdout: Stdout):
    text = json.dumps(Player(display_name="a"), names=JsonNames.CamelCase)
    stdout.print(text)
    stdout.print(json.loads[Player](text, names=JsonNames.CamelCase) else Player(display_name="b"))
"#,
        "{\"displayName\": \"a\"}\nPlayer(display_name=\"a\")\n",
    )
}

/// `for field in fields(T):` is written out for each of `T`'s fields in each copy, with
/// `field.name` its name and `field.get(x)` its value, so a generic function can walk a struct and
/// what's inside it. A type with no fields gives none.
#[test]
fn a_generic_function_loops_over_a_types_fields_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "fields_of",
        "struct Inner:
    depth: int

struct Outer:
    name: str
    inner: Inner

def names[T](value: T, ref found: list[str]):
    for field in fields(T):
        found.append(field.name)
        names(field.get(value), ref found)

def say[V](name: str, value: V, stdout: Stdout):
    stdout.print(name, value)

def describe[T](value: T, stdout: Stdout):
    for field in fields(T):
        say(field.name, value=field.get(value), stdout=stdout)

def main(stdout: Stdout):
    mut found = list[str]()
    names(Outer(name=\"a\", inner=Inner(depth=1)), ref found)
    stdout.print(found)
    describe(Inner(depth=4), stdout)
",
    )?;
    assert_eq!(printed, "[\"name\", \"inner\", \"depth\"]\ndepth 4\n");
    Ok(())
}

/// A program's own source with a call that leaves out a default, written in twice: each copy
/// takes the default once, and so does the source it's written from.
#[test]
fn a_default_left_out_in_a_source_written_in_twice_is_given_once() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "default_given_once",
            r"struct Counter:
    start: int

    def opened(self) -> with int:
        lend added(self.start)

def added(n: int, more = 1) -> int:
    n + more

def main(stdout: Stdout):
    counter = Counter(start=1)
    with counter.opened() as first:
        with counter.opened() as second:
            stdout.print(first + second)
"
        )?,
        "4\n"
    );
    Ok(())
}

/// A type parameter named only in a parameter's type, as `E` is in `make: () -> T fails E`: a
/// generic source written in where it's used doesn't know it, and a call to a generic function
/// learns it from the function it's given.
#[test]
fn a_type_parameter_only_in_a_function_parameter_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "source_with_e",
            r#"error Nope:
    why: str

struct Held[T]:
    value: T

def holding[T, E](make: () -> T fails E) -> with Held[T] fails Nope:
    value = make() else fail Nope(why="no")
    lend Held[T](value=value) else:
        pass

def one() -> int fails Nope:
    1

def main(stdout: Stdout) fails Nope:
    with holding(one)? as held:
        stdout.print(held.value)
"#
        )?,
        "1\n"
    );
    assert_eq!(
        printed_by(
            "call_with_e",
            r#"from std.concurrent import Crashed

error Nope:
    why: str

def started[T, E](make: () -> T fails E) fails Crashed:
    with launch(() => ran(make))?:
        pass

def ran[T, E](make: () -> T fails E):
    _ = make() else:
        return

def one() -> int fails Nope:
    1

def main(stdout: Stdout) fails Crashed:
    started(one)?
    stdout.print("ok")
"#
        )?,
        "ok\n"
    );
    Ok(())
}

/// `with src else fail E` on a source whose body has an `x else fail` and a `with launch(...)?`:
/// both fail with `E` once it's written in, and the launch stays one.
#[test]
fn a_source_failing_instead_holds_its_else_fails_and_its_launch() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let printed = printed_by(
        "source_failing_instead",
        r#"from std.concurrent import Crashed

error Outer:
    why: str

error Inner:
    why: str

def work():
    pass

def given(bad: bool) -> int?:
    None if bad else 1

def inner_source(bad: bool) -> with int fails Inner | Crashed:
    first = given(bad) else fail Inner(why="none")
    with launch(() => work())?:
        lend first

def outer_source(bad: bool) -> with int fails Outer:
    with inner_source(bad) else fail Outer(why="the inner source failed") as n:
        lend n + 1

def run(bad: bool, stdout: Stdout) fails Outer:
    with outer_source(bad)? as n:
        stdout.print("got", n)

def main(stdout: Stdout):
    for bad in [False, True]:
        _ = run(bad, stdout) else as why:
            stdout.print("failed", why)
            continue
        stdout.print("worked")
"#,
    )?;
    assert_eq!(printed, "got 2\nworked\nfailed Outer(why=\"the inner source failed\")\n");
    Ok(())
}

/// On a failure that's a union, `case Shut.Stuck(...)` in a `match` on what `else as` names says
/// which error's case, and a later case holds what the earlier cases left: a `Shut` once `Late` is
/// taken.
#[test]
fn a_case_names_a_unions_error_and_later_cases_hold_what_is_left() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "union_cases",
        r#"error Shut:
    Stuck(name: str)
    Jammed

    def said(self) -> str:
        match self:
            case .Stuck(name):
                f"stuck {name}"
            case .Jammed:
                "jammed"

error Late:
    waited: int

def run(n: int) fails Shut | Late:
    if n == 0: fail Shut.Stuck(name="door")
    if n == 1: fail Late(waited=1)
    if n == 2: fail Shut.Jammed

def main(stdout: Stdout):
    for n in [0, 1, 2, 3]:
        _ = run(n) else as why:
            match why:
                case Late(waited=_):
                    stdout.print("late")
                    continue
                case Shut.Stuck(name=name):
                    stdout.print("named", name)
                    continue
                case error:
                    stdout.print(error.said())
                    continue
        stdout.print("went through")
"#,
    )?;
    assert_eq!(printed, "named door\nlate\njammed\nwent through\n");
    Ok(())
}

/// A source with an `else:` around a block holding a source that opens another: the one written
/// in last is written in first, so the outer `else:` runs when the innermost fails opening, and
/// each `with` stays a `with` until it's written in.
#[test]
fn a_source_around_one_that_opens_another_writes_the_innermost_in_first() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "innermost_first",
        r#"error Boom:
    at: int

def inner(n: int, stdout: Stdout) -> with int fails Boom:
    if n < 0: fail Boom(at=n)
    lend n
    stdout.print("inner closed")

def middle(n: int, stdout: Stdout) -> with int fails Boom:
    with inner(n, stdout)? as got:
        lend got + 1
    stdout.print("middle closed")

def outer(n: int, stdout: Stdout) -> with int fails Boom:
    lend n else:
        stdout.print("outer's cleanup")
    stdout.print("outer closed")

def used(n: int, stdout: Stdout) fails Boom:
    with outer(n, stdout)? as a:
        with middle(a, stdout)? as b:
            stdout.print("got", b)

def main(stdout: Stdout):
    for n in [1, -1]:
        _ = used(n, stdout) else as why:
            stdout.print("failed", why)
            continue
        stdout.print("worked")
"#,
    )?;
    assert_eq!(
        printed,
        "got 2\ninner closed\nmiddle closed\nouter closed\nworked\nouter's cleanup\nfailed Boom(at=-1)\n"
    );
    Ok(())
}

/// A source written in at three levels of one block, each copy with a `?` on a line that names
/// what it gives, keeps each copy's names apart.
#[test]
fn one_source_twice_in_a_block_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "source_twice",
        "error Boom:
    pass

struct Conn:
    n: int

    def made(self, n: int) -> int fails Boom:
        if n < 0:
            fail Boom()
        n + self.n

    def opened(self, n: int) -> with int fails Boom:
        got = self.made(n)?
        lend got else:
            pass

def main(stdout: Stdout) fails Boom:
    conn = Conn(n=0)
    with conn.opened(0)? as outer:
        with conn.opened(1)? as first:
            with conn.opened(2)? as second:
                stdout.print(outer + first + second)
",
    )?;
    assert_eq!(printed, "3\n");
    Ok(())
}

/// `with src else fail E as x:` fails with `E` however the source fails: while opening, as here,
/// or while finishing. A source can wrap another this way and say only its own error, and the
/// block's own failures pass up as they are.
#[test]
fn a_with_else_fail_gives_its_own_failure_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "with_else_fail",
        "error First:
    Broke(why: str)

error Second:
    Lost(why: str)

def inner(fine: bool) -> with int fails First:
    if not fine: fail .Broke(why=\"inner\")
    lend 1 else:
        pass

def outer(fine: bool) -> with int fails Second:
    with inner(fine) else fail .Lost(why=\"outer\") as held:
        lend held + 1 else:
            pass

def main(stdout: Stdout):
    for fine in [True, False]:
        _ = used(fine, stdout) else as problem:
            stdout.print(\"failed\", problem)
            continue
        stdout.print(\"worked\")

def used(fine: bool, stdout: Stdout) fails Second:
    with outer(fine)? as value:
        stdout.print(value)
",
    )?;
    assert_eq!(printed, "2\nworked\nfailed Second.Lost(why=\"outer\")\n");
    Ok(())
}

/// A generic function calling a `with` source its interface gives, `db.transaction()` for
/// `D: Database`, runs the source of the type it's called with: a copy of it is made for each
/// type, once each, through another generic function, with the type written in brackets, and for
/// a parameter typed by the interface itself.
#[test]
fn an_interface_source_in_a_generic_runs_each_types_own_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "interface_source",
        r#"error Nope:
    why: str

struct Transaction:
    log: str

interface Database:
    def run(self, text: str) -> int fails Nope
    def transaction(self) -> with Transaction fails Nope

struct Lite:
    name: str

    def run(self, text: str) -> int fails Nope:
        text.len()

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") else:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        text.len() * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") else:
            pass

def saved[D: Database](db: D) -> str fails Nope:
    with db.transaction()? as tx:
        count = db.run("abc")?
        return f"{tx.log} {count}"
    ""

def twice[D: Database](db: D) -> str fails Nope:
    first = saved(db)?
    f"{first}/{saved(db)?}"

def plain(db: Database) -> int fails Nope:
    with db.transaction()? as tx:
        return tx.log.len()
    0

def main(stdout: Stdout) fails Nope:
    stdout.print(saved(Lite(name="a"))?, saved(Postgres(host="b"))?, saved(Lite(name="c"))?)
    stdout.print(twice(Postgres(host="d"))?, saved[Lite](Lite(name="e"))?)
    stdout.print(plain(Lite(name="f"))?, plain(Postgres(host="g"))?)
"#,
        "lite 3 pg 6 lite 3\npg 6/pg 6 lite 3\n4 2\n",
    )
}

/// A function whose last line is a `with` whose block always returns gives back a value on every
/// way, once the source is written in: alone, in each branch of an `if`, and in each case of
/// a `match`.
#[test]
fn a_function_ending_in_a_with_that_returns_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "with_returns_last",
        r#"struct Session:
    log: str

struct Lite:
    name: str

    def session(self) -> with Session:
        lend Session(log="lite") else:
            pass

def direct(store: Lite, text: str) -> str:
    with store.session() as session:
        return f"{session.log} {text}"

def branched(store: Lite, long: bool) -> int:
    if long:
        with store.session() as session:
            return session.log.len() * 10
    else:
        with store.session() as session:
            return session.log.len()

def matched(store: Lite, n: int) -> int:
    match n:
        case 0:
            with store.session() as session:
                return session.log.len() * 100
        case _:
            with store.session() as session:
                return session.log.len() + n

def main(stdout: Stdout):
    store = Lite(name="a")
    stdout.print(direct(store, "x"), branched(store, long=True), branched(store, long=False))
    stdout.print(matched(store, 0), matched(store, 3))
"#,
        "lite x 40 4\n400 7\n",
    )
}

/// A function whose last line is a `with` over a source that can fail after its `lend`, whose
/// block's last line is the value it gives back: alone, in each branch of an `if`, and in a case
/// of a `match`. The source's code after `lend` is written in after the block, so the value was
/// lost and the build stopped with an internal error. A failure after `lend` still passes up.
#[test]
fn a_with_whose_last_line_is_the_functions_value_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "with_last_value",
        r#"error Refused:
    code: int

struct Tally:
    count: int

def held(code: int) -> with Tally fails Refused:
    lend Tally(count=code)
    if code == 3:
        fail Refused(code=code)

def counted(code: int) -> int fails Refused:
    with held(code)? as tally:
        tally.count

def branched(code: int, twice: bool) -> int fails Refused:
    if twice:
        with held(code)? as tally:
            tally.count * 2
    else:
        with held(code)? as tally:
            tally.count

def matched(code: int) -> str fails Refused:
    match code:
        case 0:
            "none"
        case _:
            with held(code)? as tally:
                f"tally {tally.count}"

def main(stdout: Stdout):
    stdout.print(counted(4) else 0, counted(3) else 0)
    stdout.print(branched(4, twice=True) else 0, branched(5, twice=False) else 0, branched(3, twice=True) else -1)
    stdout.print(matched(0) else "failed", matched(6) else "failed", matched(3) else "failed")
"#,
        "4 0\n8 5 -1\nnone tally 6 failed\n",
    )
}

/// `with` sources, written in where they're used: a method one and a function one, one inside
/// the other, with `ref` parameters, the caller's block leaving by `return` from inside a loop,
/// by `break`, and by failing, which runs the `lend`'s `else:` and skips the cleanup below it.
/// A whole line that's a `?` on a method giving back nothing fails the block too, and an
/// `else:` written in at two places keeps each copy's `_ = ...` apart.
#[test]
fn with_sources_lend_to_their_callers_blocks_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("with_sources", WITH_SOURCES)
}

const WITH_SOURCES: &str = "error DbError:
    why: str

struct Log:
    lines = list[str]()

struct Db:
    name: str

    def session(self, user: str, ref log: Log) -> with str fails DbError:
        if user == \"\":
            fail DbError(why=\"who?\")
        log.lines.append(f\"open {self.name} for {user}\")
        lend f\"{self.name}:{user}\" else:
            log.lines.append(f\"failed {user}\")
        log.lines.append(f\"close {user}\")

def scoped(ref log: Log, label: str) -> with int:
    log.lines.append(f\"enter {label}\")
    lend label.len()
    log.lines.append(f\"leave {label}\")

def check(n: int) -> int fails DbError:
    if n == 3: fail DbError(why=\"three\")
    n

struct Probe:
    held: int

    def poke(self) fails DbError:
        if self.held > 5: fail DbError(why=\"big\")

def probe(held: int, ref log: Log) -> with Probe fails DbError:
    lend Probe(held=held) else:
        _ = log.lines.len()
        log.lines.append(f\"probe {held} failed\")

def poked(held: int, ref log: Log) fails DbError:
    with probe(held, ref log)? as box:
        box.poke()?
        log.lines.append(f\"poked {held}\")
        box.poke()?

def first_big(db: Db, ref log: Log, limit: int) -> int fails DbError:
    with db.session(\"ana\", ref log)? as session:
        with scoped(ref log, session) as size:
            for n in range(10):
                if check(n)? + size > limit:
                    return n
    -1

def count(db: Db, ref log: Log) -> int fails DbError:
    mut total = 0
    while True:
        with db.session(\"bo\", ref log)? as session:
            total += session.len()
            if total > 10:
                break
    total

def main(stdout: Stdout):
    db = Db(name=\"main\")
    mut log = Log()
    stdout.print(first_big(db, ref log, limit=5) else -9)
    stdout.print(first_big(db, ref log, limit=50) else -9)
    stdout.print(count(db, ref log) else -9)
    for held in [1, 9]:
        _ = poked(held, ref log) else as error:
            stdout.print(error.why)
            continue
        stdout.print(\"poked\")
    for line in log.lines:
        stdout.print(line)
";

/// A name a source's `match` binds is its own, wherever the source is written in: a `lend` inside
/// a case, a name used after one, and a name that's bound in one place and assigned in another.
#[test]
fn a_with_source_can_lend_and_bind_names_in_its_match() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "lending_in_a_match",
        "error Boom:
    at: int

def made() -> int fails Boom:
    return 5

def held_open(stdout: Stdout) -> with int fails Boom:
    conn = made() else as problem:
        fail problem
    lend conn
    stdout.print(\"done\", conn)

def later(stdout: Stdout) -> with int fails Boom:
    conn = 3
    lend conn
    found = made() else as problem:
        fail problem
    stdout.print(found + conn)

def bound_then_assigned(stdout: Stdout) -> with int fails Boom:
    lend 1
    got = made() else as conn:
        fail conn
    stdout.print(\"conn\", got)
    conn = 2
    stdout.print(conn)

def main(stdout: Stdout) fails Boom:
    with held_open(stdout)? as held:
        stdout.print(held)
    with later(stdout)? as more:
        stdout.print(more)
    with bound_then_assigned(stdout)? as again:
        stdout.print(again)
",
    )?;
    assert_eq!(printed, "5\ndone 5\n3\n8\n1\nconn 5\n2\n");
    Ok(())
}

/// Several handles in one `with` open in order and finish innermost first, natively as in the
/// interpreter. One that fails to open runs the `else:` of each opened before it, a block that
/// fails runs every one's, and a source that lends a tuple splits it into a name each.
#[test]
fn several_handles_in_one_with_finish_innermost_first_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("with_handles", WITH_HANDLES)
}

const WITH_HANDLES: &str = r#"error Refused:
    who: str

struct Log:
    lines = list[str]()

def opened(name: str, ref log: Log) -> with str fails Refused:
    if name == "bad":
        fail Refused(who=name)
    log.lines.append(f"open {name}")
    lend name else:
        log.lines.append(f"undo {name}")
    log.lines.append(f"close {name}")

def pair(ref log: Log) -> with (int, str):
    log.lines.append("split")
    lend (7, "w")
    log.lines.append("joined")

def both(first: str, second: str, ref log: Log) -> int fails Refused:
    with opened(first, ref log)? as left, opened(second, ref log)? as right:
        log.lines.append(f"use {left} {right}")
        if right == "c":
            fail Refused(who="late")
    1

def main(stdout: Stdout):
    mut log = Log()
    stdout.print(both("a", "b", ref log) else -1)
    stdout.print(log.lines)
    log.lines.clear()
    stdout.print(both("a", "bad", ref log) else -1)
    stdout.print(log.lines)
    log.lines.clear()
    stdout.print(both("a", "c", ref log) else -1)
    stdout.print(log.lines)
    log.lines.clear()
    with pair(ref log) as reader, writer:
        log.lines.append(f"{reader} {writer}")
    stdout.print(log.lines)
"#;

/// What a `with` source lends is the block's to use, `ref self` methods included, with or without
/// `as ref`: lent by name, the block's changes are what the source's cleanup sees. A `shared`
/// value lent the same way is changed inside a `lock`.
#[test]
fn lent_values_take_ref_self_calls_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("lent_structs", LENT_STRUCTS)?;
    same_as_the_interpreter_in_both_builds("lent_handles", LENT_HANDLES)
}

const LENT_STRUCTS: &str = "struct Sink:
    parts = list[str]()

    def write(ref self, text: str):
        self.parts.append(text)

def sink(stdout: Stdout) -> with Sink:
    mut held = Sink()
    lend held
    stdout.print(\"after the block:\", held.parts)

def made(stdout: Stdout) -> with Sink:
    lend Sink(parts=[\"start\"])
    stdout.print(\"made\")

def main(stdout: Stdout):
    with sink(stdout) as out:
        out.write(\"a\")
    with sink(stdout) as ref out:
        out.write(\"b\")
    with made(stdout) as other:
        other.write(\"c\")
        stdout.print(other.parts)
";

const LENT_HANDLES: &str = "error WriteError:
    why: str

struct File:
    lines: list[str]

    def write_line(ref self, line: str) fails WriteError:
        if line == \"\": fail WriteError(why=\"empty\")
        self.lines.append(line)

    def written(self) -> int:
        self.lines.len()

def create(name: str, stdout: Stdout) -> with shared File fails WriteError:
    shared file = File(lines=[])
    lend file
    lock file as seen:
        stdout.print(\"closing\", name, seen.written())

def save(stdout: Stdout) -> int fails WriteError:
    with create(\"notes\", stdout)? as file:
        lock file as ref held:
            held.write_line(\"fox\")?
            held.write_line(\"den\")?
    1

def main(stdout: Stdout):
    stdout.print(save(stdout) else -1)
";

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
    mut list_only = two
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
    mut nested = kept
    other = nested
    nested.packets[0].data[1] = 123
    stdout.print(nested.packets[0].data[1], other.packets[0].data[1], kept.packets[0].data[1])
    mut walked = [1, 2, 3]
    before = walked
    for ref item in walked:
        item = item * 2
    stdout.print(walked[0], walked[2], before[0], before[2])
    mut grows = [1, 2]
    grows_before = grows
    for item in grows_before:
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

/// An f-string made in one go from its pieces where they are: strings inline and in blocks,
/// ASCII or not, ints at their ends, floats short and long, formats, values written out, and
/// results that fit in a header and that don't, on both sides of 23 bytes.
#[test]
fn f_strings_made_in_one_go_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("f_strings_in_one_go", F_STRINGS_IN_ONE_GO)
}

const F_STRINGS_IN_ONE_GO: &str = r#"struct Point:
    x: int
    y: int

enum Shape:
    Dot
    Box(side: float)

def main(stdout: Stdout):
    short = "fóx"
    long = "a string long enough that it can't be held inline, ünïcode too"
    empty = ""
    n = -9223372036854775807 - 1
    big = 123456789012345678
    tiny = 2.2250738585072014e-308
    stdout.print(f"{short}")
    stdout.print(f"{long}")
    stdout.print(f"[{empty}]")
    stdout.print(f"{n}|{big}|{0}")
    stdout.print(f"{tiny}|{1.5}|{-0.0}|{1e300}")
    stdout.print(f"{True} {False}")
    stdout.print(f"{Point(x=1, y=2)} {Shape.Dot} {Shape.Box(side=2.5)}")
    stdout.print(f"{[1, 2, 3]}")
    stdout.print(f"{n:,}|{1.25:.3f}|{big:,}")
    stdout.print(f"{short}{short}{short}{short}{short}{short}{short}{short}")
    stdout.print(f"ünï{short}çø{long}{n}")
    stdout.print(f"exactly twenty-three!{short}"[0:5])
    stdout.print(f"{short} is {short.len()} chars")
    mut total = 0
    for i in range(1000):
        line = f"{i}: {short}-{long}" if i % 2 == 0 else f"{i}{i}"
        total += line.len()
    stdout.print(total)
"#;

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
    mut c = b
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
    let ran = Command::new(&executable).output_within()?;
    let interpreted = interpreted_by(&program, "own_text_inside")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
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
    _ = map.add(\"c\")
    _ = map.remove(second)
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
    let ran = Command::new(&executable).output_within()?;
    let interpreted = interpreted_by(&program, "shown_values")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
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
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    let interpreted = interpreted(name, source)?;
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

/// Programs that don't build natively yet, each waiting on named backend work. A listed program
/// that does build natively fails the tests below, so the list only ever shrinks.
const NOT_NATIVE_YET: &[&str] = &[];

fn not_native_yet(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| NOT_NATIVE_YET.contains(&name))
}

/// Every example builds through LLVM, prints exactly what it should and finishes without failing.
#[test]
fn the_examples_it_takes_print_what_they_should() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("examples")?;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(examples)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") && path.with_extension("out").exists() {
            paths.push(path);
        }
    }
    paths.sort();
    // What became of each: built and printed this, or refused with this.
    let outcomes = crate::executables::at_once(&paths, |worker, path| {
        let home = dir.join(format!("home-{worker}"));
        let executable = dir.join(format!("example-{worker}"));
        let made = fennec(
            &["build", "--llvm", &path.to_string_lossy(), "-o", &executable.to_string_lossy()],
            &home,
        )?;
        if !made.status.success() {
            return Ok(Err(String::from_utf8_lossy(&made.stderr).trim().to_owned()));
        }
        let ran = Command::new(&executable)
            .output_within()
            .map_err(|error| std::io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
        // What it printed, or that it failed.
        let printed = String::from_utf8_lossy(&ran.stdout).into_owned();
        Ok(Ok(ran.status.success().then_some(printed)))
    })?;
    let (mut refused, mut wrong, mut now_native) = (Vec::new(), Vec::new(), Vec::new());
    for (path, outcome) in paths.iter().zip(outcomes) {
        match outcome {
            Ok(_) if not_native_yet(path) => now_native.push(path),
            Err(_) if not_native_yet(path) => {}
            Err(why) => refused.push(format!("{}: {why}", path.display())),
            Ok(None) => wrong.push(path),
            Ok(Some(printed)) if printed != std::fs::read_to_string(path.with_extension("out"))? => {
                wrong.push(path);
            }
            Ok(Some(_)) => {}
        }
    }
    std::fs::remove_dir_all(&dir)?;
    assert!(now_native.is_empty(), "these build natively now: take them off NOT_NATIVE_YET: {now_native:?}");
    assert!(wrong.is_empty(), "printed the wrong thing or failed: {wrong:?}");
    assert!(refused.is_empty(), "these don't build through LLVM:\n{}", refused.join("\n"));
    Ok(())
}

/// Every program in the repository builds as native code, with nothing LLVM refuses: each example, kit and benchmark, `examples/proof` and `selfhost`. The examples
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
    // What each said: nothing where it's all native code, or why it isn't.
    let said = crate::executables::at_once(&programs, |worker, program| {
        let said = fennec(
            &["dev", "fallbacks", "--llvm", &program.to_string_lossy()],
            &dir.join(format!("home-{worker}")),
        )?;
        let told = String::from_utf8_lossy(&said.stdout);
        if said.status.success() && told.lines().last() == Some("everything compiles to native code") {
            return Ok(None);
        }
        Ok(Some(format!("{told}{}", String::from_utf8_lossy(&said.stderr))))
    })?;
    let mut not_native = Vec::new();
    for (program, why) in programs.iter().zip(said) {
        match why {
            None if not_native_yet(program) => {
                panic!("{} builds natively now: take it off NOT_NATIVE_YET", program.display())
            }
            Some(why) if !not_native_yet(program) => {
                not_native.push(format!("{}:\n  {}", program.display(), why.trim().replace('\n', "\n  ")));
            }
            _ => {}
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

/// Int keys made so a dict's fast hash, under seed 0, puts them all in the first slot: its top bits
/// differ and the low 22 are zero, undoing the hash's last two steps.
fn keys_that_all_collide(count: u64) -> String {
    const INVERSE_OF_MIX: u64 = 0xF1DE_83E1_9937_733D;
    let mut keys = String::new();
    for number in 1..=count {
        let folded = number << 22;
        let mixed = folded ^ (folded >> 29) ^ (folded >> 58);
        let key = mixed.wrapping_mul(INVERSE_OF_MIX).cast_signed();
        keys.push_str(&key.to_string());
        keys.push('\n');
    }
    keys
}

/// Keys chosen to collide stop being slow once a dict sees it: the first long probe makes it hash
/// them another way. Quadratic, these keys take minutes; the deadline is far from either time.
#[test]
fn keys_made_to_collide_dont_slow_a_dict_down() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("colliding_keys")?;
    let program = dir.join("colliding.fen");
    std::fs::write(
        &program,
        r#"def key_in(seen: dict[int, int], key: int) -> bool:
    key in seen

def main(stdout: Stdout, stdin: Stdin):
    mut seen: dict[int, int] = {}
    mut order = 0
    keys = [int(line).expect("a number") for line in stdin.lines()]
    for key in keys:
        seen[key] = order
        order += 1
    total = sum(seen[key] for key in keys)
    stdout.print(seen.len(), total, key_in(seen, keys[0]), key_in(seen, 1))
"#,
    )?;
    let run = crate::executables::fennec_command()
        .arg("run")
        .arg(&program)
        .env("FENNEC_HASH_SEED", "0")
        .output_given(keys_that_all_collide(300_000).as_bytes(), std::time::Duration::from_secs(50))?;
    assert_eq!(String::from_utf8_lossy(&run.stderr), "");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "300000 44999850000 True False\n");
    Ok(())
}

/// `count` values of `next` whose hash, by `hash`, is zero in its low 11 bits, so a dict with 2048
/// slots or fewer gives them all one first slot.
fn colliding<T>(count: usize, hash: impl Fn(&T) -> u64, next: impl Fn(u64) -> T) -> Vec<T> {
    (0..).map(next).filter(|value| hash(value).trailing_zeros() >= 11).take(count).collect()
}

/// The fast hash of words, under seed 0, as the runtime and compiled code work it out.
fn fast_hash(count: u64, words: &[u64]) -> u64 {
    const MIX: u64 = 0x9E37_79B9_7F4A_7C15;
    let mixed = words
        .iter()
        .fold(count.wrapping_mul(MIX), |hash, word| (hash.rotate_left(23) ^ word).wrapping_mul(MIX));
    (mixed ^ (mixed >> 29)) >> 1
}

fn fast_text_hash(text: &str) -> u64 {
    let words: Vec<u64> = text
        .as_bytes()
        .chunks(8)
        .map(|chunk| {
            let mut word = [0; 8];
            for (to, from) in word.iter_mut().zip(chunk) {
                *to = *from;
            }
            u64::from_le_bytes(word)
        })
        .collect();
    fast_hash(u64::try_from(text.len()).unwrap_or(0), &words)
}

/// New struct keys go into a dict as compiled code adds them itself, while it has room, and through
/// the runtime when it must grow, when another owner holds it, and when the value holds a block:
/// every key is found again, the order they went in is kept, and a copy taken partway keeps only
/// what it had, in a dev build and with `--release`.
#[test]
fn struct_keys_added_natively_are_found_in_order() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Cell:
    x: int
    y: int

def main(stdout: Stdout):
    mut seen: dict[Cell, int] = {}
    mut named: dict[Cell, str] = {}
    mut kept: dict[Cell, int] = {}
    for i in range(5000):
        cell = Cell(x=i % 71, y=i // 71)
        seen[cell] = (seen.get(cell) else 0) + i
        if i % 3 == 0:
            named[cell] = f"cell {i}"
        if i == 2500:
            kept = seen
    for i in range(5000):
        cell = Cell(x=i % 71, y=i // 71)
        seen[cell] = (seen.get(cell) else 0) + 1
    mut total = 0
    for cell, count in seen.items():
        total += cell.x * count + cell.y
    first = Cell(x=0, y=0)
    last = Cell(x=4999 % 71, y=4999 // 71)
    order = [cell.x * 100 + cell.y for cell in seen.keys()]
    stdout.print(seen.len(), kept.len(), named.len(), total, seen[first], seen[last], kept.get(last) else -1)
    stdout.print(order[0], order[1], order[4999], named[Cell(x=3 % 71, y=3 // 71)])
"#;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("struct_keys_native", source, how)?;
        assert_eq!(
            (out.as_str(), code),
            ("5000 2501 1667 436784630 1 5000 -1\n0 100 2970 cell 3\n", Some(0)),
            "{how:?}: {err}"
        );
    }
    Ok(())
}

/// A field of a dict's item changed through a copy of the dict another still holds, as
/// `mut units = given` then `units["a"].tags.append(x)`: the item is read out to change and put
/// back, so it owns what it holds, and the dict it goes back into is the copy's own. Taken out as a
/// list's item is, with no owner of its own and the dict still shared, putting it back let go of
/// the list the other dict still held.
#[test]
fn a_dict_items_field_changed_through_a_copy_leaves_the_other_dict_be() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("dict_item_through_copy", DICT_ITEM_THROUGH_COPY, how)?;
        assert_eq!(
            (out.as_str(), code),
            (
                "400 1050\nUnit(hp=1, tags=[1, 2, 3]) Unit(hp=2, tags=[4, 5, 6])\nUnit(hp=1, tags=[1, 2, 3, 7]) Unit(hp=20, tags=[4, 5, 6]) Unit(hp=1, tags=[1, 2, 3]) Unit(hp=2, tags=[4, 5, 6])\n",
                Some(0)
            ),
            "{how:?}: {err}"
        );
    }
    Ok(())
}

const DICT_ITEM_THROUGH_COPY: &str = r#"struct Unit:
    hp: int
    tags: list[int]

    def grown_by(ref self, values: list[int]):
        for value in values:
            self.tags.append(value)

def appended(given: dict[str, Unit], at: int) -> int:
    mut units = given
    units["a"].tags.append(at)
    units["a"].tags.len()

def doubled(given: dict[str, Unit], at: int) -> int:
    mut units = given
    key = "a" if at % 2 == 0 else "b"
    units[key].tags.append(at)
    own = units[key].tags
    units[key].grown_by(own)
    units[key].hp += 1
    units[key].tags.len() + units[key].hp

def main(stdout: Stdout):
    given = {"a": Unit(1, [1, 2, 3]), "b": Unit(2, [4, 5, 6])}
    stdout.print(sum(appended(given, x) for x in range(100)), sum(doubled(given, x) for x in range(100)))
    stdout.print(given["a"], given["b"])
    mut mine = given
    mine["a"].tags.append(7)
    mine["b"].hp = 20
    stdout.print(mine["a"], mine["b"], given["a"], given["b"])
"#;

/// The same through a copy of something holding the dict or list: a struct's dict field, a dict of
/// dicts, a list of dicts and a dict of lists. A list's item holding a list is made alone, as a list
/// item is, before it's taken out; it wasn't, which let go of the other list's.
#[test]
fn nested_items_changed_through_a_copy_leave_the_original_be() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let expected = "60 30\n20 50\nUnit(hp=1, tags=[1, 2, 3]) Unit(hp=2, tags=[4, 5]) Unit(hp=3, tags=[6]) Unit(hp=4, tags=[7, 8, 9, 10])\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("nested_through_copy", NESTED_THROUGH_COPY, how)?;
        assert_eq!((out.as_str(), code), (expected, Some(0)), "{how:?}: {err}");
    }
    Ok(())
}

const NESTED_THROUGH_COPY: &str = r#"struct Unit:
    hp: int
    tags: list[int]

struct Box:
    units: dict[str, Unit]
    n: int

def in_field(given: Box, at: int) -> int:
    mut b = given
    b.units["a"].tags.append(at)
    b.units["a"].hp += 1
    b.units["a"].tags.len() + b.units["a"].hp

def in_dicts(given: dict[str, dict[str, Unit]], at: int) -> int:
    mut dd = given
    dd["x"]["a"].tags.append(at)
    dd["x"]["a"].tags.len()

def in_list_of_dicts(given: list[dict[str, Unit]], at: int) -> int:
    mut xs = given
    xs[0]["a"].tags.append(at)
    xs[0]["a"].tags.len()

def in_dict_of_lists(given: dict[str, list[Unit]], at: int) -> int:
    mut d = given
    d["a"][0].tags.append(at)
    d["a"][0].tags.len()

def main(stdout: Stdout):
    boxed = Box(units={"a": Unit(1, [1, 2, 3])}, n=0)
    dicts = {"x": {"a": Unit(2, [4, 5])}}
    lists_of = [{"a": Unit(3, [6])}]
    of_lists = {"a": [Unit(4, [7, 8, 9, 10])]}
    stdout.print(sum(in_field(boxed, x) for x in range(10)), sum(in_dicts(dicts, x) for x in range(10)))
    stdout.print(sum(in_list_of_dicts(lists_of, x) for x in range(10)), sum(in_dict_of_lists(of_lists, x) for x in range(10)))
    stdout.print(boxed.units["a"], dicts["x"]["a"], lists_of[0]["a"], of_lists["a"][0])
"#;

/// A dict that has changed to its strong hash still finds every kind of key that compiled code looks
/// up itself, and keeps working when it's copied and when keys come out. If compiled code and the
/// runtime hashed a key differently for it, these lookups would miss.
#[test]
fn a_dict_that_changed_its_hash_finds_every_kind_of_key() -> std::io::Result<()> {
    const COUNT: usize = 1100;
    if !ready() {
        return Ok(());
    }
    let short = colliding(COUNT, |text: &String| fast_text_hash(text), |at| format!("{at:08x}"));
    let long = colliding(
        COUNT,
        |text: &String| fast_text_hash(text),
        |at| format!("a key too long to be held inline {at:08x}"),
    );
    let pairs = colliding(COUNT, |b: &u64| fast_hash(2, &[1, *b]), |at| at);
    let mut input = String::new();
    for key in short.iter().chain(&long) {
        input.push('T');
        input.push_str(key);
        input.push('\n');
    }
    for b in &pairs {
        input.push('P');
        input.push_str(&b.to_string());
        input.push('\n');
    }
    let dir = folder("strong_dict")?;
    let program = dir.join("strong.fen");
    std::fs::write(
        &program,
        r#"struct Pair:
    first: int
    second: int

def main(stdout: Stdout, stdin: Stdin):
    lines = stdin.lines()
    mut texts: dict[str, int] = {}
    mut pairs: dict[Pair, int] = {}
    for line in lines:
        if line.startswith("T"):
            key = line[1:]
            texts[key] = key.len()
        else:
            pairs[Pair(first=1, second=int(line[1:]).expect("a number"))] = 1
    mut whole = 0
    mut sliced = 0
    mut paired = 0
    for line in lines:
        if line.startswith("T"):
            key = line[1:]
            whole += texts[key]
            sliced += texts.get(line[1:]) else 0
        else:
            paired += pairs[Pair(first=1, second=int(line[1:]).expect("a number"))]
    missing = "not one of them" in texts
    mut copied = texts
    copied["added to the copy"] = 1
    copied["and another"] = 2
    _ = copied.pop("added to the copy") else 0
    stdout.print(texts.len(), copied.len(), whole, sliced, paired, missing, pairs.len())
"#,
    )?;
    let run = crate::executables::fennec_command()
        .arg("run")
        .arg(&program)
        .env("FENNEC_HASH_SEED", "0")
        .output_given(input.as_bytes(), std::time::Duration::from_secs(50))?;
    assert_eq!(String::from_utf8_lossy(&run.stderr), "");
    let total: usize = short.iter().chain(&long).map(String::len).sum();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        format!("{} {} {total} {total} {COUNT} False {COUNT}\n", 2 * COUNT, 2 * COUNT + 1)
    );
    Ok(())
}

/// The seed changes nothing a program can see: a dict and a set go in the order keys were added.
#[test]
fn a_dicts_order_is_the_same_for_every_seed() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("seeds")?;
    let program = dir.join("seeds.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):
    mut counts: dict[str, int] = {}
    mut seen: set[int] = set()
    for number in range(200):
        counts[str(number * 7919 % 1009)] = number
        seen.add(number * 104729 % 7907)
    stdout.print(counts)
    stdout.print(seen)
",
    )?;
    let with_seed = |seed: Option<&str>| {
        let mut command = crate::executables::fennec_command();
        command.arg("run").arg(&program);
        if let Some(seed) = seed {
            command.env("FENNEC_HASH_SEED", seed);
        }
        command
            .output_within()
            .map(|run| (String::from_utf8_lossy(&run.stdout).into_owned(), run.status.success()))
    };
    let first = with_seed(Some("1"))?;
    assert!(first.1 && first.0.len() > 100, "{first:?}");
    assert_eq!(first, with_seed(Some("987654321"))?);
    assert_eq!(first, with_seed(None)?);
    Ok(())
}

/// Sets and dicts of sized numbers, which are held as ints: a `set[u8]`, a `dict[str, u8]` whose
/// value is added to in place, and a `dict[i16, i32]`.
#[test]
fn sets_and_dicts_of_sized_numbers() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "sized_sets_dicts",
        r#"def main(stdout: Stdout):
    mut small = set[u8]()
    zero: u8 = 0
    for n in [1, 2, 255, 2]:
        small.add(u8.from(n) else zero)
    two: u8 = 2
    three: u8 = 3
    stdout.print(sorted(list(small)), two in small, three in small, small.len())
    mut counts = dict[str, u8]()
    a: u8 = 1
    big: u8 = 200
    counts["a"] = a
    counts["b"] = big
    counts["b"] += 50
    stdout.print(counts, counts["b"], "z" in counts)
    mut by = dict[i16, i32]()
    low: i16 = -300
    wide: i32 = 70000
    by[low] = wide
    stdout.print(by[low], by)
"#,
        "[1, 2, 255] True False 3\n{\"a\": 1, \"b\": 250} 250 False\n70000 {-300: 70000}\n",
    )
}

/// Sets and dicts of `u64`s and `f32`s, keys and values, which have words of their own: a `u64` past
/// `int`'s top is shown unsigned, and an `f32` as the shortest text that reads back as it, `0.1`
/// rather than a double's `0.10000000149011612`.
#[test]
fn sets_and_dicts_of_u64s_and_f32s() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "u64_f32_sets_dicts",
        r#"def main(stdout: Stdout):
    one: u64 = 1
    five: u64 = 5
    big: u64 = (one << 63) + five
    mut seen = set[u64]()
    seen.add(big)
    seen.add(one)
    seen.add(big)
    stdout.print(seen.len(), big in seen, seen)
    mut names = dict[u64, str]()
    names[big] = "top"
    names[one] = "one"
    stdout.print(names[big], names)
    tenth: f32 = 0.1
    half: f32 = 0.5
    mut weights = dict[str, f32]()
    weights["a"] = tenth
    weights["b"] = half
    stdout.print(weights, weights["a"])
    mut fs = set[f32]()
    fs.add(tenth)
    fs.add(tenth)
    stdout.print(fs, tenth in fs, half in fs)
    mut counts = dict[f32, u64]()
    counts[tenth] = big
    stdout.print(counts)
"#,
        "2 True {9223372036854775813, 1}\ntop {9223372036854775813: \"top\", 1: \"one\"}\n{\"a\": 0.1, \"b\": 0.5} 0.1\n{0.1} True False\n{0.1: 9223372036854775813}\n",
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
/// What a generator yields is kept across its stops even when nothing else reads it: a name a
/// generator expression captures, yielded as it is, and a parameter a generator yields. Natively
/// they came out empty once, as the stops saved only what the rest of the body read.
#[test]
fn a_generator_yielding_a_name_it_was_given_keeps_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "yields_a_name",
        r#"def joined(parts: Iterator[str]) -> str:
    mut whole = ""
    for part in parts:
        whole = whole + part
    whole

def repeat(word: str, times: int) -> Iterator[str]:
    for _ in range(times):
        yield word

def main(stdout: Stdout):
    small = "ab"
    big = "a string long enough to live in a block"
    n = 7
    stdout.print(joined(small for _ in range(3)), joined(big for _ in range(2)).len())
    stdout.print(joined(repeat("kit", times=2)), sum(n for _ in range(4)), list(n for _ in range(2)))
"#,
    )
}

/// A generator copied into the loop over it holds each argument as its parameter's type: a `str`
/// given for a `str?` is an optional in the copy. The copy once held it as the argument's type,
/// so the build left `main` to the JIT, as it read `str` as `str?`.
#[test]
fn a_copied_generator_holds_its_arguments_as_its_parameters_types() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "copied_parameter_types",
        r#"def or_else(word: str?) -> Iterator[str]:
    yield word else "nothing"

def when_given(word: str?) -> Iterator[str]:
    if word is not None:
        yield f"{word}!"

def main(stdout: Stdout):
    for word in or_else("hi"):
        stdout.print(word)
    for word in or_else(None):
        stdout.print(word)
    maybe: str? = "kept"
    for word in when_given(maybe):
        stdout.print(word)
    held = or_else("later")
    stdout.print(list(held), list(when_given("x")))
"#,
    )
}

/// What closures and generators capture, of each kind, comes out as the interpreter gives it:
/// a closure giving back what it captured as it is, generator expressions yielding a captured
/// string, struct, list, bool, int or float, nested generators and generators with `while`,
/// comprehensions inside generators and the other way round, and a closure inside a generator.
#[test]
fn captures_of_every_kind_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "captures",
        r#"struct Pair:
    name: str
    count: int

def joined(parts: Iterator[str]) -> str:
    mut whole = ""
    for part in parts:
        whole = whole + part
    whole

def apply(change: (int) -> str, x: int) -> str:
    change(x)

def twice(word: str) -> Iterator[str]:
    yield word
    yield word

def outer(word: str, times: int) -> Iterator[str]:
    for w in twice(word):
        for _ in range(times):
            yield w

def counting(word: str, limit: int) -> Iterator[str]:
    mut n = 0
    while n < limit:
        yield word
        n += 1

def main(stdout: Stdout):
    small = "ab"
    big = "a string long enough to live in a block"
    n = 7
    pair = Pair(name="kit", count=2)
    items = [1, 2]
    flag = True
    # closures returning a captured value as it is
    stdout.print(apply((x) => small, 1), apply((x) => big, 1).len(), apply((x) => str(n), 1))
    stdout.print(apply((x) => pair.name, 0), apply((x) => f"{items}", 0))
    # generator expressions yielding captures as they are, of each kind
    stdout.print(list(pair for _ in range(2)), list(items for _ in range(2)), list(flag for _ in range(2)))
    stdout.print(list(n for _ in range(2)), list(1.5 for _ in range(2)), joined(pair.name for _ in range(2)))
    # nested generators and generator functions
    stdout.print(joined(outer("xy", times=2)), joined(counting(small, limit=3)))
    stdout.print(joined(w for w in twice(small)), joined(small for w in twice("q")))
    # comprehensions inside generators, and generators inside comprehensions
    stdout.print(list([small, w] for w in twice("z")), [joined(small for _ in range(k)) for k in range(3)])
    stdout.print(joined(s for s in [small, big] if s.len() < 5), sum(n for _ in range(3)))
    # a closure inside a generator, and a generator inside a closure
    stdout.print(joined(apply((x) => small, 0) for _ in range(2)), apply((x) => joined(small for _ in range(x)), 2))
"#,
    )
}

/// A generator made in one place and read in another keeps what it captured: one returned from a
/// function, one held before it's read, and one made each time round a loop.
#[test]
fn generators_kept_for_later_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "kept_generators",
        r#"def joined(parts: Iterator[str]) -> str:
    mut whole = ""
    for part in parts:
        whole = whole + part
    whole

def words_of(word: str, times: int) -> Iterator[str]:
    (word for _ in range(times))

def later(word: str) -> Iterator[str]:
    made = word + "!"
    (made for _ in range(2))

def main(stdout: Stdout):
    small = "ab"
    held = words_of(small, times=3)
    stdout.print("before")
    stdout.print(joined(held), joined(later("hey")))
    mut gens: list[str] = []
    for word in ["p", "qq"]:
        gens.append(joined(word for _ in range(2)))
    stdout.print(gens)
    mut total = 0
    for k in range(3):
        total += sum(k for _ in range(2))
    stdout.print(total)
"#,
    )
}

/// Closures kept where the runtime reads them, as a list's items and in a generator's block, come
/// out as the interpreter gives them: appended, read in a loop and by index, shared by a copy of
/// the list, popped, replaced, and captured by a generator that calls one. Natively a list of
/// closures and a generator keeping one were left to the JIT, as the runtime couldn't hold them.
#[test]
fn closures_kept_in_lists_and_generators_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "closures_kept",
        r#"def add(x: int) -> int:
    x + 1

def mul(x: int) -> int:
    x * 2

def made(k: int) -> (int) -> int:
    def plus(x: int) -> int:
        x + k
    plus

def counted(word: str) -> Iterator[int]:
    def size(x: int) -> int:
        x + word.len()
    for n in range(3):
        yield size(n)

def apply_each(change: (int) -> int, limit: int) -> Iterator[int]:
    for n in range(limit):
        yield change(n)

def main(stdout: Stdout):
    mut fs: list[(int) -> int] = []
    fs.append(add)
    fs.append(mul)
    name = "a string long enough to live in a block"
    def named(x: int) -> int:
        x + name.len()
    fs.append(named)
    for k in range(3):
        fs.append(made(k))
    for f in fs:
        stdout.print(f(3))
    stdout.print(fs[2](0), fs.len())
    copied = fs
    fs.append(add)
    stdout.print(copied.len(), fs.len())
    last = fs.pop()
    if last is not None:
        stdout.print(last(10))
    fs[0] = mul
    stdout.print(fs[0](5), copied[0](5))
    stdout.print(list(counted("xy")))
    held = apply_each(named, limit=3)
    stdout.print("made")
    stdout.print(list(held))
"#,
    )
}

/// A function named in a tuple, as in `(add, 4)`, is held as its closure: unpacked, kept in a
/// name and unpacked later, and called. It once wasn't lowered, as "it gives (int) -> int where
/// function goes".
#[test]
fn functions_named_in_tuples_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "functions_in_tuples",
        "def add(x: int) -> int:
    x + 1

def twice(x: int) -> int:
    x * 2

def main(stdout: Stdout):
    (g, n) = (add, 4)
    stdout.print(g(n))
    pair = (twice, 5)
    (h, m) = pair
    stdout.print(h(m), g(h(1)))
",
    )
}

/// A generator's failure passes up however the generator was reached: from a name, and from inside
/// `with` blocks, where the loop is over a name too, as well as from the loop that calls it.
#[test]
fn a_generators_failure_passes_up_from_a_name_or_a_with() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def checked(limit: int) -> Iterator[int] fails IoError:
    for at in range(limit):
        if at == 2: fail IoError(reason=\"stop\")
        yield at

def source() -> with int:
    lend 1 else:
        pass

def in_a_name(limit: int) -> int fails IoError:
    mut seen = 0
    made = checked(limit)?
    for at in made:
        seen += at
    seen

def in_with(limit: int) -> int fails IoError:
    mut seen = 0
    with source() as held:
        for at in checked(limit)?:
            seen += at + held
    seen

def nested(limit: int) -> int fails IoError:
    mut seen = 0
    with source() as outer:
        with source() as inner:
            for at in checked(limit)?:
                seen += at + outer + inner
    seen

def listed(limit: int) -> list[int] fails IoError:
    [at * 2 for at in checked(limit)?]

## What `run` gave, or what it failed with.
def said[T](run: () -> T fails IoError) -> str:
    value = run() else as why:
        return f\"{why}\"
    f\"{value}\"

def main(stdout: Stdout):
    stdout.print(said(() => in_a_name(5)?), said(() => in_a_name(2)?))
    stdout.print(said(() => in_with(5)?), said(() => in_with(2)?))
    stdout.print(said(() => nested(5)?), said(() => nested(2)?))
    stdout.print(said(() => listed(5)?), said(() => listed(2)?))
";
    let stop = "IoError(reason=\"stop\")";
    let wanted = format!("{stop} 1\n{stop} 3\n{stop} 5\n{stop} [0, 2]\n");
    let dir = folder("generator_failure")?;
    let program = dir.join("failing.fen");
    std::fs::write(&program, source)?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("failing"));
    let output = output.to_string_lossy().into_owned();
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&output).output_within()?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            wanted,
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

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

def checked(limit: int) -> Iterator[int] fails TooBig:
    for n in range(5):
        if n >= limit: fail TooBig(at=n)
        yield n

def named(limit: int) -> Iterator[str] fails Named:
    for n in range(5):
        if n >= limit: fail Named(who=f"a long name for number {n} in a block")
        yield str(n)

def either(limit: int) -> Iterator[int] fails Named | TooBig:
    if limit == 0: fail Named(who="none")
    for n in range(limit):
        if n == 2: fail TooBig(at=n)
        yield n

def total(limit: int) -> int fails TooBig:
    sum(n for n in checked(limit)?)

def joined(limit: int) -> str fails Named:
    mut out = ""
    for word in named(limit)?:
        out = out + word
    out

def looped(limit: int) -> int fails Named | TooBig:
    mut count = 0
    for n in either(limit)?:
        count = count + n + 1
    count

## What `run` gave, or what it failed with.
def said[T, E](run: () -> T fails E) -> str:
    value = run() else as why:
        return f"{why}"
    f"{value}"

def total_or_why(limit: int, stdout: Stdout):
    found = total(limit) else as why:
        stdout.print("too big", why.at)
        return
    stdout.print(found)

def joined_or_who(limit: int, stdout: Stdout):
    text = joined(limit) else as why:
        stdout.print(why.who)
        return
    stdout.print(text)

def main(stdout: Stdout):
    stdout.print(said(() => total(9)?), said(() => total(2)?))
    total_or_why(2, stdout)
    stdout.print(said(() => joined(3)?), said(() => joined(9)?))
    joined_or_who(1, stdout)
    for limit in [0, 1, 2, 3]:
        stdout.print(limit, said(() => looped(limit)?))
"#,
    )
}

/// A generator made for a loop goes when the loop does, letting go of what it's holding right
/// then, while one with a name lasts as long as the name, and returning out of the loop lets go
/// of it before the caller goes on.
#[test]
fn a_generator_left_partway_closes_as_the_interpreter_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter(
        "generator_left",
        r#"struct Door:
    out: Stdout
    name: str

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

/// A generator abandoned while a `with` source's block is open runs the source's cleanup then, as
/// a `break` out of the block would, and one run to its end runs it as it goes past.
#[test]
fn an_abandoned_generator_runs_its_open_sources_cleanup() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "abandoned_source",
        "struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door else:
        stdout.print(\"failed\", name)
    stdout.print(\"closed\", name)

def held(name: str, stdout: Stdout) -> Iterator[int]:
    with opened(name, stdout) as door:
        yield 1
        yield 2

def main(stdout: Stdout):
    for n in held(\"left early\", stdout):
        stdout.print(\"got\", n)
        break
    stdout.print(\"after the loop\")
    stdout.print(sum(n for n in held(\"run out\", stdout)))
",
    )?;
    assert_eq!(printed, "got 1\nclosed left early\nafter the loop\nclosed run out\n3\n");
    Ok(())
}

/// A generator that can fail, held in a name and let go partway in a launched task, closes its
/// open source as it does in `main`. What closes a generator is said once, on the thread `main`
/// runs on, and the task's worker started its runtime before that, so it once let the generator go
/// with its source left open. What's printed is compared line by line, sorted, as the task's lines
/// and `main`'s may come in either order.
#[test]
fn a_generator_let_go_in_a_task_closes_what_it_has_open() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("generator_closed_in_a_task")?;
    let program = dir.join("closed.fen");
    std::fs::write(&program, include_str!("fixtures/workers/generator_closed_in_a_task.fen"))?;
    let executable = dir.join("closed");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    let printed = String::from_utf8_lossy(&ran.stdout).into_owned();
    let mut lines: Vec<&str> = printed.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "after looped in a task",
            "after named in a task",
            "after named in main",
            "closing looped in a task",
            "closing named in a task",
            "closing named in main",
            "main goes on",
        ],
        "{printed}"
    );
    Ok(())
}

/// Each source a generator holds open where it's let go closes then, innermost first and once:
/// not its `else:`, which is for a block that fails, and nothing for a block that already ended
/// or a generator that never started. A `return` out of the loop lets go of it too.
#[test]
fn an_abandoned_generator_closes_each_open_source_once_innermost_first() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("abandoned_sources", ABANDONED_SOURCES)?;
    assert_eq!(
        printed,
        "got 0\nclosed inner 0\ngot 1\nclosed inner 1\nclosed outer\n--\ngot 1\nclosed short\ngot 2\n--\n\
         closed inner 0\nclosed inner 1\nclosed outer\nkept 1\n--\nnever started\n"
    );
    Ok(())
}

const ABANDONED_SOURCES: &str = r#"struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door else:
        stdout.print("failed", name)
    stdout.print("closed", name)

def nested(stdout: Stdout) -> Iterator[int]:
    with opened("outer", stdout) as outer:
        for n in range(3):
            with opened(f"inner {n}", stdout) as inner:
                yield n
    yield 99

def after_the_block(stdout: Stdout) -> Iterator[int]:
    with opened("short", stdout) as door:
        yield 1
    yield 2

def kept(stdout: Stdout) -> int:
    walk = nested(stdout)
    for n in walk:
        if n == 1:
            return n
    0

def main(stdout: Stdout):
    for n in nested(stdout):
        stdout.print("got", n)
        if n == 1:
            break
    stdout.print("--")
    for n in after_the_block(stdout):
        stdout.print("got", n)
        if n == 2:
            break
    stdout.print("--")
    stdout.print("kept", kept(stdout))
    stdout.print("--")
    never = nested(stdout)
    stdout.print("never started")
"#;

/// A source's cleanup that fails while a generator for a loop is let go fails the loop, as a
/// failure while it gives an item does: at a `break` and at a `return` out of the loop. A failure
/// already leaving through it is the one that leaves, an error with cases here, carrying the
/// cleanup's as a note.
#[test]
fn a_cleanup_failing_as_a_generator_is_let_go_fails_the_loop() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("cleanup_failing_let_go", CLEANUP_FAILING)?;
    assert_eq!(
        printed,
        "closing broke\nbroke failed Shut.Stuck(name=\"broke\")\nclosing returned\n\
         returned failed Shut.Stuck(name=\"returned\")\nclosing failing\n\
         failing failed Other.Inside(why=\"in the loop\") [\"Shut.Stuck(name=\\\"failing\\\")\"]\n"
    );
    Ok(())
}

/// A failure already leaving through a loop whose generator's cleanup fails too is the one that
/// leaves, carrying the cleanup's as a note, as a `with` block's failing `else:` does.
#[test]
fn a_cleanup_failing_as_a_failure_leaves_a_loop_is_a_note_on_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("cleanup_failing_noted", CLEANUP_FAILING_NOTED)?;
    assert_eq!(
        printed,
        "closing broke\nbroke failed Shut(name=\"broke\")\nclosing returned\nreturned failed Shut(name=\"returned\")\n\
         closing failing\nfailing failed Other(why=\"in the loop\") [\"Shut(name=\\\"failing\\\")\"]\n"
    );
    Ok(())
}

const CLEANUP_FAILING_NOTED: &str = r#"error Shut:
    name: str

error Other:
    why: str

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail Shut(name=name)

def held(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with opened(name, stdout)? as door:
        yield 1
        yield 2

def broke(stdout: Stdout) fails Shut:
    for n in held("broke", stdout)?:
        break
    stdout.print("not here")

def returned(stdout: Stdout) -> int fails Shut:
    for n in held("returned", stdout)?:
        return n
    0

def failing(stdout: Stdout) fails Shut | Other:
    for n in held("failing", stdout)?:
        fail Other(why="in the loop")

def tell_broke(stdout: Stdout):
    broke(stdout) else as why:
        stdout.print("broke failed", why)
        return
    stdout.print("broke worked")

def tell_returned(stdout: Stdout):
    n = returned(stdout) else as why:
        stdout.print("returned failed", why)
        return
    stdout.print("returned", n)

def main(stdout: Stdout):
    tell_broke(stdout)
    tell_returned(stdout)
    failing(stdout) else as why:
        stdout.print("failing failed", why, why.notes())
        return
    stdout.print("failing worked")
"#;

const CLEANUP_FAILING: &str = r#"error Shut:
    Stuck(name: str)

error Other:
    Inside(why: str)

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail .Stuck(name=name)

def held(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with opened(name, stdout)? as door:
        yield 1
        yield 2

def broke(stdout: Stdout) fails Shut:
    for n in held("broke", stdout)?:
        break
    stdout.print("not here")

def returned(stdout: Stdout) -> int fails Shut:
    for n in held("returned", stdout)?:
        return n
    0

def failing(stdout: Stdout) fails Shut | Other:
    for n in held("failing", stdout)?:
        fail Other.Inside(why="in the loop")

def tell_broke(stdout: Stdout):
    broke(stdout) else as why:
        stdout.print("broke failed", why)
        return
    stdout.print("broke worked")

def tell_returned(stdout: Stdout):
    n = returned(stdout) else as why:
        stdout.print("returned failed", why)
        return
    stdout.print("returned", n)

def main(stdout: Stdout):
    tell_broke(stdout)
    tell_returned(stdout)
    failing(stdout) else as why:
        stdout.print("failing failed", why, why.notes())
        return
    stdout.print("failing worked")
"#;

/// A generator held in a name, let go by its last owner, has nothing there to take its cleanup's
/// failure: the task stops with it, saying so, as for a crash, where the loop over a call it's
/// made for would fail.
#[test]
fn a_cleanup_failing_as_a_named_generator_is_let_go_stops_the_task() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("named_generator_failing")?;
    let program = dir.join("named.fen");
    std::fs::write(&program, NAMED_GENERATOR_FAILING)?;
    let executable = dir.join("named");
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (out, err, code) = outcome(&Command::new(&executable).output_within()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(code, Some(1), "{err}");
    assert_eq!(out, "after quiet\nclosing quiet\n");
    assert!(
        err.contains(
            "a generator's cleanup failed as it was let go by its last owner: Shut.Stuck(name=\"quiet\")"
        ),
        "{err}"
    );
    assert!(err.contains("help: loop over the call itself"), "{err}");
    Ok(())
}

const NAMED_GENERATOR_FAILING: &str = r#"error Shut:
    Stuck(name: str)

error Other:
    Inside(why: str)

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door fails Shut:
    door = Door(name=name)
    lend door
    stdout.print("closing", name)
    fail .Stuck(name=name)

def held(name: str, stdout: Stdout) -> Iterator[int] fails Shut:
    with opened(name, stdout)? as door:
        yield 1
        yield 2


def quiet(stdout: Stdout):
    walk = held("quiet", stdout) else as why:
        stdout.print("opening failed", why)
        stdout.print("after quiet")
        return
    for n in walk:
        break
    stdout.print("after quiet")

def kept(stdout: Stdout) fails Shut:
    walk = held("kept", stdout)?
    for n in walk:
        break
    stdout.print("after kept")

def main(stdout: Stdout):
    quiet(stdout)
    kept(stdout) else as why:
        stdout.print("kept failed", why)
        return
    stdout.print("kept worked")
"#;

/// A generator that can fail, taken out of its call by `?` for a loop, goes when the loop does,
/// as one that can't fail does, closing what it holds then and not when the function ends.
#[test]
fn a_failing_generator_for_a_loop_closes_when_the_loop_ends() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "failing_generator_closed",
        r#"error Boom:
    pass

struct Door:
    name: str

def opened(name: str, stdout: Stdout) -> with Door:
    door = Door(name=name)
    lend door
    stdout.print("closed", name)

def plain(stdout: Stdout) -> Iterator[int]:
    with opened("plain", stdout) as door:
        yield 1
        yield 2

def failing(stdout: Stdout, bad: bool) -> Iterator[int] fails Boom:
    if bad:
        fail Boom()
    with opened("failing", stdout) as door:
        yield 1
        yield 2

def main(stdout: Stdout) fails Boom:
    for n in plain(stdout):
        break
    stdout.print("after plain")
    for n in failing(stdout, bad=False)?:
        break
    stdout.print("after failing")
"#,
    )?;
    assert_eq!(printed, "closed plain\nafter plain\nclosed failing\nafter failing\n");
    Ok(())
}

/// A generator over a file, let go by `break`, closes the file: one written takes its path's
/// place right then, and 200 read and let go stay under a limit of 64 open files.
#[cfg(unix)]
#[test]
fn a_generator_over_a_file_let_go_by_break_closes_the_file() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("generator_files")?;
    let program = dir.join("files.fen");
    std::fs::write(
        &program,
        r#"def numbered(fs: Fs, path: str) -> Iterator[int] fails IoError:
    with fs.create(path)? as file:
        for n in range(10):
            file.write_line(str(n))?
            yield n

def first_lines(fs: Fs, path: str) -> Iterator[str] fails IoError:
    with fs.open(path)? as file:
        for line in file.lines()?:
            yield line

def main(stdout: Stdout, fs: Fs) fails IoError:
    for n in numbered(fs, "numbers.txt")?:
        if n == 2:
            break
    with fs.open("numbers.txt")? as file:
        stdout.print(file.read_text()?.split("\n"))
    # Each loop lets go of a generator holding a file open: 200 of them run out of file
    # handles under a limit of 64 if any stays open.
    mut read = 0
    for _ in range(200):
        for line in first_lines(fs, "numbers.txt")?:
            read += 1
            break
    stdout.print(read)
"#,
    )?;
    let executable = dir.join("files");
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new("/bin/sh")
        .args(["-c", "ulimit -n 64 && exec \"$0\"", &executable.to_string_lossy()])
        .current_dir(&dir)
        .output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "[\"0\", \"1\", \"2\", \"\"]\n200\n");
    Ok(())
}

/// `list`, `sorted`, `set`, `min` and `max` of a type with an `iter` of its own take what it
/// gives, as a `for` loop does: the program's own type, and the standard library's, which write
/// themselves with `list(self)`.
#[test]
fn what_takes_every_item_takes_a_types_own_iter_as_the_jit_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "own_iter_items",
        r"from std.collections import Deque, SortedSet

struct Countdown:
    start: int

    def iter(self) -> Iterator[int]:
        mut at = self.start
        while at > 0:
            yield at
            at -= 1

def main(stdout: Stdout):
    down = Countdown(start=4)
    stdout.print(list(down), sorted(down), set(down) == {1, 2, 3, 4}, min(down), max(down))
    stdout.print(list(Countdown(start=0)), str(Deque.of([1, 2, 3])), str(SortedSet.of([3, 1, 2])))
",
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

def half(n: int) -> int fails Fault:
    if n % 2 == 1: fail Fault(f"{n} is odd")
    n // 2

def show(n: int) -> str:
    got = half(n) else as why:
        match why:
            case Fault(reason):
                return reason
    f"half is {got}"

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
    // The whole message, as it points at the generator and says what to do.
    same_as_the_jit(
        "twice",
        "def count_up(limit: int) -> Iterator[int]:\n    mut at = 0\n    while at < limit:\n        yield at\n        at += 1\n\ndef total(numbers: Iterator[int]) -> int:\n    mut sum = 0\n    for n in numbers:\n        sum += n\n    sum\n\ndef main(stdout: Stdout):\n    numbers = count_up(3)\n    for _ in range(2):\n        stdout.print(total(numbers))\n",
    )?;
    same_message_as_the_jit(
        "expect",
        "error Fault:\n    reason: str\n\ndef half(n: int) -> int fails Fault:\n    if n % 2 == 1: fail Fault(\"odd\")\n    n // 2\n\ndef main(stdout: Stdout):\n    stdout.print(half(4).expect(\"four is even\"))\n    stdout.print(half(3).expect(\"three is even\"))\n",
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

/// A task that crashes partway through filling a list lets go of what it made so far: a
/// comprehension whose length is kept aside while it runs, and the keys `sorted(key=)` makes
/// before a key crashes. What a task leaves comes back to the code that launched it, so a leak
/// shows in the blocks still held at the end, which would grow with the rounds.
#[test]
fn a_task_crashing_partway_through_a_list_leaves_nothing_behind() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let live = |rounds: u32| -> std::io::Result<Option<u64>> {
        let source = CRASHING_PARTWAY.replace("rounds = 3", &format!("rounds = {rounds}"));
        let counts = counts_of(&format!("crashing_partway_{rounds}"), &source)?;
        Ok(count(&counts, "live"))
    };
    let (few, many) = (live(3)?, live(30)?);
    assert!(few.is_some(), "the run says how many blocks it still held");
    assert_eq!(few, many, "blocks still held after 3 crashed rounds and after 30");
    Ok(())
}

/// A failure partway through a line lets go of what the line made so far and of what the
/// function holds: a failure lets go of a value held only briefly where it leaves, and `prop`
/// of the rest. Checking proofs, each way out checks that what it leaves is empty, as worked
/// out before writing; and a value left behind would show in the blocks still held at the end,
/// which would grow with the rounds.
#[test]
fn a_failure_partway_through_a_line_leaves_nothing_behind() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let live = |rounds: u32| -> std::io::Result<Option<u64>> {
        let source = failing_partway().replace("rounds = 3", &format!("rounds = {rounds}"));
        let counts = counts_of(&format!("failing_partway_{rounds}"), &source)?;
        Ok(count(&counts, "live"))
    };
    let (few, many) = (live(3)?, live(30)?);
    assert!(few.is_some(), "the run says how many blocks it still held");
    assert_eq!(few, many, "blocks still held after 3 failed rounds and after 30");
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("failing_partway_checked")?;
        let (program, executable) = (dir.join("checked.fen"), dir.join("checked"));
        std::fs::write(&program, failing_partway())?;
        let built = crate::executables::fennec_command()
            .arg("build")
            .args(how)
            .arg(&program)
            .arg("-o")
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .env("FENNEC_CHECK_PROOFS", "1")
            .output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "2 3\n",
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

const FAILING_PARTWAY: &str = r#"error Refused:
    at: int

def checked(items: list[str], at: int) -> str fails Refused:
    if at >= items.len(): fail Refused(at=at)
    items[at]

def round(at: int) -> int fails Refused:
    mut held = [f"held through the round {at}, long enough to need a block of its own"]
    held.append(f"and {at}, long enough to need a block of its own")
    first = checked(held, at % 3)?
    both = checked([f"left {at}, long enough to need a block of its own"], 0)? + checked([f"right {at}, long enough to need a block of its own"], at % 2)?
    first.len() + both.len() + held.len()

def main(stdout: Stdout):
    rounds = 3
    mut failed = 0
    mut failed_many = 0
    for at in range(rounds):
        got = round(at) else -1
        if got < 0:
            failed += 1
        made = many(at) else -1
        if made < 0:
            failed_many += 1
    stdout.print(failed, failed_many)
"#;

/// [`FAILING_PARTWAY`] with `many`, a function of 50 lines that each make a list and a string
/// held only for the line, enough short-lived values for each failure to let go of its own where
/// it leaves. Round `at` fails at line `49 - at`.
fn failing_partway() -> String {
    let lines = (0..50).fold(String::new(), |mut lines, line| {
        let _ = writeln!(
            lines,
            "    total += checked([f\"line {line} of {{at}}, long enough to need a block of its own\"], (at + {line}) % 50 // 49)?.len()"
        );
        lines
    });
    format!(
        "{FAILING_PARTWAY}\ndef many(at: int) -> int fails Refused:\n    mut total = 0\n{lines}    total\n"
    )
}

const CRASHING_PARTWAY: &str = r#"from std.concurrent import Crashed

def kept_then_crash(at: int):
    out = [f"item {i} of {100 // (10 - i)}, long enough to need a block of its own" for i in range(20)]
    print_nothing(out)

def keys_then_crash(at: int):
    words = [f"word {i}, long enough to need a block of its own" for i in range(20)]
    print_nothing(sorted(words, key=(word) => f"{word} {100 // (word.len() - 46)}"))

def print_nothing(words: list[str]):
    if words.len() == 12345:
        pass

def crashing(which: int) -> int fails Crashed:
    if which == 0:
        with launch(() => kept_then_crash(which))?:
            pass
    else:
        with launch(() => keys_then_crash(which))?:
            pass
    0

def main(stdout: Stdout):
    rounds = 3
    mut crashed = 0
    for _ in range(rounds):
        crashed += crashing(0) else 1
        crashed += crashing(1) else 1
    stdout.print(crashed)
"#;

/// What a built program says it copied and allocated, and how many blocks it still held at the
/// end, as `FENNEC_COUNTS` has it report.
fn counts_of(name: &str, source: &str) -> std::io::Result<String> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
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

/// A loop over a variable's field borrows the list while nothing changes the variable, so what
/// changes it next finds the list with one owner: a `ref` function changing the items, then a
/// plain loop reading them, round after round, copies nothing.
#[test]
fn a_loop_over_a_variables_field_leaves_it_alone_for_the_next_change() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "loop_over_field",
        "struct Enemy:\n    hp: int\n    x: float\n\nstruct World:\n    enemies = list[Enemy]()\n\ndef step(ref world: World, dt: float):\n    for ref enemy in world.enemies:\n        enemy.x += dt\n        enemy.hp -= 1\n\ndef main(stdout: Stdout):\n    mut world = World()\n    for n in range(1000):\n        world.enemies.append(Enemy(hp=n, x=0.0))\n    mut total = 0\n    for _ in range(100):\n        step(ref world, 0.5)\n        for enemy in world.enemies:\n            total += enemy.hp\n    stdout.print(total)\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// What a borrowed field can't outlast: the variable written while the loop runs, given away
/// before it's done, or the list grown from inside the loop. Each still says what it did.
#[test]
fn a_loop_over_a_variables_field_matches_the_interpreter_when_the_variable_changes() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "loop_over_changing_field",
        r#"struct Bag:
    items = list[int]()
    name = "bag"

def grow(ref bag: Bag):
    bag.items.append(bag.items.len())

def rename(ref bag: Bag, by: int):
    bag.name = f"{bag.name}-{by}"

def main(stdout: Stdout):
    mut bag = Bag()
    for i in range(5):
        bag.items.append(i)
    mut seen = []
    for item in bag.items:
        seen.append(item)
        if item == 2:
            bag = Bag(items=[7, 8], name="other")
    stdout.print(seen, bag.items, bag.name)
    mut more = Bag(items=[1, 2, 3])
    held = more.items
    mut other = more
    other = Bag(items=[9])
    junk = [40, 50, 60]
    for item in held:
        stdout.print(item, other.items.len(), junk[0])
    mut grown = Bag(items=[1, 2])
    for item in grown.items:
        stdout.print(item, grown.items)
    grow(ref grown)
    grown.items.append(30)
    stdout.print(grown.items)
    mut renamed = Bag(items=[4, 5])
    for item in renamed.items:
        renamed.name = f"{renamed.name}{item}"
        rename(ref renamed, item)
    renamed.items[0] = 100
    stdout.print(renamed.name, renamed.items)
"#,
    )
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

/// Pieces put on a string that's joined from a list, `out.append(...)` then `"".join(out)`, go
/// on with no block made for them: a number's text, as `str(n)` or in an f-string, is written as a
/// header, and a short string with characters past ASCII is counted where it is. Each made a block
/// that was copied from and let go of: 3,996 blocks here, and now only the joined string's growth.
#[test]
fn pieces_joined_into_a_string_make_no_blocks_of_their_own() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of("pieces_joined", PIECES_JOINED)?;
    assert!(count(&counts, "allocations") < Some(40), "{counts}");
    prints_in_both_builds("pieces_joined", PIECES_JOINED, "11518\n")?;
    prints_in_both_builds(
        "pieces_joined_edges",
        PIECES_JOINED_EDGES,
        "0<0>-1<-1>7<7>1234567890<1234567890>-9223372036854775808<-9223372036854775808>9223372036854775807<9223372036854775807>1.5[1.5]-0.0[-0.0]1e+300[1e+300]0.1[0.1]-2.25[-2.25]é(é)日本(日本)🦊(🦊)fóx12(fóx12)ab(ab)ça va très bien(ça va très bien)xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxé(xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxé)\n298 ) é ]\n2\n",
    )
}

/// Random strings of characters one to four bytes long, joined from a list in random pieces, against
/// the same pieces added with `+`: lengths and slices agree, where the pieces' characters are counted
/// as they go on, a string's bytes but those that go on a character. Three thousand cases.
#[test]
fn random_pieces_joined_count_their_characters_as_added_ones_do() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("pieces_counted", PIECES_COUNTED, "checked 2978 wrong 0\n")
}

const PIECES_COUNTED: &str = r#"## Random strings of characters one to four bytes long, put on a string joined from a list in
## random pieces, against the same pieces added with `+`: lengths and slices must agree.
struct Random:
    state: int

    def below(ref self, limit: int) -> int:
        self.state = (self.state * 1103515245 + 12345) % 2147483648
        (self.state // 65536) % limit

def piece(ref random: Random) -> str:
    letters = ["a", "Z", "é", "ñ", "日", "€", "🦊", "𝄞", " ", "0"]
    mut text = ""
    for _ in range(random.below(8)):
        text += letters[random.below(letters.len())]
    text

def main(stdout: Stdout):
    mut random = Random(state=7)
    mut wrong = 0
    mut checked = 0
    for _ in range(3000):
        mut out = []
        mut added = ""
        mut counted = 0
        for _ in range(random.below(12) + 1):
            part = piece(ref random)
            out.append(part)
            added = added + part
            counted += part.len()
        joined = "".join(out)
        if joined.len() != counted or joined.len() != added.len() or joined != added:
            wrong += 1
        if joined.len() > 0:
            start = random.below(joined.len())
            end = start + random.below(joined.len() - start + 1)
            if joined[start:end] != added[start:end] or joined[start] != added[start]:
                wrong += 1
            checked += 1
    stdout.print("checked", checked, "wrong", wrong)
"#;

const PIECES_JOINED: &str = r#"def main(stdout: Stdout):
    mut out = []
    for i in range(1000):
        out.append(str(i))
        out.append("é")
        out.append(f"<{i * 3}>")
        out.append("日本")
    text = "".join(out)
    stdout.print(text.len())
"#;

const PIECES_JOINED_EDGES: &str = r#"def joined(numbers: list[int], floats: list[float], words: list[str]) -> str:
    mut out = []
    for n in numbers:
        out.append(str(n))
        out.append(f"<{n}>")
    for f in floats:
        out.append(str(f))
        out.append(f"[{f}]")
    for word in words:
        out.append(word)
        out.append(f"({word})")
    "".join(out)

def main(stdout: Stdout):
    numbers = [0, -1, 7, 1234567890, -9223372036854775807 - 1, 9223372036854775807]
    floats = [1.5, -0.0, 1e300, 0.1, -2.25]
    words = ["é", "日本", "🦊", "fóx12", "ab", "ça va très bien", "x" * 30 + "é"]
    text = joined(numbers, floats=floats, words=words)
    stdout.print(text)
    stdout.print(text.len(), text[-1], text[-2], text[text.len() // 2])
    mut index = 0
    mut seen = 0
    while index < text.len():
        if text[index] == "🦊":
            seen += 1
        index += 1
    stdout.print(seen)
"#;

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
        "def main(stdout: Stdout):\n    mut a = [1, 2, 3]\n    mut b = a\n    b[0] = 9\n    stdout.print(a, b)\n    mut c = [0 for _ in range(3)]\n    d = c\n    c[1] = 5\n    stdout.print(c, d)\n    mut keep: list[list[int]] = []\n    for i in range(3):\n        row = [i, i]\n        mut held = row\n        held[0] = 7\n        keep.append(held)\n        keep.append(row)\n    stdout.print(keep)\n",
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
    _ = xs.pop()
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
    grow_before = grow
    for at, n in enumerate(grow_before):
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

def load(text: str) -> Bag fails ParseError:
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

/// JSON read into dicts, by string, int, plain struct and tuple keys, and a dict a capability
/// gives back, as the JIT has them. The variable is looked for by name in any case: Windows calls
/// it `Path`.
#[test]
fn json_into_dicts_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"from std import json

struct Spot:
    x: int
    y: int

def main(stdout: Stdout, env: Env):
    named = json.loads[dict[str, int]]("{\"ash\": 1, \"vex\": 3}") else {}
    stdout.print(named, named["vex"])
    levels = json.loads[dict[int, str]]("{\"1\": \"intro\", \"42\": \"boss\"}") else {}
    stdout.print(levels, levels[42])
    spots = {Spot(x=1, y=2): "tree", Spot(x=0, y=0): "rock"}
    back = json.loads[dict[Spot, str]](json.dumps(spots)) else {}
    stdout.print(back == spots, back[Spot(x=0, y=0)])
    wrong = json.loads[dict[str, int]]("[1, 2]") else {"none": 0}
    stdout.print(wrong)
    corners = {(0, 0): "start", (3, 4): "end"}
    stdout.print(json.loads[dict[(int, int), str]](json.dumps(corners)) else {}, corners)
    placed = {(Spot(x=1, y=2), "a"): 1}
    stdout.print((json.loads[dict[(Spot, str), int]](json.dumps(placed)) else {}) == placed)
    stdout.print("PATH" in [name.upper() for name in env.all().keys()])
"#;
    for (name, how) in [("json_dicts", &["--llvm"][..]), ("json_dicts_release", &["--llvm", "--release"][..])]
    {
        let (ours, jit) = both_built(name, source, how)?;
        assert_eq!(ours, jit);
    }
    Ok(())
}

/// Skirmish built with LLVM prints its `.out` file: a whole kit, saved to JSON and loaded back, cached and as a release.
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
        // A whole kit, as a release too: on CI's x86-64 Mac, run under Rosetta, building it once
        // took past the usual deadline, which is there to catch a hang, not to time a build.
        let built = crate::executables::fennec_command()
            .args(&args)
            .env("FENNEC_HOME", &dir)
            .current_dir(&proof)
            .output_before(FENNEC_DEADLINE * 3)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
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
    let ran = Command::new(&executable).args(["one", "two"]).output_within()?;
    let interpreted = interpreted_by(&program, "arguments_and_clock")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "2 True\none\ntwo\none two\nTrue\n");
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
    Ok(())
}

/// What compiled code does through capabilities, failing as well as not.
const NATIVE_CAPABILITIES: &str = "\
from std.path import Path

def main(stdout: Stdout, fs: Fs, process: Process, stdin: Stdin, entropy: Entropy):
    folder = \"native-capabilities\"
    stdout.print(fs.exists(folder).expect(\"it works\"))
    fs.make_dir(folder).expect(\"it works\")
    file = folder + \"/a.txt\"
    fs.write_text(file, \"one\\ntwo\\n\").expect(\"it works\")
    stdout.print(fs.read_text(file).expect(\"it works\"), fs.exists(Path(text=file)).expect(\"it works\"))
    stdout.print(fs.metadata(file).expect(\"it works\").size, fs.metadata(file).expect(\"it works\").is_file)
    stdout.print(fs.list_dir(folder).expect(\"it works\"))
    only = fs.only(folder)
    stdout.print(only.read_text(file).expect(\"it works\"))
    # Outside its folder it can't look at all, which fails rather than saying no.
    stdout.print(only.exists(\"/\") else True)
    _ = only.read_text(\"/etc/hosts\") else as hosts:
        stdout.print(hosts.reason.endswith(\"native-capabilities\"))
        \"\"
    _ = fs.read_text(\"no such file\") else as missing:
        stdout.print(missing.reason != \"\")
        \"\"
    fs.remove(file).expect(\"it works\")
    fs.remove(folder).expect(\"it works\")
    done = process.run(\"echo\", [\"hello\"]).expect(\"it works\")
    stdout.print(done.status, done.stdout, done.stderr == \"\")
    _ = process.run(\"no-such-program-at-all\", []) else as unknown:
        stdout.print(unknown.program())
        done
    echo = process.only(\"echo\")
    stdout.print(echo.run(\"echo\", [\"again\"]).expect(\"it works\").stdout)
    _ = echo.run(\"ls\", []) else as refused:
        stdout.print(refused.message())
        done
    stdout.print(stdin.lines())
    stdout.print(entropy.seed() != entropy.seed())
";

/// A program links only what the capabilities it's given need: a hello world catches no signals, so
/// it has none of the thread and socket pair catching them takes, and its runtime's state holds a
/// test's fakes only behind a box, so none of what cleaning up after them takes either. Each
/// symbol a program takes from the system costs it at every start. A program given `Signals` has
/// what catching them needs.
#[cfg(target_os = "macos")]
#[test]
fn a_program_links_only_what_its_capabilities_need() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("links_what_it_needs")?;
    let imports = |name: &str, source: &str| -> std::io::Result<String> {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        let executable = dir.join(name);
        let built =
            fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let listed = Command::new("nm").arg("-u").arg(&executable).output()?;
        Ok(String::from_utf8_lossy(&listed.stdout).into_owned())
    };
    let hello = imports("hello", "def main(stdout: Stdout):\n    stdout.print(\"hello\")\n")?;
    for unneeded in ["_socketpair", "_pthread_create", "_sigprocmask", "_fdopendir", "_unlinkat"] {
        assert!(!hello.lines().any(|line| line.trim() == unneeded), "a hello world takes {unneeded}");
    }
    let stopping = imports(
        "stopping",
        "def main(signals: Signals, stdout: Stdout):\n    stdout.print(signals.stopping())\n",
    )?;
    assert!(stopping.lines().any(|line| line.trim() == "_socketpair"), "{stopping}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A program given `Signals` catches SIGTERM, SIGINT and SIGHUP, so it can finish what it's doing:
/// it sees `stopping()` turn true, says so and exits 0. A second signal, once a stop's been asked
/// for, ends it at once, as it would have without Fennec, so a stuck program can still be stopped.
#[cfg(unix)]
#[test]
fn a_program_given_signals_stops_cleanly_and_a_second_signal_ends_it() -> std::io::Result<()> {
    use std::os::unix::process::ExitStatusExt;
    if !ready() {
        return Ok(());
    }
    let dir = folder("signals_stop")?;
    let build = |name: &str, source: &str| -> std::io::Result<PathBuf> {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        let executable = dir.join(name);
        let built =
            fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        Ok(executable)
    };
    let cleanly = build(
        "cleanly",
        "def main(signals: Signals, clock: Clock, stdout: Stdout):
    stdout.print(\"ready\")
    while not signals.stopping():
        clock.sleep(10ms)
    stdout.print(\"stopped cleanly\")
",
    )?;
    let stuck = build(
        "stuck",
        "def main(signals: Signals, clock: Clock, stdout: Stdout):
    stdout.print(\"ready\")
    signals.wait()
    stdout.print(\"asked\")
    # It goes on as though nothing asked, as a stuck program would.
    while True:
        clock.sleep(10ms)
",
    )?;
    // Starts `executable`, and gives it with a way to read each line it prints as it comes.
    let start = |executable: &Path| -> std::io::Result<(Running, std::sync::mpsc::Receiver<String>)> {
        use std::io::BufRead;
        let mut running = Running(Command::new(executable).stdout(std::process::Stdio::piped()).spawn()?);
        let stdout = running.0.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
        let (says, said) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = says.send(line);
            }
        });
        Ok((running, said))
    };
    let line =
        |said: &std::sync::mpsc::Receiver<String>| said.recv_timeout(std::time::Duration::from_secs(30));
    let send = |running: &Running, signal: &str| -> std::io::Result<()> {
        let sent =
            Command::new("kill").args([&format!("-{signal}"), &running.0.id().to_string()]).status()?;
        assert!(sent.success(), "couldn't send SIG{signal}");
        Ok(())
    };
    let wait = |running: &mut Running| -> std::io::Result<std::process::ExitStatus> {
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = running.0.try_wait()? {
                return Ok(status);
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(30), "it didn't end");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    for signal in ["TERM", "INT", "HUP"] {
        let (mut running, said) = start(&cleanly)?;
        assert_eq!(line(&said).as_deref(), Ok("ready"));
        send(&running, signal)?;
        assert_eq!(line(&said).as_deref(), Ok("stopped cleanly"), "SIG{signal}");
        assert_eq!(wait(&mut running)?.code(), Some(0), "SIG{signal}");
    }
    let (mut running, said) = start(&stuck)?;
    assert_eq!(line(&said).as_deref(), Ok("ready"));
    send(&running, "TERM")?;
    assert_eq!(line(&said).as_deref(), Ok("asked"));
    send(&running, "TERM")?;
    assert_eq!(wait(&mut running)?.signal(), Some(15));
    drop(running);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What a program writes to `Stderr` goes out at once, after what it printed before, so the two read
/// in order where they're shown together, as on a terminal.
#[cfg(unix)]
#[test]
fn standard_error_comes_out_after_what_was_printed_before() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("stderr_in_order")?;
    let program = dir.join("warn.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout, stderr: Stderr):
    stdout.print(\"first\")
    stderr.write_line(\"then a warning\")?
    stdout.print(\"last\")
",
    )?;
    let executable = dir.join("warn");
    let built = fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // Both to one pipe, as a terminal shows them.
    let ran =
        Command::new("sh").arg("-c").arg("\"$0\" 2>&1").arg(&executable).output_before(FENNEC_DEADLINE)?;
    assert!(ran.status.success());
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "first\nthen a warning\nlast\n");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A server built on the runtime `threads` names, `"1"` with threads and `"0"` without, serving
/// HTTP until the program's asked to stop, with 2 seconds' grace. Gives it running, the port it
/// listens on, and what it prints after.
#[cfg(unix)]
fn stopping_server(
    threads: &str,
    name: &str,
) -> std::io::Result<(PathBuf, Running, u16, std::io::BufReader<std::process::ChildStdout>)> {
    if threads == "1" {
        assert!(crate::executables::threads_runtime_built(), "couldn't build the runtime with threads");
    }
    let dir = folder(name)?;
    let program = dir.join("serve.fen");
    std::fs::write(
        &program,
        "from std.net import Request, Response

def respond(request: Request) -> Response:
    Response(status=200, body=request.path.encode())

def main(net: Net, signals: Signals, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=60s, until=signals, grace=2s)
    stdout.print(\"stopped\")
",
    )?;
    let executable = dir.join("serve");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .env("FENNEC_THREADS_RUNTIME", threads)
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?);
    let mut printed = std::io::BufReader::new(server.0.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?);
    let mut port = String::new();
    std::io::BufRead::read_line(&mut printed, &mut port)?;
    let port: u16 = port.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    Ok((dir, server, port, printed))
}

/// Sends SIGTERM to `running`.
#[cfg(unix)]
fn terminate(running: &Running) -> std::io::Result<()> {
    let sent = Command::new("kill").args(["-TERM", &running.0.id().to_string()]).status()?;
    assert!(sent.success(), "couldn't send SIGTERM");
    Ok(())
}

/// What a server serving until it's asked to stop prints after, and that it then exits 0.
#[cfg(unix)]
fn stopped_cleanly(
    mut server: Running,
    mut printed: std::io::BufReader<std::process::ChildStdout>,
) -> std::io::Result<()> {
    use std::io::Read;
    let mut last = String::new();
    printed.read_to_string(&mut last)?;
    assert_eq!(last, "stopped\n");
    assert_eq!(server.0.wait()?.code(), Some(0));
    Ok(())
}

/// A connection to the server on `port`, whose reads give up after 30 seconds.
#[cfg(unix)]
fn connected(port: u16) -> std::io::Result<std::net::TcpStream> {
    let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    Ok(stream)
}

/// A server serving `until=signals` drains when the program's asked to stop, on the runtime with
/// threads: a connection waiting for its next request ends at once rather than at its timeout,
/// one partway through a request is answered and closed, one that never finishes is cut short
/// when `grace` is up, and the program goes on past the server and exits 0.
#[cfg(unix)]
#[test]
fn a_server_serving_until_signals_drains_when_asked() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let (dir, server, port, printed) = stopping_server("1", "server_until_threads")?;
    let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/a";
    // Kept alive, answered, and then waiting for a next request.
    let mut idle = connected(port)?;
    idle.write_all(b"GET /a HTTP/1.1\r\n\r\n")?;
    let mut first = vec![0; answer.len()];
    idle.read_exact(&mut first)?;
    assert_eq!(first, answer);
    // Partway through a request, finished after the stop.
    let mut partway = connected(port)?;
    partway.write_all(b"GET /b HTTP/1.1\r\n")?;
    // Partway through one it never finishes.
    let mut stuck = connected(port)?;
    stuck.write_all(b"GET /c HTTP/1.1\r\n")?;
    // The server has taken each before it's asked to stop.
    std::thread::sleep(std::time::Duration::from_millis(300));
    terminate(&server)?;
    let asked = std::time::Instant::now();
    let mut rest = Vec::new();
    idle.read_to_end(&mut rest)?;
    assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(20),
        "the idle connection waited out its timeout"
    );
    partway.write_all(b"\r\n")?;
    let mut answered = Vec::new();
    partway.read_to_end(&mut answered)?;
    assert_eq!(
        String::from_utf8_lossy(&answered),
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n/b"
    );
    let mut cut = Vec::new();
    let _ = stuck.read_to_end(&mut cut);
    assert!(cut.is_empty(), "{}", String::from_utf8_lossy(&cut));
    stopped_cleanly(server, printed)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// The same on the runtime without threads, which serves a connection at a time: the one being
/// served is answered and closed when it's partway through a request, and one waiting for its next
/// request ends at once. A server waiting for a connection stops at once.
#[cfg(unix)]
#[test]
fn a_server_on_the_plain_runtime_serving_until_signals_stops_when_asked() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let answer = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/a";
    let (dir, server, port, printed) = stopping_server("0", "server_until_plain_partway")?;
    let mut partway = connected(port)?;
    partway.write_all(b"GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\n")?;
    let mut first = vec![0; answer.len()];
    partway.read_exact(&mut first)?;
    assert_eq!(String::from_utf8_lossy(&first), answer);
    terminate(&server)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    partway.write_all(b"\r\n")?;
    let mut answered = Vec::new();
    partway.read_to_end(&mut answered)?;
    assert_eq!(
        String::from_utf8_lossy(&answered),
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n/b"
    );
    stopped_cleanly(server, printed)?;
    std::fs::remove_dir_all(&dir)?;
    let (dir, server, port, printed) = stopping_server("0", "server_until_plain_idle")?;
    let mut idle = connected(port)?;
    idle.write_all(b"GET /a HTTP/1.1\r\n\r\n")?;
    let mut first = vec![0; answer.len()];
    idle.read_exact(&mut first)?;
    assert_eq!(String::from_utf8_lossy(&first), answer);
    terminate(&server)?;
    let mut rest = Vec::new();
    idle.read_to_end(&mut rest)?;
    assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
    stopped_cleanly(server, printed)?;
    std::fs::remove_dir_all(&dir)?;
    let (dir, server, _, printed) = stopping_server("0", "server_until_plain_waiting")?;
    terminate(&server)?;
    stopped_cleanly(server, printed)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

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
        command.current_dir(&dir).output_given(b"first\r\nsecond\n", FENNEC_DEADLINE)
    };
    let ran = with_lines(&mut Command::new(&executable))?;
    let interpreted = interpreted_by(&program, "stdin_lines")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
    Ok(())
}

/// `Fs` is declared in `fs.fen`, as `Connection` is in `net.fen`, and the compiler still finds it
/// by name everywhere it did: given to `main`, passed to functions and kept in a struct, with its
/// Rust methods taking a `Path` or a `str`, narrowed by `only`, and with `create` and `open`,
/// written in Fennec, refused outside what a narrowed one reaches.
const FS_BY_NAME: &str = "\
from std.path import Path

struct Saves:
    fs: Fs
    folder: str

    def path(self, name: str) -> str:
        self.folder + \"/\" + name

def write(fs: Fs, path: str, text: str) fails IoError:
    with fs.create(path)? as file:
        file.write_text(text)?

def read(saves: Saves, name: str) -> str fails IoError:
    mut text = \"\"
    with saves.fs.open(saves.path(name))? as file:
        text = file.read_text()?
    text

def main(stdout: Stdout, fs: Fs):
    folder = \"fs-by-name\"
    fs.make_dir(folder).expect(\"it works\")
    saves = Saves(fs=fs, folder=folder)
    write(fs, path=saves.path(\"a.txt\"), text=\"fox\").expect(\"it works\")
    stdout.print(read(saves, \"a.txt\").expect(\"it works\"), fs.read_text(Path(text=saves.path(\"a.txt\"))).expect(\"it works\"))
    only = fs.only(folder)
    write(only, path=folder + \"/b.txt\", text=\"den\").expect(\"it works\")
    stdout.print(read(Saves(fs=only, folder=folder), \"b.txt\").expect(\"it works\"))
    write(only, path=\"outside.txt\", text=\"no\") else as outside:
        stdout.print(\"outside what this Fs can reach\" in outside.reason)
    stdout.print(fs.exists(\"outside.txt\").expect(\"it works\"))
    fs.remove(folder + \"/a.txt\").expect(\"it works\")
    fs.remove(folder + \"/b.txt\").expect(\"it works\")
    fs.remove(folder).expect(\"it works\")
";

#[test]
fn fs_declared_in_std_is_found_by_name_natively_as_in_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("fs-by-name")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, FS_BY_NAME)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let fallbacks = fennec(&["dev", "fallbacks", "--llvm", &path(&program)], &dir)?;
    let said = String::from_utf8_lossy(&fallbacks.stdout);
    assert_eq!(said.lines().last(), Some("everything compiles to native code"), "{said}");
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    let interpreted = interpreted_by(&program, "fs_by_name")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "fox fox\nden\nTrue\nFalse\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
    Ok(())
}

/// A file whose block ends without it taking its path's place fails the `with`, and leaves nothing
/// behind: the end of the block is where what was written is flushed, synced and renamed, and any
/// of those failing is the block's failure. Here the path has become a folder by then.
#[test]
fn a_file_that_cant_take_its_place_fails_its_with_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("finish-fails")?;
    let program = dir.join("program.fen");
    std::fs::write(
        &program,
        "\
## Writes a file whose path becomes a folder before the block ends, so it can't take its place.
def blocked(fs: Fs) fails IoError:
    with fs.create(\"taken.txt\")? as file:
        file.write_text(\"never seen\")?
        fs.make_dir(\"taken.txt\")?
        fs.write_text(\"taken.txt/inside.txt\", \"here first\")?

def main(fs: Fs, stdout: Stdout):
    blocked(fs) else as problem:
        stdout.print(\"the block failed:\", problem.reason != \"\")
    stdout.print(fs.read_text(\"taken.txt/inside.txt\") else \"gone\")
    # The file written beside the path, to take its place, is gone.
    names = fs.list_dir(\".\") else []
    stdout.print([name for name in names if str(name).endswith(\".fennec-partial\")].len())
",
    )?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "the block failed: True\nhere first\n0\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    Ok(())
}

/// Writes `megabytes` through `fs.create`, a megabyte at a time. The file takes its name only
/// when the block ends, synced, and what's written goes straight to the disk, so the program
/// holds next to nothing however big the file grows.
fn big_write(megabytes: usize) -> String {
    format!(
        "\
def write_big(fs: Fs) -> int fails IoError:
    chunk = \"fennec \" * 142857 + \"fox\"
    with fs.create(\"big.txt\")? as file:
        for _ in range({megabytes}):
            file.write_text(chunk)?
    fs.metadata(\"big.txt\")?.size

def main(stdout: Stdout, fs: Fs):
    stdout.print(write_big(fs) else -1)
"
    )
}

/// Builds and runs `big_write(megabytes)`, checking the file came out whole, nothing was left
/// beside it, and the program never held `most` bytes.
fn writes_in_little_memory(name: &str, megabytes: usize, most: u64) -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder(name)?;
    let program = dir.join("big.fen");
    std::fs::write(&program, big_write(megabytes))?;
    let executable = dir.join("big");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (ran, peak) = with_peak_memory(&executable, &dir)?;
    let partial = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".fennec-partial"));
    std::fs::remove_dir_all(&dir)?;
    let size = megabytes * 1_000_002;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        format!("{size}\n"),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(!partial, "the file written beside big.txt was left behind");
    if let Some(peak) = peak {
        assert!(peak < most, "writing {megabytes} MB held {} MB", peak / (1024 * 1024));
    }
    Ok(())
}

/// 64 MB, where a program starts at about 5 MB: one that held its chunks would show at once.
#[test]
fn a_big_file_written_a_chunk_at_a_time_holds_little_memory() -> std::io::Result<()> {
    writes_in_little_memory("big-file", 64, 32 * 1024 * 1024)
}

/// The same with a gigabyte, run by hand: `cargo test -p fennec --test fennec -- --ignored
/// a_gigabyte`. It's too much time and disk for every gate.
#[test]
#[ignore = "writes a gigabyte"]
fn a_gigabyte_written_a_chunk_at_a_time_holds_little_memory() -> std::io::Result<()> {
    writes_in_little_memory("gigabyte-file", 1024, 200 * 1024 * 1024)
}

/// Writes 64 MB, then hashes it as it's read, a megabyte at a time, with `blake3` and `sha256`
/// of the file's chunks. The hashes are those of the whole text, and the program never holds
/// more than a few chunks.
const HASH_BIG_FILE: &str = "\
from std.crypto import blake3, sha256

def hashes(fs: Fs) -> list[str] fails IoError:
    chunk = \"fennec \" * 142857 + \"fox\"
    with fs.create(\"big.txt\")? as file:
        for _ in range(64):
            file.write_text(chunk)?
    mut found = list[str]()
    with fs.open(\"big.txt\")? as file:
        found.append(blake3(file.chunks(1000000)?))
    with fs.open(\"big.txt\")? as file:
        found.append(sha256(file.chunks(1000000)?))
    found

def main(stdout: Stdout, fs: Fs):
    for found in hashes(fs) else []:
        stdout.print(found)
";

/// The BLAKE3 and SHA-256 of `HASH_BIG_FILE`'s text, as the whole text hashed elsewhere gives
/// them.
const BIG_FILE_HASHES: &str = "\
b90b9707076bbeb116e530ba67192a831f21a039eaba115c14e0f3cac0b23586
e37d10b0cfad1de842e395ddc89deef2c86d9abe99d363ad67bd21b919128e69
";

/// 64 MB hashed as it's read holds next to nothing, as writing it does.
#[test]
fn a_big_file_hashed_a_chunk_at_a_time_holds_little_memory() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("hash-big-file")?;
    let program = dir.join("hash.fen");
    std::fs::write(&program, HASH_BIG_FILE)?;
    let executable = dir.join("hash");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (ran, peak) = with_peak_memory(&executable, &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        BIG_FILE_HASHES,
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    if let Some(peak) = peak {
        assert!(peak < 32 * 1024 * 1024, "hashing 64 MB held {} MB", peak / (1024 * 1024));
    }
    Ok(())
}

/// What a program printed, and the most memory it held, where `/usr/bin/time` can say.
fn with_peak_memory(executable: &Path, dir: &Path) -> std::io::Result<(Output, Option<u64>)> {
    let time = Path::new("/usr/bin/time");
    let flag = if cfg!(target_os = "macos") { "-l" } else { "-v" };
    if cfg!(windows) || !time.exists() {
        return Ok((Command::new(executable).current_dir(dir).output_within()?, None));
    }
    let ran = Command::new(time).arg(flag).arg(executable).current_dir(dir).output_within()?;
    let said = String::from_utf8_lossy(&ran.stderr);
    let peak = said.lines().find_map(|line| {
        let line = line.trim();
        if let Some(bytes) = line.strip_suffix("maximum resident set size") {
            return bytes.trim().parse::<u64>().ok();
        }
        let kilobytes = line.strip_prefix("Maximum resident set size (kbytes):")?;
        kilobytes.trim().parse::<u64>().ok().map(|kilobytes| kilobytes * 1024)
    });
    Ok((ran, peak))
}

/// One task writes 64 MB, synced, while three others only answer, all on one thread. Each disk
/// wait parks the writer and gives the thread to the others, so they finish first: blocking on
/// the disk would hold all three up until the write was done.
const WRITE_WHILE_ANSWERING: &str = "\
struct Order:
    done = list[int]()

    def finished(ref self, at: int):
        self.done.append(at)

def work(fs: Fs, order: shared Order, at: int) -> int fails IoError:
    if at == 0:
        chunk = \"x\" * 1000000
        with fs.create(\"synced.txt\")? as file:
            for _ in range(64):
                file.write_text(chunk)?
    lock order as ref held:
        held.finished(at)
    at

def main(stdout: Stdout, fs: Fs):
    shared order = Order()
    answered = [work(fs, order, at) else -1 for at in parallel([0, 1, 2, 3])]
    lock order as held:
        stdout.print(answered, held.done[3])
";

#[test]
fn a_task_writing_a_file_lets_the_others_answer() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("write-while-answering")?;
    let program = dir.join("writes.fen");
    std::fs::write(&program, WRITE_WHILE_ANSWERING)?;
    let executable = dir.join("writes");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).env("FENNEC_THREADS", "1").output_within()?;
    std::fs::remove_dir_all(&dir)?;
    // The writer started first and was the last to finish.
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "[0, 1, 2, 3] 0\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    Ok(())
}

/// A `parallel` comprehension whose items can fail runs on every thread, and the failure that
/// passes up is the first in the list's order, as it is when the items are made in order: here
/// 70007, though 71007 and others fail too. Natively it was once made in order, on one thread.
const FAILING_ITEMS: &str = "\
error TooBig:
    at: int
    why: str

def checked(x: int) -> int fails TooBig:
    if x > 70_000 and x % 1000 == 7:
        fail TooBig(at=x, why=\"a reason long enough to live in a block of its own\")
    x * 2

def all_of(xs: list[int]) -> list[int] fails TooBig:
    [checked(x)? for x in parallel(xs)]

def main(stdout: Stdout):
    small = [i for i in range(50_000)]
    big = [i for i in range(100_000)]
    made: list[int]? = all_of(small) else as error:
        stdout.print(\"failed\", error.at)
        None
    if made is not None:
        stdout.print(made.len(), made[49_999])
    all_made = all_of(big) else as error:
        stdout.print(\"failed\", error.at, error.why)
        return
    stdout.print(all_made.len())
";

#[test]
fn a_parallel_comprehension_whose_items_fail_passes_up_the_first() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("failing-items")?;
    let program = dir.join("failing.fen");
    std::fs::write(&program, FAILING_ITEMS)?;
    let executable = dir.join("failing");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let wanted = "50000 99998\nfailed 70007 a reason long enough to live in a block of its own\n";
    for _ in 0..5 {
        let ran = Command::new(&executable).current_dir(&dir).env("FENNEC_THREADS", "4").output_within()?;
        assert_eq!(String::from_utf8_lossy(&ran.stdout), wanted, "{}", String::from_utf8_lossy(&ran.stderr));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What the next test runs: `std.sqlite` in a test, on the fake `Fs`.
const SQLITE_TESTS: &str = r#"from std import sqlite
from std import sql
from std.sql import SqlError, SqlRows

enum Status:
    Clean
    Changed

struct Entry:
    path: str
    size: int
    status: Status
    note: str? = None

def made(fs: Fs, path: str) fails SqlError:
    with sqlite.open(fs, path)? as db:
        _ = db.execute(t"create table entries (path text primary key, size integer not null, status text not null, note text)")?

def add(fs: Fs, path: str, entries: list[Entry]) fails SqlError:
    with sqlite.open(fs, path)? as db:
        with db.transaction()? as tx:
            for entry in entries:
                status = "Clean" if entry.status == .Clean else "Changed"
                _ = tx.execute(t"insert into entries (path, size, status) values ({entry.path}, {entry.size}, {status})")?

def all_entries(fs: Fs, path: str) -> list[Entry] fails SqlError:
    mut found = list[Entry]()
    with sqlite.open(fs, path)? as db:
        found = db.query[Entry](t"select path, size, status, note from entries order by path")?
    found

test "rows come back as structs, their values bound"(fs: Fs):
    made(fs, "index.db")?
    sneaky = "a'; drop table entries; --"
    add(fs, "index.db", [Entry(path=sneaky, size=3, status=.Clean), Entry(path="b", size=10, status=.Changed)])?
    found = all_entries(fs, "index.db")?
    assert found.len() == 2
    assert found[0].path == sneaky and found[0].size == 3 and found[0].status == .Clean
    assert found[1].path == "b" and found[1].status == .Changed and found[1].note is None

test "each test has its own databases"(fs: Fs):
    made(fs, "index.db")?
    assert all_entries(fs, "index.db")?.len() == 0

test "a transaction that fails keeps nothing"(fs: Fs):
    made(fs, "index.db")?
    add(fs, "index.db", [Entry(path="a", size=1, status=.Clean)])?
    problem = failure_of(() => add(fs, "index.db", [Entry(path="b", size=2, status=.Clean), Entry(path="a", size=9, status=.Clean)])?)
    assert problem is not None
    match problem:
        case .Constraint(reason=reason):
            assert "UNIQUE" in reason
        case _:
            assert False
    found = all_entries(fs, "index.db")?
    assert found.len() == 1 and found[0].size == 1

test "one column reads as one value"(fs: Fs):
    made(fs, "index.db")?
    add(fs, "index.db", [Entry(path="a", size=4, status=.Clean), Entry(path="b", size=5, status=.Clean)])?
    with sqlite.open(fs, "index.db")? as db:
        assert db.query[int](t"select sum(size) from entries")? == [9]
        least = 5
        assert db.query[str](t"select path from entries where size >= {least}")? == ["b"]
        changed = db.execute(t"update entries set size = size + 1")?
        assert changed == 2

test "a column that doesn't fit fails with its name"(fs: Fs):
    made(fs, "index.db")?
    add(fs, "index.db", [Entry(path="a", size=4, status=.Clean)])?
    with sqlite.open(fs, "index.db")? as db:
        text_size = failure_of(() => db.query[Entry](t"select path, path as size, status from entries")?)
        assert text_size is not None
        match text_size:
            case .Column(name=name, reason=reason):
                assert name == "size" and reason == "it holds text, not an int"
            case _:
                assert False
        assert db.query[Entry](t"select path, status from entries") fails .Column(name="size")
        extra = failure_of(() => db.query[Entry](t"select path, size, status, 1 as extra from entries")?)
        assert extra is not None
        match extra:
            case .Column(name=name, reason=reason):
                assert name == "extra" and reason == "`Entry` has no field called `extra`"
            case _:
                assert False
        two = failure_of(() => db.query[int](t"select size, path from entries")?)
        assert two is not None
        match two:
            case .Column(reason=reason):
                assert "2 columns" in reason
            case _:
                assert False

test "a query is one statement"(fs: Fs):
    with sqlite.open(fs, "index.db")? as db:
        problem = failure_of(() => db.execute(t"create table a (x int); create table b (x int)")?)
        assert problem is not None
        match problem:
            case .Failed(reason=reason):
                assert "one statement" in reason
            case _:
                assert False

test "a narrowed Fs opens only what it reaches"(fs: Fs):
    fs.make_dir("data")?
    problem = failure_of(() => made(fs.only("data"), "elsewhere.db")?)
    assert problem is not None
    match problem:
        case .Failed(reason=reason):
            assert "outside what this Fs can reach" in reason
        case _:
            assert False
    made(fs.only("data"), "data/index.db")?

test "a driver's rows read through std.sql as SQLite's do":
    rows = SqlRows(
        columns=["path", "size", "status", "note"],
        kinds=[3, 1, 3, 0, 3, 1, 3, 3],
        at=[0, 0, 1, 0, 2, 1, 3, 4],
        ints=[3, 10],
        floats=[],
        texts=["a", "Clean", "b", "Changed", "hi"],
        blobs=[],
    )
    found = sql.rows_as[Entry](rows)?
    assert found.len() == 2 and found[0].note is None and found[1].note == "hi"
    assert found[1].status == .Changed and found[1].size == 10
"#;

/// `std.sqlite` in compiled tests, with a database in memory for each path: rows read into
/// structs by their columns' names, values bound rather than pasted into the query, a fresh
/// database for each test, a failed transaction keeping nothing, a column that doesn't fit
/// failing with its name, and a narrowed `Fs` opening only what it reaches.
#[test]
fn sqlite_in_tests_reads_rows_into_structs_and_names_a_column_that_does_not_fit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("sqlite-tests")?;
    let program = dir.join("store.fen");
    std::fs::write(&program, SQLITE_TESTS)?;
    let ran = fennec(&["test", "--all", "--verbose", &program.to_string_lossy()], &dir)?;
    let (shown, said) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{shown}{said}");
    assert!(
        said.contains("8 of 8 tests ran through LLVM") && shown.contains("8 passed, 0 failed"),
        "{shown}{said}"
    );
    Ok(())
}

/// What the next test runs, against the fake `Fs` and then the real disk.
const SQLITE_FAKE_AND_REAL: &str = r#"from std import sqlite
from std.sql import SqlError

test "a database is a file"(fs: Fs):
    with sqlite.open(fs, "real.db")? as db:
        _ = db.execute(t"create table if not exists notes (text text)")?
        _ = db.execute(t"delete from notes")?
        word = "fox"
        _ = db.execute(t"insert into notes values ({word})")?
    assert fs.exists("real.db")?
    with sqlite.open(fs, "real.db")? as db:
        assert db.query[str](t"select text from notes")? == ["fox"]
    fs.remove("real.db")?

test "its file is there to see"(fs: Fs):
    with sqlite.open(fs, "memory.db")? as db:
        _ = db.execute(t"create table if not exists notes (text text)")?
    assert fs.exists("memory.db")?
    fs.remove("memory.db")?

def opened(fs: Fs, path: str) fails SqlError:
    with sqlite.open(fs, path)? as db:
        _ = db.execute(t"select 1")?

test "a folder that isn't there fails it"(fs: Fs):
    problem = failure_of(() => opened(fs, "no/such/folder/a.db")?)
    assert problem is not None
    match problem:
        case .Failed(reason=reason):
            assert reason != ""
        case _:
            assert False

test "outside a transaction, a query reads what was there before it"(fs: Fs):
    with sqlite.open(fs, "snapshot.db")? as db:
        _ = db.execute(t"create table if not exists notes (text text)")?
        _ = db.execute(t"delete from notes")?
        old = "old"
        _ = db.execute(t"insert into notes values ({old})")?
        with db.transaction()? as tx:
            new = "new"
            _ = tx.execute(t"insert into notes values ({new})")?
            assert db.query[str](t"select text from notes")? == ["old"]
            assert tx.query[str](t"select text from notes order by text")? == ["new", "old"]
        assert db.query[str](t"select text from notes order by text")? == ["new", "old"]
    fs.remove("snapshot.db")?
"#;

/// The same tests pass on a test's fake `Fs` and, with `--real`, on files: a database written,
/// closed and opened again, its file there to see, a folder that isn't there failing it, and a
/// query outside a transaction reading what was there before it while it's still going.
#[test]
fn sqlite_tests_pass_alike_on_a_fake_fs_and_on_files() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("sqlite-fake-and-real")?;
    let program = dir.join("real.fen");
    std::fs::write(&program, SQLITE_FAKE_AND_REAL)?;
    for how in [&["test", "--all", "--verbose"][..], &["test", "--real", "--all", "--verbose"][..]] {
        let mut args = how.to_vec();
        let path = program.to_string_lossy().into_owned();
        args.push(&path);
        let ran = crate::executables::fennec_command()
            .args(&args)
            .env("FENNEC_HOME", &dir)
            .current_dir(&dir)
            .output_before(FENNEC_DEADLINE)?;
        let (shown, said) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
        assert!(ran.status.success() && shown.contains("4 passed, 0 failed"), "{how:?}: {shown}{said}");
    }
    // Nothing's left of the database on the disk.
    for left in ["real.db", "memory.db", "snapshot.db", "snapshot.db-wal"] {
        assert!(!dir.join(left).exists(), "{left} is left");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What the next test builds: 32 `parallel` rounds, each writing in a transaction of its own and
/// reading the whole table, on a file.
const SQLITE_ROUNDS: &str = r#"from std import sqlite
from std.sql import SqlError

def round(db: sqlite.Database, at: int) -> int fails SqlError:
    with db.transaction()? as tx:
        for item in range(20):
            _ = tx.execute(t"insert into items (round, item) values ({at}, {item})")?
        mine = tx.query[int](t"select count(*) from items where round = {at}")?
        if mine != [20]: return -1
    seen = db.query[int](t"select count(*) from items")?
    seen[0]

def rounds(fs: Fs, stdout: Stdout) fails SqlError:
    with sqlite.open(fs, "rounds.db")? as db:
        _ = db.execute(t"create table items (round integer, item integer)")?
        counts = [round(db, at) else -2 for at in parallel(range(32))]
        stdout.print(min(counts) >= 20, max(counts) == 640)
        stdout.print(db.query[int](t"select count(*) from items")?, db.query[int](t"select count(distinct round) from items")?)

def main(stdout: Stdout, fs: Fs):
    rounds(fs, stdout) else as err:
        stdout.print("it failed:", err.message())
        return
"#;

/// Rounds of `parallel` share one database: each transaction has a connection of its own and
/// the database's one writer, so none of the 640 rows is lost, and each round sees its own 20
/// before it commits. More rounds wait than there are blocking threads, so waiting for the
/// writer holds none of them, on four threads and on one.
#[test]
fn sqlite_from_parallel_rounds_keeps_every_write() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("sqlite-rounds")?;
    let program = dir.join("rounds.fen");
    std::fs::write(&program, SQLITE_ROUNDS)?;
    let executable = dir.join("rounds");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    for threads in ["4", "1"] {
        for made in ["rounds.db", "rounds.db-wal", "rounds.db-shm"] {
            let _ = std::fs::remove_file(dir.join(made));
        }
        let ran =
            Command::new(&executable).current_dir(&dir).env("FENNEC_THREADS", threads).output_within()?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "True True\n[640] [32]\n",
            "on {threads} threads: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// What the next test builds: everything `std.postgres` does, against the server
/// `FENNEC_TEST_POSTGRES` names, or, with none, against a port nothing listens on.
const POSTGRES_SERVER: &str = r#"from std import postgres
from std.concurrent import Timeout
from std.crypto import Secret
from std.sql import SqlError

struct Entry:
    path: str
    size: int
    weight: float
    blob: bytes
    seen: bool

struct Count:
    total: int

struct PathOnly:
    missing: str

## What a query's failure was, by its case and whether it says what's wrong.
def said(failure: SqlError) -> str:
    match failure:
        case .Constraint(reason=reason):
            return f"constraint {reason.len() > 0}"
        case .Column(name=name, reason=reason):
            return f"column {name}: {reason}"
        case .Busy():
            return "busy"
        case .Failed(reason=reason):
            return f"failed {reason.len() > 0}"

## What `run` failed with, by `said`, or `worked` when it didn't fail.
def told[T](run: () -> T fails SqlError, worked: str) -> str:
    _ = run() else as why:
        return said(why)
    worked

## What a failure says, whichever kind it is.
def said_of(error: SqlError | Timeout) -> str:
    match error:
        case Timeout(waited=_):
            "no connection came free"
        case Constraint(reason=reason) | Busy(reason=reason) | Failed(reason=reason):
            reason
        case Column(name=name, reason=reason):
            f"column {name}: {reason}"

def rows_in(db: postgres.PgDatabase) -> int fails SqlError:
    db.query[Count](t"select count(*) as total from fennec_entries")?[0].total

## Adds a row in a transaction, then fails, so it's rolled back.
def adds_then_fails(db: postgres.PgDatabase) fails SqlError:
    with db.transaction()? as tx:
        path = "rolled back"
        none = bytes([])
        _ = tx.execute(t"insert into fennec_entries values ({path}, {1}, {1.0}, {none}, {True})")?
        fail .Failed(reason="stop")

## Where a server is, as `postgres://user:password@host:port/database` says.
struct Server:
    host: str
    port: int
    user: str
    database: str
    secret: Secret[str]

## The server's URL, without its password, which goes in `password=`.
def url_of(server: Server) -> str:
    f"postgres://{server.user}@{server.host}:{server.port}/{server.database}"

def run(server: Server, stdout: Stdout, net: Net, entropy: Entropy) fails SqlError | Timeout:
    with postgres.connect(net, entropy, url_of(server), password=server.secret, pool_size=1, tls=False)? as pool:
        with pool.borrow()? as db:
            # A temporary table is this connection's own, so runs at once can't meet.
            _ = db.execute(t"create temp table fennec_entries (path text primary key, size bigint, weight double precision, blob bytea, seen boolean)")?
            for at in range(3):
                path = f"p{at}"
                size = at * 10
                weight = float(at) * 0.5
                blob = bytes([at, 255])
                seen = at % 2 == 0
                stdout.print(db.execute(t"insert into fennec_entries values ({path}, {size}, {weight}, {blob}, {seen})")?)
            least = 10
            for entry in db.query[Entry](t"select path, size, weight, blob, seen from fennec_entries where size >= {least} order by path")?:
                stdout.print(entry.path, entry.size, entry.weight, entry.blob.len(), entry.blob[1], entry.seen)
            # A value can't change what the query means, whatever it says.
            evil = "x'); drop table fennec_entries; --"
            none = bytes([])
            _ = db.execute(t"insert into fennec_entries values ({evil}, {0}, {0.0}, {none}, {False})")?
            stdout.print(rows_in(db)?)
            # Failures say what they are, and the connection goes on.
            path = "p0"
            stdout.print(told(() => db.execute(t"insert into fennec_entries values ({path}, {0}, {0.0}, {none}, {False})")?, worked="inserted twice"))
            stdout.print(told(() => db.execute(t"select from no_such_table")?, worked="no error"))
            stdout.print(told(() => db.query[PathOnly](t"select path from fennec_entries")?, worked="read a column that isn't there"))
            # A transaction that fails changes nothing; one that ends is kept.
            adds_then_fails(db) else as rolled_back:
                stdout.print(said(rolled_back), rows_in(db)?)
            kept = "kept"
            with db.transaction()? as tx:
                stdout.print(tx.execute(t"update fennec_entries set path = {kept} where path = {path}")?)
            stdout.print(rows_in(db)?, db.query[Count](t"select count(*) as total from fennec_entries where path = {kept}")?[0].total)

## Signs in with `secret`, and says how it went.
def signing_in(server: Server, secret: Secret[str]?, net: Net, entropy: Entropy) -> str:
    _ = signed_in(server, secret=secret, net=net, entropy=entropy) else as error:
        return said_of(error)
    return "signed in"

def signed_in(server: Server, secret: Secret[str]?, net: Net, entropy: Entropy) fails SqlError | Timeout:
    with postgres.connect(net, entropy, url_of(server), password=secret, tls=False)? as pool:
        with pool.borrow()? as db:
            _ = db.execute(t"select 1")?

def main(stdout: Stdout, net: Net, entropy: Entropy, env: Env):
    # With no server to use, it's a port nothing listens on, and only connecting is tried.
    address = (env.get("FENNEC_TEST_POSTGRES") else "postgres://nobody:nothing@127.0.0.1:1/none").split("//")[1]
    who = address.split("@")[0]
    place = address.split("@")[1]
    rest = place.split(":")[1]
    server = Server(
        host=place.split(":")[0],
        port=int(rest.split("/")[0]) else 5432,
        user=who.split(":")[0],
        database=rest.split("/")[1],
        secret=Secret.of(who.split(":")[1]),
    )
    _ = run(server, stdout=stdout, net=net, entropy=entropy) else as error:
        stdout.print(said_of(error).startswith(f"couldn't connect to {server.host}:{server.port}"))
        return
    stdout.print("28P01" in signing_in(server, secret=Secret.of("not the password"), net=net, entropy=entropy))
    stdout.print(signing_in(server, secret=None, net=net, entropy=entropy))
"#;

/// `std.postgres` signs in to a real server with SCRAM, runs queries with their values as
/// parameters, reads rows into structs, says what failed, and keeps or rolls back a transaction.
/// It needs `FENNEC_TEST_POSTGRES`, like `postgres://user:password@127.0.0.1:5432/database`. With
/// none set it says so, and only checks the driver builds and a refused connection is reported.
#[test]
fn postgres_works_against_a_real_server() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("postgres_server", POSTGRES_SERVER)?;
    if std::env::var_os("FENNEC_TEST_POSTGRES").is_none() {
        eprintln!(
            "skipped the real server: set FENNEC_TEST_POSTGRES to postgres://user:password@host:port/database"
        );
        assert_eq!(printed, "True\n");
        return Ok(());
    }
    assert_eq!(
        printed,
        "1\n1\n1\np1 10 0.5 2 255 False\np2 20 1.0 2 255 True\n4\nconstraint True\nfailed True\n\
         column path: `PathOnly` has no field called `path`\nfailed True 4\n1\n4 1\nTrue\n\
         the server asks for a password, and `connect` wasn't given one\n"
    );
    Ok(())
}

/// `process.run`'s options in compiled code: what the program reads, the folder it runs in and
/// variables it gets, on their own and together, and a narrowed `Process` still running only
/// what it may when given them.
const PROCESS_OPTIONS: &str = "\
def main(stdout: Stdout, process: Process, fs: Fs):
    fs.make_dir(\"inside\").expect(\"it works\")
    stdout.print(process.run(\"cat\", [], input=\"one\\ntwo\\n\").expect(\"it works\").stdout)
    stdout.print(process.run(\"pwd\", [], cwd=\"inside\").expect(\"it works\").stdout.endswith(\"/inside\\n\"))
    stdout.print(process.run(\"printenv\", [\"FENNEC_FOX\"], env={\"FENNEC_FOX\": \"red\"}).expect(\"it works\").stdout)
    all = process.run(
        \"sh\", [\"-c\", \"cat; pwd; printenv FENNEC_FOX\"], input=\"in\\n\", cwd=\"inside\", env={\"FENNEC_FOX\": \"red\"}
    ).expect(\"it works\")
    stdout.print(all.stdout.startswith(\"in\\n\"), all.stdout.endswith(\"/inside\\nred\\n\"))
    _ = process.run(\"pwd\", [], cwd=\"no-such-folder\") else as nowhere:
        stdout.print(nowhere.message())
        all
    only = process.only(\"cat\")
    stdout.print(only.run(\"cat\", [], input=\"narrowed\\n\").expect(\"it works\").stdout)
    _ = only.run(\"pwd\", [], cwd=\"inside\") else as refused:
        stdout.print(refused.message())
        all
    fs.remove(\"inside\").expect(\"it works\")
";

#[test]
fn process_options_without_the_interpreter_match_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("process-options")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, PROCESS_OPTIONS)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    let interpreted = interpreted_by(&program, "process_options")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "one\ntwo\n\nTrue\nred\n\nTrue True\nthere's no folder `no-such-folder` to run it in\nnarrowed\n\nthis Process can run only `cat`, so it can't run `pwd`\n"
    );
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
    Ok(())
}

/// `fs.walk` in compiled code, on a real tree holding a link back to its own top, which is
/// listed and not gone into, and through an `Fs` narrowed to one folder.
#[cfg(unix)]
const WALKED: &str = "\
def main(stdout: Stdout, fs: Fs):
    for path in fs.walk(\"tree\").expect(\"it works\"):
        stdout.print(path.text, fs.metadata(path).expect(\"it works\").is_dir)
    only = fs.only(\"tree/b\")
    stdout.print([path.text for path in only.walk(\"tree/b\").expect(\"it works\")])
    _ = only.walk(\"tree\") else:
        stdout.print(\"refused\")
        []
    _ = fs.walk(\"no such folder\") else as missing:
        stdout.print(missing.reason != \"\")
        []
";

#[test]
#[cfg(unix)]
fn walking_a_folder_without_the_interpreter_matches_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("walking")?;
    for file in ["tree/b/c/deep.txt", "tree/a.txt", "tree/b/z.txt"] {
        let at = dir.join(file);
        std::fs::create_dir_all(at.parent().ok_or_else(|| std::io::Error::other("no folder"))?)?;
        std::fs::write(at, "")?;
    }
    std::os::unix::fs::symlink("..", dir.join("tree/b/up"))?;
    let program = dir.join("program.fen");
    std::fs::write(&program, WALKED)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    let interpreted = interpreted_by(&program, "walked")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "tree/a.txt False\ntree/b True\ntree/b/c True\ntree/b/c/deep.txt False\ntree/b/up True\ntree/b/z.txt False\n\
         [\"tree/b/c\", \"tree/b/c/deep.txt\", \"tree/b/up\", \"tree/b/z.txt\"]\nrefused\nTrue\n"
    );
    assert_eq!(apart_from(&dir, outcome(&ran)), interpreted);
    Ok(())
}

/// The real network, with no interpreter behind it: a server and a client on this machine, each
/// way a call can come out, and looping over each.
const REAL_NETWORK: &str = "from std.net import NetError, Connection, Server
from std.time import Duration

def reason[T](run: () -> T fails NetError) -> str:
    _ = run() else as why:
        return why.message()
    \"it worked\"

def lines_of(connection: Connection) -> int fails NetError:
    mut count = 0
    for line in connection.lines()?:
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
                stdout.print(reason(() => conn.read_line(timeout=20ms)?))
                stdout.print(reason(() => accepting(server, wait=20ms)?))
                client.write(\"\\n\")?
                stdout.print(conn.read_line()?)
                client.write(\"one\\ntwo\\n\")?
                client.close()
                stdout.print(lines_of(conn)?)
                stdout.print(conn.read_line()?)
                conn.close()
                stdout.print(reason(() => conn.read_line()?))
                stdout.print(reason(() => conn.write(\"gone\")?))
    7

def printed_lines(conn: Connection, stdout: Stdout) fails NetError:
    for line in conn.lines()?:
        stdout.print(line)

## Takes the next connection, waiting up to `wait`, and hangs up, to see how taking one goes.
def accepting(server: Server, wait: Duration) fails NetError:
    with server.accept(timeout=wait)? as _:
        pass

## Connects and hangs up, to see how connecting goes.
def connecting(net: Net, host: str, port: int) fails NetError:
    with net.connect(host, port)? as _:
        pass

def looped(net: Net, stdout: Stdout) -> int fails NetError:
    with net.listen(0)? as server:
        with net.connect(\"127.0.0.1\", server.port)? as client:
            client.write(\"looped\\n\")?
        for conn in server:
            printed_lines(conn, stdout)?
            break
        port = server.port
        server.close()
        stdout.print(reason(() => accepting(server, wait=1s)?))
        return port
    0

def main(net: Net, stdout: Stdout):
    stdout.print(reason(() => talk(net, stdout)?))
    port = looped(net, stdout) else 0
    stdout.print(reason(() => connecting(net, host=\"127.0.0.1\", port=port)?) == f\"`127.0.0.1` refused a connection on port {port}\")
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

## A UDP socket on `port`, closed at once, to see how binding one goes.
def bound(net: Net, port: int) fails NetError:
    with net.udp(port=port)? as _:
        pass

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
            _ = first.receive(timeout=20ms) else as waited:
                stdout.print(waited.message())
                answer
            first.send_to(\"x\", \"no-such-host.invalid:9\") else as unsent:
                stdout.print(unsent.message())
            first.send_to(\"last\", f\"127.0.0.1:{second.port}\")?
            for packet in second:
                stdout.print(packet.data.decode() else \"\")
                second.close()

def main(net: Net, stdout: Stdout):
    mut talked_fine = True
    talk(net, stdout) else as talked:
        stdout.print(talked.message())
        talked_fine = False
    if talked_fine:
        stdout.print(\"done\")
    bound(net, port=99999) else:
        pass
";

#[test]
fn bytes_and_packets_cross_the_real_network_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("network_bytes", NETWORK_BYTES)
}

/// The server's answers reach a client that's still sending when it's refused. A refused request's answer, here a 413 while the body is still coming, isn't lost to a
/// reset: closing with what the client sent unread resets the connection, which throws away what
/// the client hadn't read yet, the answers before too. Found by fuzzing the
/// server (`fennec-fuzz run http-server`).
#[test]
fn an_http_server_s_refusal_reaches_a_client_still_sending() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let source = r"from std.net import Request, Response

def respond(request: Request) -> Response:
    Response(status=200, body=request.path.encode())

def main(net: Net, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=10s)
";
    let dir = folder("http_server_refusals")?;
    let program = dir.join("serve.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("serve"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?);
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let connect = || -> std::io::Result<std::net::TcpStream> {
        let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        Ok(stream)
    };
    // Everything the server says, read as it comes until it closes, or what went wrong.
    let heard = |stream: &mut std::net::TcpStream| -> String {
        let mut heard = Vec::new();
        match stream.read_to_end(&mut heard) {
            Ok(_) => String::from_utf8_lossy(&heard).into_owned(),
            Err(err) => format!("{}[{err}]", String::from_utf8_lossy(&heard)),
        }
    };
    // A body far bigger than the server takes, still being sent when it answers.
    let mut big = connect()?;
    big.write_all(b"POST /up HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n")?;
    let _ = big.write_all(&vec![b'x'; 4 << 20]);
    let too_big = heard(&mut big);
    // Answers first, then a refusal, with the rest of what was sent unread.
    let mut pipelined = connect()?;
    pipelined.write_all(b"GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\n\r\nNOT HTTP\r\n\r\n")?;
    let _ = pipelined.write_all(&vec![b'y'; 1 << 20]);
    let refused_after = heard(&mut pipelined);
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    // Each closes cleanly once it's answered, with no reset.
    let wanted = [
        "HTTP/1.1 413 Content Too Large\r\ncontent-length: 19\r\nconnection: close\r\n\r\nthe body is too big",
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/aHTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/b\
         HTTP/1.1 400 Bad Request\r\ncontent-length: 35\r\nconnection: close\r\n\r\nthat isn't an HTTP/1.1 request line",
    ];
    assert_eq!([too_big.as_str(), &refused_after], wanted);
    Ok(())
}

/// An HTTP server natively: a handler answers each request, a connection stays open for more
/// until it asks to close, and what's too big or isn't HTTP is answered without the handler.
#[test]
fn an_http_server_answers_natively() -> std::io::Result<()> {
    use std::io::{Read, Write};
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
    let mut server = Running(Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?);
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
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
    drop(server);
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

/// The server's answers reach a client that's still sending when it's refused, and a blank line
/// between requests is passed over, as HTTP/1.1 says a server should (RFC 9112, 2.2), rather than
/// closing. A refused request's answer, here a 413 while the body is still coming, isn't lost to a
/// reset: closing with what the client sent unread resets the connection, which throws away what
/// the client hadn't read yet, the answers before too. Found by fuzzing the
/// server (`fennec-fuzz run http-server`).
#[test]
fn an_http_server_answers_what_it_refuses_and_passes_over_blank_lines() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let source = r"from std.net import Request, Response

def respond(request: Request) -> Response:
    Response(status=200, body=request.path.encode())

def main(net: Net, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=10s)
";
    let dir = folder("http_server_blank_lines")?;
    let program = dir.join("serve.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("serve"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?);
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let connect = || -> std::io::Result<std::net::TcpStream> {
        let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        Ok(stream)
    };
    // Everything the server says, read as it comes until it closes, or what went wrong.
    let heard = |stream: &mut std::net::TcpStream| -> String {
        let mut heard = Vec::new();
        match stream.read_to_end(&mut heard) {
            Ok(_) => String::from_utf8_lossy(&heard).into_owned(),
            Err(err) => format!("{}[{err}]", String::from_utf8_lossy(&heard)),
        }
    };
    // A body far bigger than the server takes, still being sent when it answers.
    let mut big = connect()?;
    big.write_all(b"POST /up HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n")?;
    let _ = big.write_all(&vec![b'x'; 4 << 20]);
    let too_big = heard(&mut big);
    // Answers first, then a refusal, with the rest of what was sent unread.
    let mut pipelined = connect()?;
    pipelined.write_all(b"GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\n\r\nNOT HTTP\r\n\r\n")?;
    let _ = pipelined.write_all(&vec![b'y'; 1 << 20]);
    let refused_after = heard(&mut pipelined);
    let mut blank = connect()?;
    blank.write_all(b"GET /a HTTP/1.1\r\n\r\n\r\nGET /b HTTP/1.1\r\nConnection: close\r\n\r\n")?;
    let after_blank = heard(&mut blank);
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    // Each closes cleanly once it's answered, with no reset.
    let wanted = [
        "HTTP/1.1 413 Content Too Large\r\ncontent-length: 19\r\nconnection: close\r\n\r\nthe body is too big",
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/aHTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/b\
         HTTP/1.1 400 Bad Request\r\ncontent-length: 35\r\nconnection: close\r\n\r\nthat isn't an HTTP/1.1 request line",
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n/a\
         HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n/b",
    ];
    assert_eq!([too_big.as_str(), &refused_after, &after_blank], wanted);
    Ok(())
}

/// What a server says to `asked` on one connection, sent whole with the sending side then
/// closed, built with the runtime `threads` names: `"1"` for the one with threads, `"0"` for the
/// plain one. A reset after what was said shows as `[reset]`.
fn served_on(threads: &str, name: &str, asked: &[u8]) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let source = r"from std.net import Request, Response

def respond(request: Request) -> Response:
    Response(status=200, body=request.path.encode())

def main(net: Net, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=10s)
";
    if threads == "1" {
        assert!(crate::executables::threads_runtime_built(), "couldn't build the runtime with threads");
    }
    let dir = folder(name)?;
    let program = dir.join("serve.fen");
    std::fs::write(&program, source)?;
    let executable = dir.join("serve");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .env("FENNEC_THREADS_RUNTIME", threads)
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?);
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let _ = stream.write_all(asked);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut heard = Vec::new();
    let reset = stream.read_to_end(&mut heard).is_err();
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    Ok(format!("{}{}", String::from_utf8_lossy(&heard), if reset { "[reset]" } else { "" }))
}

/// A server built on the plain runtime, as `fennec build` makes one where the runtime with threads
/// isn't there, closes as gently as one on the threads runtime: a refusal with what the client
/// sent still unread reaches it whole, with no reset. `close` on the plain runtime shut the
/// socket at once. Found by fuzzing the server (`fennec-fuzz run http-server`).
#[test]
fn a_server_on_the_plain_runtime_closes_gently() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let asked = [&b"NOT HTTP\r\n\r\n"[..], &b"x".repeat(100_000)].concat();
    let said = served_on("0", "plain_runtime_close", &asked)?;
    assert_eq!(
        said,
        "HTTP/1.1 400 Bad Request\r\ncontent-length: 35\r\nconnection: close\r\n\r\nthat isn't an HTTP/1.1 request line"
    );
    Ok(())
}

/// A `\r` is taken off a line once, with the `\n` it ends with: `\r\r\n` is a line holding a bare
/// `\r`, which a request can't have, not a blank line. The runtime with threads took one off
/// when it read the line and `line_value` another. Found by fuzzing the server
/// (`fennec-fuzz run http-server`).
#[test]
fn a_bare_carriage_return_isnt_taken_off_a_line() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for threads in ["0", "1"] {
        let said =
            served_on(threads, &format!("bare_carriage_return_{threads}"), b"\r\r\nGET /a HTTP/1.1\r\n\r\n")?;
        assert!(said.starts_with("HTTP/1.1 400 Bad Request\r\n"), "threads {threads}: {said}");
    }
    Ok(())
}

/// A failure leaving `lock ... as ref`, by `fail` or `?`, puts back everything the block changed,
/// as a crash does: one value, or all those locked together, and on `parallel`'s threads too. What
/// the failure says keeps what the block saw. A local changed in the block keeps its change, and
/// `return` and `break` are ordinary ways out, which keep the block's changes.
#[test]
fn a_failure_out_of_a_lock_puts_back_what_it_changed() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("lock_failure_rollback")?;
    let program = dir.join("rollback.fen");
    std::fs::write(&program, include_str!("fixtures/lock_failure_rollback.fen"))?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("rollback"));
    let output = output.to_string_lossy().into_owned();
    let after_spending = "Account(balance=7, name=\"ann spent\", log=[\"start\", \"spent\"])";
    let after_leaving = "Account(balance=109, name=\"ann spent\", log=[\"start\", \"spent\"])";
    let wanted = format!(
        "1\n{after_spending}\nNope(why=\"ann spent spent is short\")\n{after_spending}\n\
         Nope(why=\"both undone\")\n{after_spending}\nAccount(balance=20, name=\"bob\", log=[])\n\
         107 1\n{after_leaving}\n114\n{after_leaving}\n-16\n{after_leaving}\n"
    );
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&output).output_within()?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            wanted,
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Every way of changing what a lock holds is put back when the block fails: numbers of each size,
/// strings, items, what's inside a field in place or in a list, dicts, a whole field, and what's
/// changed through `ref`.
#[test]
fn a_failure_out_of_a_lock_puts_back_every_kind_of_write() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_failure_every_write",
        include_str!("fixtures/lock_failure_every_write.fen"),
        "kinds that kept a change: 0\n",
    )
}

/// A whole field holding an enum with blocks in it, or a struct, optional or tuple holding one, is
/// put back when the block fails, whichever case it held and whichever it was given.
#[test]
fn a_failure_out_of_a_lock_puts_back_an_enum_field() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_failure_enum_fields",
        include_str!("fixtures/lock_failure_enum_fields.fen"),
        "kinds that kept a change: 0\n",
    )
}

/// A `str` parameter written under a lock counts as a string where it's kept, on a worker too.
#[test]
fn a_str_parameter_written_under_a_lock() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "str_under_a_lock",
        "struct Person:\n    name: str\n\ndef rename(person: shared Person, first: str, second: str):\n    lock person as ref current:\n        current.name = f\"{first}{second}\"\n\ndef main(stdout: Stdout):\n    shared person = Person(name=\"a\")\n    done = [at for at in parallel(range(2))]\n    rename(person, first=\"a name that is long enough \", second=\"to need a block of its own\")\n    lock person as held:\n        stdout.print(done.len(), held.name)\n",
        "2 a name that is long enough to need a block of its own\n",
    )
}

/// A crash inside `lock ... as ref` puts back everything the block changed, then gives back the
/// lock: here a handler counts, keeps its path and appends it, then crashes on `/crash`. The server
/// keeps serving, and the next request sees the value as it was before the crash: no `/crash` in
/// it, and a negative count put back as it was.
#[test]
fn a_crash_in_a_lock_puts_back_what_it_changed() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let source = r#"from std.net import Request, Response

struct Tally:
    names: list[str]
    count = -10
    last = ""

def respond(request: Request, tally: shared Tally) -> Response:
    lock tally as ref held:
        held.count += 1
        held.last = request.path
        held.names.append(request.path)
        if request.path == "/crash":
            empty: list[int] = []
            held.count += empty[3]
    lock tally as current:
        Response(status=200, body=f"{current.count} {current.last} {current.names}".encode())

def main(net: Net, stdout: Stdout):
    shared tally = Tally(names=[])
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve((request) => respond(request, tally), timeout=10s)
"#;
    let dir = folder("lock_rollback")?;
    let program = dir.join("serve.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("serve"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(
        Command::new(&executable)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    // What the server answers one request with, after the head, or nothing when it closes first.
    let ask = |path: &str| -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
        stream.write_all(format!("GET {path} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes())?;
        let mut heard = String::new();
        let _ = stream.read_to_string(&mut heard);
        Ok(heard.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned())
    };
    let answers: Vec<String> =
        ["/a", "/crash", "/b", "/crash", "/c"].into_iter().map(ask).collect::<std::io::Result<_>>()?;
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        answers,
        ["-9 /a [\"/a\"]", "", "-8 /b [\"/a\", \"/b\"]", "", "-7 /c [\"/a\", \"/b\", \"/c\"]"]
    );
    Ok(())
}

/// The server `http_requests_go_through_native_code` asks: it answers three requests with the
/// first line of each, and a 200 only for `/a`. It starts once the program is built, and gives up
/// a minute later, so a program that stops asking fails the test rather than leaving it waiting.
/// The next connection to a nonblocking listener, waiting until `deadline`.
fn next_connection(
    listener: &std::net::TcpListener,
    deadline: std::time::Instant,
) -> std::io::Result<std::net::TcpStream> {
    let conn = loop {
        match listener.accept() {
            Ok((conn, _)) => break conn,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return Err(std::io::Error::other("the program stopped asking"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    };
    conn.set_nonblocking(false)?;
    conn.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    Ok(conn)
}

/// The whole request, its body too: closing a connection with some of it unread resets it, and
/// on Windows the program can lose the answer to that.
fn whole_request(conn: &mut std::net::TcpStream) -> std::io::Result<String> {
    let mut asked = Vec::new();
    let mut chunk = [0; 4096];
    let mut wanted = None;
    while wanted.is_none_or(|wanted| asked.len() < wanted) {
        let read = std::io::Read::read(conn, &mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::other("the program closed a request part-way"));
        }
        asked.extend_from_slice(chunk.get(..read).unwrap_or_default());
        let text = String::from_utf8_lossy(&asked);
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text
                .get(..end)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, length)| length.trim().parse::<usize>().ok());
            wanted = Some(end + 4 + length.unwrap_or(0));
        }
    }
    Ok(String::from_utf8_lossy(&asked).into_owned())
}

fn answer_three_requests(listener: &std::net::TcpListener) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for _ in 0..3 {
        let mut conn = next_connection(listener, deadline)?;
        let asked = whole_request(&mut conn)?;
        let first = asked.lines().next().unwrap_or_default();
        let status = if first.contains("/a") { "200 OK" } else { "500 Oops" };
        let why = if asked.contains("X-Mine: yes") { "mine" } else { "plain" };
        let answer =
            format!("HTTP/1.1 {status}\r\nX-Why: {why}\r\nContent-Length: {}\r\n\r\n{first}", first.len());
        std::io::Write::write_all(&mut conn, answer.as_bytes())?;
    }
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
    listener.set_nonblocking(true)?;
    let source = format!(
        r#"from std.net import NetError, Response

def ask(net: Net, stdout: Stdout) fails NetError:
    mut answer = net.get("http://127.0.0.1:{port}/a", params={{"q": "x y"}}, headers={{"X-Mine": "yes"}}, timeout=5s)?
    stdout.print(answer.status, answer.ok(), answer.text(), answer.headers["x-why"], answer.body.len())
    answer = net.post("http://127.0.0.1:{port}/b", body="data")?
    stdout.print(answer.status, answer.ok(), answer.text(), answer.headers["x-why"])
    answer = net.request("put", "http://127.0.0.1:{port}/c", body="bytes".encode())?
    stdout.print(answer.status, answer.text())

def main(net: Net, stdout: Stdout):
    ask(net, stdout) else as error:
        stdout.print(error.message())
    _ = net.get("http://127.0.0.1:1/") else as refused:
        stdout.print(refused.message())
        Response(status=0)
    _ = net.get("not a url") else as not_a_url:
        stdout.print(not_a_url.message())
        Response(status=0)
"#
    );
    let dir = folder("http_native")?;
    let program = dir.join("http.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("http"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let server = std::thread::spawn(move || answer_three_requests(&listener));
    let ours = outcome(&Command::new(&executable).output_within()?);
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

/// `json=` sends a value as its JSON text, with `content-type: application/json` unless the call
/// says otherwise: a struct, a list with a type of its own, and a str, which is quoted.
#[test]
fn a_value_sent_as_json_reads_as_its_json_text() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = |port: u16| {
        format!(
            r#"from std.net import NetError

struct Draft:
    title: str
    tags: list[str]
    stars: int

def send(net: Net, stdout: Stdout) fails NetError:
    url = "http://127.0.0.1:{port}/notes"
    stdout.print(net.post(url, json=Draft(title="den", tags=["fox", "river"], stars=3))?.text())
    stdout.print(net.post(url, json=["a", "b"], headers={{"Content-Type": "application/vnd.mine+json"}})?.text())
    stdout.print(net.request("put", url, json="a \"quoted\" note")?.text())

def main(net: Net, stdout: Stdout):
    send(net, stdout) else as error:
        stdout.print(error.message())
"#
        )
    };
    let wanted = "application/json {\"title\": \"den\", \"tags\": [\"fox\", \"river\"], \"stars\": 3}\n\
                  application/vnd.mine+json [\"a\", \"b\"]\n\
                  application/json \"a \\\"quoted\\\" note\"\n";
    let dir = folder("json_body")?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let program = dir.join("post.fen");
    std::fs::write(&program, source(port))?;
    let program = program.to_string_lossy().into_owned();
    let server = std::thread::spawn(move || echo_three_bodies(&listener));
    let executable = dir.join(format!("post{}", std::env::consts::EXE_SUFFIX));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = outcome(&Command::new(&executable).output_within()?);
    let answered = server.join().map_err(|_| std::io::Error::other("the test's server panicked"))?;
    assert!(answered.is_ok(), "{answered:?}, and the program said {ran:?}");
    assert_eq!(ran.0, wanted, "{}", ran.1);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Answers three requests, each with its content type, a space and its body.
fn echo_three_bodies(listener: &std::net::TcpListener) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for _ in 0..3 {
        let mut conn = next_connection(listener, deadline)?;
        let asked = whole_request(&mut conn)?;
        let (head, body) = asked.split_once("\r\n\r\n").unwrap_or_default();
        let kind = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map_or("none", |(_, kind)| kind.trim());
        let echoed = format!("{kind} {body}");
        let answer = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{echoed}", echoed.len());
        std::io::Write::write_all(&mut conn, answer.as_bytes())?;
    }
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

/// A built program prints what it should and has none of the interpreter in it.
#[test]
fn a_built_program_has_no_interpreter_in_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("no-interpreter")?;
    let path = |file: &Path| file.display().to_string();
    let compile = |source: &Path, output: &Path| {
        crate::executables::fennec_command()
            .args(["build", "--llvm", &path(source), "-o", &path(output)])
            .env("FENNEC_HOME", &dir)
            .output_before(FENNEC_DEADLINE)
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
    let built = compile(&plain, &executable)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "[<a>] {\"key\": 1} 0\nhello\n\n");
    // A Windows executable has no symbols to look through.
    if !cfg!(windows) {
        let symbols = Command::new("nm").arg(&executable).output_within()?;
        assert!(symbols.status.success(), "{}", String::from_utf8_lossy(&symbols.stderr));
        let symbols = String::from_utf8_lossy(&symbols.stdout);
        assert!(symbols.contains("fennec_clock_now") || symbols.contains("fennec_print_block"), "{symbols}");
        // The interpreter's constructor. Rust writes each name as its length and the name. A looser
        // match finds `Interp` in the name of a small closure the linker folded together with the
        // native code's own copy, in a release build.
        assert!(!symbols.contains("6Interp11with_output"), "the interpreter is linked");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A release keeps LLVM's tools in `llvm/bin` beside `fennec`, and builds with them where there's
/// no Rust toolchain to ask: here, a copy of `fennec` and its runtimes with nothing but the
/// system's own folders on `PATH`. Without that folder, the same build can't find them.
#[test]
#[cfg(unix)]
fn a_build_finds_llvm_s_tools_beside_fennec() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("shipped")?;
    let fennec = Path::new(env!("CARGO_BIN_EXE_fennec"));
    for entry in std::fs::read_dir(fennec.parent().unwrap_or_else(|| Path::new(".")))? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name == "fennec" || name.starts_with("libfennec_runtime") {
            std::fs::copy(&path, dir.join(&name))?;
        }
    }
    let tools = fennec_llvm::Tools::find().map_err(std::io::Error::other)?;
    let program = dir.join("hello.fen");
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(\"built with what ships\")\n")?;
    let build = |home: &str| {
        Command::new(dir.join("fennec"))
            .args(["build", "--llvm", &program.to_string_lossy(), "-o", &dir.join("hello").to_string_lossy()])
            .env("PATH", "/usr/bin:/bin")
            .env("FENNEC_HOME", dir.join(home))
            .env_remove("FENNEC_LLVM_BIN")
            .output_before(FENNEC_DEADLINE)
    };
    let without = build("home-without")?;
    std::fs::create_dir_all(dir.join("llvm/bin"))?;
    for tool in ["opt", "llc", "llvm-link"] {
        std::os::unix::fs::symlink(tools.folder().join(tool), dir.join("llvm/bin").join(tool))?;
    }
    let with = build("home-with")?;
    let ran = Command::new(dir.join("hello")).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(!without.status.success(), "it built with no tools beside it and no toolchain");
    assert!(with.status.success(), "{}", String::from_utf8_lossy(&with.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "built with what ships\n");
    Ok(())
}

/// A server on this machine that answers each connection with a line 300 ms after it's made, as a
/// slow web service would, and counts the most connections it held open at once.
fn slow_server() -> std::io::Result<(u16, std::sync::Arc<std::sync::atomic::AtomicUsize>)> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let seen = Arc::clone(&most);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (open, most) = (Arc::clone(&open), Arc::clone(&most));
            std::thread::spawn(move || {
                let now = open.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(300));
                open.fetch_sub(1, Ordering::SeqCst);
                let _ = std::io::Write::write_all(&mut &stream, b"pong\n");
            });
        }
    });
    Ok((port, seen))
}

/// A native `parallel` over more fetches than threads: each item parks while it connects and while
/// its line is on the way, so two threads hold more than two connections open at once, which
/// blocking waits never could.
#[test]
fn fetches_in_parallel_wait_at_once_on_a_few_threads() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let (port, most) = slow_server()?;
    let dir = folder("parallel-fetches")?;
    let program = dir.join("fetches.fen");
    std::fs::write(
        &program,
        format!(
            "from std.net import NetError

def fetch(net: Net, port: int) -> str fails NetError:
    with net.connect(\"127.0.0.1\", port)? as conn:
        return conn.read_line()? else \"nothing\"
    \"nothing\"

def main(net: Net, stdout: Stdout):
    items = [i for i in range(16)]
    lines = [fetch(net, {port}).expect(\"it answers\") for i in parallel(items)]
    stdout.print(lines.len(), lines[0], lines[15])
"
        ),
    )?;
    let executable = dir.join("fetches");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).env("FENNEC_THREADS", "2").output_within()?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "16 pong pong\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let most = most.load(std::sync::atomic::Ordering::SeqCst);
    assert!(most > 2, "the server held at most {most} connections at once, from 2 threads");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A server's round that ends by `continue` closes its connection with no answer, and one that
/// ends by `break` stops the server: it takes no more connections, the running rounds finish, and
/// the loop ends normally, so what follows it runs.
#[test]
fn a_servers_rounds_continue_and_break_natively() -> std::io::Result<()> {
    use std::io::{BufRead as _, Write as _};
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("rounds-continue-break")?;
    let program = dir.join("rounds.fen");
    std::fs::write(
        &program,
        "from std.net import NetError

def main(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            if line == \"skip\":
                continue
            if line == \"stop\":
                break
            conn.write(f\"{line}\\n\")?
    stdout.print(\"served\")
",
    )?;
    let executable = dir.join("rounds");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_THREADS", "2")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    // What the server answers a line with, or nothing when it closes first.
    let ask = |line: &str| -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
        stream.write_all(format!("{line}\n").as_bytes())?;
        let mut back = String::new();
        let _ = std::io::BufReader::new(stream).read_line(&mut back);
        Ok(back)
    };
    let answers: Vec<String> =
        ["a", "skip", "b", "stop"].into_iter().map(ask).collect::<std::io::Result<_>>()?;
    assert_eq!(answers, ["a\n", "", "b\n", ""]);
    // After `break`, the loop ends, and the program goes on to its end and stops by itself.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let ended = loop {
        if let Some(status) = server.0.try_wait()? {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "the server went on after `break`");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(ended.success(), "{ended}");
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A built server whose rounds crash on purpose, for every tenth request: each crash ends only its
/// own round, closing its connection, and the server answers the other nine.
#[test]
fn a_server_whose_rounds_crash_answers_the_rest_natively() -> std::io::Result<()> {
    use std::io::{BufRead as _, Write as _};
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("crashing-rounds")?;
    let program = dir.join("crashing.fen");
    std::fs::write(
        &program,
        "from std.net import NetError

def main(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            if line == \"crash\":
                nothing: list[int] = []
                stdout.print(nothing[3])
            conn.write(f\"{line}\\n\")?
",
    )?;
    let executable = dir.join("crashing");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Command::new(&executable)
        .env("FENNEC_THREADS", "2")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut printed = std::io::BufReader::new(server.stdout.take().expect("its output"));
    let mut port = String::new();
    printed.read_line(&mut port)?;
    let port: u16 = port.trim().parse().unwrap_or(0);
    let (mut answered, mut closed) = (0, 0);
    for request in 1_u32..=30 {
        let line = if request.is_multiple_of(10) { "crash".to_owned() } else { format!("request {request}") };
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        stream.write_all(format!("{line}\n").as_bytes())?;
        let mut back = String::new();
        let read = std::io::BufReader::new(stream).read_line(&mut back).unwrap_or(0);
        if read == 0 {
            closed += 1;
        } else {
            assert_eq!(back, format!("{line}\n"));
            answered += 1;
        }
    }
    // A round's connection closes as it ends, which can be before it's written why, so the server
    // is given time to say so for the last one.
    let said = server.stderr.take().expect("its errors");
    let (sender, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(said).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut errors = String::new();
    while errors.matches("error:").count() < 3 {
        let Ok(line) = lines.recv_timeout(std::time::Duration::from_secs(10)) else { break };
        errors.push_str(&line);
        errors.push('\n');
    }
    server.kill()?;
    server.wait()?;
    assert_eq!((answered, closed), (27, 3), "{errors}");
    assert_eq!(errors.matches("error:").count(), 3, "each crash says why: {errors}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// On one thread, a round only computing, for far longer than the test waits, still lets another
/// connection's round answer: its loop's turns tick, and a tick lets the other tasks run. It once
/// held the thread until it finished, so the second round waited for all of it.
#[test]
fn a_round_only_computing_lets_the_others_answer() -> std::io::Result<()> {
    use std::io::{BufRead as _, Write as _};
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("ticking-rounds")?;
    let program = dir.join("ticking.fen");
    std::fs::write(
        &program,
        "from std.net import NetError

def churn(turns: int) -> int:
    mut x = 1
    mut n = 0
    while n < turns:
        x = (x * 31 + 7) % 1000003
        n += 1
    x

def main(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            if line == \"spin\":
                conn.write(f\"spun {churn(1_000_000_000_000)}\\n\")?
            else:
                conn.write(\"pong\\n\")?
",
    )?;
    let executable = dir.join("ticking");
    let mut command = crate::executables::fennec_command();
    command.args([
        "build",
        "--llvm",
        "--release",
        &program.to_string_lossy(),
        "-o",
        &executable.to_string_lossy(),
    ]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_THREADS", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let mut printed = std::io::BufReader::new(server.0.stdout.take().expect("its output"));
    let mut port = String::new();
    printed.read_line(&mut port)?;
    let port: u16 = port.trim().parse().unwrap_or(0);
    // The spinning round has started before the ping comes: its line is read, so it's computing.
    let mut spinning = std::net::TcpStream::connect(("127.0.0.1", port))?;
    spinning.write_all(b"spin\n")?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut ping = std::net::TcpStream::connect(("127.0.0.1", port))?;
    ping.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    ping.write_all(b"ping\n")?;
    let mut back = String::new();
    let read = std::io::BufReader::new(ping).read_line(&mut back);
    drop(spinning);
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(back, "pong\n", "the ping wasn't answered while the other round computed: {read:?}");
    Ok(())
}

/// A program's resident memory in KB: `ps` gives it on Unix, and `tasklist` on Windows, as
/// `"leaks.exe","1234","Console","1","12,345 K"`. No reading fails the test, so a check of it can't
/// pass by reading nothing.
fn resident_kb(id: u32) -> std::io::Result<u64> {
    let id = id.to_string();
    let said = if cfg!(windows) {
        Command::new("tasklist").args(["/FI", &format!("PID eq {id}"), "/FO", "CSV", "/NH"]).output()?
    } else {
        Command::new("ps").args(["-o", "rss=", "-p", &id]).output()?
    };
    let said = String::from_utf8_lossy(&said.stdout);
    let kb = said.trim().rsplit("\",\"").next().unwrap_or("");
    let digits: String = kb.chars().filter(char::is_ascii_digit).collect();
    let kb = digits.parse().unwrap_or(0);
    assert!(kb > 0, "no memory reading for the program: {said}");
    Ok(kb)
}

/// A round that crashes lets go of everything its functions own on the way out, as a failure
/// does: 10,000 crashing rounds, each holding a string, a list and a dict, leave the server's
/// memory where it was. A crash once left them all behind, about 1.8 KB a round.
#[test]
fn rounds_that_crash_keep_nothing_of_theirs() -> std::io::Result<()> {
    use std::io::{BufRead as _, Write as _};
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("crash-leaks")?;
    let program = dir.join("leaks.fen");
    std::fs::write(
        &program,
        "from std.net import NetError

def overflowed(n: int) -> int:
    big = 9223372036854775000
    big + n * 1000

def answer(line: str) -> str:
    words = [f\"{line} {at}\" for at in range(8)]
    seen = {word: word.len() for word in words}
    said = f\"{words} {seen.len()}\"
    f\"{overflowed(line.len())} {said}\"

def main(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            conn.write(answer(line))?
",
    )?;
    let executable = dir.join("leaks");
    let mut command = crate::executables::fennec_command();
    command.args([
        "build",
        "--llvm",
        "--release",
        &program.to_string_lossy(),
        "-o",
        &executable.to_string_lossy(),
    ]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_THREADS", "2")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let mut printed = std::io::BufReader::new(server.0.stdout.take().expect("its output"));
    let mut port = String::new();
    printed.read_line(&mut port)?;
    let port: u16 = port.trim().parse().unwrap_or(0);
    // Four at a time, so 10,000 rounds take seconds.
    let crash = |rounds: u32| -> std::io::Result<()> {
        let each = || -> std::io::Result<()> {
            for _ in 0..rounds / 4 {
                let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
                stream.write_all(b"a line long enough to need a block of its own\n")?;
                let mut back = String::new();
                let read = std::io::BufReader::new(stream).read_line(&mut back).unwrap_or(0);
                assert_eq!(read, 0, "a crashing round answered: {back}");
            }
            Ok(())
        };
        std::thread::scope(|scope| {
            let clients: Vec<_> = (0..4).map(|_| scope.spawn(each)).collect();
            clients.into_iter().try_for_each(|client| client.join().unwrap_or(Ok(())))
        })
    };
    let resident = || resident_kb(server.0.id());
    // What the first rounds make for good, like the runtime's pools, is there before the count.
    crash(2_000)?;
    let before = resident()?;
    crash(10_000)?;
    let after = resident()?;
    // Windows can't remove a program while it runs.
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    // 10,000 leaked rounds would be about 18 MB.
    assert!(after < before + 4_096, "the server grew from {before} KB to {after} KB");
    Ok(())
}

/// What the next test builds: tasks that read an inbox, fail, crash, and are launched in a loop.
const LAUNCHED: &str = "from std.concurrent import Closed, Crashed, Inbox, Timeout

enum Sound:
    Play(name: str, volume: float)
    StopAll

error Muted:
    why: str

def audio(inbox: Inbox[Sound], stdout: Stdout) fails Muted:
    for sound in inbox:
        match sound:
            case .Play(name, volume):
                if volume > 1.0: fail Muted(why=f\"{name} too loud\")
                stdout.print(f\"play {name}\")
            case .StopAll:
                stdout.print(\"stop\")

def tick(stdout: Stdout):
    stdout.print(\"tick\")

def crashes(xs: list[int], stdout: Stdout):
    stdout.print(xs[5])

def loud(stdout: Stdout) fails Closed | Crashed | Muted | Timeout:
    with launch((inbox) => audio(inbox, stdout)?, inbox_size=4)? as sounds:
        sounds.send(.Play(name=\"boom\", volume=3.0))?

def crashing(stdout: Stdout) fails Closed | Crashed | Timeout:
    xs = [1, 2]
    with launch(() => crashes(xs, stdout))?:
        pass

def counts(inbox: Inbox[int], stdout: Stdout):
    mut sum = 0
    for n in inbox:
        sum += n
    stdout.print(\"sum\", sum)

def main(stdout: Stdout) fails Closed | Crashed | Muted | Timeout:
    with launch((inbox) => audio(inbox, stdout)?, stop_after=5s, inbox_size=4)? as sounds:
        sounds.send(.Play(name=\"jump\", volume=0.8))?
        sounds.send(.StopAll)?
    with launch(() => tick(stdout))?:
        pass
    loud(stdout) else as too_loud:
        stdout.print(\"failed:\", too_loud)
    crashing(stdout) else as crashed:
        stdout.print(\"failed:\", crashed)
    for round in range(200):
        with launch((inbox) => counts(inbox, stdout), inbox_size=8)? as numbers:
            for k in range(10):
                numbers.send(k * round)?
";

/// A launched task runs beside the block that launched it, on the runtime's threads, with what
/// it was given copied for it: it reads its inbox until the block ends and closes it, and the
/// block waits for it. Its failure passes up out of the block as itself, a crash as `Crashed`.
#[test]
fn launched_tasks_run_beside_their_block_natively() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("launched")?;
    let program = dir.join("launched.fen");
    std::fs::write(&program, LAUNCHED)?;
    let executable = dir.join("launched");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    let printed = String::from_utf8_lossy(&ran.stdout);
    let sums = (0..200).fold(String::new(), |mut sums, round| {
        let _ = std::fmt::Write::write_fmt(&mut sums, format_args!("sum {}\n", 45 * round));
        sums
    });
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(
        printed,
        format!(
            "play jump\nstop\ntick\nfailed: Muted(why=\"boom too loud\")\nfailed: Crashed(why=\"the task crashed\")\n{sums}"
        )
    );
    Ok(())
}

/// A channel that couldn't hold a message fails where it's made, rather than quietly holding one.
#[test]
fn a_channel_of_no_size_fails_where_its_made() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("channel-no-size")?;
    let program = dir.join("none.fen");
    std::fs::write(
        &program,
        "from std.concurrent import channel\n\ndef main(stdout: Stdout):\n    with channel[int](size=0) as ch:\n        stdout.print(\"made\")\n",
    )?;
    let executable = dir.join("none");
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(!ran.status.success());
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(said.contains("a channel has to hold at least 1 message, but its size is 0"), "{said}");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "");
    Ok(())
}

/// Each `with launch` in a function has its own `stop_after`, whatever another one was given. A
/// sleep cut short by the stop ends the task quietly, so the sleeper never prints; if the second
/// `stop_after` were lost, it would sleep the full 3 seconds and print False.
#[test]
fn the_second_launch_in_a_function_keeps_its_stop_after() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("second_stop_after")?;
    let program = dir.join("stops.fen");
    std::fs::write(
        &program,
        "from std.concurrent import Crashed\n\ndef sleeper(clock: Clock, stdout: Stdout):\n    start = clock.time()\n    clock.sleep(3s)\n    stdout.print((clock.time() - start).nanos < 2_000_000_000)\n\ndef other(clock: Clock):\n    clock.sleep(10ms)\n\ndef main(clock: Clock, stdout: Stdout) fails Crashed:\n    with launch(() => other(clock))?:\n        with launch(() => sleeper(clock, stdout), stop_after=20ms)?:\n            pass\n    stdout.print(\"done\")\n",
    )?;
    let executable = dir.join("stops");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "done\n", "{}", String::from_utf8_lossy(&ran.stderr));
    Ok(())
}

/// A channel to `.Every` gives every listener every message. One that falls behind fails loudly
/// unless it asked for `.DropOldest`, when it goes on from the oldest message kept.
#[test]
fn a_channel_to_every_listener() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("channel_every")?;
    let program = dir.join("every.fen");
    std::fs::write(&program, include_str!("fixtures/channel_every.fen"))?;
    let executable = dir.join("every");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "[5050, 5050]\n297\nfailed: Crashed(why=\"the task crashed\")\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(String::from_utf8_lossy(&ran.stderr).contains("when_behind=.DropOldest"));
    Ok(())
}

/// A generic function opens a channel of a generic struct of its own type parameter, as a pool
/// of `T` does: `channel[Box[T]]` is written in once for each copy, of each kind.
#[test]
fn a_channel_of_a_generic_struct_in_a_generic_function_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "generic_channel",
        r#"from std.concurrent import channel

struct Box[T]:
    value: T

def boxed[T](value: T, fallback: T) -> T:
    with channel[Box[T]](size=1) as boxes:
        boxes.send(Box[T](value=value)) else return fallback
        got = boxes.receive() else return fallback
        return got.value
    fallback

def main(stdout: Stdout):
    stdout.print(boxed(3, fallback=0), boxed("fox", fallback=""), boxed([1, 2], fallback=[]))
"#,
    )?;
    assert_eq!(printed, "3 fox [1, 2]\n");
    Ok(())
}

/// A channel's messages cross it as the runtime holds them, of each kind: a number, a string long
/// enough for a block, a struct held in place, and a list. A receive or a send that waits past its
/// timeout fails as `Timeout`.
#[test]
fn channels_carry_each_kind_of_message_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "channels",
        "from std.concurrent import Closed, Timeout, channel
from std.time import Duration

struct Job:
    name: str
    weight: int

def numbers(stdout: Stdout) fails Closed | Timeout:
    with channel[int](size=4) as ch:
        ch.send(7)?
        ch.send(8)?
        stdout.print(ch.receive()?, ch.receive()?)

def signs(stdout: Stdout) fails Closed | Timeout:
    with channel[int](size=2) as ints:
        ints.send(-7)?
        stdout.print(ints.receive()?)
    with channel[float](size=2) as floats:
        floats.send(-1.5)?
        stdout.print(floats.receive()?)
    with channel[u64](size=2) as big:
        half: u64 = 9223372036854775807
        big.send(half + half + 1)?
        stdout.print(big.receive()?)
    with channel[f32](size=2) as small:
        tenth: f32 = -0.1
        small.send(tenth)?
        stdout.print(small.receive()?)
    with channel[int](size=1) as waits:
        got: int? = waits.receive(timeout=Duration(nanos=-5)) else as problem:
            stdout.print(\"at once:\", problem)
            None
        if got is not None:
            stdout.print(\"got\", got)

def words(stdout: Stdout) fails Closed | Timeout:
    with channel[str](size=4) as ch:
        ch.send(\"hi\")?
        ch.send(\"a string long enough to live in a block of its own\")?
        stdout.print(ch.receive()?, ch.receive()?.len())

def jobs(stdout: Stdout) fails Closed | Timeout:
    with channel[Job](size=2) as ch:
        ch.send(Job(name=\"first\", weight=3))?
        got = ch.receive()?
        stdout.print(got.name, got.weight)

def lists(stdout: Stdout) fails Closed | Timeout:
    with channel[list[float]](size=1) as ch:
        ch.send([1.5, 2.5])?
        stdout.print(ch.receive()?)

def waits(stdout: Stdout) fails Closed | Timeout:
    with channel[int](size=1) as ch:
        got: int? = ch.receive(timeout=Duration(nanos=2_000_000)) else as problem:
            stdout.print(\"empty:\", problem)
            None
        if got is not None:
            stdout.print(\"got\", got)
        ch.send(1)?
        ch.send(2, timeout=Duration(nanos=2_000_000)) else as full:
            stdout.print(\"full:\", full)

def main(stdout: Stdout) fails Closed | Timeout:
    numbers(stdout)?
    signs(stdout)?
    words(stdout)?
    jobs(stdout)?
    lists(stdout)?
    waits(stdout)?
",
    )?;
    assert_eq!(
        printed,
        "7 8\n-7\n-1.5\n18446744073709551615\n-0.1\nat once: Timeout(waited=Duration(nanos=-5))\nhi 50\nfirst 3\n[1.5, 2.5]\nempty: Timeout(waited=Duration(nanos=2000000))\nfull: Timeout(waited=Duration(nanos=2000000))\n"
    );
    Ok(())
}

/// A round that fails, by `fail` or `?`, stops the server, and its failure passes up out of the
/// loop to the function serving, which handles it. The rounds before it were answered. Natively a
/// round's failure once only ended that round, as a crash does.
#[test]
fn a_server_round_that_fails_passes_its_failure_up_natively() -> std::io::Result<()> {
    use std::io::{BufRead as _, Write as _};
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("failing-rounds")?;
    let program = dir.join("failing.fen");
    std::fs::write(
        &program,
        "from std.net import NetError

error Refused:
    line: str

def checked(line: str) -> str fails Refused:
    if line == \"refuse\":
        fail Refused(line=line)
    line

def serve(net: Net, stdout: Stdout) -> int fails NetError | Refused:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            conn.write(f\"{checked(line)?}\\n\")?
    0

def main(net: Net, stdout: Stdout):
    _ = serve(net, stdout) else as error:
        stdout.print(\"failed:\", error)
        return
    stdout.print(\"served\")
",
    )?;
    let executable = dir.join("failing");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_THREADS", "2")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    );
    let mut printed = std::io::BufReader::new(server.0.stdout.take().expect("its output"));
    let mut port = String::new();
    printed.read_line(&mut port)?;
    let port: u16 = port.trim().parse().unwrap_or(0);
    let ask = |line: &str| -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        stream.write_all(format!("{line}\n").as_bytes())?;
        let mut back = String::new();
        std::io::BufReader::new(stream).read_line(&mut back).unwrap_or(0);
        Ok(back)
    };
    for request in 1..=3 {
        assert_eq!(ask(&format!("request {request}"))?, format!("request {request}\n"));
    }
    assert_eq!(ask("refuse")?, "");
    // It ends by itself: nothing here kills it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = server.0.try_wait()? {
            break status;
        }
        assert!(std::time::Instant::now() <= deadline, "the server kept going after a round failed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut printed, &mut rest)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(status.code(), Some(0), "{rest}");
    assert_eq!(rest, "failed: Refused(line=\"refuse\")\n");
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
        let built = command.output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        Ok(executable)
    };
    let has_pool = |executable: &Path| -> std::io::Result<bool> {
        let symbols = Command::new("nm").arg(executable).output_within()?;
        Ok(String::from_utf8_lossy(&symbols.stdout).contains("crossbeam_deque"))
    };
    let wanted = "199999 2\n";
    let every_core = built(None, "every-core")?;
    let ran = Command::new(&every_core).env("FENNEC_THREADS", "4").output_within()?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), wanted);
    let one_thread = built(Some("0"), "one-thread")?;
    assert_eq!(String::from_utf8_lossy(&Command::new(&one_thread).output_within()?.stdout), wanted);
    // A Windows executable has no symbols to look through.
    if !cfg!(windows) {
        assert!(has_pool(&every_core)?, "the program has no pool to run on");
        assert!(!has_pool(&one_thread)?, "asked for one thread, the program has a pool");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A program that uses `parallel`, built by a `fennec` whose threads runtime is from another
/// build, fails saying so, rather than linking the plain runtime and running on one thread.
#[test]
fn a_stale_threads_runtime_fails_the_build_rather_than_running_on_one_thread() -> std::io::Result<()> {
    if !ready() || !runtime_built() || !crate::executables::runtime_beside_fennec() {
        return Ok(());
    }
    let dir = folder("threads-stale")?;
    let fennec = Path::new(env!("CARGO_BIN_EXE_fennec"));
    let mine = dir.join("bin");
    std::fs::create_dir_all(&mine)?;
    let fennec_here = mine.join(fennec.file_name().unwrap_or_default());
    if std::fs::hard_link(fennec, &fennec_here).is_err() {
        std::fs::copy(fennec, &fennec_here)?;
    }
    let plain = fennec.with_file_name(crate::executables::runtime_file());
    std::fs::copy(&plain, mine.join(crate::executables::runtime_file()))?;
    let stale = ["fennec-runtime-", "build:", &"0".repeat(64)].concat();
    std::fs::write(mine.join("libfennec_runtime_threads.a"), stale)?;
    let program = dir.join("squares.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    squares = [x * x for x in parallel([1, 2, 3])]\n    stdout.print(sum(squares))\n",
    )?;
    let built = Command::new(&fennec_here)
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &dir.join("squares").to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    let said = String::from_utf8_lossy(&built.stderr).into_owned();
    std::fs::remove_dir_all(&dir)?;
    assert!(!built.status.success(), "built with a stale threads runtime");
    assert!(
        said.contains("libfennec_runtime_threads.a") && said.contains("another build of Fennec"),
        "{said}"
    );
    Ok(())
}

/// JSON and TOML of types that save themselves go through std's readers and writers in Fennec,
/// std's collections among them, so a program reading and writing them links with no interpreter
/// in it.
#[test]
fn json_and_toml_of_plain_types_need_no_interpreter() -> std::io::Result<()> {
    if !ready() {
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
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "[{\"x\": 1, \"tags\": [\"a\"]}] [Spot(x=1, tags=[\"a\"])]\nSpot(x=2, tags=[])\n\
         {\"cells\": [[1, 2]], \"scores\": {\"a\": 1, \"b\": 2}, \"seen\": [1, 3], \"queue\": [\"go\"]} True\n"
    );
    Ok(())
}

/// A tuple saves as an array of its items, as in Python's `json.dumps`, and loads back, through
/// std's readers and writers in Fennec, with no interpreter linked: in a list, in a field, inside
/// another tuple, in an enum's case, as a dict's and a `SortedMap`'s keys, with an item that's
/// always `None`, and through TOML and `std.pack` too. An array of the wrong length doesn't load.
#[test]
fn tuples_save_and_load_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("tuples-lean")?;
    let program = dir.join("tuples.fen");
    std::fs::write(
        &program,
        "\
from std import json, pack, toml
from std.collections import SortedMap

enum Order:
    Hold
    Move(to: (int, int))

struct Path:
    steps: list[(int, str)]
    at: (int, (int, int))

def main(stdout: Stdout):
    path = Path(steps=[(1, \"a\"), (2, \"b\")], at=(3, (4, 5)))
    text = json.dumps(path)
    stdout.print(text)
    stdout.print(json.loads[Path](text) else Path(steps=[], at=(0, (0, 0))))
    orders = [Order.Hold, Order.Move(to=(2, 3))]
    stdout.print(json.dumps(orders), (json.loads[list[Order]](json.dumps(orders)) else []) == orders)
    corners = {(0, 0): \"start\", (3, 4): \"end\"}
    stdout.print(json.dumps(corners), (json.loads[dict[(int, int), str]](json.dumps(corners)) else {}) == corners)
    mut by_side = SortedMap[(int, int), str]()
    by_side[(2, 0)] = \"b\"
    by_side[(1, 0)] = \"a\"
    stdout.print(json.dumps(by_side))
    stdout.print(json.dumps((1, None, \"x\")))
    stdout.print(json.loads[list[(int, int)]](\"[[1, 2, 3]]\") else [(0, 0)])
    stdout.print((toml.loads[Path](toml.dumps(path)) else path) == path)
    stdout.print((pack.loads[Path](pack.dumps(path)) else Path(steps=[], at=(0, (0, 0)))) == path)
",
    )?;
    let executable = dir.join("tuples");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "{\"steps\": [[1, \"a\"], [2, \"b\"]], \"at\": [3, [4, 5]]}\n\
         Path(steps=[(1, \"a\"), (2, \"b\")], at=(3, (4, 5)))\n\
         [\"Hold\", {\"Move\": {\"to\": [2, 3]}}] True\n\
         [[[0, 0], \"start\"], [[3, 4], \"end\"]] True\n\
         [[[1, 0], \"a\"], [[2, 0], \"b\"]]\n\
         [1, null, \"x\"]\n\
         [(0, 0)]\n\
         True\n\
         True\n"
    );
    Ok(())
}

/// A tuple of a kit module's type, written with its module's name, and of an optional item, saves
/// and loads natively: the checker and lowering name its functions from the same type.
#[test]
fn tuples_of_a_kit_s_types_save_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("tuples-kit")?;
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(dir.join("kit.toml"), "name = \"marks\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")?;
    std::fs::write(
        dir.join("src/shapes.fen"),
        "\
export struct Spot:
    x: int

export struct Held:
    pair: (Spot, (int?, str))
",
    )?;
    std::fs::write(
        dir.join("src/main.fen"),
        "\
import shapes
from shapes import Spot
from std import json, pack

# Each optional comes from here: a list of `(Spot, int)` widened to `(Spot, int?)` isn't lowered
# yet.
def counted(n: int) -> int?:
    if n > 0:
        return n
    None

def main(stdout: Stdout):
    marks: list[(Spot, int?)] = [(Spot(x=1), counted(2)), (Spot(x=3), counted(0))]
    text = json.dumps(marks)
    stdout.print(text)
    stdout.print((json.loads[list[(Spot, int?)]](text) else []) == marks)
    held = shapes.Held(pair=(Spot(x=5), (counted(0), \"a\")))
    saved = json.dumps(held)
    stdout.print(saved, (json.loads[shapes.Held](saved) else held) == held)
    stdout.print((pack.loads[shapes.Held](pack.dumps(held)) else shapes.Held(pair=(Spot(x=0), (counted(1), \"\")))) == held)
",
    )?;
    let executable = dir.join("marks");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &dir.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", dir.join("home"))
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "[[{\"x\": 1}, 2], [{\"x\": 3}, null]]\n\
         True\n\
         {\"pair\": [{\"x\": 5}, [null, \"a\"]]} True\n\
         True\n"
    );
    Ok(())
}

/// A type's own function, like `Unpacker.of`, can be called through its module's name.
#[test]
fn a_static_method_through_a_module_name_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "module_static",
        r"from std import pack

def main(stdout: Stdout) fails ParseError:
    mut packer = pack.Packer()
    packer.add(3)
    packer.add(4)
    mut values = pack.Unpacker.of(packer.bytes())
    stdout.print(values.next[int]()?, values.next[int]()?, values.next[int]()?)
",
    )?;
    assert_eq!(printed, "3 4 None\n");
    Ok(())
}

/// A tuple saved only through `std.pack`, with nothing routed through JSON, saves natively: its
/// saving isn't trimmed away as unreached. And one saved before what holds it is known, `pairs`
/// here until its `append`, is what it holds once the check is done.
#[test]
fn a_tuple_saved_only_through_pack_saves_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("tuples-later")?;
    let program = dir.join("later.fen");
    std::fs::write(
        &program,
        "\
from std import pack

def main(stdout: Stdout):
    mut pairs = []
    saved = pack.dumps(pairs)
    pairs.append((1, \"a\"))
    stdout.print(pairs, saved.len() > 0, pack.dumps(pairs).len() > saved.len())
",
    )?;
    let executable = dir.join("later");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "[(1, \"a\")] True True\n");
    Ok(())
}

/// The reading half: a list of tuples saved through `std.pack` reads back as itself natively,
/// empty or not, with negative numbers and text that isn't ASCII, and bytes of another type
/// fail to read rather than reading as something. It's a program of its own, since `loads`
/// names its type and would settle what the program above leaves to be settled.
#[test]
fn a_list_of_tuples_reads_back_through_pack_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("tuples-loaded")?;
    let program = dir.join("loaded.fen");
    std::fs::write(
        &program,
        "\
from std import pack

def main(stdout: Stdout):
    pairs: list[(int, str)] = [(1, \"a\"), (-7, \"fén\"), (0, \"\")]
    empty: list[(int, str)] = []
    back = pack.loads[list[(int, str)]](pack.dumps(pairs)) else []
    stdout.print(back, back == pairs)
    stdout.print((pack.loads[list[(int, str)]](pack.dumps(empty)) else pairs) == empty)
    stdout.print((pack.loads[list[(int, str)]](pack.dumps([1, 2])) else empty) == empty)
",
    )?;
    let executable = dir.join("loaded");
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()])
        .env("FENNEC_HOME", &dir)
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "[(1, \"a\"), (-7, \"fén\"), (0, \"\")] True\nTrue\nTrue\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
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

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as why:
        return why.message
    "read"

def main(stdout: Stdout):
    ash = Unit(name="Ash \"the\" fox\n", level=3, kind=.Owl(wise=True), pet=None, at=Spot(x=1), tags=["a", "é"], stats={"hp": 10})
    text = json.dumps(ash)
    stdout.print(text)
    stdout.print(json.dumps([ash, ash], indent=2))
    back = json.loads[Unit](text) else ash
    stdout.print(back == ash, json.dumps(back) == text)
    stdout.print(said(() => json.loads[Unit]("{\"name\": \"a\"}")?))
    stdout.print(said(() => json.loads[Unit]("{\"name\": 1, \"level\": 2}")?))
    stdout.print(said(() => json.loads[Unit]("{\"name\": \"a\", \"at\": {\"x\": 1, \"z\": 2}, \"extra\": 1}")?))
    stdout.print(said(() => json.loads[Unit]("[1, 2")?))
    stdout.print(said(() => json.loads[list[int]]("[1, \"two\"]")?))
    stdout.print(json.loads[Spot]("{\"x\": 1, \"x\": 2}") else Spot(x=0))
    spot_text = toml.dumps(Spot(x=4))
    stdout.print(spot_text)
    stdout.print(toml.loads[Spot](spot_text) else Spot(x=0))
    stdout.print(said(() => toml.loads[Spot]("x = \"a\"")?))
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

def said(run: () -> Spot fails ParseError) -> str:
    _ = run() else as why:
        return why.message
    "read"

def main(stdout: Stdout):
    stdout.print(said(() => json.loads[Spot]("{\"x\": \"one\", \"x\": 2}")?))
    stdout.print(said(() => json.loads[Spot]("{\"y\": \"no\", \"x\": \"one\"}")?))
"#,
    )?;
    assert_eq!(ours, "`x` should be an int, but it's a str\n`y` should be a float, but it's a str\n");
    assert_eq!(jit, "read\n`x` should be an int, but it's a str\n");
    Ok(())
}

/// A list of a union's values, whose members are generic or tuples, prints each value as itself,
/// as the interpreter does, from a literal or from fields, in `print` and in an f-string.
#[test]
fn lists_of_unions_with_generic_members_print_their_values() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "union_lists",
        r#"struct Box[T]:
    value: T

def main(stdout: Stdout):
    words: Box[list[int] | str] = Box(value="kit")
    lists: Box[list[int] | str] = Box(value=[1, 2])
    stdout.print([words.value, lists.value])
    stdout.print(f"{[words.value, lists.value]}")
    both: list[list[int] | str] = ["a", [3]]
    stdout.print(both)
    pairs: list[(int, str) | bool] = [(1, "one"), True]
    stdout.print(pairs)
"#,
    )
}

/// A union's method runs the one of the type its value holds, natively: on the union, in each
/// `match` case and in a loop over a list of them. A `with` source does too once it's narrowed
/// with `is`, where each branch's `store` is its own type.
#[test]
fn a_unions_methods_and_narrowed_sources_run_the_member_it_holds() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "union_members_run",
        r#"error Nope:
    why: str

struct Transaction:
    log: str

struct Lite:
    name: str

    def run(self, text: str) -> int fails Nope:
        text.len()

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") else:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        text.len() * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") else:
            pass

type Store = Lite | Postgres

struct Mem:
    size: int

    def run(self, text: str) -> int fails Nope:
        text.len() * 3

type Three = Lite | Postgres | Mem

def saved(store: Store) -> str fails Nope:
    if store is Lite:
        with store.transaction()? as tx:
            count = store.run("abc")?
            return f"{tx.log} {count}"
    else:
        with store.transaction()? as tx:
            count = store.run("abcd")?
            return f"{tx.log} {count}"
    ""

def ran(store: Three) -> int fails Nope:
    match store:
        case Lite(name=_):
            return store.run("a")?
        case Postgres(host=_):
            return store.run("a")?
        case Mem(size=_):
            return store.run("a")?

def main(stdout: Stdout) fails Nope:
    stdout.print(saved(Lite(name="a"))?, saved(Postgres(host="b"))?)
    stdout.print(ran(Lite(name="a"))?, ran(Postgres(host="b"))?, ran(Mem(size=1))?)
    stores: list[Three] = [Lite(name="a"), Postgres(host="b"), Mem(size=1)]
    for store in stores:
        stdout.print(store.run("ab")?)
"#,
        "lite 3 pg 8\n1 2 3\n2\n4\n6\n",
    )
}

/// What's left of a three-member union after `if store is Lite: return` is the other two, read
/// natively as the union it was: its members told apart with `is` and `match`, each one's
/// fields read once it's narrowed to it, a method all of them have called on it, and it given
/// where a `Pg | Mem` goes, and that where the whole union goes.
#[test]
fn what_is_left_of_a_union_after_is_builds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "union_rest",
        r#"struct Lite:
    name: str

    def label(self) -> str:
        f"lite {self.name}"

struct Pg:
    host: str

    def label(self) -> str:
        f"pg {self.host}"

struct Mem:
    size: int

    def label(self) -> str:
        f"mem {self.size}"

type Three = Lite | Pg | Mem

def which(store: Three) -> str:
    if store is Lite:
        return "lite"
    if store is Pg:
        return f"pg {store.host}"
    f"mem {store.size}"

def kind(store: Three) -> str:
    if store is Mem:
        return "mem"
    stdout_text = f"{store}"
    match store:
        case Lite(name=_):
            return f"lite {store.name} {stdout_text}"
        case Pg(host=_):
            return f"pg {store.host} {stdout_text}"

def label(store: Three) -> str:
    if store is Lite:
        return "skip"
    store.label()

def two(store: Pg | Mem) -> str:
    if store is Pg:
        return store.host
    which(store)

def passed(store: Three) -> str:
    if store is Lite:
        return "lite"
    two(store)

def main(stdout: Stdout):
    stdout.print(which(Lite(name="a")), which(Pg(host="b")), which(Mem(size=1)))
    stdout.print(kind(Lite(name="a")), kind(Pg(host="b")), kind(Mem(size=1)))
    stdout.print(label(Lite(name="a")), label(Pg(host="b")), label(Mem(size=1)))
    stdout.print(passed(Lite(name="a")), passed(Pg(host="b")), passed(Mem(size=1)))
"#,
        "lite pg b mem 1\nlite a Lite(name=\"a\") pg b Pg(host=\"b\") mem\nskip pg b mem 1\nlite b mem 1\n",
    )
}

/// A `with` source called on a name holding a union runs the source of the member it holds,
/// natively: on two members and on three, twice in one function, and on a name given a union
/// from a field.
#[test]
fn a_with_source_on_a_union_runs_the_member_it_holds_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "union_sources",
        r#"error Nope:
    why: str

struct Transaction:
    log: str

struct Lite:
    name: str

    def run(self, text: str) -> int fails Nope:
        text.len()

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") else:
            pass

struct Mem:
    size: int

    def run(self, text: str) -> int fails Nope:
        text.len() * 3

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log=f"mem{self.size}") else:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        text.len() * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") else:
            pass

type Store = Lite | Postgres

type Three = Lite | Postgres | Mem

struct Holder:
    db: Three

def saved(store: Store) -> str fails Nope:
    with store.transaction()? as tx:
        count = store.run("abc")?
        return f"{tx.log} {count}"
    ""

def three(store: Three) -> str fails Nope:
    mut seen = ""
    with store.transaction()? as tx:
        seen = tx.log
    with store.transaction()? as tx:
        seen = f"{seen}+{tx.log}"
    seen

def held(holder: Holder) -> int fails Nope:
    db = holder.db
    with db.transaction()? as tx:
        return tx.log.len()
    0

def main(stdout: Stdout) fails Nope:
    stdout.print(saved(Lite(name="a"))?, saved(Postgres(host="b"))?)
    stdout.print(three(Lite(name="a"))?, three(Postgres(host="b"))?, three(Mem(size=7))?)
    stdout.print(held(Holder(db=Mem(size=42)))?, held(Holder(db=Lite(name="x")))?)
"#,
        "lite 3 pg 6\nlite+lite pg+pg mem7+mem7\n5 4\n",
    )
}

/// A union with a generic struct among its members, `str | Secret[str]` or `Box[int] | int`,
/// is held natively: passed in, told apart with `is`, and read as the member it holds.
#[test]
fn a_union_with_a_generic_struct_member_is_held_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "union_generic_member",
        r#"from std.crypto import Secret

struct Box[T]:
    value: T

def text_of(given: str | Secret[str]) -> (str, bool):
    if given is str:
        return (given, False)
    (given.expose_secret(), True)

def size(held: Box[int] | int) -> int:
    if held is int:
        return held
    held.value * 10

def main(stdout: Stdout):
    stdout.print(text_of("plain"), text_of(Secret.of("hidden"))[1])
    both: list[Box[int] | int] = [Box(value=4), 2]
    stdout.print(size(both[0]), size(both[1]))
"#,
        "(\"plain\", False) True\n40 2\n",
    )
}

/// A known difference: native code writes a field holding a union tagged with its member, as
/// the language says, where the JIT's JSON writer left the tag out.
#[test]
fn native_json_tags_a_union_where_the_jit_does_not() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let ((ours, _, _), (jit, _, _)) = both(
        "native_json_union_tags",
        r"from std import json

struct Circle:
    radius: float

struct Scene:
    main: Circle | int

def main(stdout: Stdout):
    stdout.print(json.dumps(Scene(main=Circle(radius=1.5))))
",
    )?;
    assert_eq!(ours, "{\"main\": {\"Circle\": {\"radius\": 1.5}}}\n");
    assert_eq!(jit, "{\"main\": {\"radius\": 1.5}}\n");
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
    _ = json.loads[Blob]("{\"data\": \"ab=c\"}") else as bad:
        stdout.print(bad.message)
        Blob(data=bytes([]))
"#,
    )?;
    let executable = dir.join("blob");
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
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
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
}

/// JSON and TOML refuse what they'd have to guess at, rather than loading something the text
/// didn't say: a number too big for a float, a struct's field given twice, a dict's int key
/// written any way but its plain digits, and a key that isn't in quotes.
#[test]
fn json_and_toml_refuse_what_they_would_have_to_guess_at() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "json_strict",
        r#"from std import json, toml

struct Point:
    x: int
    y = 0

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else:
        return "refused"
    return "loaded"

def main(stdout: Stdout):
    stdout.print(said(() => json.loads[float]("1e400")?), said(() => json.loads[float]("-1e400")?), said(() => json.loads[float]("1e300")?))
    stdout.print(said(() => toml.loads[dict[str, float]]("a = 1e400\n")?), said(() => toml.loads[dict[str, float]]("a = 1e300\n")?))
    stdout.print(said(() => json.loads[Point]("{\"x\": 1, \"x\": 2}")?), said(() => json.loads[Point]("{\"x\": 1, \"y\": 2}")?))
    for key in ["1", "-1", "+1", "01", " 1"]:
        stdout.print(said(() => json.loads[dict[int, int]](f"{{\"{key}\": 2}}")?))
    stdout.print(said(() => json.loads[dict[int, int]]("{1: 2}")?))
"#,
    )?;
    assert_eq!(
        printed,
        "refused refused loaded\nrefused loaded\nrefused loaded\nloaded\nloaded\nrefused\nrefused\nrefused\nrefused\n"
    );
    Ok(())
}

/// Rows read into a struct refuse two columns of the same name, as JSON and TOML refuse a field
/// given twice, rather than one silently taking the other's place.
#[test]
fn rows_refuse_two_columns_for_one_field() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "sql_twice",
        r#"from std import sql
from std.sql import SqlRows

struct Entry:
    size: int

def main(stdout: Stdout):
    rows = SqlRows(columns=["size", "size"], kinds=[1, 1], at=[0, 1], ints=[1, 2], floats=[], texts=[], blobs=[])
    found = sql.rows_as[Entry](rows) else:
        stdout.print("refused")
        return
    stdout.print("read", found)
"#,
    )?;
    assert_eq!(printed, "refused\n");
    Ok(())
}

/// `process.run(t"...")` runs what the template says, its words split at the spaces in the text
/// written in the source. A value is part of one argument however much it holds, so it can't add
/// an argument or start a command. Text written beside a value is that argument too. What comes
/// after `--` is shown by a program of the test's own, since each system's `printf` reads `--`
/// its own way: GNU's newer ones take it as the end of their options anywhere.
#[test]
fn process_run_takes_a_template_and_a_value_stays_one_argument() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let shower = shows_its_arguments("process_template_shower")?;
    let printed = printed_by(
        "process_template",
        &r#"from std.process import ProcessError

def main(process: Process, stdout: Stdout) fails ProcessError:
    name = "a b; rm -rf /"
    # The line break in the text is a space between words, so the format is `[%s]`.
    shown = process.run(t"printf [%s]\n one {name} --who={name}")?
    stdout.print(shown.stdout)
    empty = ""
    stdout.print(process.run(t"printf [%s]\n {empty} x{empty}")?.stdout)
    stdout.print(process.run(t"cat", input="fed in")?.stdout)
    narrow = process.only("printf")
    stdout.print(narrow.run(t"printf ok")?.stdout)
    # A value that would start an argument with `-` is refused, and nothing runs.
    upload = "--upload-pack=touch pwned"
    _ = process.run(t"printf {upload}") else as refused:
        stdout.print(refused.message())
        shown
    path = "-odd"
    stdout.print(process.run(t"SHOWER -- {path}")?.stdout.strip())
    _ = narrow.run(t"cat") else as error:
        stdout.print(error.program())
        return
    stdout.print("ran")
"#
        .replace("SHOWER", &shower),
    )?;
    std::fs::remove_dir_all(Path::new(&shower).parent().unwrap_or(Path::new(&shower)))?;
    assert_eq!(
        printed,
        "[one][a b; rm -rf /][--who=a b; rm -rf /]\n[][x]\nfed in\nok\n\
         `{upload}` starts an argument with `-`, so `printf` would read it as an option: put `--` before it, if `printf` takes that, to mean it isn't one\n\
         [--][-odd]\ncat\n"
    );
    Ok(())
}

/// Builds a program that prints each argument it's given in brackets, the same on every system,
/// and gives back its path as a template's text can name it: with `/` between folders, which
/// Windows takes too, and no spaces.
fn shows_its_arguments(name: &str) -> std::io::Result<String> {
    let dir = folder(name)?;
    let program = dir.join("shower.fen");
    std::fs::write(
        &program,
        "def main(args: Args, stdout: Stdout):\n    stdout.print(\"\".join([f\"[{arg}]\" for arg in args]))\n",
    )?;
    let executable = dir.join("shower");
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let path = executable.to_string_lossy().replace('\\', "/");
    assert!(!path.contains([' ', '{', '}']), "a template can't name {path}");
    Ok(path)
}

/// Small things that gave a wrong answer rather than saying so: a `Path` going up from `..`, text
/// base64 can't have written, a day a month doesn't have, and a delimiter that could never match.
/// `Path.inside` joins a path from outside so it can't leave the folder.
#[test]
fn small_std_answers_are_right_or_refused() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "small_answers",
        r#"from std import base64, csv, toml
from std.path import Path

def said[T](run: () -> T fails InvalidValue) -> str:
    _ = run() else:
        return "refused"
    return "loaded"

def said_toml(text: str) -> str:
    _ = toml.loads[dict[str, str]](text) else:
        return "refused"
    return "loaded"

def main(stdout: Stdout):
    stdout.print(said(() => base64.decode("aGk=")?), said(() => base64.decode("aGl=")?), said(() => base64.decode("aA==")?), said(() => base64.decode("aB==")?))
    stdout.print(Path("..").parent(), Path("a/..").parent(), Path("a/b").parent())
    root = Path("static")
    for asked in ["/a/b.txt", "a.txt", "/../secret", "a/../../b", "a\x00b", "a\\b"]:
        found = root.inside(asked) else:
            stdout.print("refused")
            continue
        stdout.print(found)
    stdout.print(said_toml("d = 2024-02-29\n"), said_toml("d = 2023-02-29\n"), said_toml("d = 2024-02-30\n"), said_toml("d = 2024-04-31\n"))
    _ = csv.loads("a;;b\n", delimiter=";;") else:
        stdout.print("refused")
        []
    stdout.print("a".ljust(4, fill="x"), "a".rjust(4, fill="x"), "a".center(3, fill="x"))
    stdout.print("a".ljust(4, fill="xy"), "a".rjust(4, fill="xy"), "a".center(5, fill="xy"))
"#,
    )?;
    assert_eq!(
        printed,
        "loaded refused loaded refused\nPath(text=\"../..\") Path(text=\"a/../..\") Path(text=\"a\")\nPath(text=\"static/a/b.txt\")\nPath(text=\"static/a.txt\")\nrefused\nrefused\nrefused\nrefused\nloaded refused refused refused\nrefused\naxxx xxxa xax\naxyx xyxa xyaxy\n"
    );
    Ok(())
}

/// CSV that `csv.dumps` writes reads back as the rows it was given: a first field starting with
/// a byte order mark keeps it, rather than `loads` taking it for the text's, and a row with no
/// fields, which CSV can't write, is refused rather than lost. Found by fuzzing `std.csv` (`fennec-fuzz run csv`).
#[test]
fn csv_dumps_writes_what_loads_reads_back_as_the_same_rows() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "csv_round_trip",
        r#"from std import csv

def back(rows: list[list[str]]) -> str:
    text = csv.dumps(rows) else return "refused"
    read = csv.loads(text) else:
        return "False"
    return str(read == rows)

def main(stdout: Stdout):
    stdout.print(back([["﻿a", "b"]]), back([["﻿"]]), back([["a"], [], ["b"]]))
"#,
    )?;
    assert_eq!(printed, "True True refused\n");
    Ok(())
}

/// Only the first U+FEFF of CSV text is its byte order mark: a second is the first field's own
/// character, which `csv.loads` keeps. Found by fuzzing `std.csv` (`fennec-fuzz run csv`).
#[test]
fn csv_reads_only_the_first_byte_order_mark() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "csv_two_marks",
        r#"from std import csv

def main(stdout: Stdout):
    rows = csv.loads("﻿﻿a,b\n﻿c") else:
        stdout.print("refused")
        return
    stdout.print(rows == [["﻿a", "b"], ["﻿c"]])
"#,
    )?;
    assert_eq!(printed, "True\n");
    Ok(())
}

/// A server can't make the client hold as much as it likes before a response's body: endless
/// header lines fail, as one endless line does, and so do endless `100 Continue`s. A chunk's data
/// has to end where its size says, with nothing after it but its line ending.
#[test]
fn a_response_cant_make_the_client_hold_without_end() -> std::io::Result<()> {
    use std::io::{Read as _, Write as _};
    if !ready() {
        return Ok(());
    }
    let answers = [
        [
            b"HTTP/1.1 200 OK\r\n".to_vec(),
            b"X-A: b\r\n".repeat(400_000),
            b"Content-Length: 2\r\n\r\nok".to_vec(),
        ]
        .concat(),
        [
            b"HTTP/1.1 100 Continue\r\n\r\n".repeat(200_000),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        ]
        .concat(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nokEXTRA\r\n0\r\n\r\n".to_vec(),
    ];
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        for answer in answers {
            let (mut stream, _) = listener.accept()?;
            let mut asked = [0_u8; 4096];
            let _ = stream.read(&mut asked)?;
            // The client may stop reading part of the way through.
            let _ = stream.write_all(&answer);
        }
        Ok(())
    });
    let source = format!(
        "from std.net import NetError\n\ndef said(net: Net) -> str:\n    answer = net.get(\"http://127.0.0.1:{port}/\", timeout=10s) else:\n        return \"refused\"\n    return answer.text()\n\ndef main(net: Net, stdout: Stdout):\n    stdout.print(said(net), said(net), said(net))\n"
    );
    let printed = printed_by("client_holds", &source)?;
    server.join().map_err(|_| std::io::Error::other("the server stopped"))??;
    assert_eq!(printed, "refused refused refused\n");
    Ok(())
}

/// A response's headers are a real dict, however many times a header came. Those that can be
/// joined are, as HTTP says, and `Set-Cookie`, which can't, has all its values in `set_cookies`.
#[test]
fn a_header_sent_twice_is_one_entry_and_keeps_every_value() -> std::io::Result<()> {
    use std::io::{Read as _, Write as _};
    if !ready() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut asked = [0_u8; 4096];
        let _ = stream.read(&mut asked)?;
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nX-Tag: one\r\nX-Tag: two\r\nContent-Length: 0\r\n\r\n",
        )
    });
    let source = format!(
        "def main(net: Net, stdout: Stdout) fails NetError:\n    answer = net.get(\"http://127.0.0.1:{port}/\")?\n    stdout.print(answer.headers.len())\n    stdout.print(answer.headers[\"x-tag\"])\n    stdout.print(answer.set_cookies)\n"
    )
    .replace("def main", "from std.net import NetError\n\ndef main");
    let printed = printed_by("headers_twice", &source)?;
    server.join().map_err(|_| std::io::Error::other("the server stopped"))??;
    assert_eq!(printed, "3\none, two\n[\"a=1\", \"b=2\"]\n");
    Ok(())
}

/// SQLite turns a NaN into NULL, so a program's value would quietly become no value. It's refused.
#[test]
fn sqlite_refuses_a_nan() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let place = std::env::temp_dir().join(format!("fennec-sqlite-nan-{}.db", std::process::id()));
    let path = place.to_string_lossy().into_owned();
    // In a Fennec string, a Windows path's `\` starts an escape, so each is written twice.
    let written = path.replace('\\', "\\\\");
    let source = format!(
        "from std import sqlite\nfrom std.sql import SqlError\n\ndef main(fs: Fs, stdout: Stdout) fails SqlError:\n    with sqlite.open(fs, \"{written}\")? as db:\n        _ = db.execute(t\"create table m (x real)\")?\n        nan = float(\"nan\") else 0.0\n        _ = db.execute(t\"insert into m values ({{nan}})\") else as refused:\n            stdout.print(refused.message())\n            0\n        one = 1.5\n        stdout.print(db.execute(t\"insert into m values ({{one}})\")?)\n"
    );
    let printed = printed_by("sqlite_nan", &source)?;
    for ending in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{ending}"));
    }
    assert_eq!(printed, "NaN can't be stored: SQLite would turn it into NULL\n1\n");
    Ok(())
}

/// A URL prints as it was written, and a host can't hold what would make it another host.
#[test]
fn a_url_prints_as_written_and_a_host_stays_a_host() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "url_strict",
        r#"from std import url

def main(stdout: Stdout):
    for text in ["mailto:a@b", "/a%2Fb", "/%ff", "http://x/a%20b", "file:///x", "https://me@example.com:8/a?b=1#c"]:
        found = url.parse(text) else as error:
            stdout.print("refused", text, error.message)
            continue
        stdout.print(str(found) == text, str(found))
    for text in ["http://a@b@c/", "http://good.com%2f.evil.com/", "http://a%40b/"]:
        found = url.parse(text) else:
            stdout.print("refused")
            continue
        stdout.print("host", found.host)
"#,
    )?;
    assert_eq!(
        printed,
        "True mailto:a@b\nTrue /a%2Fb\nTrue /%ff\nTrue http://x/a%20b\nTrue file:///x\nTrue https://me@example.com:8/a?b=1#c\nrefused\nrefused\nrefused\n"
    );
    Ok(())
}

/// What the next test builds: failed asserts and a failing `main`, each holding a secret. The
/// text is put together on a line of its own, since a diagnostic shows the line it points at.
const SECRET_SHOWN: &str = r#"from std.crypto import Secret

struct Login:
    name: str
    password: Secret[str]

error Refused:
    Denied(token: Secret[str])

def word() -> Secret[str]:
    Secret.of("hun" + "ter2")

def signs_in() fails Refused:
    fail .Denied(token=word())

test "the secret itself"():
    assert word() == Secret.of("other")

test "in a list"():
    assert [word()] == [Secret.of("other")]

test "in a tuple"():
    assert (1, word()) == (2, Secret.of("other"))

test "in a struct"():
    assert Login(name="a", password=word()) == Login(name="b", password=word())

def main():
    _ = Login(name="a", password=word())
    signs_in()?
"#;

/// A `Secret` stays hidden where a program says what went wrong: both sides of a failed `assert`,
/// inside a list, a tuple and a struct, and the failure that ends `main`.
#[test]
fn a_secret_stays_hidden_in_what_a_failure_says() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("secret_shown")?;
    let program = dir.join("secret_shown.fen");
    std::fs::write(&program, SECRET_SHOWN)?;
    let path = program.to_string_lossy().into_owned();
    let tested = fennec(&["test", &path], &dir)?;
    let tests_said =
        format!("{}{}", String::from_utf8_lossy(&tested.stdout), String::from_utf8_lossy(&tested.stderr));
    assert!(tests_said.contains("0 passed, 4 failed"), "{tests_said}");
    assert!(!tests_said.contains("hunter2"), "{tests_said}");
    assert_eq!(tests_said.matches("Secret(hidden)").count(), 8, "{tests_said}");
    let executable = dir.join("secret_shown");
    let built = fennec(&["build", "--llvm", &path, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    let main_said = String::from_utf8_lossy(&ran.stderr).into_owned();
    assert!(main_said.contains("Refused.Denied(token=Secret(hidden))"), "{main_said}");
    assert!(!main_said.contains("hunter2"), "{main_said}");
    Ok(())
}

/// Replacing a file keeps what was true of it: who can read it, and that a link stays a link to
/// the file it points at. What's being written is never readable by more than the file was.
#[cfg(unix)]
#[test]
fn replacing_a_file_keeps_its_mode_and_goes_through_a_link() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if !ready() {
        return Ok(());
    }
    let place = std::env::temp_dir().join(format!("fennec-fs-safety-{}", std::process::id()));
    std::fs::create_dir_all(&place)?;
    let mode_of = |name: &str| -> std::io::Result<u32> {
        Ok(std::fs::metadata(place.join(name))?.permissions().mode() & 0o777)
    };
    let set_mode = |name: &str, mode: u32| {
        std::fs::set_permissions(place.join(name), std::fs::Permissions::from_mode(mode))
    };
    std::fs::write(place.join("secret.txt"), "old")?;
    set_mode("secret.txt", 0o600)?;
    std::fs::write(place.join("shared.txt"), "old")?;
    set_mode("shared.txt", 0o664)?;
    std::fs::write(place.join("real.txt"), "old")?;
    std::os::unix::fs::symlink(place.join("real.txt"), place.join("link.txt"))?;
    let folder = place.to_string_lossy().into_owned();
    let source = format!(
        "def main(fs: Fs) fails IoError:\n    with fs.create(\"{folder}/secret.txt\")? as file:\n        file.write_text(\"new\")?\n    with fs.create(\"{folder}/link.txt\")? as file:\n        file.write_text(\"new\")?\n    fs.write_text(\"{folder}/shared.txt\", \"new\")?\n"
    );
    printed_by("fs_safety", &source)?;
    let (secret, shared) = (mode_of("secret.txt")?, mode_of("shared.txt")?);
    let real = std::fs::read_to_string(place.join("real.txt"))?;
    let still_a_link = std::fs::symlink_metadata(place.join("link.txt"))?.file_type().is_symlink();
    let contents = std::fs::read_to_string(place.join("secret.txt"))?;
    let left: Vec<_> =
        std::fs::read_dir(&place)?.filter_map(Result::ok).map(|entry| entry.file_name()).collect();
    std::fs::remove_dir_all(&place)?;
    assert_eq!(secret, 0o600, "the mode of a file fs.create replaced");
    assert_eq!(shared, 0o664, "the mode of a file fs.write_text replaced");
    assert!(still_a_link, "a link fs.create wrote through");
    assert_eq!((real.as_str(), contents.as_str()), ("new", "new"));
    assert_eq!(left.len(), 4, "something was left behind: {left:?}");
    Ok(())
}

/// A narrowed `Fs` stays inside its folder when a link inside it points out, to read or to write,
/// and what's really inside still works.
#[cfg(unix)]
#[test]
fn a_link_cannot_carry_a_narrowed_fs_out_of_its_folder() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let place = std::env::temp_dir().join(format!("fennec-fs-only-{}", std::process::id()));
    std::fs::create_dir_all(place.join("inside"))?;
    std::fs::create_dir_all(place.join("outside"))?;
    std::fs::write(place.join("outside/secret.txt"), "secret")?;
    std::fs::write(place.join("inside/plain.txt"), "plain")?;
    std::os::unix::fs::symlink(place.join("outside"), place.join("inside/door"))?;
    let folder = place.to_string_lossy().into_owned();
    let source = format!(
        "def read_out(fs: Fs, path: str) -> str:\n    text = fs.read_text(path) else:\n        return \"refused\"\n    return text\n\ndef main(fs: Fs, stdout: Stdout):\n    narrow = fs.only(\"{folder}/inside\")\n    stdout.print(read_out(narrow, path=\"{folder}/inside/door/secret.txt\"))\n    stdout.print(read_out(narrow, path=\"{folder}/inside/plain.txt\"))\n    narrow.write_text(\"{folder}/inside/door/new.txt\", \"x\") else:\n        stdout.print(\"refused\")\n        return\n    stdout.print(\"wrote\")\n"
    );
    let printed = printed_by("fs_only_link", &source)?;
    let escaped = place.join("outside/new.txt").exists();
    std::fs::remove_dir_all(&place)?;
    assert_eq!(printed, "refused\nplain\nrefused\n");
    assert!(!escaped, "a file was written outside the folder");
    Ok(())
}

/// A Postgres message from a server: its type, its length, its body.
fn postgres_message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut message = vec![kind];
    message.extend_from_slice(&u32::try_from(body.len() + 4).unwrap_or(0).to_be_bytes());
    message.extend_from_slice(body);
    message
}

/// A server's `Authentication` message of the type `code`, then `rest`.
fn postgres_auth(code: u32, rest: &[u8]) -> Vec<u8> {
    let mut body = code.to_be_bytes().to_vec();
    body.extend_from_slice(rest);
    postgres_message(b'R', &body)
}

/// What a server says when it's ready for a query.
fn postgres_ready() -> Vec<u8> {
    postgres_message(b'Z', b"I")
}

/// A query's whole answer, ending in the tag `tag`.
fn postgres_answer(tag: &str) -> Vec<u8> {
    let mut tagged = tag.as_bytes().to_vec();
    tagged.push(0);
    [
        postgres_message(b'1', b""),
        postgres_message(b'2', b""),
        postgres_message(b'n', b""),
        postgres_message(b'C', &tagged),
        postgres_ready(),
    ]
    .concat()
}

/// Reads what a client sends up to where it waits for an answer, and gives its messages' types,
/// or nothing once it's gone. The first thing it sends has no type.
fn postgres_flight(stream: &mut std::net::TcpStream, first: bool) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut kinds = Vec::new();
    loop {
        let mut kind = [0_u8; 1];
        if !first {
            stream.read_exact(&mut kind).ok()?;
        }
        let mut size = [0_u8; 4];
        stream.read_exact(&mut size).ok()?;
        let mut body = vec![0_u8; usize::try_from(u32::from_be_bytes(size)).ok()?.checked_sub(4)?];
        stream.read_exact(&mut body).ok()?;
        kinds.push(kind[0]);
        if first || kind[0] == b'p' || kind[0] == b'S' {
            return Some(kinds);
        }
        if kind[0] == b'X' {
            return None;
        }
    }
}

/// A server on a port of its own that gives each of `answers` in turn, one for each time the
/// client waits, then hangs up. It gives back the port, and how many passwords it was sent.
fn postgres_scripted(answers: Vec<Vec<u8>>) -> std::io::Result<(u16, std::sync::mpsc::Receiver<usize>)> {
    use std::io::Write as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (done, passwords) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let mut sent = 0;
        for (at, answer) in answers.iter().enumerate() {
            let Some(kinds) = postgres_flight(&mut stream, at == 0) else { break };
            sent += usize::from(kinds.contains(&b'p'));
            if stream.write_all(answer).is_err() {
                break;
            }
        }
        // What comes after the script, like a password it asked for, or the goodbye.
        while let Some(kinds) = postgres_flight(&mut stream, false) {
            sent += usize::from(kinds.contains(&b'p'));
        }
        let _ = done.send(sent);
    });
    Ok((port, passwords))
}

/// What the next test builds: `connect`, a transaction and a query, against each port it's given.
const POSTGRES_SCRIPTED: &str = r#"from std import postgres
from std.concurrent import Timeout
from std.crypto import Secret
from std.sql import SqlError

## What a failure says, whichever kind it is.
def said_of(error: SqlError | Timeout) -> str:
    match error:
        case Timeout(waited=_):
            "no connection came free"
        case Constraint(reason=reason) | Busy(reason=reason) | Failed(reason=reason):
            reason
        case Column(name=name, reason=reason):
            f"column {name}: {reason}"

def run(port: int, weak: bool, net: Net, entropy: Entropy) fails SqlError | Timeout:
    with postgres.connect(net, entropy, f"postgres://u@127.0.0.1:{port}/d", password=Secret.of("pw"), allow_weak_sign_in=weak, pool_size=1, tls=False)? as db:
        with db.transaction()? as tx:
            _ = tx.execute(t"select 1")?

def main(stdout: Stdout, net: Net, entropy: Entropy):
    for at, port in enumerate(PORTS):
        _ = run(port, weak=at >= STRICT, net=net, entropy=entropy) else as error:
            stdout.print(said_of(error))
            continue
        stdout.print("went through")
"#;

/// A server can't trick the driver. A commit the server answers with `ROLLBACK` fails. A server
/// that was given a password has to prove it knows it: one that claims to have checked SCRAM
/// without doing the exchange is refused, and so is one that lets the client in with no proof or
/// asks for the password as it is, which is then never sent.
#[test]
fn postgres_does_not_believe_a_server_that_has_not_proved_itself() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let signed_in = [postgres_auth(0, b""), postgres_ready()].concat();
    let sasl = postgres_auth(10, b"SCRAM-SHA-256\0\0");
    // The signature of nothing under an empty key, which is what a check before the exchange
    // compared with.
    let forged = postgres_auth(12, b"v=thNnmggU2ex3L5XXeMNfxf8Wl8STcVZTxscSFEKSxa0=");
    let works = |tag: &str| vec![postgres_answer("BEGIN"), postgres_answer("SELECT 1"), postgres_answer(tag)];
    let after = |first: Vec<Vec<u8>>, tag: &str| [first, works(tag)].concat();
    let cleartext = postgres_auth(3, b"");
    let scripts = vec![
        // With a password, and SCRAM not done: these are refused.
        after(vec![signed_in.clone()], "COMMIT"),
        after(vec![sasl, [forged, signed_in.clone()].concat()], "COMMIT"),
        after(vec![cleartext.clone(), signed_in.clone()], "COMMIT"),
        // Asked for the weak ways: these go through, up to what the commit says.
        after(vec![signed_in.clone()], "ROLLBACK"),
        after(vec![signed_in.clone()], "COMMIT"),
        after(vec![cleartext, signed_in], "COMMIT"),
    ];
    let mut ports = Vec::new();
    let mut passwords = Vec::new();
    for script in scripts {
        let (port, sent) = postgres_scripted(script)?;
        ports.push(port.to_string());
        passwords.push(sent);
    }
    let source =
        POSTGRES_SCRIPTED.replace("PORTS", &format!("[{}]", ports.join(", "))).replace("STRICT", "3");
    let printed = printed_by("postgres_scripted", &source)?;
    assert_eq!(
        printed,
        "the server let us in without proving it knows the password: `allow_weak_sign_in=True` accepts that\n\
         the server sent its SCRAM proof before the exchange\n\
         the server asks for the password as it is, so it wasn't sent: `allow_weak_sign_in=True` sends it\n\
         the transaction wasn't kept: the server answered the commit with `ROLLBACK`, as it does when something in it failed\n\
         went through\n\
         went through\n"
    );
    let sent: Vec<usize> = passwords.iter().map(|sent| sent.recv().unwrap_or(99)).collect();
    assert_eq!(sent[2], 0, "the password went to a server that asked for it as it is");
    assert_eq!(sent[5], 1, "the password wasn't sent when asked to send it as it is");
    Ok(())
}

/// A Postgres server's messages are read as the protocol writes them, and anything else fails
/// rather than reading as something: a count below zero, a cell's size below -1, an error with
/// no zero byte ending its fields, a `ReadyForQuery` that isn't one byte, and bytes left over in
/// a message. A `bool` cell is `t` or `f`, and anything else fails rather than reading as false.
/// Found by fuzzing `PgReader` (`fennec-fuzz run postgres`).
#[test]
fn postgres_refuses_messages_a_server_never_sends() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "postgres_strict",
        r#"from std import postgres
from std.postgres import PgMessage, PgReader

def read(data: bytes) -> str:
    mut reader = PgReader()
    reader.feed(data)
    message = reader.next() else as _:
        return "refused"
    _ = message else:
        return "refused"
    return "read"

def main(stdout: Stdout):
    stdout.print(read(bytes([84, 0, 0, 0, 6, 255, 255])))
    stdout.print(read(bytes([68, 0, 0, 0, 6, 255, 255])))
    stdout.print(read(bytes([68, 0, 0, 0, 10, 0, 1, 255, 255, 255, 254])))
    stdout.print(read(bytes([69, 0, 0, 0, 11, 83, 69, 82, 82, 79, 82, 0])))
    stdout.print(read(bytes([90, 0, 0, 0, 4])), read(bytes([90, 0, 0, 0, 6, 73, 73])))
    stdout.print(read(bytes([75, 0, 0, 0, 16, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3])))
    stdout.print(read(bytes([49, 0, 0, 0, 5, 120])))
    rows = [PgMessage.Row(cells=[bytes([120])], present=[True])]
    found = postgres.rows_of(["flag"], types=[16], rows=rows) else:
        stdout.print("refused")
        return
    stdout.print("read", found.ints)
"#,
    )?;
    assert_eq!(printed, "refused\nrefused\nrefused\nrefused\nrefused refused\nrefused\nrefused\nrefused\n");
    Ok(())
}

/// `fennec update` locks the layouts a kit saves in `pack.lock`. A locked type's values are then
/// written without their layout, as short as before layouts, and the next version of the type
/// reads them by the locked layout's names. Without the lock, they'd be refused. `fennec check`
/// warns about a version that couldn't load what was locked, and is quiet about one that can.
#[test]
fn pack_lock_leaves_the_layout_out_and_the_next_version_reads_by_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("pack_lock")?;
    let kit = dir.join("saver");
    std::fs::create_dir_all(kit.join("src"))?;
    std::fs::write(
        kit.join("kit.toml"),
        "name = \"saver\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n\n[dependencies]\n",
    )?;
    let main = kit.join("src/main.fen");
    // Run in the kit's folder, as `fennec update` updates the kit it's in.
    let in_kit = |args: &[&str]| {
        crate::executables::fennec_command()
            .args(args)
            .current_dir(&kit)
            .env("FENNEC_HOME", &dir)
            .output_before(FENNEC_DEADLINE)
    };
    let ran = |source: &str| -> std::io::Result<String> {
        std::fs::write(&main, source)?;
        let out = in_kit(&["run", "."])?;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let first = "from std import pack\n\nstruct Entry:\n    path: str\n    size: int\n\ndef main(fs: Fs, stdout: Stdout) fails IoError:\n    data = pack.dumps(Entry(path=\"a.txt\", size=12))\n    fs.write_bytes(\"entry.pack\", data)?\n    stdout.print(data.len())\n";
    let long = ran(first)?;
    let updated = in_kit(&["update"])?;
    let said = String::from_utf8_lossy(&updated.stdout).into_owned();
    let lock = std::fs::read_to_string(kit.join("pack.lock"))?;
    let short = ran(first)?;
    let next = "from std import pack\n\nstruct Entry:\n    path: str\n    size: float\n    tags = list[str]()\n\ndef main(fs: Fs, stdout: Stdout) fails IoError:\n    found = pack.loads[Entry](fs.read_bytes(\"entry.pack\")?) else as error:\n        stdout.print(error.message)\n        return\n    stdout.print(found.path, found.size, found.tags)\n";
    let read = ran(next)?;
    // A version that can't load what was locked says so as it's checked, without failing.
    let breaking = "from std import pack\n\nstruct Entry:\n    path: str\n    size: str\n    rank: int\n    note = \"\"\n\ndef main(stdout: Stdout):\n    stdout.print(pack.dumps(Entry(path=\"a\", size=\"1\", rank=2)).len())\n";
    std::fs::write(&main, breaking)?;
    let checked = in_kit(&["check", "."])?;
    let warned = String::from_utf8_lossy(&checked.stderr).into_owned();
    std::fs::write(&main, next)?;
    let quiet = in_kit(&["check", "."])?;
    std::fs::remove_file(kit.join("pack.lock"))?;
    let unlocked = ran(next)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!((long.as_str(), short.as_str()), ("37\n", "19\n"));
    assert!(said.contains("locked 1 layout in pack.lock"), "{said}");
    assert_eq!(lock.lines().count(), 1, "{lock}");
    assert!(lock.ends_with(" Entry S 2 path s size i\n"), "{lock}");
    assert!(checked.status.success(), "{warned}");
    assert_eq!(
        warned,
        "warning: `Entry.size` was saved as an int, which it can't load as now\n\
         warning: `Entry.rank` has no default, and data saved before it doesn't have one\n"
    );
    assert_eq!(String::from_utf8_lossy(&quiet.stderr), "");
    assert_eq!(read, "a.txt 12.0 []\n");
    assert_eq!(unlocked, "this was packed from a different `Entry`, at byte 4\n");
    Ok(())
}

/// Reading a file a chunk or a line at a time fails when reading does, inside the `with` that
/// opened it, rather than ending the loop as if the file were done: a folder read as a file, and
/// a line that isn't UTF-8.
#[test]
fn file_chunks_and_lines_fail_rather_than_end_early() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let place = std::env::temp_dir().join(format!("fennec-iter-fail-{}", std::process::id()));
    std::fs::create_dir_all(place.join("folder"))?;
    std::fs::write(place.join("bad.txt"), [97, 10, 255, 10, 98])?;
    std::fs::write(place.join("good.txt"), "one\ntwo\n")?;
    // In a Fennec string, a Windows path's `\` starts an escape, so each is written twice.
    let folder = place.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        "def chunked(fs: Fs, path: str) -> int fails IoError:\n    mut total = 0\n    with fs.open(path)? as file:\n        for chunk in file.chunks(4)?:\n            total += chunk.len()\n    total\n\ndef lined(fs: Fs, path: str) -> int fails IoError:\n    mut count = 0\n    with fs.open(path)? as file:\n        for line in file.lines()?:\n            count += line.len() + 1\n    count\n\ndef said(run: () -> int fails IoError) -> str:\n    n = run() else:\n        return \"failed\"\n    return f\"read {{n}}\"\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(said(() => chunked(fs, path=\"{folder}/folder\")?), said(() => chunked(fs, path=\"{folder}/good.txt\")?))\n    stdout.print(said(() => lined(fs, path=\"{folder}/bad.txt\")?), said(() => lined(fs, path=\"{folder}/good.txt\")?))\n"
    );
    let printed = printed_by("iter_fail", &source)?;
    std::fs::remove_dir_all(&place)?;
    assert_eq!(printed, "failed read 8\nfailed read 8\n");
    Ok(())
}

/// A server on a port of its own that reads the 8 bytes of a TLS request, answers `answer`, and
/// hangs up.
fn postgres_tls_answering(answer: &'static [u8]) -> std::io::Result<u16> {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let mut asked = [0_u8; 8];
        if stream.read_exact(&mut asked).is_ok() && asked == [0, 0, 0, 8, 4, 210, 22, 47] {
            let _ = stream.write_all(answer);
        }
    });
    Ok(port)
}

/// A database URL holds a password only as a `Secret`: a plain one with a password is refused, a
/// password both in a `Secret` URL and as `password=` is refused, and a failure shows the URL with
/// its password as `****`. The URL comes from the environment here, as a real one would, so the
/// compile-time refusal of a written one doesn't apply.
#[test]
fn a_database_url_holds_a_password_only_as_a_secret_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"from std import postgres
from std.crypto import Secret
from std.sql import SqlError

## What `settings` made of a URL, or why it couldn't.
def described(url: str | Secret[str]) -> str:
    found = postgres.settings(url) else as error:
        return error.message()
    f"{found.user} {found.host} {found.port} {found.database}"

def twice(url: Secret[str], net: Net, entropy: Entropy) fails SqlError:
    with postgres.connect(net, entropy, url, password=Secret.of("other"))? as _:
        pass

def main(stdout: Stdout, env: Env, net: Net, entropy: Entropy):
    written = env.get("FENNEC_DATABASE_URL") else "postgres://ann:hunter2@db.example/shop"
    stdout.print(described(written))
    stdout.print(described(Secret.of(written)))
    twice(Secret.of(written), net=net, entropy=entropy) else as error:
        stdout.print(error.message())
        return
    stdout.print("connected")
"#;
    let printed = printed_by("postgres_secret_url", source)?;
    assert_eq!(
        printed,
        "the database URL `postgres://ann:****@db.example/shop` has a password in it, so it has to be a `Secret`, as `env.secret(\"DATABASE_URL\")?` reads one, and never a plain `str`, which could be printed\n\
         ann db.example 5432 shop\n\
         `password` is set in the database URL and as an argument: set it in one\n"
    );
    assert!(!printed.contains("hunter2"), "{printed}");
    Ok(())
}

/// `postgres.connect` asks for TLS unless told not to, and never falls back to the clear: a server
/// that says no is refused, and so is one that says yes and then sends plain bytes, as someone
/// in the middle would to have them read as if they came over TLS.
#[test]
fn postgres_asks_for_tls_and_never_falls_back_to_the_clear() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let refused = postgres_tls_answering(b"N")?;
    let injected = postgres_tls_answering(b"SR\0\0\0\x08\0\0\0\0")?;
    let source = format!(
        "from std import postgres\nfrom std.crypto import Secret\nfrom std.sql import SqlError\n\ndef run(port: int, net: Net, entropy: Entropy) fails SqlError:\n    with postgres.connect(net, entropy, f\"postgres://u@localhost:{{port}}/d\", password=Secret.of(\"pw\"))? as _:\n        pass\n\ndef main(stdout: Stdout, net: Net, entropy: Entropy):\n    for port in [{refused}, {injected}]:\n        _ = run(port, net=net, entropy=entropy) else as error:\n            stdout.print(error.message())\n            continue\n        stdout.print(\"went through\")\n"
    );
    let printed = printed_by("postgres_tls", &source)?;
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.first().copied(), Some("the server doesn't take TLS: `tls=False` connects without it"));
    assert!(lines.get(1).is_some_and(|line| line.starts_with("TLS with the server failed: ")), "{printed}");
    Ok(())
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

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as error:
        return error.message
    return "loaded"

def changed(data: bytes, at: int, to: int) -> bytes:
    mut items = list[int]()
    for b in range(data.len()):
        items.append(to if b == at else data[b])
    bytes(items)

## Where the value starts: after the header, and the layout saved with it.
def body(data: bytes) -> int:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    at + 1 + (n | (data[at] << shift))

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
    stdout.print(said(() => pack.loads[list[Place]](data)?))
    stdout.print(said(() => pack.loads[list[Unit]](data[0:data.len() - 1])?))
    stdout.print(said(() => pack.loads[int](bytes([1, 2, 3]))?))
    stdout.print(said(() => pack.loads[int](pack.dumps(5) + bytes([0]))?))
    stdout.print(said(() => pack.loads[str](pack.dumps("hello world"), limits=Limits(length=4))?))
    mut deep = Link()
    for _ in range(10):
        deep = Link(next=deep)
    stdout.print(said(() => pack.loads[Link](pack.dumps(deep), limits=Limits(depth=4))?), said(() => pack.loads[Link](pack.dumps(deep))?))
    medic = pack.dumps(Kind.Medic)
    stdout.print(said(() => pack.loads[Kind](changed(medic, at=body(medic), to=9))?))
    truth = pack.dumps(True)
    stdout.print(said(() => pack.loads[bool](changed(truth, at=body(truth), to=2))?))
    one = pack.dumps(Guarded(points=1))
    # The points, 1, are the zigzag byte 2, and 1 is -1.
    stdout.print(said(() => pack.loads[Guarded](changed(one, at=body(one), to=1))?))
"#;

/// `std.pack` reads back what it wrote, ints at both ends, a name that isn't ASCII, a case with
/// data and a dict among them, and says why bytes don't load: a type needing a field they lack, bytes
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
        "245 True vëx -9223372036854775808 {\"a\": 1, \"b\": -1} 2.25\n-0.1 \n`x` isn't in the data, and has no default, at byte 177\nthe data ends too soon, at byte 244\nthese aren't bytes `std.pack` wrote, at byte 0\nthere's more after the value, at byte 15\na length is over the limit of 4, at byte 14\nit nests deeper than the limit of 4, at byte 31 loaded\n`Kind` has no case 9, at byte 51\na bool should be 0 or 1, at byte 14\nGuarded.points breaks its `where` rule, at byte 26\n"
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
    _ = pack.loads[Node](data, limits=limits) else:
        return 0
    return 1

## Where the value starts: after the header, and the layout saved with it.
def body(data: bytes) -> int:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    at + 1 + (n | (data[at] << shift))

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
            mut changed = items
            changed[at] = (seed >> 16) & 255
            loaded += tried(bytes(changed), Limits())
            loaded += tried(bytes(changed), small)
            # With the number for its shape changed too, it's read by the names in its layout,
            # which may be the byte changed.
            if at != 4:
                changed[4] = changed[4] ^ 1
                loaded += tried(bytes(changed), Limits())
                runs += 1
            runs += 2
    stdout.print("changed", runs, loaded > 0)
    # Bytes from a seed after a good header.
    loaded = 0
    for round in range(3000):
        seed = step(seed)
        size = (seed >> 8) % 64
        mut random = list[int]()
        for at in range(body(good)):
            random.append(items[at])
        for _ in range(size):
            seed = step(seed)
            random.append((seed >> 16) & 255)
        loaded += tried(bytes(random), Limits())
        # A layout from a seed, read by its names, since the number for its shape is changed.
        mut named = list[int]()
        for at in range(12):
            named.append(items[at] ^ 1 if at == 4 else items[at])
        named.append(size)
        for _ in range(size + 16):
            seed = step(seed)
            named.append(seed >> 16 & 255 if seed % 3 == 0 else [83, 32, 49, 108, 105, 115, 69, 111, 114, 48][(seed >> 12) % 10])
        loaded += tried(bytes(named), Limits())
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
    assert_eq!(printed, "169 1 1\ntruncated 0\nchanged 20240 True\nrandom True\n");
    Ok(())
}

/// What the next test builds: a type saved, then loaded as the next version of it.
const PACK_VERSIONS: &str = r#"from std import pack

enum Mood:
    Calm
    Angry(level: int)
    Gone

struct Spot:
    x: int
    y: int

## Saves itself, as text.
struct Code:
    text: str

    def serialize(self, ref out: Serializer):
        out.text(self.text)

    def deserialize(ref src: Deserializer) -> Code fails ParseError:
        Code(text=src.text()?)

struct Node:
    name: str
    next: Node? = None

struct Before:
    name: str
    health: int
    tags: list[str]
    mood: Mood
    dropped: dict[str, int]
    spots: list[Spot]
    pair: (int, str)
    either: int | str
    code: Code
    chain: Node
    later: Mood? = None

enum NewMood:
    Gone
    Calm
    Angry(level: int, reason = "none")

struct NewSpot:
    y: int
    x: int
    z = 0

struct NewNode:
    name: str
    depth = 0
    next: NewNode? = None

struct After:
    name: str
    health: float
    mood: NewMood
    tags: list[str]
    spots: list[NewSpot]
    pair: (int, str)
    either: str | int
    code: Code
    chain: NewNode
    level = 1
    later: NewMood? = None

struct NeedsRank:
    name: str
    code: Code
    rank: int

struct NameAsInt:
    name: int

enum NoAngry:
    Calm
    Gone

struct MoodGone:
    mood: NoAngry

struct NoCode:
    name: str

struct ShortPair:
    pair: (int, str, bool)

## Each name down the chain.
def names(node: NewNode) -> list[str]:
    rest = node.next else NewNode(name="")
    if rest.name == "": return [node.name]
    [node.name] + names(rest)

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as error:
        return error.message
    return "loaded"

## What `data` loads as, as an `After`.
def show_after(data: bytes, stdout: Stdout):
    found = pack.loads[After](data) else as error:
        stdout.print("failed:", error.message)
        return
    stdout.print(found.name, found.health, found.level, found.tags, found.pair, found.either, found.code.text)
    stdout.print([(spot.x, spot.y, spot.z) for spot in found.spots])
    stdout.print(names(found.chain), found.chain.depth)
    match found.mood:
        case .Angry(level, reason):
            stdout.print("angry", level, reason)
        case _:
            stdout.print("other")
    stdout.print(found.later == .Gone)

def main(stdout: Stdout):
    chain = Node(name="a", next=Node(name="b", next=Node(name="c")))
    old = Before(
        name="ana",
        health=7,
        tags=["a", "b"],
        mood=.Angry(level=3),
        dropped={"x": 1},
        spots=[Spot(x=1, y=2), Spot(x=3, y=4)],
        pair=(5, "five"),
        either="text",
        code=Code(text="c-1"),
        chain=chain,
        later=.Gone,
    )
    data = pack.dumps(old)
    show_after(data, stdout)
    stdout.print(said(() => pack.loads[NeedsRank](data)?))
    stdout.print(said(() => pack.loads[NameAsInt](data)?))
    stdout.print(said(() => pack.loads[MoodGone](data)?))
    stdout.print(said(() => pack.loads[NoCode](data)?))
    stdout.print(said(() => pack.loads[ShortPair](data)?))
    # Data from before layouts loads into the same shape, and only that.
    first = without_layout(data)
    stdout.print(said(() => pack.loads[Before](first)?), said(() => pack.loads[After](first)?))
    stdout.print(data.len(), first.len())

## The same data as `FPK1` wrote it: the header, then the value, with no layout between.
def without_layout(data: bytes) -> bytes:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    start = at + 1 + (n | (data[at] << shift))
    mut items = [70, 80, 75, 49]
    for place in range(4, 12):
        items.append(data[place])
    for place in range(start, data.len()):
        items.append(data[place])
    bytes(items)
"#;

/// `std.pack` loads data saved from an earlier version of a type by its names: fields moved,
/// dropped and added with defaults, an `int` that's a `float` now, cases moved and given new
/// fields, union members moved, a type inside itself, and one that saves itself, read as it
/// reads. A field with no default that the data lacks, a value of another kind, a case that's
/// gone, a tuple of another size, and a dropped field only its own type can pass over each fail,
/// naming the field. Data from before layouts loads into the same shape only.
#[test]
fn pack_loads_an_earlier_version_of_a_type_by_its_names() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("pack_versions", PACK_VERSIONS)?;
    assert_eq!(
        printed,
        "ana 7.0 1 [\"a\", \"b\"] (5, \"five\") text c-1\n[(1, 2, 0), (3, 4, 0)]\n[\"a\", \"b\", \"c\"] 0\nangry 3 none\nTrue\n`rank` isn't in the data, and has no default, at byte 282\n`name` was saved as text, not an int, at byte 234\n`mood` was saved as `NoAngry.Angry`, a case `NoAngry` doesn't have now, at byte 244\n`code` isn't in the type now, and holds a `Code`, which saves itself its own way, so where it ends can't be found, at byte 267\n`pair` was saved as a tuple of 2 items, not 3, at byte 255\nloaded this was packed from a different `After`, at byte 4\n282 60\n"
    );
    Ok(())
}

/// What the next test builds: many small values in one stream, read in order, by name, fed a
/// byte at a time, cut off, and two `Packer`s' bytes one after the other.
const PACK_STREAMS: &str = r#"from std import pack
from std.pack import Packer, Unpacker

enum Kind:
    Move
    Say(text: str)

struct Event:
    who: int
    kind: Kind

## The next version: a field added with a default, and the cases moved.
enum NewKind:
    Say(text: str)
    Move

struct NewEvent:
    who: int
    kind: NewKind
    round = 0

def main(stdout: Stdout):
    mut out = Packer()
    for at in range(100):
        out.add(Event(who=at, kind=Kind.Move if at % 2 == 0 else Kind.Say(text="hi")))
    data = out.bytes()
    one = pack.dumps(Event(who=1, kind=.Move))
    stdout.print("stream", data.len(), "one value alone", one.len(), "a hundred alone", one.len() * 100)
    mut src = Unpacker.of(data)
    mut total = 0
    mut said = 0
    while True:
        event = src.next[Event]()? else break
        total += event.who
        if event.kind != .Move:
            said += 1
    stdout.print(total, said)
    # Read as the next version, by name.
    mut later = Unpacker.of(data)
    mut rounds = 0
    mut moves = 0
    while True:
        event = later.next[NewEvent]()? else break
        rounds += event.round
        if event.kind == .Move:
            moves += 1
    stdout.print(rounds, moves)
    # The next version adds to the same file: each value reads by its own layout.
    mut more = Packer()
    more.add(NewEvent(who=500, kind=.Say(text="new"), round=3))
    mixed = data + more.bytes()
    mut both = Unpacker.of(mixed)
    mut count = 0
    mut last = NewEvent(who=0, kind=.Move)
    while True:
        event = both.next[NewEvent]()? else break
        count += 1
        last = event
    stdout.print(count, last.who, last.round)
    # A stream fed a byte at a time gives each value once all of it has come.
    mut fed = Unpacker.stream()
    mut got = 0
    for at in range(data.len()):
        fed.feed(data[at:at + 1])
        while True:
            _ = fed.next[Event]()? else break
            got += 1
    stdout.print("fed", got)
    # Cut off, bytes that are all there are fail.
    mut cut = Unpacker.of(data[0:data.len() - 1])
    mut read = 0
    mut failed = ""
    while True:
        found = cut.next[Event]() else as error:
            failed = error.message
            break
        if found is None: break
        read += 1
    stdout.print(read, failed)
    # `take` gives what's new since the last, to send.
    mut sending = Packer()
    sending.add(Event(who=1, kind=.Move))
    first = sending.take()
    sending.add(Event(who=2, kind=.Move))
    second = sending.take()
    stdout.print(first.len(), second.len())
"#;

/// A `pack.Packer` writes each type's layout once, so a hundred small values take an eighth of
/// what a hundred `dumps` do. A `pack.Unpacker` reads them back in order, or by name as the next
/// version of the type, from a file another `Packer` added to, or fed a byte at a time; bytes cut
/// off fail after the last whole value.
#[test]
fn pack_streams_write_each_layout_once() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("pack_streams", PACK_STREAMS)?;
    assert_eq!(
        printed,
        "stream 638 one value alone 53 a hundred alone 5300\n4950 50\n0 50\n101 500 3\nfed 100\n99 the data ends too soon, at byte 637\n56 4\n"
    );
    Ok(())
}

/// What the next test builds: a few bytes that ask for millions of values made from nothing.
const PACK_BOMB: &str = r#"from std import pack

struct Hollow:
    @skip
    note = "nothing saved"

struct Big:
    one = 1
    two = 2
    three = 3
    four = 4
    five = 5
    six = 6

## `n` as `std.pack` writes a length.
def varint(n: int) -> list[int]:
    mut rest = n
    mut out = list[int]()
    while rest >= 128:
        out.append((rest & 127) | 128)
        rest = rest >> 7
    out.append(rest)
    out

## Bytes that ask for `count` items that each take no bytes, from `start`, the header and a layout.
def asking(start: bytes, count: int) -> bytes:
    start + bytes(varint(count))

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as error:
        return error.message
    return "loaded"

def main(stdout: Stdout):
    empty = pack.dumps([Hollow()])
    start = empty[0:empty.len() - 1]
    # The same shape: a list of what saves nothing.
    stdout.print(said(() => pack.loads[list[Hollow]](asking(start, 2_000_000))?))
    # A layout written by hand, of structs with no fields, loaded by name into a bigger struct.
    layout = "l S 0".encode()
    named = bytes([70, 80, 75, 50, 1, 2, 3, 4, 5, 6, 7, 8, layout.len()]) + layout
    stdout.print(said(() => pack.loads[list[Big]](asking(named, 2_000_000))?))
    # What's honest loads.
    stdout.print(said(() => pack.loads[list[Hollow]](asking(start, 1000))?))
    stdout.print(said(() => pack.loads[list[Big]](asking(named, 1000))?))
    # The limit is the program's to set.
    small = pack.Limits(empty=10)
    stdout.print(said(() => pack.loads[list[Hollow]](asking(start, 100), limits=small)?))
    # Fields filled from defaults count against the items a load may read.
    few = pack.Limits(length=500)
    one_byte = "l S 1 x b".encode()
    filled = bytes([70, 80, 75, 50, 1, 2, 3, 4, 5, 6, 7, 8, one_byte.len()]) + one_byte + bytes(varint(100)) + bytes([1 for _ in range(100)])
    stdout.print(said(() => pack.loads[list[Big]](filled, limits=few)?))
"#;

/// Bytes can't ask for far more memory than they are. Items that take no bytes count against
/// `Limits.empty`, whether the data is read in order or by name, and fields filled from their
/// defaults count against the items a load may read.
#[test]
fn pack_refuses_values_made_from_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("pack_bomb", PACK_BOMB)?;
    assert_eq!(
        printed,
        "it holds more than the limit of 65536 items that take no bytes, at byte 21\nit holds more than the limit of 65536 items that take no bytes, at byte 21\nloaded\nloaded\nit holds more than the limit of 10 items that take no bytes, at byte 19\nit holds more than the limit of 500 items, at byte 90\n"
    );
    Ok(())
}

/// What the next test builds: a type saved as a union's member, written by hand as generated code will be.
const UNION_MEMBERS: &str = r#"from std import json, pack, toml

struct Circle:
    radius: float

struct Square:
    side: float

## Saved as a union of `Circle`, `Square` and `int` is: tagged with its member's type.
struct Shape:
    kind = 2
    circle = Circle(radius=0.0)
    square = Square(side=0.0)
    count = 0

    def serialize(self, ref out: Serializer):
        if self.kind == 0:
            out.member("Circle", 0)
            out.value(self.circle)
        elif self.kind == 1:
            out.member("Square", 1)
            out.value(self.square)
        else:
            out.member("int", 2)
            out.value(self.count)
        out.end_member()

    def deserialize(ref src: Deserializer) -> Shape fails ParseError:
        at = src.member(["Circle", "Square", "int"])?
        mut made = Shape()
        if at == 0:
            made = Shape(kind=0, circle=src.value[Circle]()?)
        elif at == 1:
            made = Shape(kind=1, square=src.value[Square]()?)
        else:
            made = Shape(count=src.value[int]()?)
        src.end_member()?
        made

struct Scene:
    shapes: list[Shape]

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as error:
        return error.message
    return "loaded"

## Where the value starts: after the header, and the layout saved with it.
def body(data: bytes) -> int:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    at + 1 + (n | (data[at] << shift))

def main(stdout: Stdout):
    scene = Scene(shapes=[Shape(kind=0, circle=Circle(radius=1.5)), Shape(kind=1, square=Square(side=2.0)), Shape(count=7)])
    text = json.dumps(scene)
    stdout.print(text)
    back = json.loads[Scene](text) else Scene(shapes=[])
    stdout.print(json.dumps(back) == text)
    written = toml.dumps(scene)
    stdout.print(written)
    again = toml.loads[Scene](written) else Scene(shapes=[])
    stdout.print(json.dumps(again) == text)
    data = pack.dumps(scene)
    packed = pack.loads[Scene](data) else Scene(shapes=[])
    stdout.print(json.dumps(packed) == text)
    stdout.print(said(() => json.loads[Scene]("{\"shapes\": [{\"Triangle\": {}}]}")?))
    stdout.print(said(() => json.loads[Scene]("{\"shapes\": [5]}")?))
    stdout.print(said(() => toml.loads[Scene]("[[shapes]]\nTriangle = 1\n")?))
    mut items = list[int]()
    for at in range(data.len()):
        items.append(data[at])
    items[body(data) + 1] = 9
    stdout.print(said(() => pack.loads[Scene](bytes(items))?))
"#;

/// A union's member is saved tagged with its type's name in JSON and TOML, and by its place in
/// `pack`, and read back the same, its own value whatever it is: a struct, or an `int`. A member
/// that isn't one of the union's, or something that isn't a member at all, doesn't load.
#[test]
fn union_members_are_saved_tagged_in_every_format() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("union_members", UNION_MEMBERS)?;
    assert_eq!(
        printed,
        "{\"shapes\": [{\"Circle\": {\"radius\": 1.5}}, {\"Square\": {\"side\": 2.0}}, {\"int\": 7}]}\nTrue\n[[shapes]]\n\n[shapes.Circle]\nradius = 1.5\n\n[[shapes]]\n\n[shapes.Square]\nside = 2.0\n\n[[shapes]]\nint = 7\n\nTrue\nTrue\n`shapes[0]` is \"Triangle\", which is none of `Circle | Square | int`\n`shapes[0]` should be a Circle | Square | int, but it's an int\n`shapes[0]` is \"Triangle\", which is none of `Circle | Square | int`\n`Circle | Square | int` has no member 9, at byte 34\n"
    );
    Ok(())
}

/// What the next test builds: fields that hold unions, saved with their methods made for them.
const UNION_FIELDS: &str = r#"from std import json, pack, toml

struct Circle:
    radius: float

struct Square:
    side: float

enum Event:
    Spawn(what: Circle | Square)
    Quit

struct Scene:
    main: Circle | Square
    shapes: list[Circle | Square | int]
    named: dict[str, Circle | Square]
    count: int | str = 3

struct Log:
    events: list[Event]
    picked: (Circle | int)? = None

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as error:
        return error.message
    return "loaded"

def main(stdout: Stdout):
    scene = Scene(main=Square(side=2.0), count="many", shapes=[Circle(radius=1.5), 7], named={"a": Circle(radius=2.0)})
    empty = Scene(main=Circle(radius=0.0), shapes=[], named={})
    text = json.dumps(scene)
    stdout.print(text)
    stdout.print((json.loads[Scene](text) else empty) == scene)
    stdout.print(toml.dumps(scene))
    stdout.print((toml.loads[Scene](toml.dumps(scene)) else empty) == scene)
    stdout.print((pack.loads[Scene](pack.dumps(scene)) else empty) == scene)
    log = Log(events=[Event.Spawn(Circle(radius=0.5)), Event.Quit], picked=4)
    stdout.print(json.dumps(log))
    stdout.print((json.loads[Log](json.dumps(log)) else Log(events=[])) == log)
    stdout.print((pack.loads[Log](pack.dumps(log)) else Log(events=[])) == log)
    stdout.print((pack.loads[Log](pack.dumps(Log(events=[]))) else log).picked is None)
    # A default that's one of a union's members, and what doesn't load.
    stdout.print((json.loads[Scene]("{\"main\": {\"Circle\": {\"radius\": 1.0}}, \"shapes\": [], \"named\": {}}") else scene).count)
    stdout.print(said(() => json.loads[Scene]("{\"main\": {\"Triangle\": {}}, \"shapes\": [], \"named\": {}}")?))
    stdout.print(said(() => json.loads[Scene]("{\"main\": {\"radius\": 1.0}, \"shapes\": [], \"named\": {}}")?))
    stdout.print(said(() => json.loads[Log]("{\"events\": [], \"picked\": {\"str\": \"x\"}}")?))
"#;

/// A field that holds a union, as it is, in a list, a dict's values, an optional or an enum's
/// case, is saved tagged with its member in every format, and loads back the same. A default
/// that's one of the members is the union's, and a tag that's none of them doesn't load.
#[test]
fn fields_holding_unions_are_saved_tagged_in_every_format() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by("union_fields", UNION_FIELDS)?;
    assert_eq!(
        printed,
        "{\"main\": {\"Square\": {\"side\": 2.0}}, \"shapes\": [{\"Circle\": {\"radius\": 1.5}}, {\"int\": 7}], \"named\": {\"a\": {\"Circle\": {\"radius\": 2.0}}}, \"count\": {\"str\": \"many\"}}\nTrue\n[main.Square]\nside = 2.0\n\n[[shapes]]\n\n[shapes.Circle]\nradius = 1.5\n\n[[shapes]]\nint = 7\n\n[named.a.Circle]\nradius = 2.0\n\n[count]\nstr = \"many\"\n\nTrue\nTrue\n{\"events\": [{\"Spawn\": {\"what\": {\"Circle\": {\"radius\": 0.5}}}}, \"Quit\"], \"picked\": {\"int\": 4}}\nTrue\nTrue\nTrue\n3\n`main` is \"Triangle\", which is none of `Circle | Square`\n`main` is \"radius\", which is none of `Circle | Square`\n`picked` is \"str\", which is none of `Circle | int`\n"
    );
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

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() else as error:
        return error.message
    return "loaded"

def changed(data: bytes, at: int, to: int) -> bytes:
    mut items = list[int]()
    for b in range(data.len()):
        items.append(to if b == at else data[b])
    bytes(items)

## Where the value starts: after the header, and the layout saved with it.
def body(data: bytes) -> int:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    at + 1 + (n | (data[at] << shift))

def main(stdout: Stdout):
    mut world = World()
    ash = world.units.add(Unit(name="ash"))
    gone = world.units.add(Unit(name="gone"))
    vex = world.units.add(Unit(name="vex", target=ash))
    _ = world.units.remove(gone)
    kit = world.units.add(Unit(name="kit", target=vex))
    _ = world.units.remove(ash)
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
    stdout.print(said(() => pack.loads[World](changed(data, at=body(data) + 6, to=9))?))
    stdout.print(said(() => pack.loads[World](changed(exact, at=body(exact), to=2))?))
    stdout.print(said(() => pack.loads[World](changed(exact, at=body(exact) + 20, to=20))?))
    stdout.print(said(() => pack.loads[World](changed(exact, at=body(exact) + 1, to=200))?))
    # Every truncation fails, and every byte changed loads or fails, in both modes.
    mut seed = 11
    for good in [data, exact]:
        mut cut = 0
        for size in range(good.len()):
            if said(() => pack.loads[World](good[0:size])?) == "loaded":
                cut += 1
        mut changes = 0
        for at in range(good.len()):
            for _ in range(30):
                seed = (seed * 1103515245 + 12345) % 2147483648
                loaded: World? = pack.loads[World](changed(good, at=at, to=(seed >> 16) & 255)) else as _:
                    None
                if loaded is not None:
                    # What loads has every Id somewhere in its Table, or gone.
                    for id in loaded.selected:
                        _ = loaded.units.get(id)
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
        "84 2 True True\nvex -\nkit vex\n95 88 2 kit\nTrue False\nTrue\n`units[0].target` is out of range, at byte 75\n`units` has 1 slot in use but 0 items, at byte 72\n`selected[0]` is item 10 of `units`, which has only 3\nthe data ends too soon, at byte 75\ntruncated 0 changed 2520\ntruncated 0 changed 2850\n"
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
    let source = "\
from std.net import NetError

def listening(net: Net) -> bool fails NetError:
    with net.listen(0)? as server:
        return server.port > 0
    False

def main(stdout: Stdout, net: Net):
    up = listening(net) else:
        stdout.print(\"failed\")
        return
    stdout.print(\"listening\", up)
";
    same_as_the_jit("net_kind", source)
}

/// A field read into a variable and then lent to a call that changes the place it was read from,
/// `own = xs[i].tags` then `xs[i].grown_by(own)`, takes an owner of its own: the call appends to
/// the same list, alone, and grows it, and a borrow without one read on from where it was.
/// Checking proofs, a borrow lent to a call is held for the call too, and what it came from has
/// to still hold it after: that stopped this program before it was fixed.
#[test]
fn a_borrow_lent_to_a_call_that_changes_where_it_came_from_has_an_owner() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let expected = "8000 [1, 2, 3] [4, 5, 6]\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("borrow_lent_to_change", BORROW_LENT_TO_CHANGE, how)?;
        assert_eq!((out.as_str(), code), (expected, Some(0)), "{how:?}: {err}");
        let dir = folder("borrow_lent_to_change_checked")?;
        let (program, executable) = (dir.join("checked.fen"), dir.join("checked"));
        std::fs::write(&program, BORROW_LENT_TO_CHANGE)?;
        let built = crate::executables::fennec_command()
            .arg("build")
            .args(how)
            .arg(&program)
            .arg("-o")
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .env("FENNEC_CHECK_PROOFS", "1")
            .output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            expected,
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

const BORROW_LENT_TO_CHANGE: &str = r"struct Unit:
    hp: int
    tags: list[int]

    def grown_by(ref self, values: list[int]):
        for value in values:
            self.tags.append(value)

def given(captured: list[Unit], at: int) -> int:
    mut copy_of = captured
    copy_of[at % 2].tags.append(at)
    own_tags = copy_of[at % 2].tags
    copy_of[at % 2].grown_by(own_tags)
    copy_of[at % 2].tags.len()

def main(stdout: Stdout):
    captured = [Unit(1, [1, 2, 3]), Unit(2, [4, 5, 6])]
    mut total = 0
    for x in range(1000):
        total += given(captured, x)
    stdout.print(total, captured[0].tags, captured[1].tags)
";

/// A name given its own value, `x = x`, keeps what it holds: an enum or struct held in place,
/// one inside another, a field, a list's item, a boxed enum and a parameter's copy given back.
/// Written as a copy, it let go of the lists it held before taking them again, so the next lists
/// made took their memory, and its value printed theirs. The random programs found it.
#[test]
fn a_name_given_its_own_value_keeps_what_it_holds() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let expected = "B(a) Unit(hp=1, tags=[1, 2]) Pack(unit=Unit(hp=2, tags=[3]), names=[\"n\"]) [Unit(hp=3, tags=[4, 5])] Unit(hp=4, tags=[7]) 2 3\n[6]\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("own_value", OWN_VALUE, how)?;
        assert_eq!((out.as_str(), code), (expected, Some(0)), "{how:?}: {err}");
    }
    Ok(())
}

const OWN_VALUE: &str = r#"enum Node:
    Leaf(name: str)
    Branch(label: str, children: list[Node])

enum Tree:
    Leaf
    Pair(left: Tree, right: Tree, tags: list[int])

struct Unit:
    hp: int
    tags: list[int]

struct Pack:
    unit: Unit
    names: list[str]

def show(node: Node) -> str:
    match node:
        case .Leaf(name):
            "L(" + name + ")"
        case .Branch(label, children):
            mut text = "B(" + label
            for child in children:
                text += " " + show(child)
            text + ")"

def last_use(given: Unit) -> Unit:
    mut u = given
    u = u
    u

def main(stdout: Stdout):
    mut t = Node.Branch(label="a", children=list[Node]())
    t = t
    mut u = Unit(hp=1, tags=[1, 2])
    u = u
    mut p = Pack(unit=Unit(hp=2, tags=[3]), names=["n"])
    p = p
    p.unit = p.unit
    mut xs = [Unit(hp=3, tags=[4, 5])]
    xs[0] = xs[0]
    mut tree = Tree.Pair(left=.Leaf, right=.Leaf, tags=[6])
    tree = tree
    ys = last_use(Unit(hp=4, tags=[7]))
    other = [Node.Leaf(name="x"), Node.Leaf(name="y")]
    junk = [[9, 9, 9], [8, 8], [7]]
    stdout.print(show(t), u, p, xs, ys, other.len(), junk.len())
    match tree:
        case .Pair(_, _, tags):
            stdout.print(tags)
        case .Leaf:
            pass
"#;

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

/// A finished program goes in the store under its build's root hash, with its functions' hashes
/// beside it, so building it again unchanged compiles and links nothing and gives the same bytes. A
/// change to the program, or a release build of it, is a build of its own. See `docs/store.md`.
#[test]
fn an_unchanged_build_takes_its_program_from_the_store() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("stored")?;
    let home = dir.join("home");
    let source = "def main(stdout: Stdout):\n    stdout.print(\"stored\")\n";
    std::fs::write(dir.join("main.fen"), source)?;
    let build = |args: &[&str]| -> std::io::Result<(String, Vec<u8>)> {
        let output = crate::executables::fennec_command()
            .args(args)
            .current_dir(&dir)
            .env("FENNEC_HOME", &home)
            .env_remove("FENNEC_NO_CACHE")
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        Ok((String::from_utf8_lossy(&output.stdout).into_owned(), std::fs::read(dir.join("built"))?))
    };
    let (first, program) = build(&["build", "main.fen", "-o", "built"])?;
    let (again, same) = build(&["build", "main.fen", "-o", "built"])?;
    assert!(first.contains("compiled 1 of 1"), "{first}");
    assert!(again.contains("compiled 0 of 1") && again.contains("linked in 0 ms"), "{again}");
    assert_eq!(program, same, "the program from the store is the one built");
    let ran = Command::new(dir.join("built")).output()?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "stored\n");
    let stored: Vec<String> =
        std::fs::read_dir(home.join("cache").join(fennec_build::FOLDER).join("programs"))?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()?;
    assert_eq!(stored.len(), 2, "the program and its functions' hashes: {stored:?}");
    std::fs::write(dir.join("main.fen"), source.replace("stored", "changed"))?;
    let (changed, _) = build(&["build", "main.fen", "-o", "built"])?;
    assert!(changed.contains("compiled 1 of 1"), "a changed program is built: {changed}");
    let (released, _) = build(&["build", "--release", "main.fen", "-o", "built"])?;
    assert!(!released.contains("linked in 0 ms"), "a release build is a build of its own: {released}");
    std::fs::remove_dir_all(&dir)?;
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

/// Floats have one equality, order and hash everywhere: `0.0` is `-0.0`, and NaN comes after
/// everything and is the same as any other NaN, whatever its sign. So this is held to what that
/// decision says, as the interpreter pinned it. The JIT's `==` and keys never followed it.
#[test]
fn floats_compare_and_hash_as_decided() -> std::io::Result<()> {
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

def less(a: float, b: float) -> bool:
    a < b

def most(a: float, b: float) -> bool:
    a <= b

def more(a: float, b: float) -> bool:
    a > b

def least(a: float, b: float) -> bool:
    a >= b

def main(stdout: Stdout):
    zero = 0.0
    minus = -zero
    nan = 1e300 * 1e300 - 1e300 * 1e300
    other = -nan
    one = 1.0
    stdout.print(same(zero, b=minus), same(nan, b=nan), same(nan, b=other), same(nan, b=one), same(one, b=one))
    stdout.print(different(zero, b=minus), different(nan, b=other), different(nan, b=one))
    stdout.print(less(minus, b=zero), less(zero, b=minus), less(one, b=nan), less(nan, b=one), less(nan, b=other))
    stdout.print(most(minus, b=zero), most(nan, b=other), most(nan, b=one), most(one, b=nan))
    stdout.print(more(nan, b=one), more(one, b=nan), more(nan, b=other), more(zero, b=minus))
    stdout.print(least(nan, b=one), least(one, b=nan), least(nan, b=other), least(minus, b=zero))
    stdout.print(zero == -0.0, nan == 1.0, 1.0 == nan, nan != 1.0, 1.0 != nan)
    stdout.print(nan < 1.0, 1.0 < nan, nan <= 1.0, 1.0 <= nan, nan > 1.0, 1.0 > nan, nan >= 1.0, 1.0 >= nan)
    stdout.print(Pair(x=zero, y=nan) == Pair(x=minus, y=other), Pair(x=1.0, y=nan) == Pair(x=1.0, y=2.0))
    stdout.print((zero, 1.0) < (minus, 2.0), (zero, 2.0) < (minus, 1.0))
    stdout.print((zero, nan) == (minus, other), (nan, 1.0) > (1.0, 1.0))
    stdout.print(zero in [minus], other in [nan])
    stdout.print(max([1.0, nan, 2.0]) == nan, min([other, 1.0, nan]), max(other, 1.0) == nan, min(zero, 1.0))
    stdout.print(sorted([nan, 2.0, other, 1.0])[:2], sorted([(nan, 1), (1.0, 2)])[0])
";
    let dir = folder("floats_ordered_as_decided")?;
    let program = dir.join("floats.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("floats"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output()?));
    let interpreted = interpreted("floats_ordered_as_decided", source)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, interpreted);
    assert_eq!(
        ours.0,
        "True True True False True
False False True
False False True False False
True True False True
True False False False
True False True True
True False False True True
False True False True True False True False
True False
True False
True True
True True
True 1.0 True 0.0
[1.0, 2.0] (1.0, 2)
"
    );
    Ok(())
}

/// On Linux, with the `musl` folder beside `fennec`, a release build is a program on its own: it
/// asks for no loader and no shared library, so it runs in an empty container. Where that folder
/// isn't there, nothing is checked, unless `FENNEC_EXPECT_STATIC` says it has to be.
#[test]
fn a_release_build_on_linux_needs_nothing_from_the_machine() -> std::io::Result<()> {
    let musl = Path::new(env!("CARGO_BIN_EXE_fennec")).with_file_name("musl");
    let here = cfg!(target_os = "linux") && musl.join("self-contained/crt1.o").is_file();
    if std::env::var_os("FENNEC_EXPECT_STATIC").is_some_and(|expected| !expected.is_empty()) {
        assert!(here, "FENNEC_EXPECT_STATIC is set, but there's no `musl` folder at {}", musl.display());
    }
    if !here || !ready() {
        return Ok(());
    }
    let dir = folder("static")?;
    let source = "def main(stdout: Stdout):\n    counts = {word: word.len() for word in \"a fox ran\".split()}\n    total = sum(n * 2 for n in parallel([1, 2, 3]))\n    stdout.print(counts, total)\n";
    std::fs::write(dir.join("main.fen"), source)?;
    let built = crate::executables::fennec_command()
        .args(["build", "--release", "main.fen", "-o", "main"])
        .current_dir(&dir)
        .env("FENNEC_HOME", dir.join("home"))
        .output()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // It holds musl's notices, which its license asks to go with every copy.
    let notices =
        crate::executables::fennec_command().args(["notices", "main"]).current_dir(&dir).output()?;
    let notices = String::from_utf8_lossy(&notices.stdout);
    assert!(notices.starts_with("musl libc") && notices.contains("Rich Felker"), "{notices}");
    let program = std::fs::read(dir.join("main"))?;
    // The kinds of segment that ask for a loader and for shared libraries.
    let (interp, dynamic) = (3, 2);
    let kinds = segment_kinds(&program);
    assert!(!kinds.is_empty(), "the program isn't a 64-bit ELF file");
    assert!(!kinds.contains(&interp) && !kinds.contains(&dynamic), "segment kinds {kinds:?}");
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty)?;
    let ran = Command::new(dir.join("main")).env_clear().current_dir(&empty).output()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "{\"a\": 1, \"fox\": 3, \"ran\": 3} 12\n");
    Ok(())
}

/// The kind of each segment a little-endian 64-bit ELF file's program header names, or nothing
/// when it isn't one.
fn segment_kinds(elf: &[u8]) -> Vec<u32> {
    let read = |at: usize, size: usize| -> Option<u64> {
        let bytes = elf.get(at..at + size)?;
        Some(bytes.iter().rev().fold(0, |value, byte| value << 8 | u64::from(*byte)))
    };
    let (Some(start), Some(each), Some(count)) = (read(0x20, 8), read(0x36, 2), read(0x38, 2)) else {
        return Vec::new();
    };
    if elf.get(..5) != Some(b"\x7fELF\x02") {
        return Vec::new();
    }
    (0..count)
        .filter_map(|at| read(usize::try_from(start + at * each).ok()?, 4))
        .filter_map(|kind| u32::try_from(kind).ok())
        .collect()
}

/// Floats have one equality and hash everywhere: `0.0` is `-0.0`, and NaN is the same as any other
/// NaN, whatever its sign. So this is held to what that decision says, as the interpreter pinned
/// it. The JIT's `==` and keys never followed it.
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
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    let interpreted = interpreted("floats_as_decided", source)?;
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

/// Floats as keys hash as they compare: `-0.0` is `0.0`, and NaN is one key.
#[test]
fn float_keys_hash_as_decided() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
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
    let dir = folder("float_keys_hashed")?;
    let program = dir.join("keys.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("keys"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = outcome(&Command::new(&executable).output_within()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ran.0, "True\n1 True\n1 True\n1 1\nTrue\n", "{}", ran.1);
    Ok(())
}

/// A struct, enum or union holding a float is a key, and hashes as `==` has it: `-0.0` is `0.0`
/// and every NaN is one key, in a `float` field or an `f32` one, at run time and in a constant
/// worked out while compiling. An enum with an `f32` field compared by its bits, so
/// `.At(value=f32(0.0)) == .At(value=f32(-0.0))` was `False`, and a struct with one was two keys.
#[test]
fn keys_holding_floats_hash_as_they_compare_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "float_field_keys",
        r#"struct Point:
    x: float
    y: float

struct Small:
    at: f32

enum Reading32:
    At(value: f32)
    Gone

enum Reading:
    Level(value: float)
    Off

type Mark = Point | str

def nan() -> float:
    big = 1e308 * 10.0
    big - big

def counted() -> int:
    other = -nan()
    seen = {Point(x=0.0, y=1.0), Point(x=-0.0, y=1.0), Point(x=nan(), y=2.0), Point(x=other, y=2.0)}
    seen.len()

SEEN = counted()

def main(stdout: Stdout):
    missing = nan()
    other = -missing
    mut names = {Point(x=0.0, y=1.0): "zero"}
    names[Point(x=-0.0, y=1.0)] = "negative zero"
    names[Point(x=missing, y=2.0)] = "nan"
    names[Point(x=other, y=2.0)] = "nan again"
    stdout.print(names.len(), names[Point(x=0.0, y=1.0)], names[Point(x=missing, y=2.0)])
    stdout.print(Point(x=-0.0, y=1.0) in names, Point(x=0.5, y=1.0) in names)
    small = {Small(at=f32(0.0)), Small(at=f32(-0.0)), Small(at=f32(missing)), Small(at=f32(other))}
    readings = {Reading.Level(value=-0.0): 1, Reading.Level(value=0.0): 2, Reading.Off: 3}
    marks: set[Mark] = {Point(x=0.0, y=0.0), Point(x=-0.0, y=-0.0), "a"}
    stdout.print(small.len(), readings.len(), readings[Reading.Level(value=0.0)], marks.len())
    stdout.print(SEEN, counted())
    zero = f32(0.0)
    stdout.print(Reading32.At(value=zero) == .At(value=-zero), Small(at=zero) == Small(at=-zero))
    stdout.print(Reading.Level(value=0.0) == .Level(value=-0.0), Reading32.At(value=f32(missing)) == .At(value=f32(other)))
    sized = {Reading32.At(value=zero): 1, Reading32.At(value=-zero): 2, Reading32.At(value=f32(missing)): 3, Reading32.At(value=f32(other)): 4}
    stdout.print(sized.len(), sized[Reading32.At(value=zero)], {f32(0.0), f32(-0.0), f32(missing), f32(other)}.len())
"#,
        "2 negative zero nan again\nTrue False\n2 2 2 2\n2 2\nTrue True\nTrue True\n2 2 2\n",
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
    _ = names.remove(ids[6])
    _ = names.remove(ids[2])
    _ = names.remove(ids[4])
    first = names.add(Named("a"))
    second = names.add(Named("b"))
    _ = names.remove(ids[0])
    third = names.add(Named("c"))
    fourth = names.add(Named("d"))
    fifth = names.add(Named("e"))
    stdout.print(first, second, third, fourth, fifth, names.len())
    stdout.print(names.get(ids[2]) is None, names[first], names[fifth])
    # A stale `Id` frees nothing, so it can't move where to look.
    _ = names.remove(ids[2])
    sixth = names.add(Named("f"))
    stdout.print(sixth, names.values())
    # A copy keeps looking from where it was, apart from the original.
    mut copied = names
    _ = copied.remove(ids[1])
    _ = names.remove(ids[3])
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
    _ = names.add("kit")
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
    _ = numbers.add(1)
    _ = numbers.add(2)
    numbers.remove_where(n => n % 2 == 0)
    stdout.print(numbers.values(), numbers.len())
    numbers.clear()
    stdout.print(numbers.values(), numbers.len())
    gone = numbers.add(5)
    stdout.print(numbers.values(), numbers.len(), numbers[gone])
    mut cleared = names
    _ = cleared.add("again")
    cleared.clear()
    stdout.print(cleared.len(), names.len(), cleared.values())
    _ = numbers.remove(gone)
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
    _ = world.enemies.remove(near)
    stdout.print(world.enemies.modify(near, foe => foe.take_damage(1)), world.enemies.len())
    mut numbers = Table[int]()
    one = numbers.add(1)
    _ = numbers.modify(one, n => n * 5)
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
    _ = numbers.remove(ids[0])
    _ = numbers.remove(ids[3])
    _ = numbers.remove(ids[4])
    _ = numbers.remove(ids[7])
    stdout.print(total(numbers), numbers.len())
    _ = numbers.add(100)
    stdout.print(total(numbers), numbers.len())

    mut units = Table[Unit]()
    first = units.add(Unit("first", [1]))
    _ = units.add(Unit("second is a much longer name than fits", [2, 3]))
    last = units.add(Unit("é", []))
    stdout.print(named(units))
    _ = units.remove(first)
    _ = units.remove(last)
    stdout.print(named(units))

    mut points = Table[Point]()
    gone = points.add(Point(0, 0.5))
    _ = points.add(Point(1, 1.5))
    _ = points.add(Point(2, 2.5))
    _ = points.remove(gone)
    for id, point in points.items():
        stdout.print(point.x, point.y, points[id].x)

    mut words = Table[str]()
    _ = words.add("a")
    _ = words.add("a word too long to fit in a header")
    for word in words.values():
        stdout.print(word)

    # Nothing in it, then nothing left in it.
    mut empty = Table[int]()
    for id, number in empty.items():
        stdout.print(number)
    only = empty.add(1)
    _ = empty.remove(only)
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
    _ = changing.remove(kept[1])
    for id, number in before:
        stdout.print(number)
    # Changed between rounds of an outer loop.
    for round in range(3):
        for number in changing.values():
            stdout.print(round, number)
        _ = changing.add(round * 100)
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
    let source = "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut numbers = Table[int]()\n    for n in range(1000):\n        _ = numbers.add(n)\n    mut found = 0\n    for round in range(100):\n        for id, number in numbers.items():\n            found += number\n        for number in numbers.values():\n            found += number\n    stdout.print(found)\n";
    let counts = counts_of("walking_no_list", source)?;
    assert!(count(&counts, "allocations") < Some(40), "{counts}");
    Ok(())
}

/// `for ref` and `for` over a `Table`, walking its slots in place: the slots taken out are
/// stepped over, one used again is walked where it is, and `continue` and `break` put the item
/// back as the end of the body does.
const WALKING_SLOTS: &str = r#"from std.collections import Id, Table

struct Enemy:
    name: str
    hp: int
    x: float

struct World:
    enemies = Table[Enemy]()

@hot
def step(ref world: World, dt: float):
    for ref enemy in world.enemies:
        enemy.x += dt
        if enemy.hp < 0: continue
        enemy.hp -= 1

def first_low(ref world: World) -> str:
    mut found = ""
    for ref enemy in world.enemies:
        if enemy.hp < 3:
            enemy.name = enemy.name + "!"
            found = enemy.name
            break
    found

def main(stdout: Stdout):
    mut world = World()
    a = world.enemies.add(Enemy(name="ash", hp=5, x=0.0))
    b = world.enemies.add(Enemy(name="bo", hp=-1, x=1.0))
    c = world.enemies.add(Enemy(name="cy", hp=2, x=2.0))
    _ = world.enemies.remove(b)
    d = world.enemies.add(Enemy(name="di", hp=9, x=3.0))
    for _ in range(3):
        step(ref world, 0.5)
    for enemy in world.enemies:
        stdout.print(enemy.name, enemy.hp, enemy.x)
    stdout.print(first_low(ref world), world.enemies[c].name, world.enemies[a].hp, d in world.enemies)
    mut total = 0
    for enemy in world.enemies:
        total += enemy.hp
    stdout.print(total, world.enemies.len())
"#;

#[test]
fn walking_a_tables_slots_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("walking_slots", WALKING_SLOTS)
}

/// A `@hot` loop changing each item of a `Table` in place makes nothing and copies nothing:
/// only the `Table` itself, before the rounds, is allocated.
#[test]
fn walking_a_tables_slots_in_place_allocates_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "walking_slots_in_place",
        r"from std.collections import Table

struct Enemy:
    hp: int
    x: float

@hot
def step(ref enemies: Table[Enemy], dt: float):
    for ref enemy in enemies:
        enemy.x += dt
        enemy.hp -= 1

def main(stdout: Stdout):
    mut enemies = Table[Enemy]()
    for n in range(1000):
        _ = enemies.add(Enemy(hp=n, x=0.0))
    mut total = 0
    for round in range(100):
        step(ref enemies, 0.5)
        for enemy in enemies:
            total += enemy.hp
    stdout.print(total)
",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    assert!(count(&counts, "allocations") < Some(40), "{counts}");
    Ok(())
}

/// The same with the `Table` in a field, gone through by a plain loop after each round too: the
/// plain loop reads the field where it is, so it holds no owner that would make the next round's
/// `for ref` find the `Table` shared and copy it.
#[test]
fn walking_a_tables_slots_in_a_field_copies_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "walking_slots_in_a_field",
        r"from std.collections import Table

struct Enemy:
    hp: int
    x: float

struct World:
    enemies = Table[Enemy]()

@hot
def step(ref world: World, dt: float):
    for ref enemy in world.enemies:
        enemy.x += dt
        enemy.hp -= 1

def main(stdout: Stdout):
    mut world = World()
    for n in range(1000):
        _ = world.enemies.add(Enemy(hp=n, x=0.0))
    mut total = 0
    for round in range(100):
        step(ref world, 0.5)
        for enemy in world.enemies:
            total += enemy.hp
    stdout.print(total)
",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
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
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut names = Table[str]()\n    fox = names.add(\"fox\")\n    _ = names.remove(fox)\n    stdout.print(names[fox])\n",
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
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut ours = Table[str]()\n    mut theirs = Table[str]()\n    _ = ours.add(\"fox\")\n    kit = theirs.add(\"kit\")\n    stdout.print(ours[kit])\n",
    )?;
    same_message_as_the_jit(
        "other_table_store",
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut ours = Table[str]()\n    mut theirs = Table[str]()\n    _ = ours.add(\"fox\")\n    kit = theirs.add(\"kit\")\n    ours[kit] = \"den\"\n    stdout.print(ours.len())\n",
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
    _ = world.units.add("ash")
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

def inner(n: int) -> int fails Inner:
    if n > 1: fail Inner(what="too many")
    n

def outer(n: int) -> int fails Wrapped:
    inner(n)? from Wrapped(why="inner failed")

## What `run` read, or what its failure says.
def shown(run: () -> Tool fails ParseError) -> str:
    read = run() else as error:
        return error.message
    f"{read}"

def main(stdout: Stdout):
    stdout.print(crypto.sha256("abc"), crypto.blake3("abc"))
    stdout.print(crypto.keyed_blake3("0000000000000000000000000000000000000000000000000000000000000000", "abc") else "no")
    stdout.print(crypto.equal("a", "a"), crypto.equal("a", "b"))
    tool = Tool(main_weapon="bow", uses=3)
    stdout.print(json.dumps(tool, names=.CamelCase), json.dumps(tool, names=.KebabCase))
    stdout.print(toml.dumps(tool) else "no")
    stdout.print(shown(() => toml.loads[Tool]("main_weapon = \"axe\"\nuses = 2\n")?))
    stdout.print(shown(() => toml.loads[Tool]("not toml")?))
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
    _ = xs.pop(0)
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

/// A failure says its call chain under it: the function it failed in, then each call it left on
/// its way out, as it does in a dev build and with `--release`. An error raised with `fail` is said
/// where it was raised, each `?` it passed up through in the chain. A runaway recursion's chain
/// stays short: a call repeated in a row is said once, and two functions calling each other keep
/// only the ends of the chain, with nothing that depends on how deep the machine let it go.
#[test]
fn a_failure_says_its_call_chain() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let raised = "error Missing:\n    name: str\n\ndef find(name: str) -> int fails Missing:\n    \
                  if name == \"\": fail Missing(name=\"nobody\")\n    name.len()\n\n\
                  def look(name: str) -> int fails Missing:\n    find(name)? + 1\n\n\
                  def main(stdout: Stdout) fails Missing:\n    stdout.print(look(\"\")?)\n";
    let bouncing = "def ping(n: int) -> int:\n    pong(n + 1)\n\ndef pong(n: int) -> int:\n    ping(n + 1)\n\n\
                    def main(stdout: Stdout):\n    stdout.print(ping(0))\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (_, said, code) = both_built_unpinned("raised_chain", raised, how)?;
        assert_eq!(code, Some(1));
        assert!(said.contains(" --> <dir>/raised_chain.fen:5:20\n"), "{how:?}: {said}");
        assert!(
            said.ends_with(
                "  = in `find`\n  = called from `look` at <dir>/raised_chain.fen:9:5\n  \
                 = called from `main` at <dir>/raised_chain.fen:12:18\n"
            ),
            "{how:?}: {said}"
        );
        let (_, said, _) = both_built_unpinned("bouncing_chain", bouncing, how)?;
        let calls = said.lines().filter(|line| line.contains("called from")).count();
        assert_eq!(calls, 2 * 16, "{how:?}: {said}");
        assert!(said.contains("  = ... more calls\n"), "{how:?}: {said}");
        assert!(
            said.ends_with("  = called from `main` at <dir>/bouncing_chain.fen:8:18\n"),
            "{how:?}: {said}"
        );
    }
    Ok(())
}

/// What a program built with these options printed, said and gave back, with its folder written as
/// `<dir>`, held to nothing pinned.
fn both_built_unpinned(name: &str, source: &str, how: &[&str]) -> std::io::Result<Outcome> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let output = executable.to_string_lossy().into_owned();
    let args: Vec<&str> =
        ["build"].into_iter().chain(how.iter().copied()).chain([program.as_str(), "-o", &output]).collect();
    let built = fennec(&args, &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    std::fs::remove_dir_all(&dir)?;
    Ok(ran)
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

/// `upper`, `lower`, `replace`, `find` and `split`, which run the runtime's own code.
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
    _ = parts.pop()
    _ = parts.pop(0)
    parts.remove_where((s) => s.len() > 10)
    stdout.print(parts)
    names: dict[str, str] = {"one": "uno", "long": "a value longer than twenty-three bytes"}
    for key in ["one", "long", "none"]:
        found: str? = names.get(key)
        stdout.print(found else "missing")
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

/// A list taken 8 items a turn into 8 totals, as a fixed-order float sum is, whose reads its
/// `while at + 8 <= n` guard keeps in range without checks: the same totals, at every length
/// round a multiple of 8, and a read past the end still stops the program.
#[test]
fn a_list_taken_eight_items_a_turn_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "eight_a_turn",
        r"def eights(items: list[float]) -> float:
    n = items.len()
    mut a0 = 0.0
    mut a1 = 0.0
    mut a2 = 0.0
    mut a3 = 0.0
    mut a4 = 0.0
    mut a5 = 0.0
    mut a6 = 0.0
    mut a7 = 0.0
    mut at = 0
    while at + 8 <= n:
        a0 += items[at]
        a1 += items[at + 1]
        a2 += items[at + 2]
        a3 += items[at + 3]
        a4 += items[at + 4]
        a5 += items[at + 5]
        a6 += items[at + 6]
        a7 += items[at + 7]
        at += 8
    while at < n:
        a0 += items[at]
        at += 1
    ((a0 + a1) + (a2 + a3)) + ((a4 + a5) + (a6 + a7))

def past(items: list[float]) -> float:
    mut at = 0
    mut total = 0.0
    while at + 2 <= items.len():
        total += items[at] + items[at + 2]
        at += 2
    total

def main(stdout: Stdout):
    for length in [0, 1, 7, 8, 9, 15, 16, 17, 1000]:
        xs = [float(i) * 0.1 + 1.0 / float(i + 3) for i in range(length)]
        stdout.print(length, eights(xs))
    stdout.print(past([1.0, 2.0, 3.0, 4.0]))
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
    mut c = b
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

/// The same enums read and written as JSON, and an error with cases as what a failure holds.
const ENUMS_IN_JSON_AND_FAILURES: &str = r#"from std import json


enum Token:
    Blank
    Word(text: str)
    Num(value: int)

error Failure:
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

def get(at: int) -> int fails Failure:
    if at == 0:
        fail .Why("a reason long enough to be held as a block of its own")
    if at == 1:
        fail .Plain
    at

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    # Failures holding them.
    for at in range(3):
        value = get(at) else as failure:
            stdout.print("err", failure)
            continue
        stdout.print("ok", value)
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
    # An expected value whose error is one.
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
            mut more = values
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

/// A boxed enum's case with no fields is one block for the whole run, so making it allocates
/// nothing.
#[test]
fn a_case_with_no_fields_allocates_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = format!(
        "{SHARED_LEAVES_TREE}def main(stdout: Stdout):\n    mut leaves = []\n    for _ in range(1000):\n        leaves.append(Tree.Leaf)\n    stdout.print(leaves.len(), count(build(0)))\n"
    );
    let counts = counts_of("shared_leaves_counted", &source)?;
    assert!(count(&counts, "allocations") < Some(30), "{counts}");
    Ok(())
}

/// A boxed enum only generated code meets is made outside the runtime's table, and a run that
/// counts what it allocates still counts each one: a tree of 31 nodes is 31 allocations at least.
#[test]
fn native_blocks_are_counted() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source =
        format!("{SHARED_LEAVES_TREE}def main(stdout: Stdout):\n    stdout.print(count(build(5)))\n");
    let counts = counts_of("native_blocks_counted", &source)?;
    assert!(count(&counts, "allocations") >= Some(31), "{counts}");
    assert!(count(&counts, "allocations") < Some(60), "{counts}");
    Ok(())
}

/// A recursive enum holding strings, inline and in blocks of their own, is made and freed by
/// generated code, which lets go of each string as its block goes: built and dropped over and
/// over, read along the way, in a dev build and with `--release`.
#[test]
fn native_blocks_hold_strings() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("native_strings", NAMED_TREES, how)?;
        assert_eq!((out.as_str(), code), ("118916 273\n", Some(0)), "{how:?}: {err}");
    }
    Ok(())
}

const NAMED_TREES: &str = r#"enum Named:
    Leaf(name: str)
    Pair(left: Named, right: Named, label: str)
    Empty

def build(depth: int, tag: int) -> Named:
    if depth == 0:
        if tag % 3 == 0: return .Empty
        return .Leaf(name=f"leaf number {tag} with a name long enough for a block")
    .Pair(left=build(depth - 1, tag=tag * 2), right=build(depth - 1, tag=tag * 2 + 1), label=f"p{tag}")

def size(tree: Named) -> int:
    match tree:
        case .Leaf(name):
            name.len()
        case .Pair(left, right, label):
            size(left) + size(right) + label.len()
        case .Empty:
            1

def main(stdout: Stdout):
    total = sum(size(build(10, tag=n)) for n in range(3))
    kept = build(3, tag=7)
    stdout.print(total, size(kept))
"#;

/// A recursive enum holding lists of strings and numbers is made and freed by generated code,
/// which lets go of each list as its block goes: built, read, copied and changed apart from the
/// tree, and dropped over and over, in a dev build and with `--release`.
#[test]
fn native_blocks_hold_lists() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (out, err, code) = both_built_unpinned("native_lists", WORD_TREES, how)?;
        assert_eq!((out.as_str(), code), ("14519248 2833\n", Some(0)), "{how:?}: {err}");
    }
    Ok(())
}

const WORD_TREES: &str = r#"enum Doc:
    Words(names: list[str], counts: list[int])
    Pair(left: Doc, right: Doc, title: str)
    Empty

def build(depth: int, tag: int) -> Doc:
    if depth == 0:
        if tag % 4 == 0: return .Empty
        mut names = []
        mut counts = []
        for n in range(tag % 5 + 1):
            names.append(f"word {tag} {n} long enough to sit in a block of its own")
            counts.append(n * tag)
        return .Words(names=names, counts=counts)
    .Pair(left=build(depth - 1, tag=tag * 2), right=build(depth - 1, tag=tag * 2 + 1), title=f"t{tag}")

def size(doc: Doc) -> int:
    match doc:
        case .Words(names, counts):
            mut more = names
            more.append("x")
            sum(name.len() for name in more) + sum(counts) + names.len()
        case .Pair(left, right, title):
            size(left) + size(right) + title.len()
        case .Empty:
            1

def main(stdout: Stdout):
    rounds = 3
    total = sum(size(build(10, tag=n)) for n in range(rounds))
    kept = build(3, tag=7)
    stdout.print(total, size(kept))
"#;

/// The shared block behaves as a block of its own would: compared, matched, printed, held in
/// lists and dicts, and made and let go of on other threads.
#[test]
fn cases_with_no_fields_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("shared_leaves", &format!("{SHARED_LEAVES_TREE}{SHARED_LEAVES_MAIN}"))
}

const SHARED_LEAVES_TREE: &str = "enum Tree:
    Leaf
    Node(left: Tree, right: Tree)

def build(depth: int) -> Tree:
    if depth == 0: return .Leaf
    .Node(left=build(depth - 1), right=build(depth - 1))

def count(tree: Tree) -> int:
    match tree:
        case .Leaf:
            1
        case .Node(left, right):
            1 + count(left) + count(right)

";

const SHARED_LEAVES_MAIN: &str = r#"def main(stdout: Stdout):
    mut leaves = [Tree.Leaf, build(1), Tree.Leaf]
    stdout.print(count(build(10)), leaves[0] == leaves[2], leaves[0] == leaves[1], build(2) == build(2))
    stdout.print(build(1), leaves[0])
    mut named = {"leaf": Tree.Leaf, "node": build(1)}
    named["leaf"] = build(2)
    named["other"] = .Leaf
    stdout.print(count(named["leaf"]), count(named["other"]), named["other"] == .Leaf)
    mut trees = [build(n % 4) for n in range(64)]
    for ref tree in parallel(trees):
        tree = .Node(left=tree, right=.Leaf)
    total = sum(count(tree) for tree in trees)
    stdout.print(total)
    leaves = []
    stdout.print(leaves.len(), Tree.Leaf == build(0))
"#;

#[test]
fn enums_holding_blocks_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit("enums_holding_blocks", ENUMS_HOLDING_BLOCKS)?;
    same_as_the_jit("enums_in_json_and_results", ENUMS_IN_JSON_AND_FAILURES)
}

/// Unions held in place, each with its member's case first: in lists and fields, optional,
/// narrowed by `is`, read through methods every member has, compared, and copied when changed,
/// with members that hold strings and lists.
#[test]
fn unions_holding_blocks_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("unions_holding_blocks", UNIONS_HOLDING_BLOCKS)
}

/// A member given where a builtin takes its union: a dict's key to `get`, `pop` and `in`, a
/// set's item to `add` and `in`, and a list's item to `append`.
#[test]
fn members_given_to_builtins_taking_a_union_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("members_given_to_builtins", MEMBERS_GIVEN_TO_BUILTINS)
}

/// A template string builds natively as the interpreter runs it: its text, its values of a
/// union, each value's expression and format spec, a `LiteralStr` picked by a conditional and
/// joined with `+`, and a template with no values.
#[test]
fn template_strings_build_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("templates", TEMPLATES)
}

const TEMPLATES: &str = r#"type SqlValue = int | str

def placeholders(query: Template[SqlValue]) -> str:
    mut sent = ""
    for at, text in enumerate(query.strings):
        sent += text
        if at < query.values.len():
            value = query.values[at]
            if value is int:
                sent += f"<int {value}>"
            else:
                sent += f"<str {value}>"
    sent

def order(up: bool) -> LiteralStr:
    "asc" if up else "desc"

def main(stdout: Stdout):
    name = "ash"
    least = 10
    stdout.print(placeholders(t"select {name} where score > {least} order by score"))
    stdout.print(placeholders(t"no values"), placeholders(t"{least}{name}"))
    column: LiteralStr = "score " + order(False)
    stdout.print(column)
    shown: Template[SqlValue] = t"{name:>5} and {least}"
    for part in shown.interpolations():
        stdout.print(part.expression, part.format_spec)
"#;

/// A generic type built where one of it is expected takes its types from there: a `Box[int |
/// str]` built from an int or a str, in a parameter, a name's type and a list of them.
#[test]
fn generic_types_built_where_one_is_expected_match_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds("generics_expected", GENERICS_EXPECTED)
}

/// A generic function whose type parameter is only in an `Iterator[T]` it's given works out
/// what `T` is from the iterator. It once couldn't, so the build left the caller to the JIT.
#[test]
fn a_generic_over_an_iterator_takes_its_type_from_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "generic_iterator",
        r#"def count[T](items: Iterator[T]) -> int:
    mut n = 0
    for _ in items:
        n += 1
    n

def last_of[T](items: Iterator[T], otherwise: T) -> T:
    mut last = otherwise
    for item in items:
        last = item
    last

def main(stdout: Stdout):
    stdout.print(count(x * 2 for x in range(10)), count(s for s in ["a", "b"]))
    stdout.print(count(f for f in [1.5, 2.5, 3.5]), last_of((w for w in ["p", "q"]), otherwise="none"))
"#,
    )
}

const GENERICS_EXPECTED: &str = r#"struct Box[T]:
    value: T

struct Held[T]:
    items: list[Box[T]]
    plain: list[T]

def show(box: Box[int | str]) -> str:
    value = box.value
    if value is int: return f"int {value + 1}"
    f"str {value}"

def count(held: Held[int | str]) -> str:
    f"{held.items.len()} {show(held.items[1])} {held.plain.len()}"

def main(stdout: Stdout):
    name = "x"
    stdout.print(show(Box(value=3)), show(Box(value=name)))
    stdout.print(count(Held(items=[Box(value=1), Box(value=name)], plain=[name, 2])))
    kept: Box[int | str] = Box(value=4)
    stdout.print(show(kept))
"#;

const MEMBERS_GIVEN_TO_BUILTINS: &str = r#"struct Circle:
    radius: float

struct Square:
    side: float

type Shape = Circle | Square

def main(stdout: Stdout):
    mut counts: dict[int | str, int] = {1: 2, "fox": 3}
    stdout.print(counts.get("owl") else 0, counts.get("fox") else 0, counts.get(1) else 0)
    key: int | str = "owl"
    stdout.print(counts.get(key) else 7)
    stdout.print("fox" in counts, 5 in counts, "owl" not in counts)
    _ = counts.pop("fox")
    stdout.print(counts.len(), counts)
    mut shapes: list[Shape] = []
    shapes.append(Circle(radius=1.0))
    shapes.append(Square(side=2.0))
    stdout.print(shapes.len())
    mut seen: set[int | str] = {1}
    seen.add("a")
    seen.add(2)
    seen.add("a")
    stdout.print(seen.len(), "a" in seen, 3 in seen)
"#;

const UNIONS_HOLDING_BLOCKS: &str = r#"struct Circle:
    radius: float

    def area(self) -> float:
        3.0 * self.radius * self.radius

    def name(self) -> str:
        "circle"

struct Square:
    side: float

    def area(self) -> float:
        self.side * self.side

    def name(self) -> str:
        "square"

struct Label:
    text: str
    tags: list[str]

    def area(self) -> float:
        float(self.text.len())

    def name(self) -> str:
        f"label {self.text}"

type Shape = Circle | Square | Label

struct Holder:
    shape: Shape
    note: int | str

def describe(value: int | str) -> str:
    if value is int: return f"int {value}"
    f"str {value.upper()}"

def measure(value: float | int) -> str:
    if value is float: return f"float {value}"
    f"int {value}"

def biggest(shapes: list[Shape]) -> Shape:
    mut best = shapes[0]
    for shape in shapes:
        if shape.area() > best.area():
            best = shape
    best

def maybe(at: int, shapes: list[Shape]) -> Shape?:
    if at < shapes.len():
        return shapes[at]
    None

def main(stdout: Stdout):
    mut shapes: list[Shape] = []
    for i in range(1000):
        if i % 3 == 0:
            shapes.append(Circle(radius=float(i % 7)))
        elif i % 3 == 1:
            shapes.append(Square(side=float(i % 5)))
        else:
            shapes.append(Label(text=f"l{i}", tags=["a", f"t{i}"]))
    mut total = 0.0
    mut circles = 0
    mut letters = 0
    for shape in shapes:
        total += shape.area()
        if shape is Circle:
            circles += 1
        if shape is Label:
            letters += shape.tags.len() + shape.text.len()
    stdout.print(total, circles, letters)
    big = biggest(shapes)
    stdout.print(big.name())
    copied = shapes
    mut changed = shapes
    changed[1] = Label(text="swapped", tags=[])
    (was, now) = (copied[1], changed[1])
    stdout.print(was.name(), now.name(), copied[1] == changed[1], copied[0] == changed[0])
    holder = Holder(shape=shapes[2], note="hello")
    other = Holder(shape=Circle(radius=2.0), note=42)
    stdout.print(holder.shape.name(), describe(holder.note), other.shape.area(), describe(other.note))
    stdout.print(measure(1.5), describe(-3), describe("fox"), measure(7))
    found = maybe(5, shapes)
    if found is None:
        stdout.print("none")
    else:
        shape: Shape = found
        stdout.print("found", shape.name())
    stdout.print(maybe(5000, shapes) is None)
    mut notes: list[int | str] = [1, "two", 3]
    notes.append("four")
    mut joined = []
    for note in notes:
        joined.append(describe(note))
    stdout.print(", ".join(joined), notes[1] == "two", notes[0] == 1, notes[0] == "1")
"#;

/// A list of a union holds each item in place, never in a block of its own: a thousand shapes
/// take only the list's own blocks as it grows.
#[test]
fn a_list_of_a_union_holds_its_items_in_place() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let counts = counts_of(
        "unions_in_place",
        "struct Circle:\n    radius: float\n\nstruct Square:\n    side: float\n\ndef area(shape: Circle | Square) -> float:\n    if shape is Circle: return 3.0 * shape.radius * shape.radius\n    shape.side * shape.side\n\ndef main(stdout: Stdout):\n    mut shapes: list[Circle | Square] = []\n    for i in range(1000):\n        if i % 2 == 0:\n            shapes.append(Circle(radius=float(i % 7)))\n        else:\n            shapes.append(Square(side=float(i % 5)))\n    mut total = 0.0\n    for shape in shapes:\n        total += area(shape)\n    stdout.print(total)\n",
    )?;
    assert!(count(&counts, "allocations") < Some(30), "{counts}");
    assert_eq!(count(&counts, "copies"), Some(0), "{counts}");
    Ok(())
}

/// A `parallel` call whose body may wait goes to the runtime's `_waiting` entry points, and
/// runs as one whose body only computes does: here a loop, a comprehension and a sum whose
/// ranges sleep, beside ones that don't.
/// A `parallel` comprehension whose item can fail, like `[save(fs.read(path)?)? for path in
/// parallel(paths)]`, builds natively, and fails as the interpreter does: what it fails with has
/// no way out of the runtime's ranges yet, so its items are made in order.
#[test]
fn a_parallel_comprehension_whose_item_can_fail_matches_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "parallel_tries",
        r#"def doubled(text: str) -> int fails InvalidValue:
    int(text)? * 2

def main(stdout: Stdout) fails InvalidValue:
    texts = ["1", "2", "3"]
    stdout.print([doubled(t)? for t in parallel(texts)], sum(int(t)? for t in parallel(texts)))
    stdout.print([int(t)? for t in parallel(["4", "x"])])
"#,
    )
}

#[test]
fn parallel_bodies_that_wait_match_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "parallel_waiting",
        r"from std.time import Clock

def rest(clock: Clock, x: int) -> int:
    clock.sleep(1ms)
    x * 10

def main(stdout: Stdout, clock: Clock):
    mut xs = [1, 2, 3, 4]
    for ref x in parallel(xs):
        clock.sleep(1ms)
        x += 1
    rested = [rest(clock, x) for x in parallel(xs)]
    doubled = [x * 2 for x in parallel(xs)]
    stdout.print(xs, rested, doubled, sum(rest(clock, x) for x in parallel(xs)))
",
    )
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
    twice_before = twice
    stdout.print(both(twice_before, ref twice), twice)
    mut bag = Bag(label="short", count=1)
    bag_before = bag
    stdout.print(bags(bag_before, ref bag), bag)
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

/// `list[int]()`, `dict[str, int]()` and `set[str]()` keep what they hold when the program's
/// checked again: once trimmed to build, after a `with` is written in, and as a constant.
#[test]
fn a_typed_empty_collection_keeps_its_type_when_checked_again() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "typed_empty_list",
        "def main(stdout: Stdout):\n    stdout.print(list[int](), list[int]().len())\n",
    )?;
    assert_eq!(printed, "[] 0\n");
    let printed = printed_by(
        "typed_empty_in_a_with",
        "def opened() -> with int:\n    lend 1\n\ndef main(stdout: Stdout):\n    with opened() as n:\n        stdout.print(list[int](), n)\n",
    )?;
    assert_eq!(printed, "[] 1\n");
    let printed = printed_by(
        "typed_empties",
        "EMPTY = list[int]()\n\ndef main(stdout: Stdout):\n    mut d = dict[str, int]()\n    d[\"a\"] = 1\n    s = set[str]()\n    \
         stdout.print(EMPTY, d, s, [1].len() > list[int]().len())\n",
    )?;
    assert_eq!(printed, "[] {\"a\": 1} set() True\n");
    Ok(())
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
    _ = squad.rows[0].pop()
    _ = squad.rows[0].pop()
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
        _ = people.pop()
        mut tokens = [Token.Word(long + "a"), Token.Blank, Token.Word(long + "b"), Token.Word(long + "c")]
        last = tokens.pop() else return
        early = tokens.pop(0)
        _ = tokens.pop(0)
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
    let ran = Command::new(&output).output_within()?;
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
    let ours = outcome(&Command::new(&executable).output_within()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours.0, "599999744999850000\n");
    Ok(())
}

/// `embed_folder`'s lists are made the first time it's reached and kept: a function giving one,
/// called a thousand times, makes them once, where it made both each time. A list taken from it
/// and changed is the caller's own copy, and the kept one stays as it was.
#[test]
fn an_embedded_folders_lists_are_made_once() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("embedded_once")?;
    std::fs::create_dir_all(dir.join("site/css"))?;
    std::fs::write(dir.join("site/css/app.css"), "body{}")?;
    std::fs::write(dir.join("site/index.html"), "<h1>hi</h1>")?;
    let allocations = |calls: u32| -> std::io::Result<(String, u64)> {
        let source = format!(
            "def site() -> EmbeddedFolder:\n    embed_folder(\"site\")\n\n\
             def main(stdout: Stdout):\n    mut total = 0\n    for i in range({calls}):\n        \
             files = site()\n        total += files.len() + (files.get(\"index.html\") else bytes()).len()\n    \
             mut paths = site().keys()\n    paths.append(\"extra\")\n    \
             stdout.print(total, paths, site().keys())\n"
        );
        let program = dir.join(format!("calls{calls}.fen"));
        std::fs::write(&program, source)?;
        let executable = dir.join(format!("calls{calls}"));
        let built = fennec(
            &["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()],
            &dir,
        )?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
        let said = String::from_utf8_lossy(&ran.stderr).into_owned();
        let made = said
            .split_whitespace()
            .skip_while(|word| *word != "allocations")
            .nth(1)
            .and_then(|count| count.parse().ok())
            .unwrap_or_else(|| panic!("no counts: {said}"));
        Ok((String::from_utf8_lossy(&ran.stdout).into_owned(), made))
    };
    let (once, made_once) = allocations(1)?;
    let (often, made_often) = allocations(1000)?;
    std::fs::remove_dir_all(&dir)?;
    let shown = "[\"css/app.css\", \"index.html\", \"extra\"] [\"css/app.css\", \"index.html\"]\n";
    assert_eq!(once, format!("13 {shown}"));
    assert_eq!(often, format!("13000 {shown}"));
    // Each call may make the folder's own block, but never its two lists again.
    assert!(made_often - made_once <= 999, "{made_once} against {made_often}");
    Ok(())
}

/// In a program with `parallel`, `main` works out each constant before any worker starts, and
/// throws the value away there: one of a struct type, like a `Duration`, still has its type in
/// `main`'s module.
#[test]
fn a_struct_constant_is_worked_out_ahead_for_parallel() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let source = "WAIT = 2s\n\ndef waited(n: int) -> int:\n    int(WAIT.as_millis()) + n\n\n\
                  def main(stdout: Stdout):\n    mut totals = [0, 1, 2, 3]\n    for ref t in parallel(totals):\n        \
                  t = waited(t)\n    stdout.print(totals)\n";
    assert_eq!(printed_by("struct_constant_ahead", source)?, "[2000, 2001, 2002, 2003]\n");
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
        let ran = Command::new(&executable).output_within()?;
        let interpreted = interpreted_by(&program, &format!("failing_main_{name}"))?;
        assert_eq!(apart_from(&dir, outcome(&ran)), interpreted, "{name}");
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

/// An error's notes natively: none to start with, each `add_note` changing only that error, not
/// a copy taken before, `==` and printing passing them over, a union of errors noting the error
/// it holds, and a failure reaching `main` or `expect` showing them under it. All as on the
/// interpreter, for an error with cases as for one without.
#[test]
fn notes_go_with_errors_as_on_the_interpreter() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("error-notes")?;
    let path = |file: &Path| file.display().to_string();
    let expected = "error Missing:\n    path: str\n\ndef load(path: str) -> int fails Missing:\n    mut m = Missing(path=path)\n    m.add_note(\"looked everywhere\")\n    fail m\n\ndef main(stdout: Stdout):\n    stdout.print(load(\"x\").expect(\"x is there\"))\n";
    for (name, source, note) in [
        (
            "notes",
            include_str!("fixtures/error_notes.fen"),
            "note: while loading a long one\nnote: second line\n  and a third\n",
        ),
        ("expected", expected, "but it failed: Missing(path=\"x\")\nnote: looked everywhere\n"),
        (
            "cases",
            include_str!("fixtures/error_notes_with_cases.fen"),
            "note: while dialing -3\nnote: second line\n  and a third\n",
        ),
    ] {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        let executable = dir.join(name);
        let built = fennec(&["build", "--llvm", &path(&program), "-o", &path(&executable)], &dir)?;
        assert!(built.status.success(), "{name}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        let interpreted = interpreted_by(&program, &format!("error_notes_{name}"))?;
        assert_eq!(apart_from(&dir, outcome(&ran)), interpreted, "{name}");
        let said = String::from_utf8_lossy(&ran.stderr);
        assert!(said.contains(note), "{name}: {said}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
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

/// A failed `assert` says all the JIT did, each side as the interpreter showed it.
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
    let conditions = [
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
    .into_iter()
    .enumerate()
    .collect::<Vec<_>>();
    // Each builds on its own, so they build at once: one at a time took over a minute.
    let outcomes = crate::executables::at_once(&conditions, |_, (number, condition)| {
        let source =
            format!("{setup}\ndef main(stdout: Stdout):\n    stdout.print(1)\n    assert {condition}\n");
        both(&format!("assert_{number}"), &source)
    })?;
    for ((_, condition), ((ours_out, ours_err, ours_code), (jit_out, jit_err, jit_code))) in
        conditions.iter().zip(outcomes)
    {
        assert_eq!((&ours_out, ours_code), (&jit_out, jit_code), "{condition}");
        assert_eq!(ours_err, jit_err, "{condition}");
    }
    Ok(())
}

/// `_ = check(n) else as why:` on a call that gives nothing but can fail keeps nothing and runs
/// the call, as the statement `check(n) else as why:` does.
#[test]
fn discarding_what_gives_nothing_runs_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("discard_nothing", DISCARD_NOTHING, "fine 1\nbig\nagain big\n")
}

const DISCARD_NOTHING: &str = r#"error Bad:
    why: str

def check(n: int) fails Bad:
    if n > 1:
        fail Bad("big")

def main(stdout: Stdout):
    for n in [1, 2]:
        _ = check(n) else as why:
            stdout.print(why.why)
            continue
        stdout.print("fine", n)
    check(5) else as why:
        stdout.print("again", why.why)
"#;

/// A failed `assert value fails Pattern` says what really happened, natively: what the call failed
/// with instead, or what it gave, and stops the program as a failed `assert` does.
#[test]
fn a_failed_assert_that_something_fails_says_what_happened() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let setup = "error Oops:\n    line: int\n\nstruct Config:\n    size: int\n\ndef load(text: str) -> Config fails Oops:\n    if text == \"bad\":\n        fail Oops(line=4)\n    Config(size=text.len())\n";
    for (asserted, said) in [
        ("load(\"bad\") fails Oops(line=3)", "= help: it failed with Oops(line=4)"),
        ("load(\"good\") fails Oops()", "= help: it gave Config(size=4)"),
    ] {
        let source = format!(
            "{setup}\ndef main(stdout: Stdout):\n    assert load(\"bad\") fails Oops()\n    stdout.print(1)\n    assert {asserted}\n    stdout.print(2)\n"
        );
        let dir = folder("assert_fails_native")?;
        let program = dir.join("asserted.fen");
        std::fs::write(&program, &source)?;
        let executable = dir.join("asserted");
        let built = fennec(
            &["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()],
            &dir,
        )?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        let (out, err) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
        assert_eq!(out, "1\n", "{asserted}: {err}");
        assert!(err.contains("this assertion failed") && err.contains(said), "{asserted}: {err}");
        assert_eq!(ran.status.code(), Some(1), "{asserted}");
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

/// A `match` case naming a union's member narrows the subject to it, natively: its own fields
/// read there, and its own `with` source runs, for each of three members.
#[test]
fn a_match_case_narrows_its_subject_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "match_narrows",
        r#"error Nope:
    why: str

struct Transaction:
    log: str

struct Lite:
    name: str

    def run(self, text: str) -> int fails Nope:
        text.len()

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") else:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        text.len() * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") else:
            pass

struct Mem:
    size: int

    def run(self, text: str) -> int fails Nope:
        text.len() * 3

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="mem") else:
            pass

type Store = Lite | Postgres | Mem

def saved(store: Store) -> str fails Nope:
    match store:
        case Lite(name=_):
            with store.transaction()? as tx:
                count = store.run("abc")?
                return f"{tx.log} {count} {store.name}"
        case Postgres(host=_):
            with store.transaction()? as tx:
                count = store.run("abc")?
                return f"{tx.log} {count} {store.host}"
        case Mem(size=_):
            with store.transaction()? as tx:
                count = store.run("abc")?
                return f"{tx.log} {count} {store.size}"
    ""

def main(stdout: Stdout) fails Nope:
    stdout.print(saved(Lite(name="a"))?, saved(Postgres(host="b"))?, saved(Mem(size=4))?)
"#,
        "lite 3 a pg 6 b mem 9 4\n",
    )
}

/// A value of a union like `int | str`, read from a field or a list's item, prints and formats as
/// itself, a string member unquoted, as in the interpreter: by `print`, `str` and an f-string.
/// Inside a list it's shown quoted, as any string in a list is.
#[test]
fn a_unions_value_from_a_field_or_an_item_prints_as_itself() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "union_text",
        r#"struct Box[T]:
    value: T

def main(stdout: Stdout):
    held: int | str = "ash"
    boxed: Box[int | str] = Box(value="ash")
    items: list[int | str] = ["ash", 1]
    stdout.print(held, boxed.value, items[0], items[1], items)
    stdout.print(f"{boxed.value} {items[0]}", str(items[0]), str(boxed.value).len())
    numbers: Box[int | str] = Box(value=3)
    floats: Box[float | str] = Box(value=1.5)
    flags: list[bool | str] = [True, "no"]
    lists: Box[list[int] | str] = Box(value=[1, 2])
    stdout.print(numbers.value, floats.value, flags[0], flags[1], lists.value, flags)
    stdout.print(f"{numbers.value}|{floats.value}|{flags[1]}|{lists.value}", str(lists.value))
"#,
    )
}

/// A name narrowed to `None`, like `x` after `x = None`, prints and formats as `None`, as in the
/// interpreter. LLVM refused it once, and the JIT got it wrong (#572).
#[test]
fn a_name_narrowed_to_none_prints_as_the_interpreter_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_interpreter_in_both_builds(
        "narrowed_none",
        r#"def main(stdout: Stdout):
    mut x: int? = 5
    stdout.print(x)
    x = None
    stdout.print(x)
    stdout.print(f"[{x}]", str(x), x is None)
    mut s: str? = "kit"
    s = None
    stdout.print(s, f"{s}")
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

/// `==` and `!=` on an optional string, against a string, another optional string or `None`:
/// short and long strings, and `None`, whose header is all zero, on either side.
#[test]
fn optional_strings_compare_as_the_jit_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "optional_strings_compare",
        r#"def first(words: list[str]) -> str?:
    if words.len() > 0:
        return words[0]
    None

def main(stdout: Stdout):
    long = "a string long enough that it has a block of its own"
    short = first(["kit"])
    missing = first([])
    held = first([long])
    stdout.print(short == "kit", short != "kit", short == "vex", short == "")
    stdout.print(missing == "kit", missing != "kit", missing == "", missing == None, short == None)
    stdout.print(held == long, held == "a string long enough", held == short, missing == missing)
    stdout.print(short == first(["kit"]), short != missing, "kit" == short, "" == missing)
"#,
    )
}

/// A generic's `T?` with `T` an optional is that optional, as the checker has it: `first` of a
/// `list[str?]` gives back a `str?`, and a `SortedMap[int, str?]`'s `get` and `remove` do too.
#[test]
fn an_optional_of_an_optional_is_the_optional_as_on_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "optional_of_optional",
        r#"from std.collections import SortedMap

def first[T](items: list[T]) -> T?:
    if items.len() > 0:
        return items[0]
    None

def words() -> list[str?]:
    ["kit", None]

def numbers() -> list[int?]:
    [None, 3]

def main(stdout: Stdout):
    stdout.print(first(words()), first(words()[1:]) is None, first(numbers()) is None)
    mut map = SortedMap[int, str?]()
    map[1] = None
    map[2] = "kit"
    stdout.print(map.get(1) is None, map.get(2), map.get(3) is None)
    stdout.print(map.remove(2), map.remove(1) is None, map.len())
"#,
    )
}

/// `?` in `main`, which then can fail: going on when nothing failed.
#[test]
fn a_main_giving_back_a_result_matches_the_jit() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_the_jit(
        "main_gives_back",
        r#"def number(text: str) -> int fails InvalidValue:
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

def weak(level: int) -> int fails TooWeak:
    if level < 5: fail TooWeak(level)
    level

def named(name: str) -> str fails NotFound:
    if name == "": fail NotFound(name)
    name

def challenge(name: str, level: int) -> str fails NotFound | TooWeak:
    who = named(name)?
    power = weak(level)?
    f"{who} at {power}"

def three(name: str, level: int, code: int) -> str fails NotFound | Plain | TooWeak:
    if code > 0: fail Plain(code)
    challenge(name, level)?

## Whether `challenge` fails with `error`, compared with the failure on the left.
def fails_with(name: str, level: int, error: NotFound | TooWeak) -> bool:
    _ = challenge(name, level) else as why:
        return why == error
    False

## The same, compared with the failure on the right.
def fails_by(name: str, level: int, error: NotFound | TooWeak) -> bool:
    _ = challenge(name, level) else as why:
        return error == why
    False

def main(stdout: Stdout):
    for name, level in [("ash", 7), ("kit", 1), ("", 9)]:
        text = challenge(name, level) else as why:
            match why:
                case TooWeak(level):
                    stdout.print("weak", level)
                    continue
                case _:
                    error = why
                    stdout.print("other", error, f"{error}")
                    continue
        stdout.print(text)
    for code in [0, 3]:
        text = three("", level=1, code=code) else as why:
            match why:
                case Plain(code):
                    stdout.print("plain", code)
                    continue
                case NotFound(name):
                    stdout.print("not found", name.len())
                    continue
                case _:
                    error = why
                    stdout.print("other", error)
                    continue
        stdout.print(text)
    kit = three("kit", level=1, code=0) else as error:
        f"{error}"
    stdout.print(kit)
    stdout.print(fails_with("kit", 1, error=TooWeak(level=1)))
    stdout.print(fails_with("kit", 1, error=TooWeak(level=2)))
    stdout.print(fails_by("kit", 1, error=TooWeak(level=1)))
    stdout.print(not fails_with("kit", 1, error=TooWeak(level=1)))
    stdout.print(fails_with("ash", 7, error=TooWeak(level=1)))
    stdout.print(fails_with("", 1, error=NotFound(name="")))
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

def pick(level: int) -> int fails NotFound | TooWeak:
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
        r#"def number(text: str) -> int fails InvalidValue:
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
        n = int(word) else:
            fail ParseError(message=f"`{word}` should be an int", line=0, column=self.at)
        n

    def float(ref self) -> float fails ParseError:
        word = self.next()?
        x = float(word) else:
            fail ParseError(message=f"`{word}` should be a float", line=0, column=self.at)
        x

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

    def none_next(ref self) -> bool:
        self.at < self.parts.len() and self.parts[self.at] == "-"

    def exact(self) -> bool:
        False

    def ordered(ref self, name: str) -> bool fails ParseError:
        False

    def end_ordered(ref self):
        pass

    def map_count(ref self) -> int fails ParseError:
        -1

    def end_map(ref self):
        pass

    def items(ref self, name: str, count: int) fails ParseError:
        pass

    def item[T: Deserializable](ref self) -> T fails ParseError:
        T.deserialize(ref self)?

    def end_items(ref self) fails ParseError:
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

    def member(ref self, names: list[str]) -> int fails ParseError:
        fail ParseError(message="no unions here", line=0, column=0)

    def end_member(ref self) fails ParseError:
        pass

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
    pet: str?
    @serialize(name="hp")
    health = 10 where health >= 0
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
    unit = read(text) else as error:
        return f"failed: {error.message} at {error.column}"
    orders = " ".join([str(order) for order in unit.orders])
    pet = unit.pet else "no pet"
    f"{unit.name} {unit.health} {pet} {unit.pace} {unit.ready} {unit.tags} {unit.seen.len()} {unit.scores} {orders}"

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
    let ours = outcome(&Command::new(&executable).output_within()?);
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
    let ours = outcome(&Command::new(&executable).output_within()?);
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
    let ours = outcome(&Command::new(&executable).output_within()?);
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
    _ = none.pop()
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

def add(a: str, b: str) -> int fails InvalidValue:
    int(a)? + int(b)?

def main(stdout: Stdout):
    for text in ["42", " -7 ", "4x2", "", "99999999999999999999"]:
        n = int(text) else as error:
            stdout.print(error.message)
            continue
        stdout.print(n)
    for text in ["2.5", " 1e3 ", "nope", "a \"quoted\" value long enough for a block"]:
        x = float(text) else as error:
            stdout.print(error.message)
            continue
        stdout.print(x)
    stdout.print(int("12") else 0, int("x") else -1, float("0.5") else 0.0, float("y") else -1.5)
    stdout.print(add("1", b="2") else -1, add("1", b="two") else -1)
    for pattern in ["[0-9]+", "[0-9+"]:
        regex = Regex(pattern) else as error:
            stdout.print(error.message)
            continue
        stdout.print(regex.pattern)
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
from std.net import NetError

test \"files are its own\"(fs: Fs):
    assert not fs.exists(\"a\")?
    fs.write_text(\"a\", \"one\")?
    assert fs.read_text(\"a\")? == \"one\"

test \"files are its own, again\"(fs: Fs):
    assert not fs.exists(\"a\")?
    fs.write_text(\"a\", \"two\")?

test \"what it prints is kept\"(stdout: Stdout, clock: Clock):
    stdout.print(\"at\", clock.now().nanos)
    assert stdout.captured() == \"at 0\\n\"

## Connects and hangs up, to see how connecting goes.
def connecting(net: Net, host: str, port: int) fails NetError:
    with net.connect(host, port)? as _:
        pass

test \"a failure comes back as its case\"(net: Net):
    assert connecting(net, host=\"localhost\", port=9001) fails .Refused(host=\"localhost\", port=9001)
",
    )?;
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["test", "--all", "--verbose", &path], &dir)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(ran.status.success(), "{said}");
    assert!(said.contains("4 of 4 tests ran through LLVM"), "{said}");
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
    problem = failure_of(() => process.run(\"echo\", [\"hello\"])?)
    assert problem is not None
    assert problem.program() == \"echo\"
    assert process.ran().len() == 1
",
    )?;
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["test", "--all", "--verbose", &path], &dir)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(ran.status.success(), "{said}");
    assert!(said.contains("2 of 2 tests ran through LLVM"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A property test runs through LLVM with each input, and one that fails there shrinks what it
/// failed for there too, as the interpreter does.
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
    assert!(said.contains("3 of 3 tests ran through LLVM"), "{said}");
    assert!(out.contains("ints stay small (100 inputs)"), "{out}");
    assert!(said.contains("it failed for value = 500"), "{said}");
    assert!(!said.contains("through LLVM and passes"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A failing property test shrinks a float, a struct and an input whose body passes a failure up
/// through LLVM, as the interpreter does, rather than running again another way.
#[test]
fn every_failing_property_test_shrinks_through_llvm() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("test_shrinking_all")?;
    let program = dir.join("shrinking.fen");
    std::fs::write(
        &program,
        "\
struct Point:
    x: int
    y: int

def parsed(text: str) -> int fails InvalidValue:
    int(text)?

test \"floats stay small\" for x: float:
    assert x < 3.5

test \"points stay near\" for p: Point:
    assert p.x < 50

test \"texts are numbers\" for text: str:
    n = parsed(text)?
    assert n >= 0
",
    )?;
    let path = program.to_string_lossy().into_owned();
    let ran = fennec(&["test", "--all", "--verbose", &path], &dir)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    std::fs::remove_dir_all(&dir)?;
    assert!(!ran.status.success(), "{said}");
    assert!(said.contains("3 of 3 tests ran through LLVM"), "{said}");
    assert!(said.contains("it failed for x = 9.0;"), "{said}");
    assert!(said.contains("it failed for p = Point(x="), "{said}");
    assert!(said.contains("it failed for text = \"\""), "{said}");
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
        ("passed_up", "(fs: Fs):\n    _ = fs.read_text(\"gone\")?\n"),
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
