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
    if llvm_expected() {
        let missing = tools.as_ref().err().map_or("", String::as_str);
        assert!(tools.is_ok(), "FENNEC_EXPECT_LLVM is set, but {missing}");
        assert!(runtime, "FENNEC_EXPECT_LLVM is set, but the runtime library didn't build");
    }
    tools.is_ok() && runtime
}

/// Whether `FENNEC_EXPECT_LLVM` says LLVM is the backend that has to work here, as CI's Linux and
/// Windows test jobs say, so what's missing fails a test rather than skipping it.
pub(crate) fn llvm_expected() -> bool {
    std::env::var_os("FENNEC_EXPECT_LLVM").is_some_and(|expected| !expected.is_empty())
}

/// A folder of its own, emptied when the test is done.
fn folder(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-llvm-{}-{name}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn same_as_pinned(name: &str, source: &str) -> std::io::Result<()> {
    let (ours, wanted) = both(name, source)?;
    assert_eq!(ours, wanted);
    Ok(())
}

/// A known difference: the IR gives an index being written, and a value being formatted, the span
/// of what they're in, where the pinned outcome underlines the part that failed. Same message and
/// outcome.
fn same_message_as_pinned(name: &str, source: &str) -> std::io::Result<()> {
    let ((ours_out, ours_err, ours_code), (jit_out, jit_err, jit_code)) = both(name, source)?;
    assert_eq!((ours_out, ours_code), (jit_out, jit_code));
    assert_eq!(ours_err.lines().next(), jit_err.lines().next());
    Ok(())
}

/// What the program printed, its errors and its exit code, as pinned in `tests/fixtures/outcomes`
/// before the engines were removed.
fn pinned_outcome(name: &str, source: &str) -> std::io::Result<Outcome> {
    pinned_in("outcomes", name, source)
}

/// The outcome pinned as `name` for the program at `program` (see `pinned_outcome`).
pub(crate) fn pinned_outcome_of(program: &Path, name: &str) -> std::io::Result<Outcome> {
    pinned_in("outcomes", name, &std::fs::read_to_string(program)?)
}

/// What the program printed, its errors and its exit code, built with LLVM, and what was pinned.
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
    let wanted = pinned(name, source, || Ok(ours.clone()))?;
    std::fs::remove_dir_all(&dir)?;
    Ok((ours, wanted))
}

/// The outcome pinned for program `name` in `tests/fixtures/pinned`, one file for each program,
/// named as the test names it. They were pinned before the engines were removed.
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
fn writing_to_stdout_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "stdout_write",
        r#"struct Point:
    x: int
    label: str

def maybe(present: bool) -> str?:
    if present:
        return "here"
    return None

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
fn bytes_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "bytes_all",
        r#"struct Packet:
    id: int
    data: bytes

enum Held:
    Empty
    Full(data: bytes)

def first(data: bytes) -> int:
    return data[0]

def maybe(present: bool) -> bytes?:
    if present:
        return bytes([9, 8])
    return None

def main(stdout: Stdout):
    empty = bytes()
    data = bytes([0, 1, 127, 128, 255])
    stdout.print(empty, len(empty), data, len(data))
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
    stdout.print(encoded, len(encoded), len(text))
    back = encoded.decode() except error:
        stdout.print(error.message)
        return
    stdout.print(back, back == text)
    bad = bytes([255, 254]).decode() except error:
        error.message
    stdout.print(bad)
    for hex in ["00ff", "00 FF", "", "abc", "zz", "a é"]:
        made = bytes.fromhex(hex) except error:
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
    stdout.print(many[0], many[-1], len(many), many)
    for held in [Held.Empty, Held.Full(data=bytes([1]))]:
        match held:
            case .Empty:
                stdout.print("empty")
            case .Full(inner):
                stdout.print(inner, inner[0], held)
    stdout.print(maybe(True), maybe(False), maybe(True) is None, maybe(False) is None)
    got = maybe(True) else bytes()
    stdout.print(got, len(got))
    pair = (data[:1], 3)
    stdout.print(pair)
    stdout.print(f"{data[:1]} and {empty}", str(data[:2]))
    mut grown = bytes()
    for at in range(300):
        grown = grown + bytes([at % 256])
    stdout.print(len(grown), grown[299], grown[255], grown[:3])
    named = {"a": 1}
    stdout.print(named["a"], grown[:9].hex())
"#,
    )
}

/// What fails with `bytes` says what was pinned.
#[test]
fn bytes_fail_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, call) in [
        ("bytes_past_end", "bytes([1, 2])[2]"),
        ("bytes_before_start", "bytes([1, 2])[-3]"),
        ("bytes_too_big", "bytes([1, 256])"),
        ("bytes_negative", "bytes([-1])"),
    ] {
        same_message_as_pinned(
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
fn str_of_text_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "str_of_text",
        "def main(stdout: Stdout):\n    name = \"fox\"\n    stdout.print(str(name), str(\"\"))\n",
    )
}

/// A text growing in a field: short then long, from each kind of text, and with a copy held
/// elsewhere that has to keep what it had.
#[test]
fn a_text_growing_in_a_field_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(log.text, len(log.text))
    held = log.text
    log.add(" and a good deal longer than a header can hold")
    stdout.print(held, log.text)
    kept = log.text
    log.text += " été"
    log.text += log.text
    stdout.print(kept, log.text, len(log.text))
    mut world = World()
    for at in range(100):
        world.log.text += str(at)
    world.log.hp += 1
    stdout.print(world.log.text, world.log.hp)
    copy = world
    world.log.text += "!"
    stdout.print(len(copy.log.text), len(world.log.text))
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
    return at

def by_fail(ref units: list[Unit], at: int) -> int fails Bad:
    for ref unit in units:
        unit.tags.append(at)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    return 0

def by_try(ref units: list[Unit]) -> int fails Bad:
    for ref unit in units:
        unit.tags.append(8)
        unit.hp += maybe(unit.hp)?
    return 0

def main(stdout: Stdout):
    mut units = [Unit([1], 0), Unit([2], 1), Unit([3], 5)]
    by_fail_value = by_fail(ref units, 9) except Bad(): -1
    stdout.print(by_fail_value, units)
    mut others = [Unit([1], 0), Unit([2], 1), Unit([3], 5)]
    by_try_value = by_try(ref others) except Bad(): -2
    stdout.print(by_try_value, others)
"#;
    same_as_pinned("failing_out_of_for_mut", shared)?;
    // Out of two loops at once, held to its pinned outcome.
    let nested = "error Bad:\n    why: str\n\ndef nested(ref grid: list[list[int]]) -> int fails Bad:\n    for ref row in grid:\n        for ref cell in row:\n            cell += 1\n            if cell == 5:\n                fail Bad(\"inner\")\n    return 0\n\ndef main(stdout: Stdout):\n    mut grid = [[1, 2], [4, 7], [9]]\n    nested_value = nested(ref grid) except Bad(): -3\n    stdout.print(nested_value, grid)\n";
    let dir = folder("failing_out_of_two_loops")?;
    let program = dir.join("two_loops.fen");
    std::fs::write(&program, nested)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("two_loops"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    let pinned_outcome = pinned_outcome("two_loops", nested)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, pinned_outcome);
    assert_eq!(ours.0, "-3 [[2, 3], [5, 7], [9]]\n");
    Ok(())
}

/// `else:` after a call that gives nothing back when it doesn't fail, as `conn.write(...)` does:
/// the block runs only when it fails, leaving by `continue`, `break` or `return`. It once wasn't
/// lowered, as "a value of type nothing".
#[test]
fn else_after_a_call_that_gives_nothing_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "else_on_nothing",
        "error Odd:
    n: int

def check(n: int) fails Odd:
    if n % 2 == 1:
        fail Odd(n=n)

def first_odd(limit: int) -> int:
    mut at = 0
    while at < limit:
        check(at) except Odd():
            return at
        at += 1
    return -1

def main(stdout: Stdout):
    mut kept = 0
    for n in range(6):
        check(n) except Odd():
            continue
        kept += 1
    mut seen = 0
    for n in [2, 4, 5, 6]:
        check(n) except Odd():
            break
        seen += 1
    stdout.print(kept, seen, first_odd(10), first_odd(0))
",
    )
}

/// `units[at].tags.append(x)` with the list in a local: the inner list goes on growing in place,
/// and what's shared with it, at any level, doesn't change.
/// What a program printed, how it ended and its first error line, built with LLVM and in dev
/// and `--release` both, is as pinned. What the backend remembers about a local
/// has to end when something changes it, and only optimizing shows some of that.
fn same_as_expected_in_both_builds(name: &str, source: &str) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let output = executable.to_string_lossy().into_owned();
    let (wanted_out, wanted_err, wanted_code) = pinned_outcome(name, source)?;
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

/// An `except` block that gives a value: `except why:` with what failed in it, an `else:` after
/// a `None`, `case` arms on the failure before the value, and one that leaves.
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
    return n

def load(n: int) -> World fails Bad:
    if n > 5:
        fail Bad("no world")
    return World(size=n)

def find(names: list[str], name: str) -> int?:
    for at, held in enumerate(names):
        if held == name:
            return at
    return None

def main(stdout: Stdout):
    small = risky(9) except why:
        stdout.print(f"failed: {why.why}")
        0
    stdout.print(small)
    world = load(8) except why:
        case Bad(text):
            stdout.print(text)
            World(size=1)
    stdout.print(world.size)
    kept = load(2) except Bad():
        World(size=-1)
    stdout.print(kept.size)
    where = find(["a", "b"], "c") else:
        stdout.print("not there")
        -1
    stdout.print(where)
    for turn in range(3):
        held = risky(turn * 4) except Bad():
            continue
        stdout.print(held)
"#;

/// `except` handles a failure natively in each of its forms: any failure named, `case` arms under
/// the name, one case on the line giving a value or leaving, a bare call's, and a `with` source's
/// cleanup, `lend x except:`.
#[test]
fn except_handles_a_failure_in_each_form() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "except_forms",
        EXCEPT_FORMS,
        "failed: big\n0\nlong 9\n8080\n-1\nskipped 2: two\nrolled back\ninside 1\n",
    )
}

/// A result kept in place, its value or its error in the caller's slot, for every kind of value:
/// nothing, an int, a long text, a list, a dict, an optional, a struct in place and one in a block,
/// on success and on failure, `fails A | B`, and a failure passed up through three `?`. Called
/// directly, through a function value and an arrow, by `parallel` workers, out of `lock ... as ref`
/// and in a launched task. Built both ways, kept in place and as a block (`FENNEC_BOXED_RESULTS`),
/// in dev and release, each printing the same and holding nothing at the end.
#[test]
fn a_result_of_every_kind_is_kept_in_place_wherever_it_is_given_back() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for boxed in ["0", "1"] {
        for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
            let dir = folder("results_in_place")?;
            let (program, executable) = (dir.join("results.fen"), dir.join("results"));
            std::fs::write(&program, RESULTS_IN_PLACE)?;
            let mut command = crate::executables::fennec_command();
            command.arg("build").args(how).arg(&program).arg("-o").arg(&executable).env("FENNEC_HOME", &dir);
            if boxed == "1" {
                command.env("FENNEC_BOXED_RESULTS", "1");
            }
            let built = command.output_before(FENNEC_DEADLINE)?;
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
            std::fs::remove_dir_all(&dir)?;
            let said = String::from_utf8_lossy(&ran.stderr).into_owned();
            assert_eq!(
                String::from_utf8_lossy(&ran.stdout),
                "5157 1164 5157 566 570 140\n",
                "boxed {boxed} {how:?}: {said}"
            );
            assert!(said.lines().any(|line| line.ends_with(" live 0")), "boxed {boxed} {how:?}: {said}");
        }
    }
    Ok(())
}

const RESULTS_IN_PLACE: &str = r#"from std.concurrent import Closed, Crashed

error Oops:
    why: str

error Other:
    code: int

struct Point:
    x: int
    y: int

struct Named:
    name: str
    tags: list[str]

struct Counter:
    seen: int

def reason(n: int) -> str:
    return "this failed for a reason long enough to need a block " + str(n)

def nothing(n: int) fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return

def number(n: int) -> int fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return n

def text(n: int) -> str fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return "a text that's longer than any header holds " + str(n)

def items(n: int) -> list[int] fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return [n, n + 1]

def table(n: int) -> dict[str, int] fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return {"n": n, "twice": n * 2}

def maybe(n: int) -> int? fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    if n % 2 == 0: return n
    return None

def point(n: int) -> Point fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return Point(x=n, y=-n)

def named(n: int) -> Named fails Oops:
    if n % 3 == 2: fail Oops(why=reason(n))
    return Named(name="a name longer than any header holds " + str(n), tags=["tag " + str(n)])

def either(n: int) -> int fails Oops | Other:
    if n % 3 == 2: fail Oops(why=reason(n))
    if n % 3 == 1: fail Other(code=n)
    return n

def deep_one(n: int) -> str fails Oops:
    return text(n)? + "!"

def deep_two(n: int) -> str fails Oops:
    return deep_one(n)? + "!"

def deep_three(n: int) -> str fails Oops:
    return deep_two(n)? + "!"

# Every kind, called directly, each failure caught: a number that says what came back.
def every(n: int) -> int:
    mut total = 0
    nothing(n) except Oops(why):
        total += len(why)
    got_number = number(n) except Oops(why): len(why)
    got_text = text(n) except Oops(why): why
    got_items = items(n) except Oops(why): [len(why)]
    got_table = table(n) except Oops(why): {why: 1}
    got_maybe = maybe(n) except Oops(why): len(why)
    got_point = point(n) except Oops(why): Point(x=len(why), y=0)
    got_named = named(n) except Oops(why): Named(name=why, tags=list[str]())
    got_either = either(n) except why:
        case Oops(text):
            len(text)
        case Other(code):
            code * 100
    got_deep = deep_three(n) except Oops(why): why
    total += got_number + len(got_text) + len(got_items) + len(got_table) + (got_maybe else 7)
    total += got_point.x + len(got_named.name) + len(got_named.tags) + got_either + len(got_deep)
    return total

def through(run: (int) -> str fails Oops, n: int) -> int:
    got = run(n) except Oops(why):
        return len(why)
    return len(got)

def counted(n: int) -> int:
    return through(text, n=n) + through((k) => deep_three(k)?, n=n)

def lengths(rounds: int) -> Iterator[int]:
    for n in range(rounds):
        got = text(n) except Oops(why): why
        kept = named(n) except Oops(why): Named(name=why, tags=list[str]())
        yield len(got) + len(kept.tags) - 1

def generated(rounds: int) -> int:
    mut total = 0
    for length in lengths(rounds):
        total += length
    return total

def tasked(n: int, out: shared Counter) fails Oops:
    said = text(n)?
    lock out as ref held:
        held.seen += len(said)
    return

def locked(out: shared Counter, n: int) -> int:
    lock out as ref held:
        got = text(n) except Oops(why):
            return len(why)
        held.seen += 1
        return len(got)

def main(stdout: Stdout) fails Crashed | Oops:
    rounds = 12
    mut direct = 0
    mut valued = 0
    for n in range(rounds):
        direct += every(n)
        valued += counted(n)
    workers = sum(every(n) for n in parallel(range(rounds)))
    shared out = Counter(seen=0)
    mut in_lock = 0
    for n in range(rounds):
        in_lock += locked(out, n=n)
    for n in [0, 3, 6]:
        with launch(() => tasked(n, out)?)?:
            pass
    lock out as held:
        stdout.print(direct, valued, workers, generated(rounds), in_lock, held.seen)
    return
"#;

const EXCEPT_FORMS: &str = r#"error Bad:
    Empty
    Long(size: int)

error Big:
    why: str

def risky(n: int) -> int fails Big:
    if n > 5:
        fail Big("big")
    return n

def load(n: int) -> int fails Bad:
    if n == 0:
        fail .Empty
    if n > 5:
        fail .Long(size=n)
    return n

def act(n: int) fails Big:
    if n == 2:
        fail Big("two")

def guarded(stdout: Stdout) -> with int fails Big:
    lend 1 except:
        stdout.print("rolled back")

def first(n: int) -> int:
    x = risky(n) except Big(): return -1
    return x

def used(stdout: Stdout) fails Big:
    with guarded(stdout)? as one:
        fail Big(f"inside {one}")

def main(stdout: Stdout):
    small = risky(9) except why:
        stdout.print(f"failed: {why.why}")
        0
    stdout.print(small)
    sized = load(9) except why:
        case .Long(size):
            stdout.print(f"long {size}")
            size
        case .Empty:
            0
    port = risky(7) except Big(): 8080
    stdout.print(port)
    stdout.print(first(9))
    for turn in range(4):
        act(turn) except why:
            stdout.print(f"skipped {turn}: {why.why}")
    used(stdout) except why:
        stdout.print(why.why)
    _ = sized
"#;

/// Leaving a `with` block from a `case` arm under `except why:`, or from the one case of
/// `except Case():`, runs its source's cleanup as leaving from anywhere else in the block does:
/// `return`, `break` and `continue` from a named case, from `case _:` and from the one case, from
/// nested blocks, and a `fail`, which runs the `lend`'s `except:` first.
#[test]
fn leaving_from_an_except_arm_runs_the_with_cleanup() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "except_arms_leave",
        EXCEPT_ARMS_LEAVE,
        "open named 0 0\nkept 0\nclose named 0 0\nopen named 0 1\ncaught 1\nclose named 0 1\nopen other 0 0\nkept 0\nclose other 0 0\nopen other 0 1\nother Refused.Big(code=1)\nclose other 0 1\nopen one 0 0\nkept 0\nclose one 0 0\nopen one 0 1\nclose one 0 1\ngave 1 9 7\nopen named 1 0\nkept 0\nclose named 1 0\nopen named 1 1\ncaught 1\nclose named 1 1\nopen other 1 0\nkept 0\nclose other 1 0\nopen other 1 1\nother Refused.Big(code=1)\nclose other 1 1\nopen one 1 0\nkept 0\nclose one 1 0\nopen one 1 1\nclose one 1 1\ngave -1 -1 -1\nopen named 2 0\nkept 0\nclose named 2 0\nopen named 2 1\ncaught 1\nclose named 2 1\nopen other 2 0\nkept 0\nclose other 2 0\nopen other 2 1\nother Refused.Big(code=1)\nclose other 2 1\nopen one 2 0\nkept 0\nclose one 2 0\nopen one 2 1\nclose one 2 1\ngave -1 -1 -1\nopen outer\nopen inner\nclose inner\nclose outer\ngave 1\nopen failed\nundo failed\nfailed with 11\n",
    )
}

const EXCEPT_ARMS_LEAVE: &str = r#"error Refused:
    Big(code: int)
    Odd(code: int)

error Small:
    code: int

def capped(n: int, limit: int) -> int fails Refused:
    if n > limit: fail .Big(code=n)
    return n

def tiny(n: int) -> int fails Small:
    if n > 0: fail Small(code=n)
    return n

def ward(name: str, stdout: Stdout) -> with str:
    stdout.print("open", name)
    lend name except:
        stdout.print("undo", name)
    stdout.print("close", name)

def named(stdout: Stdout, leave: int) -> int:
    for i in range(2):
        with ward(f"named {leave} {i}", stdout) as _:
            got = capped(i, limit=0) except why:
                case.Big(code):
                    stdout.print("caught", code)
                    if leave == 0: return code
                    if leave == 1: break
                    continue
                case.Odd(code):
                    code
            stdout.print("kept", got)
    return -1

def other(stdout: Stdout, leave: int) -> int:
    for i in range(2):
        with ward(f"other {leave} {i}", stdout) as _:
            got = capped(i, limit=0) except why:
                case.Odd(code):
                    code
                case _:
                    stdout.print("other", f"{why}")
                    if leave == 0: return 9
                    if leave == 1: break
                    continue
            stdout.print("kept", got)
    return -1

def one(stdout: Stdout, leave: int) -> int:
    for i in range(2):
        with ward(f"one {leave} {i}", stdout) as _:
            if leave == 0:
                got = tiny(i) except Small(): return 7
                stdout.print("kept", got)
            elif leave == 1:
                got = tiny(i) except Small(): break
                stdout.print("kept", got)
            else:
                got = tiny(i) except Small(): continue
                stdout.print("kept", got)
    return -1

def nested(stdout: Stdout) -> int:
    with ward("outer", stdout) as _:
        with ward("inner", stdout) as _:
            got = capped(1, limit=0) except why:
                case.Big(code):
                    return code
                case.Odd(code):
                    code
            stdout.print("kept", got)
    return -1

def failed(stdout: Stdout) -> int fails Refused:
    with ward("failed", stdout) as _:
        got = capped(1, limit=0) except why:
            case.Big(code):
                fail .Odd(code=code + 10)
            case.Odd(code):
                code
        stdout.print("kept", got)
    return -1

def main(stdout: Stdout):
    for leave in range(3):
        stdout.print("gave", named(stdout, leave), other(stdout, leave), one(stdout, leave))
    stdout.print("gave", nested(stdout))
    code = failed(stdout) except why:
        case.Odd(code):
            code
        case.Big(code):
            code
    stdout.print("failed with", code)
"#;

/// On what can fail as well as give `None`, `?` passes the failure up and `else` handles the
/// `None`, and `except why:` handles only the failure, its block giving a value that may be `None`.
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
    return Entry(size=n)

def lookup(n: int) -> int fails Bad:
    entry = cached(n)? else Entry(size=-1)
    return entry.size

def main(stdout: Stdout):
    lookup_value = lookup(1) except Bad(): -9
    lookup_value2 = lookup(3) except Bad(): -9
    lookup_value3 = lookup(7) except Bad(): -9
    stdout.print(lookup_value, lookup_value2, lookup_value3)
    held = cached(7) except why:
        stdout.print(f"failed: {why.why}")
        None
    stdout.print(held is None)
    other = cached(1) except Bad():
        Entry(size=0)
    found = other else Entry(size=-2)
    stdout.print(found.size)
"#;

/// `total = lock scores as current:` gives the block's last line, read or after a change, as
/// A `shared` value's made declaration holds the checked type itself, never its text: a program's
/// own `Delivery`, which the checker holds under a name a program can't write, with a field named
/// by a keyword, and a dict of lists of tuples of it come through unchanged.
#[test]
fn a_shared_value_holds_any_type_the_checker_holds_unchanged() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "shared_any_type",
            r#"# The library has a `Delivery` too: this one is the program's own.
struct Delivery:
    parcel: str
    if: int

def main(stdout: Stdout):
    latest: shared = Delivery(parcel="box", if=3)
    lock latest as ref held:
        held.if += 1
    lock latest as held:
        stdout.print(held.parcel, held.if)
    batches: shared = {"a": [(1, Delivery(parcel="x", if=1))]}
    lock batches as ref batch:
        batch["b"] = [(2, Delivery(parcel="y", if=2))]
    lock batches as batch:
        stdout.print(len(batch), batch["b"][0][1].if)
"#
        )?,
        "box 4\n2 2\n"
    );
    Ok(())
}

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
        len(current)
    count += 1
    stdout.print(total, count)
    kept = lock scores as ref current:
        current.append(9)
        if len(current) > 3:
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
    return i * 10

def text(ref k: str) -> int:
    k = \"b\"
    return 5

def bump(ref t: Total) -> int:
    t.v = 10
    return 2

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
    while len(xs) > 0:
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
    return value

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
    return xs[at]

def main(stdout: Stdout):
    # An index, as a position and as a length.
    mut i = 0
    mut xs = [1, 2, 3, 4]
    stdout.print(xs[i])
    bump(ref i)
    stdout.print(xs[i])
    mut n = len(xs)
    bump(ref n)
    stdout.print(n, xs[n - 2])

    mut p = 0
    mut qs = [1, 2, 3]
    stdout.print(param_index(ref p, ref qs), p, qs)

    # A list's length, kept across a change.
    mut ys = [1, 2, 3]
    size = len(ys)
    grown(ref ys)
    stdout.print(size, len(ys), ys[size])
    shrunk(ref ys)
    shrunk(ref ys)
    stdout.print(len(ys), ys[-1])
    replaced(ref ys)
    stdout.print(len(ys), ys)

    # A loop's list changing in its body, by index up to a length read once.
    mut zs = [1, 2, 3, 4]
    for at in range(len(zs)):
        if at < len(zs):
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
    stdout.print(inner, len(inner), len(lists[1]))

    # A struct's list and index, through `mut self` and a `mut` argument.
    mut box = Box([1, 2, 3], 0, "old")
    held = box.items
    box.more(4)
    stdout.print(len(held), len(box.items), box.items[box.at])
    box.next()
    stdout.print(box.items[box.at], box.at)
    box.fewer()
    box.fewer()
    stdout.print(len(box.items), box.items[-1])
    box.swapped()
    stdout.print(box.items, held)
    name = box.name
    box.renamed("newer")
    stdout.print(name, box.name, len(box.name))
    newbox(ref box)
    stdout.print(box.items, box.at, box.name)

    # In places: through a list's item, and a field of a field.
    mut boxes = [Box([1], 0, "a"), Box([2], 0, "b")]
    other = boxes[0]
    boxes[0].more(5)
    boxes[0].next()
    stdout.print(boxes[0].items[boxes[0].at], other.items, other.at)
    newbox(ref boxes[1])
    stdout.print(boxes[1].items, boxes[1].name, len(boxes))
    mut pair = Pair(Box([1], 0, "l"), right=Box([2], 0, "r"))
    pair.left.more(6)
    pair.left.next()
    newbox(ref pair.right)
    stdout.print(pair.left.items[pair.left.at], pair.right.items, pair.right.name)

    # A string: its length, its characters, and where reading it had got to.
    mut text = "héllo wörld, with some more words"
    before = len(text)
    c = text[6]
    stdout.print(before, c, text[7])
    retext(ref text, "日本語のテキストです")
    stdout.print(len(text), text[1], text[7])
    longer(ref text)
    stdout.print(len(text), text[-1], text[3])
    retext(ref text, "ascii now")
    stdout.print(len(text), text[2], text)
    mut chars = 0
    for at in range(len(text)):
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
    stdout.print(d["b"], len(d))
    if "b" in d:
        removed(ref d, "b")
        d["b"] = 70
    stdout.print(d["b"], len(d))

    # An optional and a case, known and then changed.
    # An optional known to hold something, given to what can make it `None`.
    mut maybe: int? = 3
    if maybe is not None:
        before_clear = maybe + 1
        cleared(ref maybe)
        stdout.print(before_clear, maybe is None)
    mut text_maybe: str? = "held"
    if text_maybe is not None:
        size_before = len(text_maybe)
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
            stdout.print(size, tags, len(tags))
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
    stdout.print(chain, len(chain), chain[-1])

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
        one[0] = len(one)
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
    stdout.print(len(long), long[35], long[20])
    longer(ref long)
    stdout.print(len(long), long[40], long[39])
    appended_twice(ref long)
    stdout.print(len(long), long[42], long[41], long[0])
    mut plain = "a" * 40
    stdout.print(len(plain), plain[39])
    longer(ref plain)
    stdout.print(len(plain), plain[40], plain[39], plain[20])
    mut short = "ab"
    stdout.print(len(short), short[1])
    longer(ref short)
    stdout.print(len(short), short[2], short)
    for round in range(30):
        longer(ref short)
    stdout.print(len(short), short[31], short[1])

    # In places, read before and after.
    mut names = ["é" * 30, "b"]
    stdout.print(len(names[0]), names[0][29])
    longer(ref names[0])
    stdout.print(len(names[0]), names[0][30], names[1])
    mut held = Box([1], 0, "é" * 30)
    stdout.print(len(held.name), held.name[25])
    longer(ref held.name)
    held.renamed(held.name + "z")
    stdout.print(len(held.name), held.name[31], held.name[25])

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
    same_as_expected_in_both_builds("given_mut", GIVEN_MUT)
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
    for at in range(len(xs)):
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
    for at in range(len(box.items)):
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
    for at in range(len(xs)):
        mut i = at
        stdout.print(xs[i])
        bumped(ref i)
        stdout.print(xs[i])
",
        ),
    ] {
        same_as_expected_in_both_builds(name, source)?;
    }
    Ok(())
}

/// What's read as an argument keeps the value it had, in order, while a later `mut` argument
/// changes it and what's known of it ends: both the order arguments run in and an index that
/// changes have to hold at once.
const READ_THEN_CHANGED: &str = r#"def bump(ref at: int) -> int:
    at += 1
    return at

def pair(a: int, b: int) -> str:
    return f"{a}/{b}"

def three(a: int, b: int, c: int) -> str:
    return f"{a}/{b}/{c}"

def listed(a: list[int], b: int) -> str:
    return f"{a}/{b}"

def put(a: int, ref item: int, ref at: int) -> str:
    at += 1
    item += 100
    return f"{a}"

def maybe_cleared(a: int, ref value: int?) -> int:
    value = None
    return a

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
    stdout.print(zs, zs.pop(), len(zs))

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
    same_as_expected_in_both_builds("read_then_changed", READ_THEN_CHANGED)
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
        self.tags.append(len(values))
        for value in values:
            self.tags.append(value)

    def absorb(ref self, other: Unit):
        self.hp += other.hp
        for tag in other.tags:
            self.tags.append(tag)

    def renamed(ref self, name: str):
        self.name = self.name + name
        self.inner.grow(len(name))

    def replaced(ref self):
        self = unit(self.hp * 2)

    def kept_in(ref self, ref into: list[list[int]]):
        into.append(self.tags)
        self.tags.append(-1)

    def checked(ref self, value: int) -> int fails Bad:
        if value < 0:
            fail Bad("negative")
        self.tags.append(value)
        return value

    def tried(ref self, value: int) -> int fails Bad:
        self.hp += 100
        return self.checked(value)?

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
    return Unit(hp, [hp], Inner([hp]), f"u{hp}")

def band() -> Band:
    return Band([unit(1), unit(2)], unit(3), [[unit(4), unit(5)], [unit(6), unit(7)]])

def show(stdout: Stdout, units: list[Unit]):
    for one in units:
        stdout.print(one.hp, one.tags, one.inner.tags, one.name)

def ruled_at(size: int) -> Ruled:
    return Ruled(size, []).expect("a size that's allowed")

def swap_ends(ref a: list[int], ref b: list[int]):
    a.append(len(b))
    b.append(len(a))

def try_all(ref units: list[Unit]) -> int fails Bad:
    _ = units[0].tried(1)?
    _ = units[1].tried(-1)?
    return units[0].tried(2)?

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
    lists[0].tags.append(len(lists[0].tags))
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
        for at_now in range(len(looped)):
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
    got = try_all(ref failing) except bad:
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
    same_as_pinned("changed_places", CHANGED_PLACES)
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
                 def pop(ref self) -> int:\n        return self.items.pop() else 0\n\n    \
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
             lock table as held:\n        seen = len(held.counts) + len(held.names)\n",
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

/// Changes to a list in a `shared` value under its lock that copy nothing: an `insert`, made in place
/// and noted in the lock's rollback log for a crash to take out again, and a change inside one of
/// its items, like a list in a struct in it. That one still copies: reaching the item, the log
/// holds it as it was, so a list in it has a second owner, and changing it copies it.
#[test]
#[ignore = "a change inside an item of a list under a `lock ... as ref` copies what it changes: the log holds the item, so a list in it is shared (#1469); an insert is made in place"]
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
    return 1

def main(stdout: Stdout):
    shared big = Big(items=[n for n in range(10_000)], table={1: 2}, name=\"big\")
    for i in range(100):
        lock big as ref held:
            held.items.append(i)
    gone = refused(big) except Refused(): 0
    lock big as held:
        stdout.print(len(held.items), gone, held.items[-1] + len(held.items) - 99, len(held.name))
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

/// Plain words a lock saves, past the room its buffer starts with, written once or in a loop, go
/// back on a failure and stay at an end: saved by native code itself while there's room, and by the
/// runtime once it's full.
#[test]
fn plain_words_a_lock_saves_go_back_on_a_failure_however_many() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "lock_plain_saves",
        include_str!("fixtures/lock_plain_saves.fen"),
        "10 24500 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 False\n",
    )
}

/// Writes under a lock that nothing after them can undo go unsaved, and the rest are saved: kept,
/// put back by a failure, or by a crash in a launched task, every value is right. Built checking
/// proofs, which stop at a rollback after a write left unsaved.
#[test]
fn a_write_nothing_after_can_undo_goes_unsaved_and_the_rest_go_back() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("lock_unsaved_writes")?;
        let (program, executable) = (dir.join("unsaved.fen"), dir.join("unsaved"));
        std::fs::write(&program, include_str!("fixtures/workers/lock_unsaved_writes.fen"))?;
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
            include_str!("fixtures/workers/lock_unsaved_writes.out"),
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// Locks taken alone begin and end in native code, with no log; two taken together, a save the
/// runtime sees, a failure and a crash in a launched task each make the log first. Whatever the
/// mix, what came before a lock that failed or crashed goes back exactly, and the rest is kept.
/// Built checking proofs, which stop at a lock's end that no begin came before.
#[test]
fn locks_begun_without_the_runtime_put_back_exactly_whatever_comes_inside() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("lock_begun_inline")?;
        let (program, executable) = (dir.join("inline.fen"), dir.join("inline"));
        std::fs::write(&program, include_str!("fixtures/workers/lock_begun_inline.fen"))?;
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
            include_str!("fixtures/workers/lock_begun_inline.out"),
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// A put after a lookup of the same key under a lock writes where the lookup found it, unless what
/// came between grew the dict, popped the key or put another: each kept, and each put back by a
/// failure.
#[test]
fn a_put_after_a_lookup_under_a_lock_is_right_whatever_came_between() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("lock_found_entry", include_str!("fixtures/lock_found_entry.fen"))
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
    let source = "error Boom:\n    step: int\n\nstruct Bins:\n    counts: dict[int, int]\n    names: dict[int, str]\n\ndef put(bins: shared Bins, key: int, name: str, failing: bool) fails Boom:\n    lock bins as ref held:\n        held.counts[key] = (held.counts.get(key) else 0) + 1\n        held.names[key] = held.names.get(key) else name\n        if failing:\n            fail Boom(step=key)\n\ndef main(stdout: Stdout, args: Args):\n    rounds = int(args[0]) except InvalidValue(): 0\n    shared bins = Bins(counts={}, names={})\n    mut failed = 0\n    for step in range(rounds):\n        name = f\"name {step}, long enough to be held in a block of its own\"\n        _ = put(bins, key=step % 2, name=name, failing=step % 3 == 0) except Boom():\n            failed += 1\n    lock bins as held:\n        stdout.print(failed, held.counts[0], held.counts[1], held.names[0], held.names[1])\n";
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
    // The name's put of every round, rolled back or kept, but for the three whose key wasn't there
    // yet: rounds 0, 1 and 2, as round 0's was put back. The count's put, a number into a dict
    // with one owner, is written in place without the runtime once its key is there.
    assert_eq!(field(&fewer.1, "found_puts"), "57", "{}", fewer.1);
    assert_eq!(field(&fewer.1, "live"), field(&more.1, "live"), "{} / {}", fewer.1, more.1);
    Ok(())
}

/// A dict put under a lock where the lookup found the key, with the dict and the struct holding it
/// alone, is written in place, its old value saved: a failure, and a crash in a task of its own,
/// put the value back exactly, as does a failure after the same lock grows the dict by the old path.
/// A dict held a second time, or a key not there yet, takes the old path. Built checking proofs, so
/// a wrong "alone" would stop at the put.
#[test]
fn a_found_put_under_a_lock_is_written_in_place_and_put_back() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("lock_put_in_place")?;
        let (program, executable) = (dir.join("put.fen"), dir.join("put"));
        std::fs::write(&program, include_str!("fixtures/lock_put_in_place.fen"))?;
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
            "put back 1 {1: 11, 2: 20}\nput back 3 {1: 11, 2: 20, 3: 1}\nput back 1 True\nput back 1 True\n\
             held twice 3 {1: 11, 2: 21, 3: 1}\n\
             crashed {1: 11, 2: 21, 3: 1}\n{1: 11, 2: 21, 3: 1}\n",
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// A put after a lookup of the same key under a lock writes where the lookup found it, with no
/// second search, only when the dict there now is the one it searched: a different key, a
/// different dict, a key worked out again with a side effect, a field set to another dict, a dict
/// grown through another local, and a key's local changed in between each search. Kept, or put
/// back by a failure, every one is right. Built checking proofs.
#[test]
fn a_put_after_a_lookup_reuses_it_only_for_the_same_key_and_dict() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("lock_one_lookup")?;
        let (program, executable) = (dir.join("one.fen"), dir.join("one"));
        std::fs::write(&program, include_str!("fixtures/lock_one_lookup.fen"))?;
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
            "2 {0: 6, 1: 40227, 50: 1, 2: 30227, 51: 1, 3: 30233, 52: 1, 4: 30236, 53: 1, 55: 1, 56: 1, 57: 1, 58: 1, \
             60: 1, 61: 1} {3: 31232}\n",
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// A put after the dict grew since its key was looked up writes into the dict as it is now: a
/// lookup's entry in the old block is never used. Built checking proofs, which stop at a put in
/// place whose entry isn't one of the dict's own.
#[test]
fn a_put_after_the_dict_grew_writes_where_the_dict_is_now() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("lock_put_after_growth")?;
        let (program, executable) = (dir.join("grown.fen"), dir.join("grown"));
        std::fs::write(&program, include_str!("fixtures/lock_put_after_growth.fen"))?;
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
            "plain 6 41\nlocked 6 41\nlocked dict 6 41\n",
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// Puts in place copy nothing and leave nothing more held, however many rounds, failing or not;
/// with the dict held twice, each takes the old path and copies it, as it must.
#[test]
fn a_put_in_place_copies_nothing_unless_the_dict_is_held_twice() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("lock_put_in_place_counts")?;
    let program = dir.join("counts.fen");
    std::fs::write(&program, include_str!("fixtures/lock_put_in_place_counts.fen"))?;
    let executable = dir.join("counts");
    let built = fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let run = |rounds: &str, twice: &str| -> std::io::Result<(String, String)> {
        let ran =
            Command::new(&executable).args([rounds, twice]).env("FENNEC_COUNTS", "1").output_within()?;
        let said = String::from_utf8_lossy(&ran.stderr).into_owned();
        let counts = said.lines().find(|line| line.starts_with("copies ")).unwrap_or_default().to_owned();
        Ok((String::from_utf8_lossy(&ran.stdout).into_owned(), counts))
    };
    let (fewer, more, twice) = (run("60", "0")?, run("120", "0")?, run("60", "10")?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        (fewer.0.as_str(), more.0.as_str(), twice.0.as_str()),
        ("20 20 20\n", "40 40 40\n", "20 25 25\n")
    );
    let field = |counts: &str, name: &str| {
        counts.split_whitespace().skip_while(|word| *word != name).nth(1).unwrap_or_default().to_owned()
    };
    assert_eq!(field(&fewer.1, "copies"), "0", "{}", fewer.1);
    assert_eq!(field(&more.1, "copies"), "0", "{}", more.1);
    assert_eq!(field(&fewer.1, "live"), field(&more.1, "live"), "{} / {}", fewer.1, more.1);
    assert_eq!(field(&twice.1, "copies"), "10", "{}", twice.1);
    Ok(())
}

/// A dict only its own lookups and puts hold is put into without checking it's alone, and the
/// dicts something else may hold still copy when they must: copied to another name, passed to a
/// call, kept in a list, or given by a caller. Built checking proofs, so a dict wrongly taken as
/// alone would stop the program.
#[test]
fn a_dict_held_only_by_itself_and_ones_held_twice_give_what_they_should() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r"def keep(d: dict[int, int]) -> int:
    return len(d)

def alone(n: int) -> int:
    mut counts: dict[int, int] = {}
    for i in range(n):
        counts[i % 3] = (counts.get(i % 3) else 0) + 1
    return counts[0] * 100 + counts[1] * 10 + counts[2]

def copied(n: int) -> int:
    mut counts: dict[int, int] = {}
    counts[0] = 1
    before = counts
    for i in range(n):
        counts[0] = counts[0] + 1
    return before[0] * 100 + counts[0]

def passed(n: int) -> int:
    mut counts: dict[int, int] = {}
    mut sizes = 0
    for i in range(n):
        counts[i] = i
        sizes += keep(counts)
    return sizes

def kept(n: int) -> int:
    mut counts: dict[int, int] = {}
    counts[0] = 5
    held = [counts]
    for i in range(n):
        counts[0] = counts[0] + 1
    return held[0][0] * 100 + counts[0]

def given(n: int, ref counts: dict[int, int]) -> int:
    for i in range(n):
        counts[0] = counts[0] + 1
    return counts[0]

def main(stdout: Stdout):
    stdout.print(alone(10), copied(4), passed(5), kept(3))
    mut outside = {0: 7}
    stdout.print(given(2, ref outside), outside[0])
    return
";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("dict_alone")?;
        let (program, executable) = (dir.join("alone.fen"), dir.join("alone"));
        std::fs::write(&program, source)?;
        let built = crate::executables::fennec_command()
            .arg("build")
            .args(how)
            .arg(&program)
            .arg("-o")
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .env("FENNEC_CHECK_PROOFS", "1")
            .output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{how:?}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        // `alone` counts 4, 3, 3; `copied` leaves `before` at 1; `passed` sums 1 to 5; `kept`
        // leaves the list's dict at 5; `given` changes the caller's dict, as `ref` says.
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "433 105 15 508\n9 9\n",
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// What's changed in place stays right when it's read while changing, and when something still holds
/// the list as it was.
#[test]
fn a_list_changed_in_a_shared_value_is_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("shared_places", include_str!("fixtures/shared_places.fen"))
}

#[test]
fn changing_through_a_place_copies_nothing_when_nothing_else_holds_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let types = "struct Inner:\n    tags: list[int]\n\n    def grow(ref self, value: int):\n        self.tags.append(value)\n\nstruct Unit:\n    hp: int\n    tags: list[int]\n    inner: Inner\n\n    def grow(ref self, value: int):\n        self.tags.append(value)\n\nstruct Band:\n    units: list[Unit]\n\n    def grow(ref self, at: int, value: int):\n        self.units[at].grow(value)\n        self.units[at].inner.grow(value)\n\ndef unit() -> Unit:\n    return Unit(0, [], Inner([]))\n\ndef main(stdout: Stdout):\n";
    for (name, body) in [
        (
            "item",
            "    mut xs = [unit(), unit()]\n    for n in range(2000):\n        xs[n % 2].grow(n)\n    stdout.print(len(xs[0].tags))\n",
        ),
        (
            "deep",
            "    mut xs = [unit(), unit()]\n    for n in range(2000):\n        xs[n % 2].inner.grow(n)\n    stdout.print(len(xs[0].inner.tags))\n",
        ),
        (
            "grid",
            "    mut grid = [[unit()], [unit()]]\n    for n in range(2000):\n        grid[n % 2][0].grow(n)\n        grid[n % 2][-1].inner.grow(n)\n    stdout.print(len(grid[0][0].tags))\n",
        ),
        (
            "method",
            "    mut band = Band([unit(), unit()])\n    for n in range(2000):\n        band.grow(n % 2, n)\n    stdout.print(len(band.units[0].tags))\n",
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
    same_as_pinned(
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
    while w < len(items):
        items[w] = 0
        bump(ref w)
        if w < len(items):
            items[w] = 7
        bump(ref w)
    stdout.print(items)
",
    )
}

/// Natively, a place's index can be known from the line before, so a call that changes the index
/// has to forget it. A call that changes the index of a place it's also given doesn't compile.
/// An index proven not negative is checked once, unsigned. One kept as -1 for none still counts
/// from the end where nothing says it isn't -1, and one counted down past 0 does too. Built
/// checking proofs, so a wrong proof would stop the program.
#[test]
fn an_index_proven_not_negative_and_one_that_counts_from_the_end() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"import std.json as json

NONE = -1

struct Node:
    next: int

struct Cursor:
    at: int

def pick(xs: list[int], cursor: Cursor) -> int:
    return xs[cursor.at]

def stepped_down(xs: list[int]) -> int:
    mut at = 0
    mut total = 0
    for _ in range(3):
        total += xs[at]
        at -= 1
    return total

def walk(nodes: list[Node], start: int) -> int:
    mut at = start
    mut seen = 0
    while at != NONE:
        seen += 1
        at = nodes[at].next
    return seen

def last_of(nodes: list[Node], at: int) -> int:
    return nodes[at].next

def down(xs: list[int]) -> int:
    mut i = len(xs) - 1
    mut total = 0
    while i >= 0:
        total += xs[i]
        i -= 1
    return total

def down_too_far(xs: list[int]) -> int:
    mut i = len(xs) - 1
    mut total = 0
    while i > -3:
        total += xs[i]
        i -= 1
    return total

def main(stdout: Stdout):
    nodes = [Node(next=2), Node(next=NONE), Node(next=1)]
    stdout.print(walk(nodes, start=0), last_of(nodes, at=NONE))
    stdout.print(down([1, 2, 3]), down_too_far([1, 2, 3]))
    xs = [10, 20, 30]
    read = json.loads[Cursor]("{\"at\": -1}") except ParseError(): Cursor(at=0)
    stdout.print(pick(xs, Cursor(at=1)), pick(xs, read))
    stdout.print(stepped_down(xs))
"#;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("index_signs")?;
        let (program, executable) = (dir.join("signs.fen"), dir.join("signs"));
        std::fs::write(&program, source)?;
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
        // `walk` goes 0, 2, 1; `last_of` reads the last node; `down_too_far` adds 3, 2, 1, then
        // xs[-1] and xs[-2], 3 and 2. A cursor JSON gave -1 reads the last item: a field a reader
        // can fill isn't proven, however the program itself sets it. `stepped_down` reads xs[0],
        // xs[-1] and xs[-2]: an index a later turn makes negative isn't proven from the first.
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "3 1\n6 11\n20 30\n60\n",
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// What every call gives a helper is known in it: `go`'s sums are proven from positions and a
/// count stepped once a turn, and a build checking proofs checks what it's given on the way in.
/// `shifted` is given both ends of int across its calls, so its sum keeps its check, and fails.
#[test]
fn what_every_call_gives_a_helper_is_known_in_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def go(i: int, j: int) -> float:
    return 1.0 / float((i + j) * (i + j + 1) // 2 + i + 1)

def row(v: list[float], i: int) -> float:
    mut total = 0.0
    mut j = 0
    for x in v:
        total += go(i, j=j) * x
        j += 1
    return total

def shifted(a: int, b: int) -> int:
    return a + b

def counted(v: list[int]) -> int:
    mut seen = 0
    for x in v:
        seen += 2
    return seen - 1

def main(stdout: Stdout):
    v = [1.0, 2.0, 3.0]
    rows = [row(v, i) for i in range(len(v))]
    stdout.print(f"{rows[0] + rows[1] + rows[2]:.6f}")
    stdout.print(shifted(3, b=4), shifted(-9, b=2), counted([5, 6, 7]))
    big = 9223372036854775800
    stdout.print(shifted(big, b=7))
    stdout.print(shifted(big, b=8))
    return
"#;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("given_ranges")?;
        let (program, executable) = (dir.join("given.fen"), dir.join("given"));
        std::fs::write(&program, source)?;
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
        let err = String::from_utf8_lossy(&ran.stderr);
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "4.477991\n7 -7 5\n9223372036854775807\n",
            "{how:?}: {err}"
        );
        assert!(err.starts_with("error: this calculation overflows an int"), "{how:?}: {err}");
        assert_eq!(ran.status.code(), Some(1), "{how:?}: {err}");
    }
    Ok(())
}

/// Reading a string with characters of more than one byte, in order, backwards and by place, runs
/// the same checking proofs, which checks the string's cursor is inside it each time it's used.
#[test]
fn strings_read_by_place_run_the_same_checking_their_cursor() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def count(s: str) -> int:
    mut n = 0
    for i in range(len(s)):
        if s[i] == "é" or s[i - 1] == "a":
            n += 1
    return n

def backwards(s: str) -> str:
    mut out = ""
    mut i = len(s) - 1
    while i >= 0:
        out += s[i]
        i -= 1
    return out

def main(stdout: Stdout):
    s = "café naïve über alles, déjà vu"
    stdout.print(count(s), backwards(s), s[7], s[-2], s[3], s[0])
    return
"#;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        for proofs in ["0", "1"] {
            let dir = folder("string_cursor")?;
            let (program, executable) = (dir.join("cursor.fen"), dir.join("cursor"));
            std::fs::write(&program, source)?;
            let built = crate::executables::fennec_command()
                .arg("build")
                .args(how)
                .arg(&program)
                .arg("-o")
                .arg(&executable)
                .env("FENNEC_HOME", &dir)
                .env("FENNEC_CHECK_PROOFS", proofs)
                .output_before(FENNEC_DEADLINE)?;
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            let ran = Command::new(&executable).output_within()?;
            std::fs::remove_dir_all(&dir)?;
            assert_eq!(
                String::from_utf8_lossy(&ran.stdout),
                "5 uv \u{e0}j\u{e9}d ,sella rebü evïan \u{e9}fac \u{ef} v \u{e9} c\n",
                "{how:?} proofs {proofs}: {}",
                String::from_utf8_lossy(&ran.stderr)
            );
        }
    }
    Ok(())
}

/// A program whose code is handed every kind of fact runs the same built with them, with them
/// stripped (`FENNEC_STRIP_FACTS=1`), and checking proofs, which checks the facts it can.
#[test]
fn facts_stripped_or_checked_run_the_same() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "struct Unit:\n    hp: int\n\ndef bump(ref unit: Unit, by: int, seen: list[int]) -> int:\n    unit.hp += by % 7 + len(seen)\n    return unit.hp\n\ndef fill(xs: list[int]) -> list[int]:\n    mut ys = xs\n    ys.append(len(xs))\n    return ys\n\ndef pick(xs: list[int], at: int) -> int:\n    return xs[at]\n\ndef main(stdout: Stdout):\n    mut unit = Unit(hp=1)\n    stdout.print(bump(ref unit, by=10, seen=[1]), fill([1, 2]), pick([4, 5], at=1))\n    return\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        for (setting, on) in
            [("FENNEC_STRIP_FACTS", "0"), ("FENNEC_STRIP_FACTS", "1"), ("FENNEC_CHECK_PROOFS", "1")]
        {
            let dir = folder("facts_stripped")?;
            let (program, executable) = (dir.join("facts.fen"), dir.join("facts"));
            std::fs::write(&program, source)?;
            let built = crate::executables::fennec_command()
                .arg("build")
                .args(how)
                .arg(&program)
                .arg("-o")
                .arg(&executable)
                .env("FENNEC_HOME", &dir)
                .env(setting, on)
                .output_before(FENNEC_DEADLINE)?;
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            let ran = Command::new(&executable).output_within()?;
            std::fs::remove_dir_all(&dir)?;
            assert_eq!(
                String::from_utf8_lossy(&ran.stdout),
                "5 [1, 2, 2] 5\n",
                "{how:?} {setting}={on}: {}",
                String::from_utf8_lossy(&ran.stderr)
            );
        }
    }
    Ok(())
}

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
    return Unit(unit.name, unit.hp - by, unit.tags)

def renamed(unit: Unit) -> Unit:
    return Unit(unit.name + "!", unit.hp, [unit.hp, len(unit.tags)])

struct Crowd:
    numbers: list[int]
    by: int
    units: list[Unit]

    def size(self) -> int:
        return len(self.numbers)

    def step(ref self):
        self.numbers = [number - self.by for number in self.numbers]

struct Outer:
    crowd: Crowd

struct Ruled:
    numbers: list[int] where len(numbers) > 0

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
    seen = [number + seen[0] + len(seen) for number in seen]
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
    same_as_expected_in_both_builds("replacing_its_source", REPLACING_ITS_SOURCE)
}

/// A list a counted loop fills once a turn, with room made for every turn and nothing else in
/// the loop touching it, is filled with no room check. Every way that rule can fail keeps the
/// check: the list shared or read in the loop, a count starting below 0 or from a variable,
/// two appends a turn, and a loop inside another that makes the list each time round.
#[test]
fn lists_filled_within_their_room_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned("filled_within_room", FILLED_WITHIN_ROOM)
}

const FILLED_WITHIN_ROOM: &str = r#"def main(stdout: Stdout):
    mut total = 0
    for round in range(3):
        numbers = [i * 2 + round for i in range(1000)]
        names = [f"n{i}" for i in range(50)]
        total += numbers[999] + len(numbers) + len(names[49])
    stdout.print(total)
    zeros = [0 for _ in range(4)]
    grid = [[r * 10 + c for c in range(3)] for r in range(3)]
    stdout.print(zeros, grid, [len(row) for row in grid])
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
        read.append(len(read) + i)
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
    same_message_as_pinned(
        "local_item_field_appends",
        r"struct Unit:
    tags: list[int]
    hp: int

struct Box:
    items: list[int]
    next: list[Box]

def grow(ref units: list[Unit], by: int):
    for at in range(len(units)):
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
    units[0].tags.append(len(units[0].tags))
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
        "struct Unit:\n    tags: list[int]\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units = [Unit([1, 2, 3], 0), Unit([1, 2, 3], 1)]\n    for round in range(1000):\n        for at in range(len(units)):\n            units[at].tags.append(round)\n    stdout.print(len(units[0].tags))\n",
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
    return "".join(parts)

def show_all(text: str) -> str:
    mut out = []
    for at in range(len(text)):
        out.append(text[at])
    return "".join(out)

def main(stdout: Stdout):
    text = long_text()
    stdout.print(len(text), show_all(text) == text)
    # Backward, forward, jumps, ends, negative.
    mut seen = []
    mut back = len(text) - 1
    while back >= 0:
        seen.append(text[back])
        back -= 1
    stdout.print(seen[0], seen[1], seen[len(seen) - 1], len(seen))
    stdout.print(text[0], text[1], text[-1], text[-2], text[5], text[100], text[3], text[len(text) - 1])
    mut jumps = []
    for at in [0, 200, 1, 199, 50, 51, 49, -1, -200, 7]:
        jumps.append(text[at])
    stdout.print(jumps)
    # Slices: every way of bounding one, inside, past the ends, empty, backward.
    stdout.print(text[:5], text[5:], text[1:6], text[-5:], text[:-200], text[-3:-1], text[3:3], text[9:2])
    stdout.print(text[0:100000], text[-100000:3], text[200:205], text[205:200], len(text[:]))
    mut pieces = []
    mut piece_at = 0
    while piece_at < len(text):
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
        if grown[-1] != grown[len(grown) - 1]:
            stdout.print("wrong")
    stdout.print(len(grown), grown[100], grown[-1], grown[150:153])
    # Strings let go of, their addresses free for new ones with other text.
    for round in range(200):
        made = f"é{round} ümläut ümläut ümläut ümläut ümläut"
        last = made[len(made) - 1]
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
        before = moved[len(moved) - 2]
    stdout.print(len(moved), before, moved[1], moved[2], moved[3], moved[299], moved[-1], moved[1:4])
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
    stdout.print(short[0], short[1], short[2], short[-1], short[1:], short[:1], len(short))
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
        while self.at < len(self.text):
            if self.text[self.at] == "x":
                total += 1
            self.at += 1
            self.seen = len(self.text)
        return total

    def read_growing(ref self) -> int:
        mut total = 0
        while self.at < len(self.text) and len(self.text) < 400:
            if self.text[self.at] == "a":
                self.text += "ax"
            self.at += 1
            total += len(self.text)
        return total

    def read_replacing(ref self) -> int:
        mut total = 0
        while self.at < len(self.text):
            if self.at == 2:
                self.text = "a replacement long enough to have its own block of text"
            total += len(self.text)
            self.at += 1
        return total

def main(stdout: Stdout):
    mut one = Reader(text="xaxax long enough to have a block of its own")
    stdout.print(one.read(), one.at, one.seen)
    mut two = Reader(text="a string long enough to have a block of its own ")
    stdout.print(two.read_growing(), two.at, len(two.text))
    mut three = Reader(text="short")
    stdout.print(three.read_replacing(), three.at, three.text)
    mut four = Reader(text="short")
    for round in range(40):
        four.grow("é" if round % 7 == 0 else "ab")
        stdout.print(len(four.text), four.text[four.at - 1], four.at)
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
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
    }
    Ok(())
}

#[test]
fn reading_a_string_that_isnt_ascii_finds_the_right_character() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned("character_places", CHARACTER_PLACES)
}

/// Reading and slicing a long string in order, one accented letter in it. How long this takes
/// is held by the `json_unicode` kernel; a test this size says only that it's right.
#[test]
fn reading_a_string_that_isnt_ascii_in_order_takes_a_step_for_each_character() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "character_steps",
        "def main(stdout: Stdout):\n    text = \"é\" + \"a\" * 3000\n    mut found = 0\n    for at in range(len(text)):\n        if text[at] == \"a\":\n            found += 1\n    mut pieces = 0\n    for at in range(len(text) - 3):\n        pieces += len(text[at:at + 3])\n    stdout.print(found, pieces)\n",
    )
}

/// Reading a string that isn't ASCII by position, as a parser does: the cursor's character again,
/// the next, characters of each width, back and from the end, positions outside it, an empty
/// string, and slices, after which reading carries on from where it was.
#[test]
fn reading_characters_by_position_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "characters_by_position",
        r#"def words(text: str) -> list[str]:
    mut found = []
    mut at = 0
    while at < len(text):
        while at < len(text) and text[at] == " ":
            at += 1
        start = at
        while at < len(text) and text[at] != " ":
            at += 1
        if at > start:
            found.append(text[start:at])
    return found

def main(stdout: Stdout):
    text = "fóx ünïcode 日本語 🦊🦊 and plain ascii past an inline header é"
    stdout.print(words(text))
    stdout.print(words(""), words("é"), words(" é 🦊 "))
    mut seen = []
    for at in [0, 0, 1, 1, 2, 5, 4, 30, 29, 31, -1, -2, -60, 2, 3, 13, 12, 14, 15, 16, 17]:
        if -len(text) <= at and at < len(text):
            seen.append(f"{at}={text[at]}")
    stdout.print("".join(seen))
    stdout.print(text[4:11], text[12:15], text[16:18], text[-1:], text[:3], text[5])
    mut total = 0
    for at in range(len(text)):
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
    for at in range(len(text)):
        first = text[at]
        again = text[at]
        out.append(f"{first}{again}")
        for back in range(1, 4):
            if at >= back:
                out.append(text[at - back:at])
    stdout.print("|".join(out))
    stdout.print(text[0], text[4], text[9], text[3], text[-1], text[-1], text[len(text) - 1], text[2], text[2])
"#,
        "aa|bb|a|日日|b|ab|cc|日|b日|ab日|🦊🦊|c|日c|b日c|dd|🦊|c🦊|日c🦊|ee|d|🦊d|c🦊d|  |e|de|🦊de|éé| |e |de |  |é| é|e é|ff| |é | é |xx|f| f|é f|xx|x|fx| fx|xx|x|xx|fxx|xx|x|xx|xxx|xx|x|xx|xxx|xx|x|xx|xxx|xx|x|xx|xxx|xx|x|xx|xxx|ññ|x|xx|xxx\na 🦊   c ñ ñ ñ 日 日\n",
    )
}

/// A loop over a string's characters: a text all ASCII takes a byte at a time with nothing to
/// test, and any other its characters' bytes, whether it's inline or a block's, and the body
/// breaking, going on, or looping over text of its own.
#[test]
fn looping_over_characters_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
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
    return "".join(out)

def until(text: str, stop: str) -> int:
    mut seen = 0
    for c in text:
        if c == stop:
            break
        if c == "a":
            continue
        seen += 1
    return seen

def main(stdout: Stdout):
    for text in ["", "a", "short ascii", "héllo wörld 🦊 and 日本", "a much longer ASCII text than any inline header can hold",
                 "a much longer text than a header holds, ending in é", "🦊🦊🦊🦊🦊🦊🦊🦊🦊", "x1 y2 z3 " * 20]:
        stdout.print(kinds(text), until(text, stop="z"), until(text, stop="🦊"), len(text))
    mut joined = []
    for c in "nested é":
        for d in "ab":
            joined.append(c + d)
    stdout.print("".join(joined))
    mut changed = 0
    for c in "rename ü inside":
        more = c + "!"
        changed += len(more)
    stdout.print(changed)
    words = ["plain", "wörd", "日本語", "long enough ascii word to need a block of its own"]
    stdout.print([sum(1 for c in w if c != "o") for w in words])
"##,
    )
}

/// Indexing and slicing text that isn't ASCII, in and out of order, from either end, clamped
/// and empty, as native code finds characters from a block's cursor.
#[test]
fn indexing_and_slicing_a_string_that_isnt_ascii_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "wide_slices",
        r#"def main(stdout: Stdout):
    text = "héllo wörld, ünïcode ✓ and 🦊 fox; then plain ascii after all that"
    n = len(text)
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
        "def main(stdout: Stdout):\n    text = \"é\" + \"abc日\" * 1000\n    stdout.print(len(text))\n";
    let read = "def main(stdout: Stdout):\n    text = \"é\" + \"abc日\" * 1000\n    mut found = 0\n    mut size = 0\n    for at in range(len(text) - 2):\n        if text[at] == \"日\":\n            found += 1\n        size += len(text[at:at + 2])\n    stdout.print(found, size)\n";
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
    return sum(unit.hp for unit in units)

def by_fail(ref units: list[Unit]) -> int fails Bad:
    for ref unit in units:
        unit.tags.append(9)
        unit.hp += 1
        if unit.hp == 2:
            fail Bad("stop")
    return 0

def by_return(ref units: list[Unit]) -> int:
    for ref unit in units:
        unit.tags.append(8)
        if unit.hp == 1:
            return len(unit.tags)
    return -1

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
    return [Unit([1], "first name long enough not to be inline", 0), Unit([2], "b", 1), Unit([3], "c", 5)]

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
    by_fail_value = by_fail(ref a) except Bad(): -1
    stdout.print(by_fail_value, a)
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
        unit.tags.append(len(d[0].tags))
        unit.hp += len(d) + total(d)
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
        row.append(len(row))
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
    same_as_pinned("for_mut_items", FOR_MUT_ITEMS)?;
    let counts = counts_of(
        "for_mut_counts",
        "struct Unit:\n    tags: list[int]\n    hp: int\n\ndef main(stdout: Stdout):\n    mut units = [Unit([1, 2, 3], 0), Unit([1, 2, 3], 1)]\n    for round in range(1000):\n        for ref unit in units:\n            unit.tags.append(round)\n            if round % 2 == 0:\n                unit.hp += 1\n    stdout.print(len(units[0].tags))\n",
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
    return sum(unit.health for unit in units)

def total_in(world: World) -> int:
    return total(world.units)

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
        return x

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
    undone(b) except why:
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
    return sum(unit.health for unit in units)

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
        ref last = world.units[len(world.units) - 1]
        if turn == 1:
            continue
        last.health += turn
    stdout.print(world.units[1].health)
"#;

/// A `ref` case whose body returns puts the case back before the function returns, and the
/// function returns what the body did, on every way through: a `def` that ends in such a
/// `match` gives its value with `return` in each case, as every `def` does since 2026-11-11.
#[test]
fn a_ref_case_that_returns_puts_the_case_back_first() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "ref_case_returns",
        r"enum Slot:
    Empty
    Held(count: int)

def bump(ref slot: Slot) -> int:
    match slot:
        case .Held(ref count):
            count += 1
            return count
        case .Empty:
            return 0

def bump_or_keep(ref slot: Slot, keep: bool) -> int:
    match slot:
        case .Held(ref count):
            count += 1
            if keep:
                return count
        case .Empty:
            pass
    return -1

def main(stdout: Stdout):
    mut slot = Slot.Held(count=1)
    stdout.print(bump(ref slot), bump(ref slot), slot)
    mut empty = Slot.Empty
    stdout.print(bump(ref empty), empty)
    stdout.print(bump_or_keep(ref slot, True), bump_or_keep(ref slot, False), slot)
",
        "2 3 Slot.Held(count=3)\n0 Slot.Empty\n4 -1 Slot.Held(count=5)\n",
    )
}

/// `case .Button(ref clicks):` changes the field inside the enum the `match` is over, natively,
/// as the language says: a case that finishes, one that leaves by `return`, and one that fails,
/// each putting the case back, and a `_` field kept as it was.
#[test]
fn ref_cases_change_fields_where_they_are_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("ref_cases", REF_CASES)
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
    return 0

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
    capped(ref kind) except failure:
        first = f"{failure}"
    mut second = "fine"
    capped(ref kind) except failure:
        second = f"{failure}"
    stdout.print(first, second)
    stdout.print(kind)
"#;

/// `for key, ref value in x.items()` changes each value of a dict, or item of a `Table`, where it
/// is, natively, with its key or its `Id`, and a `continue` leaves the rest of that one as it was.
#[test]
fn a_ref_over_items_changes_each_value_where_it_is_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "ref_over_items",
            r#"from std.collections import Table

struct Unit:
    health: int
    target = 0

def main(stdout: Stdout):
    mut units = Table[Unit]()
    first = units.add(Unit(health=1))
    _ = units.add(Unit(health=5))
    for id, ref unit in units.items():
        unit.health += 1
        if id == first:
            unit.target = 7
    stdout.print(units)
    mut by_name = {"a": Unit(health=1), "b": Unit(health=2)}
    for name, ref unit in by_name.items():
        if name == "b":
            continue
        unit.health += 10
    stdout.print(by_name)
    mut scores = {"x": 1}
    for key, ref score in scores.items():
        score *= 3
    stdout.print(scores)
"#
        )?,
        "Table([Unit(health=2, target=7), Unit(health=6, target=0)])\n\
         {\"a\": Unit(health=11, target=0), \"b\": Unit(health=2, target=0)}\n{\"x\": 3}\n"
    );
    Ok(())
}

/// `for at, ref item in enumerate(items)` changes each item where it is, natively, as the
/// language says: numbers, structs and lists, a `continue` that still counts what it skips, and a
/// nested one over the items' own lists.
#[test]
fn enumerate_with_ref_changes_items_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("enumerate_ref", ENUMERATE_REF)
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
fn isdigit_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "isdigit",
        r#"def main(stdout: Stdout):
    long = "1234567890" * 5
    for text in ["", "0", "9", "5", "/", ":", "a", "12", "1a", "a1", " 1", "1 ", "٣", "１", "1٣", "é", "🦊", long, long + "x", "x" + long, long[:23], long[:24]]:
        stdout.print(text, text.isdigit())
    mut found = 0
    for at in range(len(long)):
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
fn a_remainder_kept_in_range_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("remainder_in_range", REMAINDER_IN_RANGE)
}

const REMAINDER_IN_RANGE: &str = r#"def stirred(n: int) -> int:
    mut x = n % 2_147_483_646 + 1
    for _ in range(100):
        x = x * 48_271 % 2_147_483_647
    return x % 1_000

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
fn dividing_by_a_power_of_two_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("power_of_two_division", POWER_OF_TWO_DIVISION)
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
/// still fails.
#[test]
fn steps_proved_safe_match_what_was_pinned_at_the_ends_of_the_ints() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let program = |last: &str| {
        format!(
            "def closing(low: int, high: int) -> (int, int):\n    mut a = low\n    mut b = high\n    while a < b:\n        a += 1\n        b -= 1\n    return (a, b)\n\ndef counted(limit: int, start: int) -> int:\n    mut count = start\n    while count < limit and count % 1000 != 999:\n        count += 1\n    return count\n\ndef stirred(n: int) -> int:\n    mut seed = 42\n    mut total = 0\n    for _ in range(n):\n        seed = (seed * 3877 + 29573) % 139968\n        total += seed % 10\n    return total\n\ndef main(stdout: Stdout):\n    highest = 9223372036854775807\n    lowest = -9223372036854775807 - 1\n    stdout.print(closing(highest - 9, high=highest), closing(lowest, high=lowest + 7), closing(5, high=1))\n    stdout.print(counted(highest, start=highest - 5), counted(10, start=3), stirred(1000))\n    mut x = highest - 1\n    if x < highest:\n        x += 1\n    stdout.print(x)\n    {last}\n"
        )
    };
    same_as_expected_in_both_builds("proved_steps", &program("stdout.print(\"done\")"))?;
    same_as_expected_in_both_builds("proved_steps_then_past", &program("x += 1\n    stdout.print(x)"))
}

/// A print that fails stops the program there, as anything that fails does. Its standard output
/// is a file it may only write 512 bytes of (`ulimit -f 1`, with the signal for passing that
/// ignored), so writing fails once a run has kept enough to write out: what comes after the loop
/// never runs, and the failure says why. It used to carry on and say so only at the end.
#[cfg(unix)]
#[test]
fn a_print_that_fails_stops_the_program() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def main(stdout: Stdout, stderr: Stderr):\n    for i in range(100000):\n        stdout.print(i)\n    stderr.write_line(\"went on\")?\n    return\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("print_fails")?;
        let (program, executable) = (dir.join("prints.fen"), dir.join("prints"));
        std::fs::write(&program, source)?;
        let built = crate::executables::fennec_command()
            .arg("build")
            .args(how)
            .arg(&program)
            .arg("-o")
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new("sh")
            .arg("-c")
            .arg("trap '' XFSZ; ulimit -f 1; exec \"$0\" > \"$1\"")
            .arg(&executable)
            .arg(dir.join("printed.txt"))
            .output_within()?;
        std::fs::remove_dir_all(&dir)?;
        let err = String::from_utf8_lossy(&ran.stderr);
        assert!(err.starts_with("error: couldn't write output"), "{how:?}: {err}");
        assert!(!err.contains("went on"), "{how:?}: {err}");
        assert_eq!(ran.status.code(), Some(1), "{how:?}: {err}");
    }
    Ok(())
}

/// A product past the check is known not to have overflowed, which lets LLVM drop later checks:
/// one that does overflow still fails, from either sign, and one that doesn't goes on to its sums.
/// Two ints known not negative multiply unsigned: past `i64::MAX` and past `u64::MAX` both fail.
#[test]
fn a_product_that_overflows_still_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let program = |a: &str, b: &str| {
        format!(
            "def scaled(i: int, j: int) -> float:\n    return 1.0 / float((i + j) * (i + j + 1) // 2 + i + 1)\n\ndef main(stdout: Stdout):\n    mut total = 0.0\n    for i in range(50):\n        for j in range(50):\n            total += scaled(i, j=j)\n    stdout.print(f\"{{total:.6f}}\")\n    big = {a}\n    stdout.print(big * {b})\n"
        )
    };
    for (name, a, b) in [
        ("product_fits", "3037000499", "3037000499"),
        ("product_too_big", "3037000500", "3037000500"),
        ("product_too_small", "-3037000500", "3037000500"),
        ("product_far_too_big", "4294967296", "4294967296"),
    ] {
        same_as_expected_in_both_builds(name, &program(a, b))?;
    }
    Ok(())
}

/// Sized integers natively: arithmetic, floor division and remainder as `int`'s, `~` within an
/// unsigned size's width, shifts by an `int`, conversions written out, lists of them, and `/`
/// giving a float. Nothing was pinned for sized numbers, so the outcomes are written here.
#[test]
fn sized_integers_work_out_as_their_sizes_say() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "sized_integers",
        "def mix(a: u8, b: u8) -> u8:
    return u8((u16(a) * 7 + u16(b)) % 251)

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
    octets: list[u8] = [1, 2, 255]
    stdout.print(octets, octets[2] == 255, i32(low) * 1000, 3 * i32(low) < 0)
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
    return hash

def scaled(by: f32) -> f32:
    return by * 2.0

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

/// An optional string crosses a `shared` value's lock as what it is. `None`'s header is all zero,
/// which reads as a block at no address, and that was made a text on the way across: a `None` put
/// in a `shared list[str?]` read back as the empty string, by a task or by the same thread. Here
/// `None`, the empty string, a short string and one in a block each go in and come out the same,
/// in a list of them and inside a tuple.
#[test]
fn an_optional_string_that_is_none_crosses_a_lock_as_none() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds("none_text_crosses", NONE_TEXT_CROSSES, NONE_TEXT_CROSSES_PRINTS)?;
    assert!(counts_of("none_text_crosses_counts", NONE_TEXT_CROSSES)?.ends_with(" live 0"));
    Ok(())
}

const NONE_TEXT_CROSSES: &str = r#"# An optional string crosses a `shared` value's lock as what it is: `None` stays `None`, and the
# empty string stays the empty string, in a list of them and inside a tuple, written by a task
# and by the thread that reads them.

def put(out: shared list[(int, str?)], pair: (int, str?)):
    lock out as ref held:
        held.append(pair)
    return

def put_one(out: shared list[str?], one: str?):
    lock out as ref held:
        held.append(one)
    return

def main(stdout: Stdout, args: Args):
    # No arguments are given, so `nothing` is `None`, but the compiler can't know it.
    nothing: str? = None if len(args) < 5 else "given"
    texts: list[str?] = [nothing, "", "short", "a text long enough for a block of its own"]
    shared out = list[(int, str?)]()
    shared ones = list[str?]()
    for at in range(len(texts)):
        one = texts[at]
        pair = (at, one)
        with launch(() => put(out, pair)):
            pass
        with launch(() => put_one(ones, one)):
            pass
        put(out, pair)
        put_one(ones, one)
    lock ones as held:
        stdout.print(held)
        stdout.print(held[0] == None, held[1] == None, held[2] == "", held[3] == "", held[2] == None)
        stdout.print([held[at * 2] == texts[at] and held[at * 2 + 1] == texts[at] for at in range(len(texts))])
    lock out as held:
        stdout.print(held)
        stdout.print(held[0][1] == None, held[2][1] == "", held[2][1] == None, held[1] == (0, nothing))
    return
"#;

const NONE_TEXT_CROSSES_PRINTS: &str = r#"[None, None, "", "", "short", "short", "a text long enough for a block of its own", "a text long enough for a block of its own"]
True True True True False
[True, True, True, True]
[(0, None), (0, None), (1, ""), (1, ""), (2, "short"), (2, "short"), (3, "a text long enough for a block of its own"), (3, "a text long enough for a block of its own")]
True True False True
"#;

/// Every other way a missing value goes to another task, pinned beside the lock: a `str?`, a
/// `list[int]?` and a `dict[str, int]?` that are `None`, in a tuple and in a struct, sent on a
/// channel by this thread and by a task that captured them, handed to `parallel` as items and given
/// back by it, and put in a `shared` list alone and in a struct. Each comes out `None`, and an
/// empty string, list or dict beside it comes out empty.
#[test]
fn optionals_that_are_none_reach_another_task_as_none_every_way() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds("none_crosses_every_way", NONE_EVERY_WAY, NONE_EVERY_WAY_PRINTS)?;
    assert!(counts_of("none_crosses_every_way_counts", NONE_EVERY_WAY)?.ends_with(" live 0"));
    Ok(())
}

const NONE_EVERY_WAY: &str = r#"# Optionals that are `None` going every way a value goes to another task: a channel, a task's
# captures, `parallel`'s items and what it gives back, and a lock, for `str?`, `list[int]?` and
# `dict[str, int]?`, each inside a tuple, and where one can be, alone.

from std.concurrent import Closed, Crashed, Sender, channel

struct Maybes:
    text: str?
    items: list[int]?
    counts: dict[str, int]?

def echo_text(back: Sender[(int, str?)], one: (int, str?)):
    back.send(one) except Closed():
        pass

def echo_all(back: Sender[Maybes], one: Maybes):
    back.send(one) except Closed():
        pass

def same(one: str?) -> str?:
    return one

def is_none(one: str?) -> bool:
    return one == None

def put_list(out: shared list[list[int]?], one: list[int]?):
    lock out as ref held:
        held.append(one)
    return

def put_dict(out: shared list[dict[str, int]?], one: dict[str, int]?):
    lock out as ref held:
        held.append(one)
    return

def put_all(out: shared list[Maybes], one: Maybes):
    lock out as ref held:
        held.append(one)
    return

def main(stdout: Stdout, args: Args) fails Crashed:
    # No arguments are given, so each of these is `None`, but the compiler can't know it.
    given = len(args) > 4
    no_text: str? = "given" if given else None
    no_list: list[int]? = [1] if given else None
    no_dict: dict[str, int]? = {"a": 1} if given else None
    texts: list[str?] = [no_text, "", "short", "a text long enough for a block of its own"]
    # A channel, sent by this thread and by a task that captured the value.
    with channel[(int, str?)](size=8) as box:
        for at in range(len(texts)):
            pair = (at, texts[at])
            box.sender().send(pair) except Closed():
                pass
            with launch(() => echo_text(box.sender(), pair)):
                pass
        got = [box.receive() else (9, "closed") for at in range(8)]
        stdout.print(got)
        stdout.print(got[0][1] == None, got[1][1] == None, got[2][1] == None, got[2][1] == "", got[3] == (1, ""))
    nothing = Maybes(text=no_text, items=no_list, counts=no_dict)
    empty = Maybes(text="", items=[], counts={})
    with channel[Maybes](size=4) as box:
        box.sender().send(nothing) except Closed():
            pass
        with launch(() => echo_all(box.sender(), nothing)):
            pass
        box.sender().send(empty) except Closed():
            pass
        got = [box.receive() else empty for at in range(3)]
        stdout.print(got)
        stdout.print(got[0] == nothing, got[1] == nothing, got[2] == empty, got[1].items == None, got[1].counts == None, got[2].items == None)
    # `parallel`'s items, and what it gives back.
    back = [same(one) for one in parallel(texts)]
    nones = [is_none(one) for one in parallel(texts)]
    stdout.print(back, back == texts, nones)
    # A lock, for a list and a dict that may be missing, alone and in a struct.
    shared lists = list[list[int]?]()
    shared dicts = list[dict[str, int]?]()
    shared alls = list[Maybes]()
    with launch(() => put_list(lists, no_list)):
        pass
    put_list(lists, no_list)
    put_list(lists, [])
    with launch(() => put_dict(dicts, no_dict)):
        pass
    put_dict(dicts, no_dict)
    put_dict(dicts, {})
    with launch(() => put_all(alls, nothing)):
        pass
    put_all(alls, nothing)
    put_all(alls, empty)
    lock lists as held:
        stdout.print(held, held[0] == None, held[1] == None, held[2] == None)
    lock dicts as held:
        stdout.print(held, held[0] == None, held[1] == None, held[2] == None)
    lock alls as held:
        stdout.print(held)
        stdout.print(held[0] == nothing, held[1] == nothing, held[2] == empty, held[0].text == None, held[2].text == None)
    return
"#;

const NONE_EVERY_WAY_PRINTS: &str = r#"[(0, None), (0, None), (1, ""), (1, ""), (2, "short"), (2, "short"), (3, "a text long enough for a block of its own"), (3, "a text long enough for a block of its own")]
True True False True True
[Maybes(text=None, items=None, counts=None), Maybes(text=None, items=None, counts=None), Maybes(text="", items=[], counts={})]
True True True True True False
[None, "", "short", "a text long enough for a block of its own"] True [True, False, False, False]
[None, None, []] True True False
[None, None, {}] True True False
[Maybes(text=None, items=None, counts=None), Maybes(text=None, items=None, counts=None), Maybes(text="", items=[], counts={})]
True True True True False
"#;

/// A struct held in place compares, orders as a tuple and crosses a lock whatever its fields hold, as
/// it would boxed: here a `u64` and an `f32` beside a text, which `==` refused in place while the
/// same struct boxed, with a `dict` beside them, compared fine. A `u64` orders unsigned.
#[test]
fn a_struct_in_place_compares_and_crosses_a_lock_whatever_its_fields_hold() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "in_place_any_field",
        IN_PLACE_ANY_FIELD,
        "True False True\nTrue False True\nPart(big=6, small=6.0, name=\"a name long enough for a block 6\") True 3\n",
    )?;
    assert!(counts_of("in_place_any_field_counts", IN_PLACE_ANY_FIELD)?.ends_with(" live 0"));
    Ok(())
}

const IN_PLACE_ANY_FIELD: &str = r#"struct Part:
    big: u64
    small: f32
    name: str


def make(n: int) -> Part:
    return Part(big=u64(n), small=f32(n), name=f"a name long enough for a block {n}")

def main(stdout: Stdout):
    noise = [make(n + 100) for n in range(3)]
    stdout.print(make(1) == make(1), make(1) == make(2), make(1) != make(2))
    stdout.print((u64(1), "a") < (u64(2), "a"), (u64(2) * u64(1 << 62), "b") < (u64(1), "b"), (u64(3), "c") <= (u64(3), "c"))
    shared store = [make(5)]
    lock store as ref held:
        held.append(make(6))
    mut taken = make(0)
    lock store as held:
        taken = held[1]
    stdout.print(taken, taken == make(6), len(noise))
    return
"#;

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
    stdout.print(sorted(named, key=(s) => u64(len(s))), sorted(named, key=(s) => f32(len(s))))
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
    while at + 2 <= len(xs):
        total += xs[at] + xs[at + 1]
        at += 2
    return total

def after_a_guard(x0: int, b: int) -> int:
    mut x = x0
    if x < b:
        bump(ref x)
        return x + 1
    return 0

def down_after_a_guard(x0: int, b: int) -> int:
    mut x = x0
    if x > b:
        drop(ref x)
        return x - 1
    return 0

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
    let source = "def n() -> int:\n    return 7\n\ndef main(stdout: Stdout):\n    v = (n() % 5) + 1\n    mut d = float(v)\n    d = 0.0\n    stdout.print(1.0 / d)\n";
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
    return half + half

def large() -> f32:
    f: f32 = 16777216.0
    return f

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
        "def number(x: float) -> float:\n    return x\n\ndef main(stdout: Stdout):\n    stdout.print(number(1e17) % 3.0, number(5.5) % 0.1, number(-7.5) % 2.0, number(7.5) % -2.0)\n    stdout.print(number(6.0) % 3.0, number(1e300) % 7.0, number(0.3) % 0.1)\n",
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
    from_value = u8.from(200) except InvalidValue(): u8(0)
    parse_value = u8.parse(\"17\") except InvalidValue(): u8(0)
    from_value2 = i8.from(-5) except InvalidValue(): i8(0)
    parse_value2 = u32.parse(\"4294967295\") except InvalidValue(): u32(0)
    stdout.print(from_value, parse_value, from_value2, parse_value2)
    for text in [\"300\", \"-1\", \"seven\"]:
        n = u8.parse(text) except problem:
            stdout.print(\"failed:\", problem.message)
            continue
        stdout.print(\"fit\", n)
    small = u8.parse(\"250\") except InvalidValue(): u8(0)
    sized = i16.from(40000) except InvalidValue(): i16(-1)
    stdout.print(small + 5, sized)
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

/// `int.parse` reads a short int written plainly where its text is, and leaves everything else
/// to the runtime: so each of these reads as Rust's `trim().parse::<i64>()` reads it, on either
/// side of 18 digits, with a sign, with space around it, past the top, and in a block.
#[test]
fn an_int_read_from_text_is_read_the_same_however_its_written() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "int_read_plainly",
        "def main(stdout: Stdout):
    texts = [
        \"0\", \"-0\", \"7\", \"-7\", \"007\", \"123456789012345678\", \"-123456789012345678\",
        \"1234567890123456789\", \"-9223372036854775808\", \"9223372036854775807\",
        \"9223372036854775808\", \" 42 \", \"+5\", \"4_2\", \"\", \"-\", \"--1\", \"12a\", \"a12\",
        \"\u{661}\u{662}\", \"000000000000000000000000000042\", \"-00000000000000000000000000042\",
    ]
    for text in texts:
        n = int.parse(text) except problem:
            stdout.print(\"failed:\", problem.message)
            continue
        stdout.print(n)
",
        "0\n0\n7\n-7\n7\n123456789012345678\n-123456789012345678\n\
         1234567890123456789\n-9223372036854775808\n9223372036854775807\n\
         failed: \"9223372036854775808\" isn't an int\n42\n5\n\
         failed: \"4_2\" isn't an int\nfailed: \"\" isn't an int\nfailed: \"-\" isn't an int\n\
         failed: \"--1\" isn't an int\nfailed: \"12a\" isn't an int\nfailed: \"a12\" isn't an int\n\
         failed: \"\u{661}\u{662}\" isn't an int\n42\n-42\n",
    )
}

/// A `u64` too big, from text as long as it likes, is quoted by its first 20 digits and how many
/// it has, so a hostile number can't make a message as long as itself. One just past the top is
/// quoted whole, as before.
#[test]
fn a_u64_too_big_is_quoted_by_its_start_and_its_length() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "u64_too_big_quoted",
        r#"def said(run: () -> u64 fails InvalidValue) -> str:
    n = run() except why:
        return why.message
    return f"{n}"

def main(stdout: Stdout):
    digits = "1234567890" * 500
    stdout.print(said(() => u64.parse(digits)?))
    stdout.print(said(() => u64.parse("18446744073709551616")?))
"#,
    )?;
    assert_eq!(
        printed,
        "12345678901234567890… (5,000 digits) doesn't fit in u64, which holds 0 to 18446744073709551615\n\
         18446744073709551616 doesn't fit in u64, which holds 0 to 18446744073709551615\n"
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
    n = run() except why:
        return f\"[{why.message}]\"
    return f\"{n}\"

def main(stdout: Stdout):
    top = u64.parse(\"18446744073709551615\") except InvalidValue(): u64(0)
    half = u64.parse(\"9223372036854775808\") except InvalidValue(): u64(0)
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
fn numbers_calls_and_printing_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "numbers",
        "from std.math import sqrt\n\ndef fib(n: int) -> int:\n    if n < 2: return n\n    return fib(n - 1) + fib(n - 2)\n\ndef main(stdout: Stdout):\n    stdout.print(\"fib\", fib(20), sqrt(2.0), 1.5 > 1.0)\n    stdout.print(7 // -2, 7 % -2, -7 // 2, -7 % 2, -(3))\n    mut total = 0\n    for i in range(5):\n        total += i * i\n    stdout.print(total)\n",
    )
}

/// An int's text is written natively, digit pairs from the last: every length of digits, both
/// ends of `int`, signs, and appended to a growing string, in `str` and in an f-string.
#[test]
fn an_ints_text_of_every_length_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(out, len(out))
"#,
    )
}

/// A builtin's value nothing keeps is still made and let go of natively, each on a line of its own,
/// with what's printed after the same.
#[test]
fn a_value_nothing_keeps_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "discarded",
        r#"def doubled(xs: list[int]) -> list[int]:
    return [x * 2 for x in xs]

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
fn a_std_constant_reads_both_ways_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "std_constants",
        "from std import string\nfrom std.string import DIGITS, PUNCTUATION\n\ndef main(stdout: Stdout):\n    stdout.print(string.DIGITS, DIGITS, string.DIGITS == DIGITS)\n    stdout.print(len(string.ASCII_LETTERS), len(PUNCTUATION), len(string.WHITESPACE))\n    stdout.print([c for c in \"a1b2\" if string.DIGITS.find(c) is not None])\n",
    )
}

/// `list` of a capability, `round` to places, the codes of text, and clearing and taking keys out
/// of dicts and sets.
#[test]
fn more_small_calls_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "more_small_calls",
        r#"struct Key:
    x: int
    y: int

def main(args: Args, stdout: Stdout):
    stdout.print(len(list(args)))
    for arg in args:
        stdout.print(len(arg) > 0)
    stdout.print(round(3.14159, 2), round(2.5, 0), round(1234.5, -2), round(0.125, 2))
    stdout.print("héllo".codes(), "".codes(), "€".code())
    for code in [65, 233, 8364, -1, 55296, 1114112]:
        text = str.from_code(code) except error:
            stdout.print(error.message)
            continue
        stdout.print(text, len(text))
    mut numbers = {1: 2, 3: 4}
    shared = numbers
    numbers.clear()
    stdout.print(numbers, shared, len(numbers))
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
    stdout.print(ints, len(ints))
    mut words = {"a", "b", "a long string that is held in a block"}
    words.discard("a")
    words.discard("nope")
    words.remove("a long string that is held in a block")
    stdout.print(words)
    mut keys = {Key(x=1, y=2), Key(x=3, y=4)}
    keys.discard(Key(x=1, y=2))
    keys.discard(Key(x=9, y=9))
    keys.remove(Key(x=3, y=4))
    stdout.print(keys, len(keys))
"#,
    )
}

/// `set.remove` of a key that isn't there fails as pinned.
#[test]
fn removing_what_a_set_lacks_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, set, key) in [
        ("remove_int", "{1, 2}", "3"),
        ("remove_str", "{\"a\"}", "\"b\""),
        ("remove_long", "{\"a\"}", "\"a long string that is held in a block\""),
    ] {
        same_message_as_pinned(
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
fn sorting_by_tuple_keys_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "tuple_keys",
        r#"struct Unit:
    name: str
    hp: int

def main(stdout: Stdout):
    words = ["bramble", "kit", "ash", "a long word that sits in a block", "kit"]
    stdout.print(sorted(words, key=(word) => (len(word), word)))
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

/// `sort`, `reverse` and `swap` move a list's in-place items where they lie, each keeping what it
/// holds, and a dict's set writes a value held in place into its entry's own block: what each
/// prints, a list shared before it's turned round keeping its order, and the blocks made,
/// one copy for that shared list and none for a sort, are as pinned, with nothing left held.
#[test]
fn moving_in_place_items_about_counts_nothing_and_copies_only_what_is_shared() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("moved_in_place", MOVED_IN_PLACE, MOVED_IN_PLACE_PRINTS)?;
    assert_eq!(
        counts_of("moved_in_place_counts", MOVED_IN_PLACE)?,
        "copies 1 copied_bytes 672 largest_copy 672 allocations 114 allocated_bytes 11840 live 0"
    );
    Ok(())
}

const MOVED_IN_PLACE: &str = r#"enum Outcome:
    Good(value: int)
    Bad(reason: str)

struct Inner:
    label: str
    weights: list[int]

struct Item:
    id: int
    name: str
    inner: Inner
    state: Outcome

def make(n: int) -> Item:
    state = Outcome.Good(value=n) if n % 3 == 0 else Outcome.Bad(reason=f"a reason long enough for a block {n}")
    return Item(id=n, name=f"a name long enough for a block {n}", inner=Inner(label=f"a label long enough for a block {n}", weights=[n, n * 2]), state=state)

def main(stdout: Stdout):
    mut items = [make(n) for n in range(7)]
    noise = [make(n + 100) for n in range(3)]
    items.reverse()
    items.swap(0, -1)
    kept = items
    items.reverse()
    stdout.print([item.id for item in items], [item.id for item in kept])
    items.sort(key=item => item.name)
    stdout.print([item.id for item in items])
    items.sort(key=item => (item.id % 2, -item.id))
    stdout.print([item.id for item in items], items[0] == make(5), items[6])
    mut pairs = [(n % 3, f"a text long enough for a block {n}") for n in range(5)]
    pairs.sort()
    pairs.reverse()
    stdout.print(pairs[0], pairs[4])
    mut by_key = dict[int, Item]()
    for round in range(3):
        for n in range(3):
            mut item = make(n + round * 10)
            item.inner.weights.append(round)
            by_key[n] = item
    held = by_key[1]
    by_key[1] = make(77)
    stdout.print(held.inner, by_key[1].id, by_key[2].inner.weights)
    mut maybes: list[Inner?] = [Inner(label=f"a label long enough for a block {n}", weights=[n]) if n % 2 == 0 else None for n in range(5)]
    maybes.reverse()
    maybes.swap(0, 1)
    stdout.print(maybes[1], maybes[0], len(noise))
    return
"#;

const MOVED_IN_PLACE_PRINTS: &str = r#"[6, 1, 2, 3, 4, 5, 0] [0, 5, 4, 3, 2, 1, 6]
[0, 1, 2, 3, 4, 5, 6]
[6, 4, 2, 0, 5, 3, 1] False Item(id=1, name="a name long enough for a block 1", inner=Inner(label="a label long enough for a block 1", weights=[1, 2]), state=Outcome.Bad(reason="a reason long enough for a block 1"))
(2, "a text long enough for a block 2") (0, "a text long enough for a block 0")
Inner(label="a label long enough for a block 21", weights=[21, 42, 2]) 77 [22, 44, 2]
Inner(label="a label long enough for a block 4", weights=[4]) None 3
"#;

/// `sorted` of a type with its own `<` sorts by it: stable, so items `<` says are
/// neither before nor after keep their order, and the standard library's `Uuid` too.
#[test]
fn sorting_by_a_types_own_order_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "sorting_by_own_order",
        r#"from std.uuid import Uuid

struct Score:
    points: int
    name: str

    def <(self, other: Score) -> bool:
        return self.points > other.points

def main(stdout: Stdout):
    scores = [Score(points=3, name="ash"), Score(points=7, name="vex"), Score(points=3, name="kit"), Score(points=9, name="bo")]
    stdout.print([score.name for score in sorted(scores)])
    empty: list[Score] = []
    stdout.print(len(sorted(empty)), [score.name for score in sorted([Score(points=1, name="one")])])
    later = Uuid.parse("018f2a6e-0000-7000-8000-000000000002") except InvalidValue(): Uuid()
    first = Uuid.parse("018f2a6e-0000-7000-8000-000000000001") except InvalidValue(): Uuid()
    stdout.print(sorted([later, first]) == [first, later])
"#,
    )
}

/// Shifts, `~`, sorting a list where it is, a `match` on a bool and `random_choice`.
#[test]
fn bits_sorts_and_choices_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "bits_sorts_and_choices",
        r#"from std.random import Random

def name(flag: bool) -> str:
    match flag:
        case True:
            return "yes"
        case False:
            return "no"

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
    words.sort(key=word => len(word))
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
fn lists_keep_value_semantics_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "sharing",
        r"def changed(items: list[int]) -> list[int]:
    mut copy = items
    copy[0] = 99
    return copy

def grown(ref items: list[int]):
    items.append(7)
    items[0] = 5

def same(items: list[int]) -> list[int]:
    return items

def show(items: list[int], stdout: Stdout):
    mut at = 0
    while at < len(items):
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
        h.append(len(k))
        stdout.print(len(k), len(h))
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
    return [first, first + "!", first + "?"]

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
    stdout.print(words, copied, words == copied, "hi!" in words, "nope" not in words, len(words))
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
fn arrays_keep_value_semantics_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("arrays", ARRAYS)
}

/// An array too big to keep in place is held as a list, which nothing in the program can tell:
/// here one of ints, one of strings and one in a struct. One just small enough is in place.
#[test]
fn a_big_array_behaves_as_a_small_one() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
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
    stdout.print(sum(big), sum(copy), len(big), big[9998], copy[0], big == copy)
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
fn an_index_outside_an_array_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    for (name, at) in [("array_past_the_end", "3"), ("array_before_the_start", "-4")] {
        let source = format!(
            "def main(stdout: Stdout):\n    values = array[int, 3]()\n    mut at = {at}\n    stdout.print(values[at + 0])\n"
        );
        same_as_expected_in_both_builds(name, &source)?;
    }
    Ok(())
}

/// A failure inside the standard library's own code is said to be where the program called it,
/// not somewhere in the standard library, as pinned: here `swap`'s index.
#[test]
fn a_failure_inside_std_points_at_the_call_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "std_failure_at_the_call",
        "def shuffled(xs: list[int]) -> list[int]:\n    mut out = xs\n    out.swap(0, 7)\n    return out\n\ndef main(stdout: Stdout):\n    stdout.print(shuffled([1, 2, 3]))\n",
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
        return out

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
        return len(self.lines)

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
    return mine

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
    return True

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
    return read()

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
    same_as_expected_in_both_builds(
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
    same_as_expected_in_both_builds("two_cells", TWO_CELLS)
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
        let pinned_outcome = pinned_outcome_of(&program, &format!("taken_twice_{name}"))?;
        assert_eq!(apart_from(&dir, ran.clone()), pinned_outcome, "{name}");
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
    same_as_expected_in_both_builds("shared_map_walked", include_str!("fixtures/shared_map_walked.fen"))
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
    let (printed, ..) = pinned_outcome("shared_same_said", SHARED_SAME)?;
    let wanted = "True True False False True\nTrue False\nTrue False False True 1\nTrue False\nTrue False\nTrue False\n";
    assert_eq!(printed, wanted);
    same_as_expected_in_both_builds("shared_same", SHARED_SAME)?;
    // A type's own `def ==` still decides, wherever it's written.
    let own = "\
struct Account:
    total = 0

    def ==(self, other: Account) -> bool:
        return self.total > 100

def main(stdout: Stdout):
    shared a = Account(total=1)
    shared c = Account(total=200)
    stdout.print(a == a, a == c, c == a, c == c, a != c, c != a)
";
    same_as_expected_in_both_builds("shared_own_same", own)
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
    let pinned_outcome = pinned_outcome_of(&program, "shared_names")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
    Ok(())
}

/// `shared T` on a plain struct and a list, kept in a dict and a struct's fields, and reached only
/// in a `lock`: two locked together for a transfer, one locked to read. It builds natively.
#[test]
fn shared_values_reached_in_locks_build_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("shared_locks", SHARED_LOCKS)
}

/// A `lock ... as ref` block's changes are saved for a crash to put back, and kept when it ends
/// normally: an int, a string and a list changed, two values locked together, a `return` from
/// inside, and appends in a loop. What's printed after is as pinned.
#[test]
fn changes_in_a_lock_to_write_are_kept_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
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
    return True

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
        stdout.print(second.balance, second.owner, len(second.log))
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
    return True

def balance(account: shared Account) -> int:
    lock account as current:
        return current.balance

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
    return n

def closed(n: int) -> int fails Shut:
    if n > 5:
        fail Shut(why="big")
    return n

def only_refused() -> str fails Refused:
    return f"{capped(-3)?}"

def either(run: () -> str fails Refused | Shut) -> str:
    return run() except why:
        case Refused(code):
            f"refused {code}"
        case Shut(why=reason):
            f"shut {reason}"

def each(step: (int) -> int fails Refused | Shut, n: int) -> str:
    stepped = step(n) except why:
        case Refused(code):
            return f"refused {code}"
        case Shut(why=reason):
            return f"shut {reason}"
    return f"{stepped}"

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
    return n // 2

def twice(run: () -> int fails Bad) -> int fails Bad:
    return run()? + run()?

def main(stdout: Stdout) fails Bad:
    stdout.print(twice(() => half(8)? + 1)?)
    twice_value = twice(() => half(-2)?) except Bad(): 99
    stdout.print(twice_value)
"#,
        "10\n99\n",
    )
}

/// `except name:` gives the block what failed, and it still has to leave: by `return`, `continue`
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
    text = risky(flag) except problem:
        stdout.print(\"saw\", problem.reason)
        return \"left\"
    nothing(flag) except trouble:
        stdout.print(\"also\", trouble.reason)
        return \"unreached\"
    return text

def again(stdout: Stdout) fails Boom:
    for flag in [1, 0, 2]:
        made = risky(flag) except problem:
            stdout.print(\"skip\", problem.reason)
            continue
        stdout.print(made)
    made = risky(0) except problem:
        fail problem
    stdout.print(made)

def main(stdout: Stdout):
    stdout.print(run(0, stdout))
    stdout.print(run(1, stdout))
    again(stdout) except problem:
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
    lend name except:
        n = closed(name)?
        stdout.print("not here", n)

def used(stdout: Stdout) fails Shut | Other:
    with opened("door", stdout)? as door:
        fail Other(why="in the block")

def main(stdout: Stdout):
    used(stdout) except why:
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
    lend 7 except:
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
    lend 1 except:
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
    value = inner_returns(stdout) except problem:
        problem.at
    stdout.print(value)
    for run in [0, 1, 2, 3, 4, 6]:
        ran(stdout, run) except problem:
            stdout.print(\"failed\", problem.at, problem.notes())
            continue
        stdout.print(\"no failure\")
    mut kind = Kind.Button(clicks=1)
    put_back(ref kind) except problem:
        stdout.print(\"failed\", problem.at, problem.notes(), clicks_of(kind))
        return
    stdout.print(\"no failure\")
";

/// Every way out of a `with` block runs its source's cleanup, innermost first when several are left
/// at once: the code after `lend` for a success-style way out (`return`, `break`, `continue`, an
/// `else` doing one of those, a `return` in a nested `if` or a `match`), and `lend x except:` for a
/// failure-style one (`fail`, `?` on a line, kept, or in an expression, an `else` that fails, a
/// A `?` failing inside an expression inside a `with` block runs the source's cleanup wherever it
/// sits: inside another `?`'s call, in a `return`, an assignment or a line of its own, a field read
/// off it, an index, an f-string, a list item, a slice's bounds, a match's subject, a conditional's
/// test, and `f()? else fail ...` on a `T? fails E` call. With nothing failing, each gives what it
/// Arguments and operands run left to right whatever `?`s are among them: what's written to a
/// `?`'s left runs before it and what's to its right after it, with the `?` failing and passing,
/// nested and outermost, in a call, an operator, an f-string and a list, inside a `with` block and
/// outside one. A failure in the block runs its cleanup after what ran to its left, and nothing to
/// its right runs. A name read to a `?`'s left is read before the `?`'s call changes it through
/// `ref`.
#[test]
fn arguments_run_left_to_right_around_a_question_mark() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "question_order",
        include_str!("fixtures/question_order.fen"),
        include_str!("fixtures/question_order.out"),
    )
}

/// would without the block.
#[test]
fn a_question_mark_inside_an_expression_in_a_with_block_runs_its_cleanup() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "with_inner_question_marks",
        include_str!("fixtures/with_inner_question_marks.fen"),
        include_str!("fixtures/with_inner_question_marks.out"),
    )
}

/// crash). The ways out from inside an `else` once ran neither, so a file or a channel stayed open.
#[test]
fn every_way_out_of_a_with_block_runs_its_cleanup() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("with_every_way_out")?;
    let program = dir.join("ways.fen");
    std::fs::write(&program, include_str!("fixtures/with_every_way_out.fen"))?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("ways"));
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
        let err = String::from_utf8_lossy(&ran.stderr);
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "  ended else return\n-1\n  ended else return in a line\n-2\n  failed else fail\n-10\n  failed else as\n-11\n  ended continue 0\n  ended continue 1\n  ended continue 2\n  ended break 0\n  ended break 1\n  ended break 2\n1\n  failed ? on a line\n-12\n  failed ? kept\n-13\n  failed ? in an expression\n-14\n  ended return in an if\n7\n  ended return in a match\n8\n  failed fail in an if\n-15\n  ended inner, else return\n  ended outer, else return\n-3\n  failed inner, else fail\n  failed outer, else fail\n-16\n  failed inner, ?\n  failed outer, ?\n-17\n  ended inner 0, else break\n  ended outer 0, else break\n  ended inner 1, else break\n  ended outer 1, else break\n  ended inner 2, else break\n  ended outer 2, else break\n2\n  failed inner, crash\n  failed outer, crash\n",
            "{how:?}: {err}"
        );
        // The last way out is a crash, through both its blocks' `lend x except:`.
        assert!(!ran.status.success(), "{how:?}");
        assert!(err.contains("index 2 is out of range for length 1"), "{how:?}: {err}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A channel whose block ends by an `else return` or an `else fail` is closed and freed, so a
/// launched task still holding a Sender for it is told `Closed` when it sends, rather than its
/// message going nowhere.
#[test]
fn a_send_to_a_channel_whose_block_left_from_an_else_is_closed() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "channel_dropped_by_an_else",
        include_str!("fixtures/channel_dropped_by_an_else.fen"),
        "gave up -2\nfailed out\nclosed\nclosed\n",
    )
}

/// The same when the block ends by a `return` from the one case of `except Timeout() | Closed():`,
/// or from a `case` arm under `except why:`: the channel is closed, as the block's way out runs
/// its cleanup from there too.
#[test]
fn a_send_to_a_channel_whose_block_left_from_an_except_case_is_closed() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for name in ["channel_dropped_by_an_except_case", "channel_dropped_by_an_except_arm"] {
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}.fen")),
        )?;
        prints_in_both_builds(name, &source, "gave up -2\nfailed out\nclosed\nclosed\n")?;
    }
    Ok(())
}

/// A crash in a `with` block runs its sources' `lend x except:` cleanups on the way out, innermost
/// first, and only those whose blocks haven't ended. A cleanup's own calls run as normal while the
/// crash waits, and a cleanup that crashes too is said, with the first crash the one that ends
/// the program.
#[test]
fn a_crash_runs_the_cleanups_of_the_blocks_it_leaves() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def lent(stdout: Stdout, name: str) -> with int:
    lend 1 except:
        stdout.print(f"cleanup {name}")
    stdout.print(f"done {name}")

def breaks(stdout: Stdout) -> with int:
    lend 0 except:
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

/// A crash runs a cleanup with a `?` in it, wherever the `?` is, and stops it where one fails: a
/// line of its own, a value kept, one deeper in a line, and a `fail` in a loop each end it, with the
/// rest skipped, and its failure said under the crash as a note, while a `?` in a function written
/// there passes up from that function. A cleanup with any of these once didn't run at all.
#[test]
fn a_crash_runs_a_cleanup_with_a_question_mark_until_one_fails() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("crash_cleanup_question_marks")?;
    let program = dir.join("cleanup.fen");
    std::fs::write(&program, include_str!("fixtures/crash_cleanup_question_marks.fen"))?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("cleanup"));
    let output = output.to_string_lossy().into_owned();
    // What each crash's cleanup prints, and the failure that ended it, if one did.
    let wanted = [
        ("1", "cleanup 1\nkept 1\ndeeper 1\n", Some("Refused(code=1)")),
        ("3", "cleanup 3\nkept 3\ndeeper 3\ninner 3\nended 3\n", None),
        ("4", "cleanup 4\nkept 4\ndeeper 4\ninner 4\n", Some("Refused(code=4)")),
        ("7", "cleanup 7\nkept 7\ndeeper 7\ninner -1\n", Some("Refused(code=7)")),
        ("12", "cleanup 12\nkept 12\n", Some("Refused(code=12)")),
        ("25", "cleanup 25\n", Some("Refused(code=25)")),
    ];
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        for (code, out, failed) in wanted {
            let ran = Command::new(&output).arg(code).output_within()?;
            let err = String::from_utf8_lossy(&ran.stderr);
            assert_eq!(String::from_utf8_lossy(&ran.stdout), out, "{how:?} {code}: {err}");
            assert!(!ran.status.success(), "{how:?} {code}");
            assert!(err.contains(&format!("index {code} is out of range for length 1")), "{err}");
            // A failure is a note under the crash's message, on the line after it.
            let crashed = format!("error: index {code} is out of range for length 1\n");
            let noted = failed.map(|failure| format!("{crashed}note: the cleanup failed: {failure}\n"));
            assert!(err.contains(noted.as_deref().unwrap_or(&crashed)), "{how:?} {code}: {err}");
            assert_eq!(err.matches("note: ").count(), usize::from(failed.is_some()), "{how:?} {code}: {err}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A crash's cleanup whose own failure passes from an `else as` block or through `? from`, with a
/// function written inside a case of a `match` whose `?` stays its own: each ends the cleanup where
/// it fails, noted under the crash, and one that crashes too has nothing noted. A `fail` in such a
/// block, or a function's `?` taken as the cleanup's, once made the cleanup's rewrite an internal
/// error.
#[test]
fn a_crash_s_cleanup_ends_where_an_else_as_block_or_a_question_mark_from_fails() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("crash_cleanup_shapes")?;
    let program = dir.join("shapes.fen");
    std::fs::write(&program, include_str!("fixtures/crash_cleanup_shapes.fen"))?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("shapes"));
    let output = output.to_string_lossy().into_owned();
    // What each crash's cleanup prints, and the note under the crash, if any.
    let wanted = [
        ("25", "cleanup 25\n", Some("Wrapped(why=\"Refused(code=25)\")")),
        ("6", "cleanup 6\nkept 6\ninner -1\nended 6\n", None),
        ("12", "cleanup 12\nkept 12\ninner -1\n", Some("Wrapped(why=\"too big\")")),
        ("4", "cleanup 4\nkept 4\nother\nended 4\n", None),
        ("7", "cleanup 7\n", None),
    ];
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        for (code, out, failed) in wanted {
            let ran = Command::new(&output).arg(code).output_within()?;
            let err = String::from_utf8_lossy(&ran.stderr);
            assert_eq!(String::from_utf8_lossy(&ran.stdout), out, "{how:?} {code}: {err}");
            assert!(!ran.status.success(), "{how:?} {code}");
            let crashed = format!("error: index {code} is out of range for length 1\n");
            let noted = failed.map(|failure| format!("{crashed}note: the cleanup failed: {failure}\n"));
            assert!(err.contains(noted.as_deref().unwrap_or(&crashed)), "{how:?} {code}: {err}");
            assert_eq!(err.matches("note: ").count(), usize::from(failed.is_some()), "{how:?} {code}: {err}");
            // A cleanup that crashes too gives back nothing to note, and its crash is said apart.
            let again = "index 7 is out of range for length 2 (while cleaning up after a crash)";
            assert_eq!(err.contains(again), code == "7", "{how:?} {code}: {err}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Not built yet: a crash's cleanup with a `?` inside a comprehension, which can't end the cleanup
/// from where it is, doesn't run at all, as before any `?` could be in one. The program builds and
/// crashes as it would with no cleanup. Turning the comprehension into a loop would let it run.
#[test]
fn a_crash_s_cleanup_with_a_question_mark_in_a_comprehension_is_left_out() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "error Refused:\n    code: int\n\ndef capped(n: int, limit: int) -> int fails Refused:\n    if n > limit:\n        fail Refused(code=n)\n    return n\n\ndef guarded(code: int, stdout: Stdout) -> with int fails Refused:\n    lend code except:\n        stdout.print(\"cleanup\", code)\n        kept = [capped(step, limit=code)? for step in range(3)]\n        stdout.print(\"kept\", len(kept))\n    stdout.print(\"released\", code)\n\ndef crashes(code: int, stdout: Stdout) fails Refused:\n    with guarded(code, stdout)? as held:\n        values = [1]\n        stdout.print(values[held])\n\ndef main(stdout: Stdout):\n    crashes(4, stdout) except Refused():\n        stdout.print(\"failed\")\n";
    let dir = folder("crash_cleanup_comprehension")?;
    let program = dir.join("comprehension.fen");
    std::fs::write(&program, source)?;
    let output = dir.join("comprehension").to_string_lossy().into_owned();
    let built = fennec(&["build", &program.to_string_lossy(), "-o", &output], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&output).output_within()?;
    let err = String::from_utf8_lossy(&ran.stderr);
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "", "{err}");
    assert!(err.contains("index 4 is out of range for length 1"), "{err}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A launched task that crashes inside a `with` runs its source's cleanup, which a failing `x = f()?`
/// or a `fail` in a loop ends, with its failure said under the task's crash, and the task's crash
/// still comes out as `Crashed`.
#[test]
fn a_crashing_task_s_cleanup_ends_where_it_fails_and_says_so() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("crash_cleanup_in_a_task")?;
    let program = dir.join("task.fen");
    std::fs::write(&program, include_str!("fixtures/crash_cleanup_in_a_task.fen"))?;
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("task"));
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
        let err = String::from_utf8_lossy(&ran.stderr);
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "crashed, cleanup 9\ncrashed, cleanup 2\ncrashed, cleanup 4, ended 4\n",
            "{how:?}: {err}"
        );
        let notes: Vec<&str> = err.lines().filter(|line| line.starts_with("note: ")).collect();
        assert_eq!(
            notes,
            ["note: the cleanup failed: Refused(code=9)", "note: the cleanup failed: Refused(code=2)"],
            "{how:?}: {err}"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A task that crashes inside a `with` runs the block's `lend x except:` before its crash ends it,
/// and a task that doesn't runs the rest of the source instead.
#[test]
fn a_crashing_task_runs_its_blocks_cleanup() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "task_crash_cleanup",
        r#"from std.concurrent import Crashed, Sender, Timeout, channel

## Says `text` on `done`, whether or not anything's there to hear it.
def tell(done: Sender[str], text: str):
    done.send(text) except Closed():
        pass

def lent(done: Sender[str]) -> with int:
    lend 7 except:
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
    _ = launched(done, at=at) except Crashed():
        return "crashed"
    return "ended"

def launched(done: Sender[str], at: int) fails Crashed:
    with launch(() => job(done, at)) as task:
        pass

def main(stdout: Stdout):
    with channel[str](size=4) as said:
        stdout.print(ran(said.sender(), at=1), said.receive() else "none")
        ended = ran(said.sender(), at=9)
        late = said.receive_within(1s) except Timeout(): None
        stdout.print(ended, late else "none")
"#,
        "ended finished\ncrashed crashed\n",
    )
}

/// A struct holding a text, sent to a task and read there through its inbox's loop, then sent back
/// as it is and inside an f-string, comes back whole at every length around the most a header
/// holds in itself, on one thread and on many. The inbox's generator once kept each `Event` in its
/// item slot without an owner of its text, so a text in a block of its own was freed under it.
#[test]
fn a_received_struct_s_text_sent_on_comes_back_whole_at_every_length() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("received_text_sent_on")?;
    let program = dir.join("sent_on.fen");
    std::fs::write(
        &program,
        r#"from std.concurrent import Closed, Crashed, Inbox, Sender, channel

struct Event:
    user: int
    text: str

def reader(inbox: Inbox[Event], out: Sender[str]) fails Closed:
    for event in inbox:
        out.send(event.text)?
        out.send(f"got {event.text}")?
    return

def text_of(length: int, n: int) -> str:
    return "".join(["x" for _ in range(length - 1)]) + str(n)

def main(stdout: Stdout) fails Closed | Crashed:
    for length in [20, 23, 24, 28, 31, 32, 40, 64]:
        mut whole = 0
        with channel[str](size=16) as results:
            back = results.sender()
            with launch((inbox) => reader(inbox, back)?, inbox_size=4)? as to_reader:
                for n in range(5):
                    to_reader.send(Event(user=n, text=text_of(length, n=n)))?
            for n in range(5):
                sent = text_of(length, n=n)
                if (results.receive() else "") == sent:
                    whole += 1
                if (results.receive() else "") == f"got {sent}":
                    whole += 1
        stdout.print(length, whole)
    return
"#,
    )?;
    let wanted = "20 10\n23 10\n24 10\n28 10\n31 10\n32 10\n40 10\n64 10\n";
    prints_with_nothing_left(&dir, &program, wanted, &["1", "4", "16"])
}

/// Builds `program` in both builds and runs it at each count of `threads` under `FENNEC_COUNTS`:
/// it prints `wanted`, and has no block left live at the end, so nothing it holds was let go of
/// too soon or kept too long.
fn prints_with_nothing_left(
    dir: &Path,
    program: &Path,
    wanted: &str,
    threads: &[&str],
) -> std::io::Result<()> {
    let output = program.with_extension("");
    let (program, output) = (program.to_string_lossy().into_owned(), output.to_string_lossy().into_owned());
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        for count in threads {
            let ran = Command::new(&output)
                .env("FENNEC_THREADS", count)
                .env("FENNEC_COUNTS", "1")
                .output_within()?;
            let said = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
            assert_eq!(said.0, wanted, "{how:?} on {count} threads: {}", said.1);
            assert!(said.1.contains(" live 0"), "{how:?} on {count} threads left blocks live: {}", said.1);
        }
    }
    std::fs::remove_dir_all(dir)
}

/// A message a task reads with `for ... in inbox` stays its own while it makes other values: a
/// struct's text and an enum case's text and list, checked with `==` after fifty other texts are
/// made, where a text freed under it read as one of those. Fable's thread-crossing reproducer, and
/// its three-case enum, which came out wrong silently.
#[test]
fn a_message_read_in_an_inbox_loop_stays_whole_while_the_task_makes_others() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("message_stays_whole")?;
    let program = dir.join("message_stays_whole.fen");
    std::fs::write(
        &program,
        r#"from std.concurrent import Closed, Crashed, Inbox, Sender, channel

struct Event:
    user: int
    text: str

enum Message:
    Said(text: str)
    Count(n: int)
    Many(texts: list[str])

def long(n: int) -> str:
    return "xxxxxxxxxxxxxxxxxxxxxxxxxx" + str(n % 9) + "y"

def filler() -> int:
    mut made = list[str]()
    for at in range(50):
        made.append("zzzzzzzzzzzzzzzzzzzzzzzzzz" + str(at % 9) + "!")
    return len(made)

def shown(message: Message) -> str:
    match message:
        case .Said(text):
            return f"said {text}"
        case .Count(n):
            return f"count {n}"
        case .Many(texts):
            joined = "".join(texts)
            return f"many {joined}"

def events(inbox: Inbox[Event], out: Sender[str]) fails Closed:
    for event in inbox:
        made = filler()
        out.send(f"{event.text}|{made}")?
    return

def messages(inbox: Inbox[Message], out: Sender[str]) fails Closed:
    for message in inbox:
        made = filler()
        out.send(f"{shown(message)}|{made}")?
    return

def main(stdout: Stdout) fails Closed | Crashed:
    mut wrong = 0
    with channel[str](size=4) as results:
        back = results.sender()
        with launch((inbox) => events(inbox, out=back)?, inbox_size=4)? as to_reader:
            for n in range(6):
                to_reader.send(Event(user=n, text=long(n)))?
                if (results.receive() else "nothing") != f"{long(n)}|50":
                    wrong += 1
        with launch((inbox) => messages(inbox, out=back)?, inbox_size=4)? as to_reader:
            for n in range(60):
                message = Message.Said(text=long(n)) if n % 3 == 0 else (Message.Count(n=n) if n % 3 == 1 else Message.Many(texts=[long(n), long(n + 1)]))
                to_reader.send(message)?
                if (results.receive() else "nothing") != f"{shown(message)}|50":
                    wrong += 1
    stdout.print("wrong", wrong)
    return
"#,
    )?;
    prints_with_nothing_left(&dir, &program, "wrong 0\n", &["1", "4"])
}

/// A generator's item and what it keeps across a `yield` are its own: a struct holding a text in a
/// block of its own, yielded, and another kept from one item to the next, both read whole after.
#[test]
fn a_generator_owns_the_struct_it_yields_and_the_one_it_keeps() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("generator_owns_structs")?;
    let program = dir.join("generator_owns_structs.fen");
    std::fs::write(
        &program,
        r#"struct Note:
    at: int
    text: str

def long_text(n: int) -> str:
    return f"a note long enough to need a block of its own, number {n}"

def notes(count: int) -> Iterator[Note]:
    mut kept = Note(at=-1, text=long_text(-1))
    for at in range(count):
        yield Note(at=at, text=long_text(at))
        yield kept
        kept = Note(at=at, text=long_text(at * 10))
    return

def main(stdout: Stdout):
    mut whole = 0
    mut seen = 0
    for note in notes(6):
        seen += 1
        if note.text == long_text(note.at) or note.text == long_text(note.at * 10):
            whole += 1
    stdout.print(seen, whole)
    return
"#,
    )?;
    prints_with_nothing_left(&dir, &program, "12 12\n", &["1"])
}

/// A struct field that starts as a module constant, pushed to and popped through `ref self` methods,
/// gives back what was pushed in a launched task as it does on the main thread, after the main
/// thread has read the constant over and over. Each read there once took the constant's count down,
/// below the mark the first launch shares never-freed blocks by, so the task's push was lost.
#[test]
fn a_constant_field_pushed_to_in_a_task_keeps_what_was_pushed() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "constant_field_in_task",
        r#"from std.concurrent import Crashed

error Oops:
    why: str

NOTHING = list[(str, int)]()

struct Holder:
    items = NOTHING

    def push(ref self, n: int):
        self.items.append(("x", n))

    def top(ref self) -> int fails Oops:
        last = self.items.pop() else fail Oops(why="empty")
        return last[1]

def whole() -> int fails Oops:
    mut holder = Holder()
    holder.push(7)
    return holder.top()? * 10 + len(NOTHING)

def reads() -> int:
    mut total = 0
    for _ in range(50):
        total += len(NOTHING)
    return total

## Called only on the main thread, so its code counts the constant there.
def main_reads() -> int:
    return len(NOTHING) + len(NOTHING) + len(NOTHING)

def in_task(stdout: Stdout) fails Oops:
    stdout.print("task:", whole()?, reads())

def main(stdout: Stdout) fails Oops | Crashed:
    # Kept to say after the task, which prints first.
    before = reads() + main_reads()
    first = whole()?
    with launch(() => in_task(stdout)?)?:
        pass
    stdout.print("main:", before, first, "after:", whole()?, len(NOTHING))
"#,
        "task: 70 0\nmain: 0 70 after: 70 0\n",
    )
}

/// The coverage lane's repro: a struct whose fields default to a list, a dict and a list of
/// pairs that are module constants, changed on the main thread, in `parallel` and in a task.
const CONSTANT_DEFAULTS: &str = r#"from std.concurrent import Crashed

struct Point:
    x: int
    y: int

    def moved(ref self, by: int):
        self.x += by

NUMBERS3 = [12, 13]
COUNTS3 = {"a": 12}
NOTHING3 = list[(str, int)]()
SPOT3 = Point(x=12, y=1)

struct Bin3:
    at = 0
    items = NOTHING3
    counts = COUNTS3
    numbers = NUMBERS3

    def pushed(ref self, n: int):
        self.items.append(("x", n))
        self.counts["n"] = n
        self.numbers.append(n)

    def top(ref self) -> int:
        last = self.items.pop() else return -1
        return last[1]

def used3(n: int) -> str:
    mut bin = Bin3()
    bin.pushed(n)
    top = bin.top()
    mut mine = NUMBERS3
    mine.append(n)
    mine[0] = n
    mut spot = SPOT3
    spot.moved(n)
    mut counts = COUNTS3
    counts["m"] = n
    return f"{top} {len(bin.items)} {bin.counts} {bin.numbers} {mine} {spot} {counts} {len(NOTHING3)} {NUMBERS3} {COUNTS3} {SPOT3}"

struct Said3:
    texts: list[str]

def tasked3(said: shared Said3):
    text = used3(2)
    lock said as ref held:
        held.texts.append(text)
    return

def constants3(stdout: Stdout) fails Crashed:
    shared said = Said3(texts=list[str]())
    stdout.print(len(NUMBERS3), len(COUNTS3), len(NOTHING3), SPOT3.x)
    stdout.print("main", used3(1))
    for line in [used3(i) for i in parallel(range(3))]:
        stdout.print("parallel", line)
    with launch(() => tasked3(said))?:
        pass
    lock said as held:
        for text in held.texts:
            stdout.print("task", text)
    stdout.print("after", used3(3), NUMBERS3, COUNTS3, len(NOTHING3))
    return

def main(stdout: Stdout) fails Crashed:
    constants3(stdout)?
    return
"#;

/// What [`CONSTANT_DEFAULTS`] prints.
const CONSTANT_DEFAULTS_PRINTED: &str = r#"2 1 0 12
main 1 0 {"a": 12, "n": 1} [12, 13, 1] [1, 13, 1] Point(x=13, y=1) {"a": 12, "m": 1} 0 [12, 13] {"a": 12} Point(x=12, y=1)
parallel 0 0 {"a": 12, "n": 0} [12, 13, 0] [0, 13, 0] Point(x=12, y=1) {"a": 12, "m": 0} 0 [12, 13] {"a": 12} Point(x=12, y=1)
parallel 1 0 {"a": 12, "n": 1} [12, 13, 1] [1, 13, 1] Point(x=13, y=1) {"a": 12, "m": 1} 0 [12, 13] {"a": 12} Point(x=12, y=1)
parallel 2 0 {"a": 12, "n": 2} [12, 13, 2] [2, 13, 2] Point(x=14, y=1) {"a": 12, "m": 2} 0 [12, 13] {"a": 12} Point(x=12, y=1)
task 2 0 {"a": 12, "n": 2} [12, 13, 2] [2, 13, 2] Point(x=14, y=1) {"a": 12, "m": 2} 0 [12, 13] {"a": 12} Point(x=12, y=1)
after 3 0 {"a": 12, "n": 3} [12, 13, 3] [3, 13, 3] Point(x=15, y=1) {"a": 12, "m": 3} 0 [12, 13] {"a": 12} Point(x=12, y=1) [12, 13] {"a": 12} 0
"#;

/// The same for a dict of lists, a text too long for a header, and a nested struct's list.
const DEFAULTS_OF_EVERY_KIND: &str = r#"from std.concurrent import Crashed

NAMES = {"a": [1]}
GREETING = "a text longer than any header holds"
EMPTY = list[int]()

struct Inner:
    seen = EMPTY

struct Shelf:
    names = NAMES
    text = GREETING
    inner = Inner()

    def filled(ref self, n: int):
        self.names["b"] = [n]
        self.names["a"].append(n)
        self.text += "!"
        self.inner.seen.append(n)

def used(n: int) -> str:
    mut shelf = Shelf()
    shelf.filled(n)
    mut again = Shelf()
    again.inner.seen.append(n + 10)
    return f"{shelf.names} {shelf.text} {shelf.inner.seen} {again.inner.seen} {NAMES} {GREETING} {EMPTY}"

struct Said:
    texts: list[str]

def tasked(said: shared Said):
    text = used(2)
    lock said as ref held:
        held.texts.append(text)
    return

def main(stdout: Stdout) fails Crashed:
    shared said = Said(texts=list[str]())
    stdout.print("main", used(1))
    for line in [used(i) for i in parallel(range(3))]:
        stdout.print("parallel", line)
    with launch(() => tasked(said))?:
        pass
    lock said as held:
        for text in held.texts:
            stdout.print("task", text)
    stdout.print("after", used(3), NAMES, GREETING, EMPTY)
    return
"#;

/// What [`DEFAULTS_OF_EVERY_KIND`] prints.
const DEFAULTS_OF_EVERY_KIND_PRINTED: &str = r#"main {"a": [1, 1], "b": [1]} a text longer than any header holds! [1] [11] {"a": [1]} a text longer than any header holds []
parallel {"a": [1, 0], "b": [0]} a text longer than any header holds! [0] [10] {"a": [1]} a text longer than any header holds []
parallel {"a": [1, 1], "b": [1]} a text longer than any header holds! [1] [11] {"a": [1]} a text longer than any header holds []
parallel {"a": [1, 2], "b": [2]} a text longer than any header holds! [2] [12] {"a": [1]} a text longer than any header holds []
task {"a": [1, 2], "b": [2]} a text longer than any header holds! [2] [12] {"a": [1]} a text longer than any header holds []
after {"a": [1, 3], "b": [3]} a text longer than any header holds! [3] [13] {"a": [1]} a text longer than any header holds [] {"a": [1]} a text longer than any header holds []
"#;

/// A struct field that defaults to a module constant holds that constant's block, which is never
/// freed and is shared by every reader, so it's never taken as made alone: changing the field
/// copies it. A list, a dict, a long text and a nested struct's list, on the main thread, in
/// `parallel` and in a launched task, in both builds, checking proofs and not. The constants
/// stay as written. The coverage lane's fuzz found a false proof here, before #1531.
#[test]
fn a_field_defaulting_to_a_constant_is_shared_however_it_is_changed() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let programs = [
        (CONSTANT_DEFAULTS, CONSTANT_DEFAULTS_PRINTED),
        (DEFAULTS_OF_EVERY_KIND, DEFAULTS_OF_EVERY_KIND_PRINTED),
    ];
    for (source, wanted) in programs {
        for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
            for proofs in ["0", "1"] {
                let dir = folder("constant_defaults")?;
                let (program, executable) = (dir.join("defaults.fen"), dir.join("defaults"));
                std::fs::write(&program, source)?;
                let built = crate::executables::fennec_command()
                    .arg("build")
                    .args(how)
                    .arg(&program)
                    .arg("-o")
                    .arg(&executable)
                    .env("FENNEC_HOME", &dir)
                    .env("FENNEC_CHECK_PROOFS", proofs)
                    .output_before(FENNEC_DEADLINE)?;
                assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
                let ran = Command::new(&executable).output_within()?;
                std::fs::remove_dir_all(&dir)?;
                assert_eq!(
                    String::from_utf8_lossy(&ran.stdout),
                    wanted,
                    "{how:?} proofs {proofs}: {}",
                    String::from_utf8_lossy(&ran.stderr)
                );
            }
        }
    }
    Ok(())
}

/// What the main thread printed before a launch comes out before what the task prints, on its own
/// output, as it was printed first: the main thread's kept output is written as the task starts.
#[test]
fn what_was_printed_before_a_launch_comes_out_before_the_task_s() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "printed_before_launch",
        r#"from std.concurrent import Crashed

def tasked(stdout: Stdout):
    stdout.print("task")
    return

def main(stdout: Stdout) fails Crashed:
    stdout.print("before")
    with launch(() => tasked(stdout))?:
        pass
    stdout.print("after")
    return
"#,
        "before\ntask\nafter\n",
    )
}

/// A task that crashes while the code that launched it waits on a channel inside the `with launch`
/// block ends that wait, so the block ends and says `Crashed`, rather than leaving its launcher
/// waiting for good: a `receive` gives nothing, as from a closed channel, and a `send` fails with
/// `Closed`. On the main thread, and in a task that launched one, on one thread and on many.
#[test]
fn a_task_s_crash_ends_its_launcher_s_wait_on_a_channel() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("crash_ends_launcher_wait")?;
    let program = dir.join("crash_ends_wait.fen");
    std::fs::write(
        &program,
        r#"from std.concurrent import Channel, Closed, Crashed, Inbox, Sender, channel

# Crashes before it sends its answer.
def worker(out: Sender[int], n: int) fails Closed:
    short = [1, 2]
    out.send(short[n])?
    return

def waited(back: Sender[int], results: Channel[int]) -> int fails Closed | Crashed:
    mut got = 0
    with launch(() => worker(back, n=5)?)?:
        got = results.receive() else -1
    return got

def receives() -> str:
    with channel[int](size=1) as results:
        got = waited(results.sender(), results) except Crashed() | Closed():
            return "receive: crashed"
        return f"receive: {got}"

# Crashes at its first message, leaving its inbox full.
def reader(inbox: Inbox[int]) fails Closed:
    short = [1, 2]
    for n in inbox:
        _ = short[n + 5]
    return

def sent_until_closed() -> int fails Closed | Crashed:
    mut sent = 0
    with launch((inbox) => reader(inbox)?, inbox_size=1)? as to_reader:
        for n in range(100):
            to_reader.send(n) except Closed():
                break
            sent += 1
    return sent

def sends() -> str:
    _ = sent_until_closed() except Crashed() | Closed():
        return "send: crashed"
    return "send: ended"

# A task that launches the worker, and waits on it in its turn.
def middle(out: Sender[str]) fails Closed:
    out.send(receives())?
    return

def main(stdout: Stdout) fails Closed | Crashed:
    stdout.print(receives())
    stdout.print(sends())
    with channel[str](size=1) as said:
        with launch(() => middle(said.sender())?)?:
            stdout.print("in a task:", said.receive() else "nothing")
    return
"#,
    )?;
    let wanted = "receive: crashed\nsend: crashed\nin a task: receive: crashed\n";
    let (program, output) = (program.to_string_lossy().into_owned(), dir.join("crash_ends_wait"));
    let output = output.to_string_lossy().into_owned();
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.as_str(), "-o", &output])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        for threads in ["1", "4", "16"] {
            let ran = Command::new(&output).env("FENNEC_THREADS", threads).output_within()?;
            let said = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
            assert_eq!(said.0, wanted, "{how:?} on {threads} threads: {}", said.1);
            assert!(ran.status.success(), "{how:?} on {threads} threads: {}", said.1);
        }
    }
    std::fs::remove_dir_all(&dir)
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
        return Mat2(
            a=self.a * other.a + self.b * other.c,
            b=self.a * other.b + self.b * other.d,
            c=self.c * other.a + self.d * other.c,
            d=self.c * other.b + self.d * other.d,
        )

extend Mat2:
    def *(self, other: Vec2) -> Vec2:
        return Vec2(x=self.a * other.x + self.b * other.y, y=self.c * other.x + self.d * other.y)

    def *(self, other: float) -> Mat2:
        return Mat2(a=self.a * other, b=self.b * other, c=self.c * other, d=self.d * other)

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
    got_value = json.loads[dict[Reached, str]](text) except ParseError(): {}
    stdout.print((got_value) == spots)
    got_value2 = pack.loads[dict[Reached, str]](pack.dumps(spots)) except ParseError(): {}
    stdout.print((got_value2) == spots)
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
    got_value = json.loads[Player](text, names=JsonNames.CamelCase) except ParseError(): Player(display_name="b")
    stdout.print(got_value)
"#,
        "{\"displayName\": \"a\"}\nPlayer(display_name=\"a\")\n",
    )
}

/// `json.loads` finds which field a name is by comparing the name's bytes where they are in the
/// text, several at a time. Names that begin one another, names of each size the compare takes a
/// different way (1, 2 and 3 bytes, 8, 23, and past a header's room), a name one byte off, a
/// longer one, one with an escape and one that isn't ASCII each match or miss as they should, in
/// any order. A text that ends inside a name fails as a string that never ends, with nothing
/// read past it. An enum's cases, found the same way, do too.
#[test]
fn json_matches_a_fields_name_by_its_bytes_whatever_its_size_and_wherever_the_text_ends()
-> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("json_names_by_bytes", NAMES_BY_BYTES, NAMES_BY_BYTES_PRINTED)
}

/// The program of [`json_matches_a_fields_name_by_its_bytes_whatever_its_size_and_wherever_the_text_ends`].
const NAMES_BY_BYTES: &str = r#"from std import json

struct Near:
    a: int
    ab: int
    abc: int
    abd: int

struct Wide:
    eight_by: int
    a_name_of_twenty_three_: int
    a_name_longer_than_a_header_holds: int
    a_name_longer_than_a_header_holdt: int
    grosse: int

enum Mode:
    On
    Once
    Onced
    Off

struct Switch:
    mode: Mode
    n: int

def near(stdout: Stdout, text: str):
    got_value = json.loads[Near](text) except why:
        stdout.print(f"no: {why.message}")
        return
    stdout.print(got_value)
    return

def wide(stdout: Stdout, text: str):
    got_value = json.loads[Wide](text) except why:
        stdout.print(f"no: {why.message}")
        return
    stdout.print(got_value)
    return

def switch(stdout: Stdout, text: str):
    got_value = json.loads[Switch](text) except why:
        stdout.print(f"no: {why.message}")
        return
    stdout.print(got_value)
    return

def main(stdout: Stdout):
    # Names that begin one another, in order, out of order, and with space before the colon.
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"abd\": 4}")
    near(stdout, "{\"abd\": 4, \"abc\": 3, \"ab\": 2, \"a\": 1}")
    near(stdout, "{\"abc\" : 3, \"a\"\t: 1, \"abd\"\n: 4, \"ab\" :2}")
    # A name that's one byte off, one that's longer, and one written with an escape.
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"abe\": 4}")
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"abdd\": 4}")
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"ab\\u0064\": 4}")
    # The text ends inside a name, at each place.
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"abd")
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"ab")
    near(stdout, "{\"a\": 1, \"ab\": 2, \"abc\": 3, \"")
    near(stdout, "{\"a")
    # Names of 8 and 23 bytes, two past a header's room that differ in the last byte, and one
    # that isn't ASCII.
    wide(stdout, "{\"eight_by\": 1, \"a_name_of_twenty_three_\": 2, \"a_name_longer_than_a_header_holds\": 3, \"a_name_longer_than_a_header_holdt\": 4, \"grosse\": 5}")
    wide(stdout, "{\"grosse\": 5, \"a_name_longer_than_a_header_holdt\": 4, \"a_name_longer_than_a_header_holds\": 3, \"a_name_of_twenty_three_\": 2, \"eight_by\": 1}")
    wide(stdout, "{\"eight_by\": 1, \"a_name_of_twenty_three_\": 2, \"a_name_longer_than_a_header_holds\": 3, \"a_name_longer_than_a_header_holdu\": 4, \"grosse\": 5}")
    wide(stdout, "{\"eight_by\": 1, \"a_name_of_twenty_three_\": 2, \"a_name_longer_than_a_header_holds\": 3, \"a_name_longer_than_a_header_hold")
    wide(stdout, "{\"eight_by\": 1, \"a_name_of_twenty_three_\": 2, \"a_name_longer_than_a_header_holds\": 3, \"a_name_longer_than_a_header_holdt\": 4, \"größe\": 5}")
    # An enum's cases by name, which begin one another too.
    switch(stdout, "{\"mode\": \"On\", \"n\": 1}")
    switch(stdout, "{\"mode\": \"Once\", \"n\": 2}")
    switch(stdout, "{\"mode\": \"Onced\", \"n\": 3}")
    switch(stdout, "{\"mode\": \"Off\", \"n\": 4}")
    switch(stdout, "{\"mode\": \"Onc\", \"n\": 5}")
    switch(stdout, "{\"mode\": \"Oncede\", \"n\": 6}")
    switch(stdout, "{\"mode\": \"On")
    return
"#;

/// What [`NAMES_BY_BYTES`] prints.
const NAMES_BY_BYTES_PRINTED: &str = r#"Near(a=1, ab=2, abc=3, abd=4)
Near(a=1, ab=2, abc=3, abd=4)
Near(a=1, ab=2, abc=3, abd=4)
no: `Near` has no field "abe", but the text has one
no: `Near` has no field "abdd", but the text has one
Near(a=1, ab=2, abc=3, abd=4)
no: this string never ends, at line 1, column 29
no: this string never ends, at line 1, column 29
no: this string never ends, at line 1, column 29
no: this string never ends, at line 1, column 2
Wide(eight_by=1, a_name_of_twenty_three_=2, a_name_longer_than_a_header_holds=3, a_name_longer_than_a_header_holdt=4, grosse=5)
Wide(eight_by=1, a_name_of_twenty_three_=2, a_name_longer_than_a_header_holds=3, a_name_longer_than_a_header_holdt=4, grosse=5)
no: `Wide` has no field "a_name_longer_than_a_header_holdu", but the text has one
no: this string never ends, at line 1, column 87
no: `Wide` has no field "größe", but the text has one
Switch(mode=Mode.On, n=1)
Switch(mode=Mode.Once, n=2)
Switch(mode=Mode.Onced, n=3)
Switch(mode=Mode.Off, n=4)
no: `mode` is "Onc", which isn't a case of `Mode`
no: `mode` is "Oncede", which isn't a case of `Mode`
no: this string never ends, at line 1, column 10
"#;

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
    return n + more

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
    value = make() except why:
        fail Nope(why="no")
    lend Held[T](value=value) except:
        pass

def one() -> int fails Nope:
    return 1

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
    _ = make() except E():
        return

def one() -> int fails Nope:
    return 1

def main(stdout: Stdout) fails Crashed:
    started(one)?
    stdout.print("ok")
"#
        )?,
        "ok\n"
    );
    Ok(())
}

/// A source whose body has an `x else fail` and a `with launch(...)?` fails with its own error,
/// and a source over it passes that up with `?`: the launch stays one.
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

def work():
    pass

def given(bad: bool) -> int?:
    return None if bad else 1

def inner_source(bad: bool) -> with int fails Outer | Crashed:
    first = given(bad) else fail Outer(why="the inner source failed")
    with launch(() => work())?:
        lend first

def outer_source(bad: bool) -> with int fails Outer | Crashed:
    with inner_source(bad)? as n:
        lend n + 1

def run(bad: bool, stdout: Stdout) fails Outer | Crashed:
    with outer_source(bad)? as n:
        stdout.print("got", n)

def main(stdout: Stdout):
    for bad in [False, True]:
        _ = run(bad, stdout) except why:
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
                return f"stuck {name}"
            case .Jammed:
                return "jammed"

error Late:
    waited: int

def run(n: int) fails Shut | Late:
    if n == 0: fail Shut.Stuck(name="door")
    if n == 1: fail Late(waited=1)
    if n == 2: fail Shut.Jammed

def main(stdout: Stdout):
    for n in [0, 1, 2, 3]:
        _ = run(n) except why:
            case Late(waited=_):
                stdout.print("late")
                continue
            case Shut.Stuck(name=name):
                stdout.print("named", name)
                continue
            case _:
                error = why
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
    lend n except:
        stdout.print("outer's cleanup")
    stdout.print("outer closed")

def used(n: int, stdout: Stdout) fails Boom:
    with outer(n, stdout)? as a:
        with middle(a, stdout)? as b:
            stdout.print("got", b)

def main(stdout: Stdout):
    for n in [1, -1]:
        _ = used(n, stdout) except why:
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
        return n + self.n

    def opened(self, n: int) -> with int fails Boom:
        got = self.made(n)?
        lend got except:
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

/// A source wrapping another says only its own error when the other fails with it: the failure
/// is replaced inside the source, and the wrapper passes it up with `?`, while opening, as here,
/// or while finishing.
#[test]
fn a_with_else_fail_gives_its_own_failure_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let printed = printed_by(
        "with_else_fail",
        "error Second:
    Lost(why: str)

def inner(fine: bool) -> with int fails Second:
    if not fine: fail .Lost(why=\"outer\")
    lend 1 except:
        pass

def outer(fine: bool) -> with int fails Second:
    with inner(fine)? as held:
        lend held + 1 except:
            pass

def main(stdout: Stdout):
    for fine in [True, False]:
        _ = used(fine, stdout) except problem:
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
        return len(text)

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") except:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        return len(text) * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") except:
            pass

def saved[D: Database](db: D) -> str fails Nope:
    with db.transaction()? as tx:
        count = db.run("abc")?
        return f"{tx.log} {count}"
    return ""

def twice[D: Database](db: D) -> str fails Nope:
    first = saved(db)?
    return f"{first}/{saved(db)?}"

def plain(db: Database) -> int fails Nope:
    with db.transaction()? as tx:
        return len(tx.log)
    return 0

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
        lend Session(log="lite") except:
            pass

def direct(store: Lite, text: str) -> str:
    with store.session() as session:
        return f"{session.log} {text}"

def branched(store: Lite, long: bool) -> int:
    if long:
        with store.session() as session:
            return len(session.log) * 10
    else:
        with store.session() as session:
            return len(session.log)

def matched(store: Lite, n: int) -> int:
    match n:
        case 0:
            with store.session() as session:
                return len(session.log) * 100
        case _:
            with store.session() as session:
                return len(session.log) + n

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
        return tally.count

def branched(code: int, twice: bool) -> int fails Refused:
    if twice:
        with held(code)? as tally:
            return tally.count * 2
    else:
        with held(code)? as tally:
            return tally.count

def matched(code: int) -> str fails Refused:
    match code:
        case 0:
            return "none"
        case _:
            with held(code)? as tally:
                return f"tally {tally.count}"

def main(stdout: Stdout):
    counted_value = counted(4) except Refused(): 0
    counted_value2 = counted(3) except Refused(): 0
    stdout.print(counted_value, counted_value2)
    branched_value = branched(4, twice=True) except Refused(): 0
    branched_value2 = branched(5, twice=False) except Refused(): 0
    branched_value3 = branched(3, twice=True) except Refused(): -1
    stdout.print(branched_value, branched_value2, branched_value3)
    matched_value = matched(0) except Refused(): "failed"
    matched_value2 = matched(6) except Refused(): "failed"
    matched_value3 = matched(3) except Refused(): "failed"
    stdout.print(matched_value, matched_value2, matched_value3)
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
    same_as_expected_in_both_builds("with_sources", WITH_SOURCES)
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
        lend f\"{self.name}:{user}\" except:
            log.lines.append(f\"failed {user}\")
        log.lines.append(f\"close {user}\")

def scoped(ref log: Log, label: str) -> with int:
    log.lines.append(f\"enter {label}\")
    lend len(label)
    log.lines.append(f\"leave {label}\")

def check(n: int) -> int fails DbError:
    if n == 3: fail DbError(why=\"three\")
    return n

struct Probe:
    held: int

    def poke(self) fails DbError:
        if self.held > 5: fail DbError(why=\"big\")

def probe(held: int, ref log: Log) -> with Probe fails DbError:
    lend Probe(held=held) except:
        _ = len(log.lines)
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
    return -1

def count(db: Db, ref log: Log) -> int fails DbError:
    mut total = 0
    while True:
        with db.session(\"bo\", ref log)? as session:
            total += len(session)
            if total > 10:
                break
    return total

def main(stdout: Stdout):
    db = Db(name=\"main\")
    mut log = Log()
    first_big_value = first_big(db, ref log, limit=5) except DbError(): -9
    stdout.print(first_big_value)
    first_big_value2 = first_big(db, ref log, limit=50) except DbError(): -9
    stdout.print(first_big_value2)
    count_value = count(db, ref log) except DbError(): -9
    stdout.print(count_value)
    for held in [1, 9]:
        _ = poked(held, ref log) except error:
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
    conn = made() except problem:
        fail problem
    lend conn
    stdout.print(\"done\", conn)

def later(stdout: Stdout) -> with int fails Boom:
    conn = 3
    lend conn
    found = made() except problem:
        fail problem
    stdout.print(found + conn)

def bound_then_assigned(stdout: Stdout) -> with int fails Boom:
    lend 1
    got = made() except conn:
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

/// Several handles in one `with` open in order and finish innermost first, natively. One that
/// fails to open runs the `else:` of each opened before it, a block that fails runs every one's,
/// and a source that lends a tuple splits it into a name each.
#[test]
fn several_handles_in_one_with_finish_innermost_first_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("with_handles", WITH_HANDLES)
}

const WITH_HANDLES: &str = r#"error Refused:
    who: str

struct Log:
    lines = list[str]()

def opened(name: str, ref log: Log) -> with str fails Refused:
    if name == "bad":
        fail Refused(who=name)
    log.lines.append(f"open {name}")
    lend name except:
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
    return 1

def main(stdout: Stdout):
    mut log = Log()
    both_value = both("a", "b", ref log) except Refused(): -1
    stdout.print(both_value)
    stdout.print(log.lines)
    log.lines.clear()
    both_value2 = both("a", "bad", ref log) except Refused(): -1
    stdout.print(both_value2)
    stdout.print(log.lines)
    log.lines.clear()
    both_value3 = both("a", "c", ref log) except Refused(): -1
    stdout.print(both_value3)
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
    same_as_expected_in_both_builds("lent_structs", LENT_STRUCTS)?;
    same_as_expected_in_both_builds("lent_handles", LENT_HANDLES)
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
        return len(self.lines)

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
    return 1

def main(stdout: Stdout):
    save_value = save(stdout) except WriteError(): -1
    stdout.print(save_value)
";

/// Changing an item in place is only safe once the list is alone: another copy of the list
/// mustn't see it, however the item's own count reads.
#[test]
fn changing_an_item_doesnt_show_through_a_copy() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(len(holder.packets), len(keep.packets), holder.packets[2].id, a.id)
    mut kept = holder
    copied = kept
    kept.packets.append(a)
    stdout.print(len(kept.packets), len(copied.packets))
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
fn f_strings_made_in_one_go_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned("f_strings_in_one_go", F_STRINGS_IN_ONE_GO)
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
    stdout.print(f"{short} is {len(short)} chars")
    mut total = 0
    for i in range(1000):
        line = f"{i}: {short}-{long}" if i % 2 == 0 else f"{i}{i}"
        total += len(line)
    stdout.print(total)
"#;

#[test]
fn structs_tuples_and_formats_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    return q

def nudge(ref p: Point):
    p.y = p.y + 1.0
    p.on = not p.on

def pair(n: int) -> (int, float):
    return (n + 1, float(n) / 2.0)

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
    stdout.print(points[1].x, b.x, len(points), points[2].on)
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
    let pinned_outcome = pinned_outcome_of(&program, "own_text_inside")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
    Ok(())
}

/// A value whose field writes its own text prints the same alone as inside a union or a list:
/// the channel's `Timeout`, whose `Duration` has its own `def str`, didn't build printed alone.
#[test]
fn a_value_whose_field_writes_its_own_text_prints_the_same_alone() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"struct Tag:
    name: str

    def str(self, ref out: StrWriter):
        out.write("<" + self.name + ">")

error Late:
    tag: Tag
    after: int

error Gone:
    why: str

struct Holder:
    tag: Tag

def main(stdout: Stdout):
    late = Late(tag=Tag(name="t"), after=3)
    stdout.print(late)
    either: Late | Gone = late
    stdout.print(either)
    stdout.print([late])
    held = Holder(tag=Tag(name="h"))
    stdout.print(held)
    stdout.print([held])
    stdout.print(f"{held} and {late}")
"#;
    let dir = folder("own-text-alone")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, source)?;
    let executable = dir.join("program");
    let path = |file: &Path| file.display().to_string();
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"].into_iter().chain(how.iter().copied()).collect();
        let built = fennec(&[&args[..], &[&path(&program), "-o", &path(&executable)]].concat(), &dir)?;
        assert!(built.status.success(), "{how:?}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        let printed = String::from_utf8_lossy(&ran.stdout).into_owned();
        let lines: Vec<&str> = printed.lines().collect();
        let [alone, in_union, in_list, holder, holders, formatted] = lines[..] else {
            panic!("{how:?}: {printed}{}", String::from_utf8_lossy(&ran.stderr));
        };
        assert!(alone.contains("<t>"), "{how:?}: {printed}");
        assert_eq!(alone, in_union, "{how:?}");
        assert_eq!(format!("[{alone}]"), in_list, "{how:?}");
        assert!(holder.contains("<h>"), "{how:?}: {printed}");
        assert_eq!(format!("[{holder}]"), holders, "{how:?}");
        assert_eq!(formatted, format!("{holder} and {alone}"), "{how:?}");
    }
    std::fs::remove_dir_all(&dir)?;
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
fn values_read_from_their_blocks_show_as_pinned() -> std::io::Result<()> {
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
    let pinned_outcome = pinned_outcome_of(&program, "shown_values")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
    Ok(())
}

/// What a program prints through LLVM is what was pinned for it before the engines were removed.
fn same_as_expected(name: &str, source: &str) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join(name));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ours = apart_from(&dir, outcome(&Command::new(&executable).output_within()?));
    let pinned_outcome = pinned_outcome(name, source)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, pinned_outcome);
    Ok(())
}

/// Evaluation runs left to right: an operand keeps the value it had when it was read, even if one
/// after it changes what it names.
#[test]
fn an_earlier_operand_keeps_its_value_when_a_later_one_changes_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected(
        "left_to_right",
        r#"struct Box:
    items: list[int]
    count: int

    def grow(ref self) -> int:
        self.items.append(9)
        self.count = self.count + 1
        return self.count

struct Pair:
    first: list[int]
    second: int

def show(a: list[int], b: int) -> str:
    return f"{a} {b}"

def take(ref a: list[int]) -> int:
    return a.pop() else 0

def pair(a: int, b: int) -> str:
    return f"{a} {b}"

def bump(ref a: int) -> int:
    a = a + 1
    return a

def through(called: (list[int], int) -> str, ref items: list[int]) -> str:
    return called(items, items.pop() else 0)

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
    stdout.print(len(e), e.pop(1), len(e))
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
    stdout.print(show(u, len(show(u, u.pop() else 0))), u)
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
    stdout.print([m.pop(), len(m), m.pop(), len(m)], m)
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
///
/// Each is run with `fennec run`, in the Fennec folder every test that runs the examples shares,
/// so one built already, by another test or an earlier run, is the program the store keeps,
/// launched again. Built in a folder of its own and copied out, each would be a new file, which
/// macOS holds at its first launch while it's assessed.
#[test]
fn the_examples_it_takes_print_what_they_should() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(examples)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") && path.with_extension("out").exists() {
            paths.push(path);
        }
    }
    paths.sort();
    // What became of each: printed this, or failed, to build or to run, saying this.
    let outcomes = crate::executables::at_once(&paths, |_, path| {
        let ran = crate::executables::fennec_command()
            .args(["run", "--no-test", &path.to_string_lossy()])
            .output_before(FENNEC_DEADLINE)
            .map_err(|error| std::io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
        if !ran.status.success() {
            return Ok(Err(String::from_utf8_lossy(&ran.stderr).trim().to_owned()));
        }
        Ok(Ok(String::from_utf8_lossy(&ran.stdout).into_owned()))
    })?;
    let (mut failed, mut wrong, mut now_native) = (Vec::new(), Vec::new(), Vec::new());
    for (path, outcome) in paths.iter().zip(outcomes) {
        match outcome {
            Ok(_) if not_native_yet(path) => now_native.push(path),
            Err(_) if not_native_yet(path) => {}
            Err(why) => failed.push(format!("{}: {why}", path.display())),
            Ok(printed) if printed != std::fs::read_to_string(path.with_extension("out"))? => {
                wrong.push(path);
            }
            Ok(_) => {}
        }
    }
    assert!(now_native.is_empty(), "these build natively now: take them off NOT_NATIVE_YET: {now_native:?}");
    assert!(wrong.is_empty(), "printed the wrong thing: {wrong:?}");
    assert!(failed.is_empty(), "these didn't build through LLVM, or failed:\n{}", failed.join("\n"));
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
fn dicts_of_blocks_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "dict_values",
        r#"struct Book:
    title: str
    pages: dict[str, list[int]]

def main(stdout: Stdout):
    mut lists = {"a": [1, 2], "b": []}
    lists["c"] = [3]
    copied = lists
    lists["a"] = [9]
    stdout.print(lists, copied, lists["a"], copied["a"][1], len(lists))
    lists["c"].append(4)
    stdout.print(lists["c"], copied["c"])
    stdout.print(lists.get("c"), lists.get("nope"), "a" in lists, "z" in lists)
    stdout.print(lists.keys(), lists.values(), lists.items())
    for key, value in lists.items():
        stdout.print(key, value, len(value))
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
    stdout.print(raw, len(raw["c"]))
    book = Book(title="t", pages={"one": [1], "two": [2, 2]})
    stdout.print(book, book.pages["two"], book.pages.keys())
    mut empty: dict[str, list[int]] = {}
    empty.clear()
    stdout.print(empty, empty.get("a"), len(empty))
    mut grown: dict[int, list[int]] = {}
    for at in range(100):
        grown[at] = [at, at + 1]
    stdout.print(grown[99], len(grown), sum(sum(items) for items in grown.values()))
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
    return key in seen

def main(stdout: Stdout, stdin: Stdin):
    mut seen: dict[int, int] = {}
    mut order = 0
    keys = [int(line).expect("a number") for line in stdin.lines()]
    for key in keys:
        seen[key] = order
        order += 1
    total = sum(seen[key] for key in keys)
    stdout.print(len(seen), total, key_in(seen, keys[0]), key_in(seen, 1))
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
    stdout.print(len(seen), len(kept), len(named), total, seen[first], seen[last], kept.get(last) else -1)
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
    return len(units["a"].tags)

def doubled(given: dict[str, Unit], at: int) -> int:
    mut units = given
    key = "a" if at % 2 == 0 else "b"
    units[key].tags.append(at)
    own = units[key].tags
    units[key].grown_by(own)
    units[key].hp += 1
    return len(units[key].tags) + units[key].hp

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
    return len(b.units["a"].tags) + b.units["a"].hp

def in_dicts(given: dict[str, dict[str, Unit]], at: int) -> int:
    mut dd = given
    dd["x"]["a"].tags.append(at)
    return len(dd["x"]["a"].tags)

def in_list_of_dicts(given: list[dict[str, Unit]], at: int) -> int:
    mut xs = given
    xs[0]["a"].tags.append(at)
    return len(xs[0]["a"].tags)

def in_dict_of_lists(given: dict[str, list[Unit]], at: int) -> int:
    mut d = given
    d["a"][0].tags.append(at)
    return len(d["a"][0].tags)

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
            texts[key] = len(key)
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
    stdout.print(len(texts), len(copied), whole, sliced, paired, missing, len(pairs))
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
        from_value = u8.from(n) except InvalidValue(): zero
        small.add(from_value)
    two: u8 = 2
    three: u8 = 3
    stdout.print(sorted(list(small)), two in small, three in small, len(small))
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
    stdout.print(len(seen), big in seen, seen)
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
fn dicts_of_plain_values_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(links[first] == second, links[second] == first, first in links, len(links))
    mut homes = {"first": first}
    homes["second"] = second
    stdout.print(homes["second"] == second, homes.get("nope") is None, len(homes))
"#,
    )
}

/// A `pop` of a key that isn't there and a `remove` of an item that isn't there fail and change
/// nothing. Generated code stored the failed `pop`'s result, nothing, over the dict before it
/// looked at whether the call had failed, and the runtime had already copied a dict or set with
/// two owners and moved the caller's owner to the copy. So a task that crashed on one left the
/// dict and what it held behind: 5 blocks here. Now the dict is written only once the call has
/// worked, and a shared one without the key is left alone. Alone and with two owners, each fails
/// in a task that crashes and then works on a key that's there, and the counts are pinned: nothing
/// left, and no copy for a take that fails or a `discard` that finds nothing.
#[test]
fn a_pop_or_remove_that_fails_changes_nothing_and_leaves_nothing() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds("failed_takes", FAILED_TAKES, FAILED_TAKES_PRINTS)?;
    assert_eq!(
        counts_of("failed_takes_counts", FAILED_TAKES)?,
        "copies 20 copied_bytes 1104 largest_copy 80 allocations 91 allocated_bytes 11128 live 0"
    );
    Ok(())
}

const FAILED_TAKES: &str = r#"# A `pop` of a key that isn't there, and a `remove` of an item that isn't there, fail and change
# nothing: the dict or set is as it was, alone or with another owner, and a task that crashes on
# one leaves nothing behind. Each then works on a key that is there.

from std.concurrent import Crashed

def popped(key: int, two_owners: bool) -> int:
    mut local = {1: "one, long enough for a block of its own", 2: "two"}
    other = local if two_owners else {9: "nine"}
    _ = local.pop(key)
    return len(local) * 10 + len(other)

def removed(key: str, two_owners: bool) -> int:
    mut local = {"one, long enough for a block of its own", "two"}
    other = local if two_owners else {"nine"}
    local.remove(key)
    return len(local) * 10 + len(other)

def discarded(key: str, two_owners: bool) -> int:
    mut local = {"one, long enough for a block of its own", "two"}
    other = local if two_owners else {"nine"}
    local.discard(key)
    return len(local) * 10 + len(other)

def note(out: shared list[int], value: int):
    lock out as ref held:
        held.append(value)
    return

def try_pop(out: shared list[int], key: int, two_owners: bool):
    note(out, popped(key, two_owners))

def try_remove(out: shared list[int], key: str, two_owners: bool):
    note(out, removed(key, two_owners))

def pops(out: shared list[int], key: int, two_owners: bool) fails Crashed:
    with launch(() => try_pop(out, key, two_owners)):
        pass

def removes(out: shared list[int], key: str, two_owners: bool) fails Crashed:
    with launch(() => try_remove(out, key, two_owners)):
        pass

def main(stdout: Stdout):
    shared out = list[int]()
    for two_owners in [False, True]:
        for key in [5, 2]:
            _ = pops(out, key, two_owners) except Crashed():
                stdout.print(f"pop({key}) crashed, two owners: {two_owners}")
        for key in ["missing", "two"]:
            _ = removes(out, key, two_owners) except Crashed():
                stdout.print(f"remove({key}) crashed, two owners: {two_owners}")
        stdout.print(discarded("missing", two_owners), discarded("two", two_owners))
    lock out as held:
        stdout.print(held)
    return
"#;

const FAILED_TAKES_PRINTS: &str = r"pop(5) crashed, two owners: False
remove(missing) crashed, two owners: False
21 11
pop(5) crashed, two owners: True
remove(missing) crashed, two owners: True
22 12
[11, 11, 12, 12]
";

/// A key a dict of blocks doesn't have fails as pinned.
#[test]
fn a_missing_key_in_a_dict_of_lists_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "missing_list",
        "def main(stdout: Stdout):
    held = {\"a\": [1]}
    stdout.print(held[\"b\"])
",
    )
}

#[test]
fn dicts_sets_closures_and_tuples_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "collections",
        r#"struct Key:
    x: int
    y: int

def twice(n: int, change: (int) -> int) -> int:
    return change(change(n))

def main(stdout: Stdout):
    mut counts: dict[int, int] = {}
    for i in range(50):
        counts[i % 7] = (counts.get(i % 7) else 0) + i
    stdout.print(counts[3], len(counts), 3 in counts, 99 in counts, (counts.get(99) else -1))
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
    stdout.print(keys[Key(x=1, y=2)], len(keys), Key(x=9, y=9) in keys)
    mut seen = {1, 2}
    seen.add(2)
    seen.add(3)
    stdout.print(len(seen), 3 in seen, 4 in seen)
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
    stdout.print(stack.pop(), len(stack))
    stdout.print(sorted([3, 1, 2])[0], sorted(["b", "a"], key=(s) => s)[0])
    mut text = "abc"
    text += "def"
    stdout.print(text, len(text), text[1:3], "bc" in text, text < "b")
    zeros = [0 for _ in range(5)]
    mut flags = [False for _ in range(3)]
    flags[1] = True
    mut none = [0 for _ in range(0 - 2)]
    none.append(7)
    stdout.print(len(zeros), zeros[4], flags, none)
    first = "abc"
    stdout.print("a" < "b", "ab" < "a", "" < "a", first <= "abc", "é" > "z", "abcdefghij" < "abcdefghik", "b" >= "abc")
    maybe: int? = None
    other: int? = 5
    stdout.print(maybe is None, other is None)
    if other is not None:
        stdout.print(other + 1)
"#,
    )?;
    same_message_as_pinned(
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
    same_as_expected_in_both_builds(
        "yields_a_name",
        r#"def joined(parts: Iterator[str]) -> str:
    mut whole = ""
    for part in parts:
        whole = whole + part
    return whole

def repeat(word: str, times: int) -> Iterator[str]:
    for _ in range(times):
        yield word

def main(stdout: Stdout):
    small = "ab"
    big = "a string long enough to live in a block"
    n = 7
    stdout.print(joined(small for _ in range(3)), len(joined(big for _ in range(2))))
    stdout.print(joined(repeat("kit", times=2)), sum(n for _ in range(4)), list(n for _ in range(2)))
"#,
    )
}

/// A generator copied into the loop over it holds each argument as its parameter's type: a `str`
/// given for a `str?` is an optional in the copy. The copy once held it as the argument's type,
/// so `main` didn't build natively, as it read `str` as `str?`.
#[test]
fn a_copied_generator_holds_its_arguments_as_its_parameters_types() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
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

/// What closures and generators capture, of each kind, comes out as pinned:
/// a closure giving back what it captured as it is, generator expressions yielding a captured
/// string, struct, list, bool, int or float, nested generators and generators with `while`,
/// comprehensions inside generators and the other way round, and a closure inside a generator.
#[test]
fn captures_of_every_kind_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "captures",
        r#"struct Pair:
    name: str
    count: int

def joined(parts: Iterator[str]) -> str:
    mut whole = ""
    for part in parts:
        whole = whole + part
    return whole

def apply(change: (int) -> str, x: int) -> str:
    return change(x)

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
    stdout.print(apply((x) => small, 1), len(apply((x) => big, 1)), apply((x) => str(n), 1))
    stdout.print(apply((x) => pair.name, 0), apply((x) => f"{items}", 0))
    # generator expressions yielding captures as they are, of each kind
    stdout.print(list(pair for _ in range(2)), list(items for _ in range(2)), list(flag for _ in range(2)))
    stdout.print(list(n for _ in range(2)), list(1.5 for _ in range(2)), joined(pair.name for _ in range(2)))
    # nested generators and generator functions
    stdout.print(joined(outer("xy", times=2)), joined(counting(small, limit=3)))
    stdout.print(joined(w for w in twice(small)), joined(small for w in twice("q")))
    # comprehensions inside generators, and generators inside comprehensions
    stdout.print(list([small, w] for w in twice("z")), [joined(small for _ in range(k)) for k in range(3)])
    stdout.print(joined(s for s in [small, big] if len(s) < 5), sum(n for _ in range(3)))
    # a closure inside a generator, and a generator inside a closure
    stdout.print(joined(apply((x) => small, 0) for _ in range(2)), apply((x) => joined(small for _ in range(x)), 2))
"#,
    )
}

/// A generator made in one place and read in another keeps what it captured: one returned from a
/// function, one held before it's read, and one made each time round a loop.
#[test]
fn generators_kept_for_later_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "kept_generators",
        r#"def joined(parts: Iterator[str]) -> str:
    mut whole = ""
    for part in parts:
        whole = whole + part
    return whole

def words_of(word: str, times: int) -> Iterator[str]:
    return (word for _ in range(times))

def later(word: str) -> Iterator[str]:
    made = word + "!"
    return (made for _ in range(2))

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

/// A closure that captures a struct holding blocks lets go of them when it's let go of: an arrow
/// reading a struct's list, and the `else:` a `with` source's crash runs, which captures a `ref`
/// argument. Each round left the struct's blocks behind.
#[test]
fn a_closure_lets_go_of_a_struct_it_captured() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let live = |rounds: u32| -> std::io::Result<(Option<u64>, String)> {
        let source = STRUCT_CAPTURED.replace("rounds = 3", &format!("rounds = {rounds}"));
        let counts = counts_of(&format!("struct_captured_{rounds}"), &source)?;
        let printed = printed_by(&format!("struct_captured_printed_{rounds}"), &source)?;
        Ok((count(&counts, "live"), printed))
    };
    let (few, many) = (live(3)?, live(30)?);
    assert_eq!(few.1, "129 3\n");
    assert_eq!(many.1, "1310 30\n");
    assert!(few.0.is_some(), "the run says how many blocks it still held");
    assert_eq!(few.0, many.0, "blocks still held after 3 rounds and after 30");
    Ok(())
}

const STRUCT_CAPTURED: &str = r#"struct Bag:
    items: list[str]

struct Ledger:
    lines = list[str]()

def apply(run: () -> int) -> int:
    return run()

def entry(ref ledger: Ledger) -> with str:
    lend "x" except:
        ledger.lines.append("undo")

def once() -> int:
    mut ledger = Ledger()
    ledger.lines.append("first line, long enough for a block of its own")
    with entry(ref ledger) as label:
        assert label == "x"
    return len(ledger.lines)

def main(stdout: Stdout):
    rounds = 3
    mut total = 0
    mut lines = 0
    for round in range(rounds):
        bag = Bag(items=[f"item {round}, long enough for a block of its own"])
        total += apply(() => len(bag.items) + len(bag.items[0]))
        lines += once()
    stdout.print(total, lines)
"#;

/// What a function value or an arrow gives back held in place, an enum holding a text or a struct
/// holding one, is the caller's, as a direct call's is: kept, read and let go of, or thrown away,
/// with nothing left held. Each call once kept a second owner of its text.
#[test]
fn a_result_held_in_place_through_a_function_value_is_let_go_of() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("in_place_through_values")?;
    std::fs::write(dir.join("in_place_through_values.fen"), IN_PLACE_THROUGH_VALUES)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "in_place_through_values", how)?;
        assert_eq!(printed, "185 185 185 55\n", "{how:?}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const IN_PLACE_THROUGH_VALUES: &str = r#"enum Outcome:
    Good(value: int)
    Bad(reason: str)

struct Said:
    text: str
    count: int

def judge(n: int) -> Outcome:
    if n % 2 == 0:
        return .Good(value=n)
    return .Bad(reason="this failed for a reason number " + str(n))

def said(n: int) -> Said:
    return Said(text="a text long enough for a block, number " + str(n), count=n)

def use(got: Outcome) -> int:
    match got:
        case .Bad(reason):
            return len(reason)
        case .Good(value):
            return value

def apply(run: (int) -> Outcome, n: int) -> int:
    got = run(n)
    return use(got)

def thrown(run: (int) -> Outcome, n: int) -> int:
    _ = run(n)
    return 1

def counted(make: (int) -> Said, n: int) -> int:
    kept = make(n)
    return kept.count + len(kept.text) - 40

def main(stdout: Stdout):
    mut direct = 0
    mut valued = 0
    mut arrowed = 0
    mut other = 0
    for n in range(10):
        direct += use(judge(n))
        valued += apply(judge, n=n)
        arrowed += apply((k) => judge(k), n=n)
        other += thrown(judge, n=n) + counted(said, n=n)
    stdout.print(direct, valued, arrowed, other)
    return
"#;

/// Closures kept where the runtime reads them, as a list's items and in a generator's block, come
/// out as pinned: appended, read in a loop and by index, shared by a copy of the list, popped,
/// replaced, and captured by a generator that calls one. A list of closures and a generator keeping
/// one once didn't build natively, as the runtime couldn't hold them.
#[test]
fn closures_kept_in_lists_and_generators_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "closures_kept",
        r#"def add(x: int) -> int:
    return x + 1

def mul(x: int) -> int:
    return x * 2

def made(k: int) -> (int) -> int:
    def plus(x: int) -> int:
        return x + k
    return plus

def counted(word: str) -> Iterator[int]:
    def size(x: int) -> int:
        return x + len(word)
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
        return x + len(name)
    fs.append(named)
    for k in range(3):
        fs.append(made(k))
    for f in fs:
        stdout.print(f(3))
    stdout.print(fs[2](0), len(fs))
    copied = fs
    fs.append(add)
    stdout.print(len(copied), len(fs))
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
fn functions_named_in_tuples_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "functions_in_tuples",
        "def add(x: int) -> int:
    return x + 1

def twice(x: int) -> int:
    return x * 2

def main(stdout: Stdout):
    (g, n) = (add, 4)
    stdout.print(g(n))
    pair = (twice, 5)
    (h, m) = pair
    stdout.print(h(m), g(h(1)))
",
    )
}

/// A generator that fails lets go of what it failed with when it's let go of. The header's slot
/// for the failure was laid out as a number, so the generator's free left its block behind: an
/// error, its notes and what it holds, for every failure. A generator holding a `with` is one of
/// its own, not written into its loop, so these are. Each fails as it closes or inside the
/// `with`, stopped where one can be: run to its end, with nothing given, left partway, made and
/// never started, and in a task. Then a thousand fail one after another. The counts are pinned:
/// nothing left. On main this ends with 10,023 blocks.
#[test]
fn a_generator_that_fails_lets_go_of_its_failure_wherever_its_stopped() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds("failed_generators", FAILED_GENERATORS, FAILED_GENERATORS_PRINTS)?;
    assert_eq!(
        counts_of("failed_generators_counts", FAILED_GENERATORS)?,
        "copies 2 copied_bytes 80 largest_copy 40 allocations 16063 allocated_bytes 1324784 live 0"
    );
    Ok(())
}

const FAILED_GENERATORS: &str = r#"# A generator that fails lets go of what it failed with when it's let go of. Each of these holds a
# `with`, so it's a generator of its own and not written into its loop. It fails as it closes, or
# in the `with`'s body, and is stopped at every point one can be: run to its end, left partway,
# made and never started, and in a task. Then a thousand of them fail one after another.

from std.concurrent import Crashed

struct Tally:
    count: int

error Shut:
    code: int
    why: str

def shutting(code: int) -> with Tally fails Shut:
    lend Tally(count=code)
    fail Shut(code=code, why=f"closing {code}, in words long enough for a block")

def lending(code: int) -> with Tally:
    lend Tally(count=code)

def closes_badly(limit: int) -> Iterator[int] fails Shut:
    with shutting(limit)? as tally:
        for at in range(limit):
            yield at + tally.count

def fails_inside(limit: int) -> Iterator[int] fails Shut:
    with lending(limit) as tally:
        for at in range(limit):
            yield at + tally.count
        fail Shut(code=limit + 100, why=f"inside {limit}, in words long enough for a block")

def to_the_end(limit: int) -> int fails Shut:
    mut running = 0
    for value in closes_badly(limit)?:
        running += value
    return running

def left_partway(limit: int) -> int fails Shut:
    mut running = 0
    for value in closes_badly(limit)?:
        running += value
        if value > limit:
            break
    return running

def never_started(limit: int) -> int fails Shut:
    made = closes_badly(limit)?
    return limit

def inside(limit: int) -> int fails Shut:
    mut running = 0
    for value in fails_inside(limit)?:
        running += value
    return running

def show(stdout: Stdout, name: str, run: () -> int fails Shut):
    got = run() except why:
        case Shut():
            stdout.print(name, "shut", why.code, why.why)
            return
    stdout.print(name, got)
    return

def quietly(limit: int) -> int:
    ended = to_the_end(limit) except Shut(): -1
    partway = left_partway(limit) except Shut(): -2
    within = inside(limit) except Shut(): -3
    return ended + partway + within

def in_a_task(limit: int):
    _ = quietly(limit)

def tasked(limit: int) fails Crashed:
    with launch(() => in_a_task(limit)):
        pass

def main(stdout: Stdout):
    show(stdout, "to the end:", () => to_the_end(3)?)
    show(stdout, "nothing given:", () => to_the_end(0)?)
    show(stdout, "left partway:", () => left_partway(3)?)
    show(stdout, "never started:", () => never_started(3)?)
    show(stdout, "inside:", () => inside(3)?)
    _ = tasked(3) except Crashed():
        stdout.print("the task crashed")
    mut failed = 0
    for round in range(1_000):
        failed += quietly(round % 4)
    stdout.print("a thousand:", failed)
    return
"#;

const FAILED_GENERATORS_PRINTS: &str = r"to the end: shut 3 closing 3, in words long enough for a block
nothing given: shut 0 closing 0, in words long enough for a block
left partway: shut 3 closing 3, in words long enough for a block
never started: 3
inside: shut 103 inside 3, in words long enough for a block
a thousand: -6000
";

/// A generator's failure passes up however the generator was reached: from a name, and from inside
/// `with` blocks, where the loop is over a name too, as well as from the loop that calls it.
#[test]
fn a_generators_failure_passes_up_from_a_name_or_a_with() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def checked(limit: int) -> Iterator[int] fails IoError:
    for at in range(limit):
        if at == 2: fail .Other(reason=\"stop\")
        yield at

def source() -> with int:
    lend 1 except:
        pass

def in_a_name(limit: int) -> int fails IoError:
    mut seen = 0
    made = checked(limit)?
    for at in made:
        seen += at
    return seen

def in_with(limit: int) -> int fails IoError:
    mut seen = 0
    with source() as held:
        for at in checked(limit)?:
            seen += at + held
    return seen

def nested(limit: int) -> int fails IoError:
    mut seen = 0
    with source() as outer:
        with source() as inner:
            for at in checked(limit)?:
                seen += at + outer + inner
    return seen

def listed(limit: int) -> list[int] fails IoError:
    return [at * 2 for at in checked(limit)?]

## What `run` gave, or what it failed with.
def said[T](run: () -> T fails IoError) -> str:
    value = run() except why:
        return f\"{why}\"
    return f\"{value}\"

def main(stdout: Stdout):
    stdout.print(said(() => in_a_name(5)?), said(() => in_a_name(2)?))
    stdout.print(said(() => in_with(5)?), said(() => in_with(2)?))
    stdout.print(said(() => nested(5)?), said(() => nested(2)?))
    stdout.print(said(() => listed(5)?), said(() => listed(2)?))
";
    let stop = "IoError.Other(reason=\"stop\")";
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
fn generators_that_fail_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    return sum(n for n in checked(limit)?)

def joined(limit: int) -> str fails Named:
    mut out = ""
    for word in named(limit)?:
        out = out + word
    return out

def looped(limit: int) -> int fails Named | TooBig:
    mut count = 0
    for n in either(limit)?:
        count = count + n + 1
    return count

## What `run` gave, or what it failed with.
def said[T, E](run: () -> T fails E) -> str:
    value = run() except why:
        return f"{why}"
    return f"{value}"

def total_or_why(limit: int, stdout: Stdout):
    found = total(limit) except why:
        stdout.print("too big", why.at)
        return
    stdout.print(found)

def joined_or_who(limit: int, stdout: Stdout):
    text = joined(limit) except why:
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
fn a_generator_left_partway_closes_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected(
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
    return 0

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
    lend door except:
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
    lend door except:
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
    return 0

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
    return 0

def failing(stdout: Stdout) fails Shut | Other:
    for n in held("failing", stdout)?:
        fail Other(why="in the loop")

def tell_broke(stdout: Stdout):
    broke(stdout) except why:
        stdout.print("broke failed", why)
        return
    stdout.print("broke worked")

def tell_returned(stdout: Stdout):
    n = returned(stdout) except why:
        stdout.print("returned failed", why)
        return
    stdout.print("returned", n)

def main(stdout: Stdout):
    tell_broke(stdout)
    tell_returned(stdout)
    failing(stdout) except why:
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
    return 0

def failing(stdout: Stdout) fails Shut | Other:
    for n in held("failing", stdout)?:
        fail Other.Inside(why="in the loop")

def tell_broke(stdout: Stdout):
    broke(stdout) except why:
        stdout.print("broke failed", why)
        return
    stdout.print("broke worked")

def tell_returned(stdout: Stdout):
    n = returned(stdout) except why:
        stdout.print("returned failed", why)
        return
    stdout.print("returned", n)

def main(stdout: Stdout):
    tell_broke(stdout)
    tell_returned(stdout)
    failing(stdout) except why:
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
    walk = held("quiet", stdout) except why:
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
    kept(stdout) except why:
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
fn what_takes_every_item_takes_a_types_own_iter_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
fn generators_and_results_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    return 0

def half(n: int) -> int fails Fault:
    if n % 2 == 1: fail Fault(f"{n} is odd")
    return n // 2

def show(n: int) -> str:
    got = half(n) except why:
        case Fault(reason):
            return reason
    return f"half is {got}"

def main(stdout: Stdout):
    stdout.print(sum(n for n in running(odd_squares(count_up(30)))))
    for word in words(3):
        stdout.print(word, len(word))
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
    same_as_pinned(
        "twice",
        "def count_up(limit: int) -> Iterator[int]:\n    mut at = 0\n    while at < limit:\n        yield at\n        at += 1\n\ndef total(numbers: Iterator[int]) -> int:\n    mut subtotal = 0\n    for n in numbers:\n        subtotal += n\n    return subtotal\n\ndef main(stdout: Stdout):\n    numbers = count_up(3)\n    for _ in range(2):\n        stdout.print(total(numbers))\n",
    )?;
    same_message_as_pinned(
        "expect",
        "error Fault:\n    reason: str\n\ndef half(n: int) -> int fails Fault:\n    if n % 2 == 1: fail Fault(\"odd\")\n    return n // 2\n\ndef main(stdout: Stdout):\n    stdout.print(half(4).expect(\"four is even\"))\n    stdout.print(half(3).expect(\"three is even\"))\n",
    )
}

#[test]
fn regexes_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "regexes",
        r#"from std.re import Regex, is_regex

def main(stdout: Stdout):
    words = Regex(r"(\w)(\w*)")
    for found in words.find_all("fox and éclair"):
        stdout.print(found.text, found.start, found.end, len(found.groups), found.groups[0], found.groups[1])
    optional = Regex(r"a(x)?b")
    for found in optional.find_all("ab axb"):
        stdout.print(found.text, found.groups[0] is None)
    stdout.print(words.replace("fox den", r"\2\1"), len(Regex(" +").split("a  b c")))
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
    if at >= len(items): fail Refused(at=at)
    return items[at]

def round(at: int) -> int fails Refused:
    mut held = [f"held through the round {at}, long enough to need a block of its own"]
    held.append(f"and {at}, long enough to need a block of its own")
    first = checked(held, at % 3)?
    both = checked([f"left {at}, long enough to need a block of its own"], 0)? + checked([f"right {at}, long enough to need a block of its own"], at % 2)?
    return len(first) + len(both) + len(held)

def main(stdout: Stdout):
    rounds = 3
    mut failed = 0
    mut failed_many = 0
    for at in range(rounds):
        got = round(at) except Refused(): -1
        if got < 0:
            failed += 1
        made = many(at) except Refused(): -1
        if made < 0:
            failed_many += 1
    stdout.print(failed, failed_many)
"#;

/// A program that reads a value where letting go of it too soon is easiest to get wrong prints the
/// same in a dev build and a release, built as written and checking proofs, and holds as many
/// blocks at its end after 30 rounds as after 3. Too soon would read what's freed, which the
/// sanitizer's job sees; too late adds up.
fn read_until_its_last_use(name: &str, source: &str, wanted: &str) -> std::io::Result<()> {
    prints_in_both_builds(name, source, wanted)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder(&format!("{name}_checked"))?;
        let (program, executable) = (dir.join(format!("{name}.fen")), dir.join(name));
        std::fs::write(&program, source)?;
        let built = crate::executables::fennec_command()
            .arg("build")
            .args(how)
            .arg(&program)
            .arg("-o")
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .env("FENNEC_CHECK_PROOFS", "1")
            .output_before(FENNEC_DEADLINE)?;
        assert!(built.status.success(), "{how:?}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            wanted,
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    let live = |rounds: u32| -> std::io::Result<Option<u64>> {
        let source = source.replace("rounds = 3", &format!("rounds = {rounds}"));
        Ok(count(&counts_of(&format!("{name}_{rounds}"), &source)?, "live"))
    };
    let (few, many) = (live(3)?, live(30)?);
    assert!(few.is_some(), "the run says how many blocks it still held");
    assert_eq!(few, many, "blocks still held after 3 rounds and after 30");
    Ok(())
}

/// A list read on each turn of a loop is kept for every turn, though its last read is inside it,
/// and one replaced on each turn, `current = longer`, is kept until it's replaced.
#[test]
fn a_value_read_in_a_loop_is_kept_for_every_turn() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    read_until_its_last_use("read_in_a_loop", READ_IN_A_LOOP, "w0\n11 4 c0\nw1\n11 4 c1\nw2\n11 4 c2\n")
}

const READ_IN_A_LOOP: &str = r#"def main(stdout: Stdout):
    rounds = 3
    for round in range(rounds):
        words = [f"w{round}", "bb", "ccc"]
        mut total = 0
        for at in range(5):
            total += len(words[at % 3])
            if at == 4:
                stdout.print(words[0])
        mut current = [f"c{round}"]
        mut steps = 0
        while steps < 3:
            longer = current + [f"s{steps}"]
            current = longer
            steps += 1
        stdout.print(total, len(current), current[0])
"#;

/// A value read in an `else` block a failure runs is kept for it, though the call that fails reads
/// it last on the way that doesn't, and one read only in the `else` is kept until the call.
#[test]
fn a_value_read_where_a_failure_goes_is_kept_for_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    read_until_its_last_use(
        "read_where_a_failure_goes",
        READ_WHERE_A_FAILURE_GOES,
        "6 0\n7 1\nnote 2 bad 8\nlabel 2\n-1 0\n",
    )
}

const READ_WHERE_A_FAILURE_GOES: &str = r#"error Bad:
    why: str

def risky(n: int) -> int fails Bad:
    if n % 3 == 2: fail Bad(why=f"bad {n}")
    return n

def main(stdout: Stdout):
    rounds = 3
    for round in range(rounds):
        note = f"note {round}"
        got = risky(len(note) + round) except why:
            stdout.print(note, why.why)
            -1
        label = f"label {round}"
        other = risky(round) except Bad():
            stdout.print(label)
            0
        stdout.print(got, other)
"#;

/// A value a `with` source reads before its `lend`, in its `else:` and after it, is kept across
/// the caller's block, and the caller's own, read inside the block and after it, is kept too.
#[test]
fn a_value_read_around_a_lend_is_kept_across_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    read_until_its_last_use(
        "read_around_a_lend",
        READ_AROUND_A_LEND,
        "ANN0 ann0\n4\nANN1 ann1\n4\nANN2 ann2\n4\n6 open ann0\n",
    )
}

const READ_AROUND_A_LEND: &str = r#"struct Ledger:
    lines = list[str]()

def entry(ref ledger: Ledger, name: str) -> with str:
    ledger.lines.append(f"open {name}")
    lend name.upper() except:
        ledger.lines.append(f"undo {name}")
    ledger.lines.append(f"close {name}")

def main(stdout: Stdout):
    rounds = 3
    mut ledger = Ledger()
    for round in range(rounds):
        name = f"ann{round}"
        with entry(ref ledger, name) as label:
            stdout.print(label, name)
        stdout.print(len(name))
    stdout.print(len(ledger.lines), ledger.lines[0])
"#;

/// A value an arrow function captures is kept for every call of it, though making the arrow is
/// the last read written where the value is, and one read again after is kept for that too.
#[test]
fn a_value_an_arrow_captures_is_kept_for_its_calls() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    read_until_its_last_use(
        "captured_by_an_arrow",
        CAPTURED_BY_AN_ARROW,
        "hi 0 1 hi 0!2\n4 i0\nhi 1 1 hi 1!2\n4 i1\nhi 2 1 hi 2!2\n4 i2\n",
    )
}

const CAPTURED_BY_AN_ARROW: &str = r#"def apply(run: (int) -> str, n: int) -> str:
    return run(n)

def count_with(run: () -> int) -> int:
    return run() + run()

def main(stdout: Stdout):
    rounds = 3
    for round in range(rounds):
        greeting = f"hi {round}"
        stdout.print(apply((x) => f"{greeting} {x}", 1), apply((x) => f"{greeting}!{x}", 2))
        items = [f"i{round}", "j"]
        stdout.print(count_with(() => len(items)), items[0])
"#;

/// [`FAILING_PARTWAY`] with `many`, a function of 50 lines that each make a list and a string
/// held only for the line, enough short-lived values for each failure to let go of its own where
/// it leaves. Round `at` fails at line `49 - at`.
fn failing_partway() -> String {
    let lines = (0..50).fold(String::new(), |mut lines, line| {
        let _ = writeln!(
            lines,
            "    total += len(checked([f\"line {line} of {{at}}, long enough to need a block of its own\"], (at + {line}) % 50 // 49)?)"
        );
        lines
    });
    format!(
        "{FAILING_PARTWAY}\ndef many(at: int) -> int fails Refused:\n    mut total = 0\n{lines}    return total\n"
    )
}

/// A function too big to optimize in reasonable time, `long` of 400 lines that each make a list
/// and a string, is compiled as written: in a dev build on its own, and in a release apart from
/// the whole, which keeps what it calls, the runtime's formatting that only it uses included. Both
/// print what it adds up.
#[test]
fn a_huge_function_is_compiled_as_written_and_gives_the_same() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let lines = 400;
    let mut body = String::new();
    let mut total: i64 = 0;
    for line in 0..lines {
        let _ =
            writeln!(body, "    total += at_or_zero([{line}, {line} + 1], {line} % 2) + len(f\"{{total}}\")");
        let picked = if line % 2 == 0 { line } else { line + 1 };
        let written = i64::try_from(total.to_string().len()).unwrap_or(0);
        total += picked + written;
    }
    let source = format!(
        "def at_or_zero(items: list[int], at: int) -> int:\n    if len(items) == 0: return 0\n    return items[at % len(items)]\n\ndef long() -> int:\n    mut total = 0\n{body}    return total\n\ndef main(stdout: Stdout):\n    stdout.print(long())\n"
    );
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let dir = folder("huge_function")?;
        let (program, executable) = (dir.join("huge.fen"), dir.join("huge"));
        std::fs::write(&program, &source)?;
        let built = crate::executables::fennec_command()
            .arg("build")
            .args(how)
            .arg(&program)
            .arg("-o")
            .arg(&executable)
            .env("FENNEC_HOME", &dir)
            .output_before(FENNEC_DEADLINE)?;
        let said = String::from_utf8_lossy(&built.stderr);
        assert!(built.status.success(), "{how:?}: {said}");
        // It runs slower, so the build says so.
        assert!(said.contains("hint: `long` is too big to optimize"), "{how:?}: {said}");
        let ran = Command::new(&executable).output_within()?;
        std::fs::remove_dir_all(&dir)?;
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            format!("{total}\n"),
            "{how:?}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
    Ok(())
}

/// A release builds a huge function apart and keeps what it calls seen from outside the rest, by
/// name. A generic's instance, `f_twice[int]`, has brackets, which the list of names kept read as
/// a pattern: it was made internal, and linking said "Undefined symbols".
#[test]
fn a_huge_function_calling_a_generic_links_in_a_release() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let lines = 400;
    let mut body = String::new();
    let mut total: i64 = 0;
    for line in 0..lines {
        let _ = writeln!(body, "    total += at_or_zero(twice({line}), {line} % 2) + len(f\"{{total}}\")");
        let written = i64::try_from(total.to_string().len()).unwrap_or(0);
        total += line + written;
    }
    let source = format!(
        "def twice[T](value: T) -> list[T]:\n    return [value, value]\n\ndef at_or_zero(items: list[int], at: int) -> int:\n    if len(items) == 0: return 0\n    return items[at % len(items)]\n\ndef long() -> int:\n    mut total = 0\n{body}    return total\n\ndef main(stdout: Stdout):\n    stdout.print(long())\n"
    );
    let dir = folder("huge_generic")?;
    let (program, executable) = (dir.join("huge.fen"), dir.join("huge"));
    std::fs::write(&program, &source)?;
    let built = crate::executables::fennec_command()
        .args(["build", "--llvm", "--release"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_HOME", &dir)
        .output_before(FENNEC_DEADLINE)?;
    let said = String::from_utf8_lossy(&built.stderr).into_owned();
    assert!(built.status.success(), "{said}");
    assert!(said.contains("hint: `long` is too big to optimize"), "{said}");
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        format!("{total}\n"),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    Ok(())
}

const CRASHING_PARTWAY: &str = r#"from std.concurrent import Crashed

def kept_then_crash(at: int):
    out = [f"item {i} of {100 // (10 - i)}, long enough to need a block of its own" for i in range(20)]
    print_nothing(out)

def keys_then_crash(at: int):
    words = [f"word {i}, long enough to need a block of its own" for i in range(20)]
    print_nothing(sorted(words, key=(word) => f"{word} {100 // (len(word) - 46)}"))

def print_nothing(words: list[str]):
    if len(words) == 12345:
        pass

def crashing(which: int) -> int fails Crashed:
    if which == 0:
        with launch(() => kept_then_crash(which))?:
            pass
    else:
        with launch(() => keys_then_crash(which))?:
            pass
    return 0

def main(stdout: Stdout):
    rounds = 3
    mut crashed = 0
    for _ in range(rounds):
        crashing_value = crashing(0) except Crashed(): 1
        crashed += crashing_value
        crashing_value2 = crashing(1) except Crashed(): 1
        crashed += crashing_value2
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
/// are, so it has one owner while it changes. Each way gives what was pinned, copies and failures
/// included.
#[test]
fn a_field_changed_with_any_argument_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("changed_with_any_argument", CHANGED_WITH_ANY_ARGUMENT)
}

const CHANGED_WITH_ANY_ARGUMENT: &str = r#"struct Bag:
    items = list[int]()

    def add(ref self, value: int):
        self.items.append(value)

def push(ref items: list[int], value: int):
    items.append(value)

def twice(x: int) -> int:
    return x * 2

struct Holder:
    bag = Bag()
    items = list[int]()
    counts = dict[int, int]()
    text = ""

    def put(ref self, value: int, flag: bool):
        self.items.append(1 if flag else 0)
        self.items.append(sum([value for _ in range(2)]))
        self.items.append(len(self.items) if flag else twice(value))
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
    zero = len(holder.items) - len(holder.items)
    holder.items.append(10 // zero if zero > 0 else 3)
    stdout.print(len(holder.items))
    holder.items.append(10 // zero)
    stdout.print(len(holder.items))
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
            "struct Bag:\n    items = list[int]()\n\n    def add(ref self, value: int):\n        self.items.append(value)\n\ndef push(ref items: list[int], value: int):\n    items.append(value)\n\nstruct Holder:\n    bag = Bag()\n    items = list[int]()\n\n    def put(ref self, value: int, flag: bool):\n        {shape}\n\n    def size(self) -> int:\n        return len(self.items) + len(self.bag.items)\n\ndef main(stdout: Stdout):\n    mut holder = Holder()\n    for i in range(2000):\n        holder.put(i, i % 2 == 0)\n    stdout.print(holder.size())\n"
        );
        let counts = counts_of("changed_field", &source)?;
        assert_eq!(count(&counts, "copies"), Some(0), "{shape}: {counts}");
    }
    Ok(())
}

/// A list or dict that starts as a shared empty constant, as json's reader and writer start
/// theirs, is made anew the first time it's written, with nothing copied and no copy counted, and
/// the constant stays empty. Each write counted a copy of nothing, 2,537 in `api_handler`.
#[test]
fn a_shared_empty_list_or_dict_written_once_copies_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "NO_ITEMS = list[int]()\nNO_TAGS = dict[str, int]()\n\nstruct Holder:\n    items = NO_ITEMS\n    tags = NO_TAGS\n\ndef main(stdout: Stdout):\n    mut total = 0\n    for i in range(50):\n        mut holder = Holder()\n        holder.items.append(i)\n        holder.tags[\"a\"] = i\n        total += len(holder.items) + len(holder.tags) + holder.items[0]\n    stdout.print(total, len(NO_ITEMS), len(NO_TAGS))\n    return\n";
    let dir = folder("shared_empty_written")?;
    std::fs::write(dir.join("shared_empty_written.fen"), source)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "shared_empty_written", how)?;
        assert_eq!(printed, "1325 0 0\n", "{how:?}");
        assert_eq!(count(&counts, "copies"), Some(0), "{how:?}: {counts}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A key held in a block, a tuple's or a struct's, is let go of by every dict and set operation
/// it's given to: `in`, `get`, reading, setting, `+=`, `setdefault`, `update`, `remove`, `pop` and
/// iteration on a dict, and `add`, `in`, `remove`, `discard`, `update` and the set operations on a
/// set, for keys that are there and keys that aren't, on the main thread, in a launched task and
/// in `parallel`. And where a key that isn't there fails, in reading it, `+=`, `pop` and a set's
/// `remove`, each in a task it stops: the key goes, and so does the dict the task held. No block
/// is left.
///
/// Three things left blocks behind. `pop`, `remove` and `discard` never let go of the key they
/// were handed, a block a call: `dicts_large` ended with 1 live and `struct_keys_native` with 4.
/// Reading a key that isn't there left the key its failure was told of. And `pop` of one gave
/// back no dict with its failure, which native code stored over the dict before looking, losing
/// it and all it held.
#[test]
fn a_key_given_to_a_dict_or_a_set_is_let_go_of_whatever_comes_of_it() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("keys_let_go")?;
    let program = dir.join("keys_let_go.fen");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/keys_let_go.fen"), &program)?;
    prints_with_nothing_left(&dir, &program, "3065\n3065\n4400\n8\n", &["1", "4"])
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
fn a_loop_over_a_variables_field_sees_the_variable_change() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "loop_over_changing_field",
        r#"struct Bag:
    items = list[int]()
    name = "bag"

def grow(ref bag: Bag):
    bag.items.append(len(bag.items))

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
        stdout.print(item, len(other.items), junk[0])
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
        "struct Var:\n    value: int\n    links: list[int]\n\nstruct Holder:\n    vars: list[Var]\n\n    def add(ref self, value: int):\n        self.vars.append(Var(value=value, links=[]))\n\n    def link(ref self, at: int, other: int):\n        self.vars[at].links.append(other)\n\ndef main(stdout: Stdout):\n    mut holder = Holder(vars=[])\n    for i in range(100):\n        holder.add(i)\n    for i in range(1000):\n        holder.link(i % 100, other=i)\n    stdout.print(len(holder.vars[5].links))\n",
    )?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    // A method written in Fennec changing a field: `self.inner.add(x)` and std's `extend`.
    let counts = counts_of(
        "methods",
        "struct Inner:\n    items: list[int]\n\n    def add(ref self, item: int):\n        self.items.append(item)\n\nstruct Outer:\n    inner: Inner\n    items: list[int]\n\n    def push(ref self, item: int):\n        self.inner.add(item)\n\n    def grow(ref self, more: list[int]):\n        self.items.extend(more)\n\ndef main(stdout: Stdout):\n    mut outer = Outer(inner=Inner(items=[]), items=[])\n    for i in range(100):\n        outer.push(i)\n    for i in range(100):\n        outer.grow([i])\n    stdout.print(len(outer.inner.items), len(outer.items))\n",
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
        return (self.state // 65536) % limit

def piece(ref random: Random) -> str:
    letters = ["a", "Z", "é", "ñ", "日", "€", "🦊", "𝄞", " ", "0"]
    mut text = ""
    for _ in range(random.below(8)):
        text += letters[random.below(len(letters))]
    return text

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
            counted += len(part)
        joined = "".join(out)
        if len(joined) != counted or len(joined) != len(added) or joined != added:
            wrong += 1
        if len(joined) > 0:
            start = random.below(len(joined))
            end = start + random.below(len(joined) - start + 1)
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
    stdout.print(len(text))
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
    return "".join(out)

def main(stdout: Stdout):
    numbers = [0, -1, 7, 1234567890, -9223372036854775807 - 1, 9223372036854775807]
    floats = [1.5, -0.0, 1e300, 0.1, -2.25]
    words = ["é", "日本", "🦊", "fóx12", "ab", "ça va très bien", "x" * 30 + "é"]
    text = joined(numbers, floats=floats, words=words)
    stdout.print(text)
    stdout.print(len(text), text[-1], text[-2], text[len(text) // 2])
    mut index = 0
    mut seen = 0
    while index < len(text):
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
    stdout.print(total, len(first), len(second))
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
    let source = "def main(stdout: Stdout):\n    mut items = [1]\n    for i in range(200):\n        items.append(i)\n    other = items\n    items.append(1)\n    stdout.print(len(other), f\"{len(items)} long\")\n";
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
    let building = counts_of("packed_built", &format!("{source}    stdout.print(len(packed))\n"))?;
    let looped = counts_of(
        "packed_looped",
        &format!(
            "{source}    mut total = 0\n    for _ in range(100):\n        for at in range(len(packed)):\n            for item in packed[at]:\n                total += item\n    stdout.print(total)\n"
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
    same_as_pinned(
        "copies_kept",
        "def main(stdout: Stdout):\n    mut a = [1, 2, 3]\n    mut b = a\n    b[0] = 9\n    stdout.print(a, b)\n    mut c = [0 for _ in range(3)]\n    d = c\n    c[1] = 5\n    stdout.print(c, d)\n    mut keep: list[list[int]] = []\n    for i in range(3):\n        row = [i, i]\n        mut held = row\n        held[0] = 7\n        keep.append(held)\n        keep.append(row)\n    stdout.print(keep)\n",
    )
}

/// `==` with a literal of up to eight bytes reads the length and one word: empty, eight
/// bytes, nine, a zero byte and two-byte characters all compare as pinned.
#[test]
fn strings_compare_with_short_literals_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
/// negative indexes are each checked again, and the last read fails.
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
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
    }
    Ok(())
}

/// Pieces only ever added to a `list[str]` and joined with `""` are kept as that string, f-strings
/// and `str` going straight on its end: added to in a function given it, joined before more is
/// added, and alongside lists looked at some other way, or joined with something between.
#[test]
fn pieces_joined_once_match_what_was_pinned() -> std::io::Result<()> {
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
    return "-".join(items)

def put(ref out: list[str], more: list[str]):
    for item in more:
        out.append(item)

# Its list is given back, not joined, so `put` gets a list from here too.
def gathered(rows: list[list[str]]) -> list[str]:
    mut cells: list[str] = []
    for row in rows:
        put(ref cells, row)
    return cells

def main(stdout: Stdout):
    mut out = ["start "]
    add(3, ref out)
    stdout.print("".join(out))
    mut empty: list[str] = []
    stdout.print(len("".join(empty)))
    mut looked = ["a", "b"]
    looked.append("c")
    stdout.print(len(looked), looked[2], shown(looked), "".join(looked))
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
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
    }
    Ok(())
}

/// `for at, item in enumerate(list)` makes no pairs when the list stays as it is, and still does
/// when the pairs are kept, looked at whole, or the list grows while it's gone through.
#[test]
fn enumerates_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"def total(names: list[str]) -> int:
    mut subtotal = 0
    for at, name in enumerate(names):
        subtotal += at * len(name)
    return subtotal

def main(stdout: Stdout):
    names = ["fox", "kit", "den"]
    stdout.print(total(names))
    for at, name in enumerate(names):
        if at > 0:
            stdout.print(at, name)
    pairs = enumerate(names)
    stdout.print(len(pairs), pairs[1])
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
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
    }
    Ok(())
}

/// A list that's packed in native code, as it crosses to the JSON writer.
#[test]
fn json_of_packed_lists_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
/// list of plain structs, and failing on bad text, as pinned.
/// `json.dumps` writes into one text that grows in place, so writing twice as many items does
/// about twice the work, counted, not timed: each int's text was added to the writer's straight
/// from `str(value)`, which copied the whole text each time, so it allocated twice per item.
#[test]
fn json_dumps_grows_its_text_in_place() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let allocated = |count: usize| -> std::io::Result<(u64, u64)> {
        let source = format!(
            "from std import json\n\ndef main(stdout: Stdout):\n    items = [at for at in range({count})]\n    stdout.print(len(json.dumps(items)))\n    return\n"
        );
        let said = counts_of(&format!("json_dumps_linear_{count}"), &source)?;
        let number = |name: &str| {
            let mut words = said.split(' ');
            words.by_ref().find(|word| *word == name);
            words.next().and_then(|word| word.parse::<u64>().ok()).unwrap_or(u64::MAX)
        };
        Ok((number("allocations"), number("allocated_bytes")))
    };
    let (small, small_bytes) = allocated(10_000)?;
    let (big, big_bytes) = allocated(40_000)?;
    // A handful, whatever the count: the list, the text and its few regrowths.
    assert!(small < 100 && big < 100, "{small} and {big} allocations");
    // Four times the items, at most about four times the bytes, where copying the text for each
    // item made it sixteen times.
    assert!(big_bytes < small_bytes * 6, "{small_bytes} bytes, then {big_bytes}");
    Ok(())
}

#[test]
fn json_matches_what_was_pinned() -> std::io::Result<()> {
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
    return json.loads[Bag](text)?

def main(stdout: Stdout):
    b = Bag(items=[Item(name="a", heals=1)], at=Point(x=1, y=2))
    text = json.dumps(b)
    stdout.print(text, json.dumps(b.at), json.dumps([1, 2]), json.dumps("q"), json.dumps(3))
    back = load(text) except ParseError(): Bag(items=[], at=Point(x=0, y=0))
    stdout.print(back.items[0].name, back.at.y, back.count)
    points = json.loads[list[Point]]("[{\"x\": 3, \"y\": 4}]") except ParseError(): []
    stdout.print(len(points), points[0].x)
    failed = load("{") except ParseError(): Bag(items=[], at=Point(x=9, y=9))
    stdout.print(failed.at.x)
"#;
    for (name, how) in [("json", &["--llvm"][..]), ("json_release", &["--llvm", "--release"][..])] {
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
    }
    Ok(())
}

/// JSON read into dicts, by string, int, plain struct and tuple keys, and a dict a capability
/// gives back, as pinned. The variable is looked for by name in any case: Windows calls
/// it `Path`.
#[test]
fn json_into_dicts_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"from std import json

struct Spot:
    x: int
    y: int

def main(stdout: Stdout, env: Env):
    named = json.loads[dict[str, int]]("{\"ash\": 1, \"vex\": 3}") except ParseError(): {}
    stdout.print(named, named["vex"])
    levels = json.loads[dict[int, str]]("{\"1\": \"intro\", \"42\": \"boss\"}") except ParseError(): {}
    stdout.print(levels, levels[42])
    spots = {Spot(x=1, y=2): "tree", Spot(x=0, y=0): "rock"}
    back = json.loads[dict[Spot, str]](json.dumps(spots)) except ParseError(): {}
    stdout.print(back == spots, back[Spot(x=0, y=0)])
    wrong = json.loads[dict[str, int]]("[1, 2]") except ParseError(): {"none": 0}
    stdout.print(wrong)
    corners = {(0, 0): "start", (3, 4): "end"}
    got_value = json.loads[dict[(int, int), str]](json.dumps(corners)) except ParseError(): {}
    stdout.print(got_value, corners)
    placed = {(Spot(x=1, y=2), "a"): 1}
    got_value2 = json.loads[dict[(Spot, str), int]](json.dumps(placed)) except ParseError(): {}
    stdout.print((got_value2) == placed)
    stdout.print("PATH" in [name.upper() for name in env.all().keys()])
"#;
    for (name, how) in [("json_dicts", &["--llvm"][..]), ("json_dicts_release", &["--llvm", "--release"][..])]
    {
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
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
fn narrowed_capabilities_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r#"from std.process import Output

def narrowed(process: Process) -> Process:
    return process.only("echo")

def main(stdout: Stdout, process: Process):
    mut kept = 0
    for i in range(2000):
        echo = narrowed(process)
        again = echo
        if i % 500 == 0:
            found = again.run("echo", [f"{i}"]) except .NotRun() | .LooksLikeAnOption() | .TimedOut() | .TooMuchOutput(): Output()
            stdout.print(found.stdout.strip())
        kept += 1
    stdout.print(kept)
"#;
    for (name, how) in [("narrowed", &["--llvm"][..]), ("narrowed_release", &["--llvm", "--release"][..])] {
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
    }
    Ok(())
}

#[test]
fn arguments_and_the_clock_match_what_was_pinned() -> std::io::Result<()> {
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
    stdout.print(len(args), args.program != \"\")
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
    let pinned_outcome = pinned_outcome_of(&program, "arguments_and_clock")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "2 True\none\ntwo\none two\nTrue\n");
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
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
    exists_value = only.exists(\"/\") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): True
    stdout.print(exists_value)
    _ = only.read_text(\"/etc/hosts\") except hosts:
        stdout.print(hosts.message().endswith(\"native-capabilities\"))
        \"\"
    _ = fs.read_text(\"no such file\") except missing:
        stdout.print(missing.message() != \"\")
        \"\"
    fs.remove(file).expect(\"it works\")
    fs.remove(folder).expect(\"it works\")
    done = process.run(\"echo\", [\"hello\"]).expect(\"it works\")
    stdout.print(done.status, done.stdout, done.stderr == \"\")
    _ = process.run(\"no-such-program-at-all\", []) except unknown:
        stdout.print(unknown.program())
        done
    echo = process.only(\"echo\")
    stdout.print(echo.run(\"echo\", [\"again\"]).expect(\"it works\").stdout)
    _ = echo.run(\"ls\", []) except refused:
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
        |said: &std::sync::mpsc::Receiver<String>| said.recv_timeout(crate::executables::WALL_DEADLINE);
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
            assert!(started.elapsed() < crate::executables::WALL_DEADLINE, "it didn't end");
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
) -> std::io::Result<(fennec_temp::Temp, Running, u16, std::io::BufReader<std::process::ChildStdout>)> {
    if threads == "1" {
        assert!(crate::executables::threads_runtime_built(), "couldn't build the runtime with threads");
    }
    let dir = folder(name)?;
    let program = dir.join("serve.fen");
    std::fs::write(
        &program,
        "from std.net import Request, Response

def respond(request: Request) -> Response:
    return Response(status=200, body=request.path.encode())

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
    let mut server = Running(
        Command::new(&executable)
            .env("FENNEC_LIVE_REPORT", dir.join("live"))
            .stdout(std::process::Stdio::piped())
            .spawn()?,
    );
    let mut printed = std::io::BufReader::new(server.0.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?);
    let mut port = String::new();
    std::io::BufRead::read_line(&mut printed, &mut port)?;
    let port: u16 = port.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    Ok((dir, server, port, printed))
}

/// Waits until the live count a program writes to `report`, as `FENNEC_LIVE_REPORT` asks, says it's
/// serving at least `at_least` connections, so a test acts once the server has taken them rather
/// than after a guess at how long that takes. Fails if it never does.
#[cfg(unix)]
pub(crate) fn serving(report: &Path, at_least: usize) {
    let started = std::time::Instant::now();
    loop {
        let counts = std::fs::read_to_string(report).unwrap_or_default();
        let connections = counts
            .lines()
            .find_map(|line| line.strip_prefix("connections "))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        if connections >= at_least {
            return;
        }
        assert!(
            started.elapsed() < crate::executables::WALL_DEADLINE,
            "the server took {connections} of {at_least} connections:\n{counts}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Waits until the live count at `report` says `stopping 1`: the server has taken the stop and is
/// draining, on either runtime. Fails if it never does.
#[cfg(unix)]
pub(crate) fn stopping(report: &Path) {
    let started = std::time::Instant::now();
    loop {
        let counts = std::fs::read_to_string(report).unwrap_or_default();
        if counts.lines().any(|line| line == "stopping 1") {
            return;
        }
        assert!(
            started.elapsed() < crate::executables::WALL_DEADLINE,
            "the server never said it was stopping:\n{counts}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
    stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    // The server has taken each before it's asked to stop, as its live count says.
    serving(&dir.join("live"), 3);
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
    // The server has taken the stop before the request's finished, as its live count says.
    stopping(&dir.join("live"));
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
fn capabilities_called_natively_match_what_was_pinned() -> std::io::Result<()> {
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
    let pinned_outcome = pinned_outcome_of(&program, "stdin_lines")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
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
        return self.folder + \"/\" + name

def write(fs: Fs, path: str, text: str) fails IoError:
    with fs.create(path)? as file:
        file.write_text(text)?

def read(saves: Saves, name: str) -> str fails IoError:
    mut text = \"\"
    with saves.fs.open(saves.path(name))? as file:
        text = file.read_text()?
    return text

def main(stdout: Stdout, fs: Fs):
    folder = \"fs-by-name\"
    fs.make_dir(folder).expect(\"it works\")
    saves = Saves(fs=fs, folder=folder)
    write(fs, path=saves.path(\"a.txt\"), text=\"fox\").expect(\"it works\")
    stdout.print(read(saves, \"a.txt\").expect(\"it works\"), fs.read_text(Path(text=saves.path(\"a.txt\"))).expect(\"it works\"))
    only = fs.only(folder)
    write(only, path=folder + \"/b.txt\", text=\"den\").expect(\"it works\")
    stdout.print(read(Saves(fs=only, folder=folder), \"b.txt\").expect(\"it works\"))
    write(only, path=\"outside.txt\", text=\"no\") except outside:
        stdout.print(\"outside what this Fs can reach\" in outside.message())
    stdout.print(fs.exists(\"outside.txt\").expect(\"it works\"))
    fs.remove(folder + \"/a.txt\").expect(\"it works\")
    fs.remove(folder + \"/b.txt\").expect(\"it works\")
    fs.remove(folder).expect(\"it works\")
";

#[test]
fn fs_declared_in_std_is_found_by_name_natively() -> std::io::Result<()> {
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
    let pinned_outcome = pinned_outcome_of(&program, "fs_by_name")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "fox fox\nden\nTrue\nFalse\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
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
    blocked(fs) except problem:
        stdout.print(\"the block failed:\", problem.message() != \"\")
    read_text_value = fs.read_text(\"taken.txt/inside.txt\") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): \"gone\"
    stdout.print(read_text_value)
    # The file written beside the path, to take its place, is gone.
    names = fs.list_dir(\".\") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): []
    stdout.print(len([name for name in names if str(name).endswith(\".fennec-partial\")]))
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
    return fs.metadata(\"big.txt\")?.size

def main(stdout: Stdout, fs: Fs):
    write_big_value = write_big(fs) except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): -1
    stdout.print(write_big_value)
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
    if let Some(peak) = measured(peak) {
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
    return found

def main(stdout: Stdout, fs: Fs):
    hashes_value = hashes(fs) except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): []
    for found in hashes_value:
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
    // The same program hashing a file of one chunk, a megabyte, for what any run holds anyway.
    let small = dir.join("hash_small.fen");
    std::fs::write(&small, HASH_BIG_FILE.replace("range(64)", "range(1)"))?;
    let small_executable = dir.join("hash_small");
    let built = fennec(&["build", "--llvm", &path(&small), "-o", &path(&small_executable)], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (_, small_peak) = with_peak_memory(&small_executable, &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        BIG_FILE_HASHES,
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let (Some(peak), Some(small_peak)) = (measured(peak), measured(small_peak)) else { return Ok(()) };
    let mb = |bytes: u64| bytes / (1024 * 1024);
    // Hashing 64 MB a chunk at a time holds hardly more than hashing one chunk. Measured as what
    // it adds, since AddressSanitizer adds a share of its own to every run, which isn't what the
    // program holds: 13 MB hashing 64 MB here, and 21 MB under the sanitizer, 35 MB in CI's.
    assert!(
        peak.saturating_sub(small_peak) < 16 * 1024 * 1024,
        "hashing 64 MB held {} MB, against {} MB for one",
        mb(peak),
        mb(small_peak)
    );
    if std::env::var_os("FENNEC_SANITIZE").is_none() {
        assert!(peak < 32 * 1024 * 1024, "hashing 64 MB held {} MB", mb(peak));
    }
    Ok(())
}

/// The peak `with_peak_memory` read, which only Windows, with no `/usr/bin/time`, goes without.
/// Anywhere else a peak that wasn't read fails, so a memory limit is never passed unchecked.
fn measured(peak: Option<u64>) -> Option<u64> {
    assert!(
        peak.is_some() || cfg!(windows),
        "no peak memory was read: /usr/bin/time is missing, or what it printed has changed"
    );
    peak
}

/// What a program printed, and the most memory it held, where `/usr/bin/time` can say.
fn with_peak_memory(executable: &Path, dir: &Path) -> std::io::Result<(Output, Option<u64>)> {
    peak_memory_of(executable, &[], dir)
}

/// What a command printed, given `args` and run with `dir` as its folder and its Fennec home,
/// and the most memory it held, where `/usr/bin/time` can say.
fn peak_memory_of(program: &Path, args: &[&str], dir: &Path) -> std::io::Result<(Output, Option<u64>)> {
    peak_memory_with(program, args, &[], dir)
}

/// [`peak_memory_of`], with these set in the command's environment.
fn peak_memory_with(
    program: &Path,
    args: &[&str],
    set: &[(&str, &str)],
    dir: &Path,
) -> std::io::Result<(Output, Option<u64>)> {
    let time = Path::new("/usr/bin/time");
    let flag = if cfg!(target_os = "macos") { "-l" } else { "-v" };
    if cfg!(windows) || !time.exists() {
        let ran = Command::new(program)
            .args(args)
            .current_dir(dir)
            .env("FENNEC_HOME", dir)
            .envs(set.iter().copied())
            .output_within()?;
        return Ok((ran, None));
    }
    let ran = Command::new(time)
        .arg(flag)
        .arg(program)
        .args(args)
        .current_dir(dir)
        .env("FENNEC_HOME", dir)
        .envs(set.iter().copied())
        .output_within()?;
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
    return at

## What a round answered, or -1 when it fails.
def answered_by(fs: Fs, order: shared Order, at: int) -> int:
    done = work(fs, order, at) except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other(): -1
    return done

def main(stdout: Stdout, fs: Fs):
    shared order = Order()
    answered = [answered_by(fs, order, at) for at in parallel([0, 1, 2, 3])]
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
    return x * 2

def all_of(xs: list[int]) -> list[int] fails TooBig:
    return [checked(x)? for x in parallel(xs)]

def main(stdout: Stdout):
    small = [i for i in range(50_000)]
    big = [i for i in range(100_000)]
    made: list[int]? = all_of(small) except error:
        stdout.print(\"failed\", error.at)
        None
    if made is not None:
        stdout.print(len(made), made[49_999])
    all_made = all_of(big) except error:
        stdout.print(\"failed\", error.at, error.why)
        return
    stdout.print(len(all_made))
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

/// What the next test runs: a `parallel` loop's body printing, and an ordered stream's read until a
/// `break`, each with a line printed after it.
const PRINTING_BODIES: &str = r#"def streamed(stdout: Stdout, n: int) -> int:
    stdout.print("streamed", n)
    return n * n

def main(stdout: Stdout):
    numbers = [n for n in range(200)]
    for n in parallel(numbers):
        stdout.print("loop", n)
    stdout.print("after loop")
    many = [n for n in range(20000)]
    for square in (streamed(stdout, n) for n in parallel(many)):
        if square > 900:
            break
    stdout.print("after break")
"#;

/// What `parallel` bodies print follows real call order, so it's compared sorted, but each print
/// comes out whole, none is lost, and everything printed after a loop or a stream comes after all
/// that its bodies printed, with workers writing as they go and any number of them. A stream's
/// chunks the reader never took may still have run, and print, before its `break` ends them.
#[test]
fn what_parallel_bodies_print_comes_out_whole_and_before_what_follows() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("printing-bodies")?;
    let program = dir.join("printing.fen");
    std::fs::write(&program, PRINTING_BODIES)?;
    let executable = dir.join("printing");
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
    let looped: Vec<String> = (0..200).map(|n| format!("loop {n}")).collect();
    for threads in ["1", "4", "16"] {
        for _ in 0..20 {
            let ran = Command::new(&executable)
                .current_dir(&dir)
                .env("FENNEC_THREADS", threads)
                .env("FENNEC_PARALLEL_STRESS", "1")
                .output_within()?;
            let said = String::from_utf8_lossy(&ran.stderr);
            let out = String::from_utf8_lossy(&ran.stdout);
            let lines: Vec<&str> = out.lines().collect();
            let after_loop = lines.iter().position(|line| *line == "after loop").expect(&out);
            let after_break = lines.iter().position(|line| *line == "after break").expect(&out);
            let mut in_loop: Vec<&str> = lines[..after_loop].to_vec();
            in_loop.sort_by_key(|line| line.split(' ').nth(1).and_then(|n| n.parse::<u32>().ok()));
            assert_eq!(in_loop, looped, "{threads} threads: {said}\n{out}");
            let mut seen: Vec<u32> = lines[after_loop + 1..after_break]
                .iter()
                .map(|line| line.strip_prefix("streamed ").and_then(|n| n.parse().ok()).expect(line))
                .collect();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), after_break - after_loop - 1, "each printed once: {out}");
            assert!((0..=31).all(|n| seen.contains(&n)), "{threads} threads: every item read printed\n{out}");
            assert_eq!(after_break + 1, lines.len(), "{threads} threads: nothing after: {said}\n{out}");
        }
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
    return found

test "rows come back as structs, their values bound"(fs: Fs):
    made(fs, "index.db")?
    sneaky = "a'; drop table entries; --"
    add(fs, "index.db", [Entry(path=sneaky, size=3, status=.Clean), Entry(path="b", size=10, status=.Changed)])?
    found = all_entries(fs, "index.db")?
    assert len(found) == 2
    assert found[0].path == sneaky and found[0].size == 3 and found[0].status == .Clean
    assert found[1].path == "b" and found[1].status == .Changed and found[1].note is None

test "each test has its own databases"(fs: Fs):
    made(fs, "index.db")?
    assert len(all_entries(fs, "index.db")?) == 0

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
    assert len(found) == 1 and found[0].size == 1

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
    assert len(found) == 2 and found[0].note is None and found[1].note == "hi"
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
    return seen[0]

## A round's count, or -2 when it fails.
def counted(db: sqlite.Database, at: int) -> int:
    count = round(db, at) except .Constraint() | .Busy() | .Column() | .Failed(): -2
    return count

def rounds(fs: Fs, stdout: Stdout) fails SqlError:
    with sqlite.open(fs, "rounds.db")? as db:
        _ = db.execute(t"create table items (round integer, item integer)")?
        counts = [counted(db, at) for at in parallel(range(32))]
        stdout.print(min(counts) >= 20, max(counts) == 640)
        stdout.print(db.query[int](t"select count(*) from items")?, db.query[int](t"select count(distinct round) from items")?)

def main(stdout: Stdout, fs: Fs):
    rounds(fs, stdout) except err:
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
            return f"constraint {len(reason) > 0}"
        case .Column(name=name, reason=reason):
            return f"column {name}: {reason}"
        case .Busy():
            return "busy"
        case .Failed(reason=reason):
            return f"failed {len(reason) > 0}"

## What `run` failed with, by `said`, or `worked` when it didn't fail.
def told[T](run: () -> T fails SqlError, worked: str) -> str:
    _ = run() except why:
        return said(why)
    return worked

## What a failure says, whichever kind it is.
def said_of(error: SqlError | Timeout) -> str:
    match error:
        case Timeout(waited=_):
            return "no connection came free"
        case Constraint(reason=reason) | Busy(reason=reason) | Failed(reason=reason):
            return reason
        case Column(name=name, reason=reason):
            return f"column {name}: {reason}"

def rows_in(db: postgres.PgDatabase) -> int fails SqlError:
    return db.query[Count](t"select count(*) as total from fennec_entries")?[0].total

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
    return f"postgres://{server.user}@{server.host}:{server.port}/{server.database}"

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
                stdout.print(entry.path, entry.size, entry.weight, len(entry.blob), entry.blob[1], entry.seen)
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
            adds_then_fails(db) except rolled_back:
                stdout.print(said(rolled_back), rows_in(db)?)
            kept = "kept"
            with db.transaction()? as tx:
                stdout.print(tx.execute(t"update fennec_entries set path = {kept} where path = {path}")?)
            stdout.print(rows_in(db)?, db.query[Count](t"select count(*) as total from fennec_entries where path = {kept}")?[0].total)

## Signs in with `secret`, and says how it went.
def signing_in(server: Server, secret: Secret[str]?, net: Net, entropy: Entropy) -> str:
    _ = signed_in(server, secret=secret, net=net, entropy=entropy) except error:
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
    port = int(rest.split("/")[0]) except InvalidValue(): 5432
    server = Server(
        host=place.split(":")[0],
        port=port,
        user=who.split(":")[0],
        database=rest.split("/")[1],
        secret=Secret.of(who.split(":")[1]),
    )
    _ = run(server, stdout=stdout, net=net, entropy=entropy) except error:
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
    _ = process.run(\"pwd\", [], cwd=\"no-such-folder\") except nowhere:
        stdout.print(nowhere.message())
        all
    only = process.only(\"cat\")
    stdout.print(only.run(\"cat\", [], input=\"narrowed\\n\").expect(\"it works\").stdout)
    _ = only.run(\"pwd\", [], cwd=\"inside\") except refused:
        stdout.print(refused.message())
        all
    fs.remove(\"inside\").expect(\"it works\")
";

#[test]
fn process_options_called_natively_match_what_was_pinned() -> std::io::Result<()> {
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
    let pinned_outcome = pinned_outcome_of(&program, "process_options")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "one\ntwo\n\nTrue\nred\n\nTrue True\nthere's no folder `no-such-folder` to run it in\nnarrowed\n\nthis Process can run only `cat`, so it can't run `pwd`\n"
    );
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
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
    _ = only.walk(\"tree\") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other():
        stdout.print(\"refused\")
        []
    _ = fs.walk(\"no such folder\") except missing:
        stdout.print(missing.message() != \"\")
        []
";

#[test]
#[cfg(unix)]
fn walking_a_folder_natively_matches_what_was_pinned() -> std::io::Result<()> {
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
    let pinned_outcome = pinned_outcome_of(&program, "walked")?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "tree/a.txt False\ntree/b True\ntree/b/c True\ntree/b/c/deep.txt False\ntree/b/up True\ntree/b/z.txt False\n\
         [\"tree/b/c\", \"tree/b/c/deep.txt\", \"tree/b/up\", \"tree/b/z.txt\"]\nrefused\nTrue\n"
    );
    assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome);
    Ok(())
}

/// The real network, natively: a server and a client on this machine, each
/// way a call can come out, and looping over each.
const REAL_NETWORK: &str = "from std.net import NetError, Connection, Server
from std.time import Duration

def reason[T](run: () -> T fails NetError) -> str:
    _ = run() except why:
        return why.message()
    return \"it worked\"

def lines_of(connection: Connection) -> int fails NetError:
    mut count = 0
    for line in connection.lines()?:
        count += len(line)
    return count

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
    return 7

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
    return 0

def main(net: Net, stdout: Stdout):
    stdout.print(reason(() => talk(net, stdout)?))
    port = looped(net, stdout) except .NotFound() | .Refused() | .TimedOut() | .Closed | .Denied() | .Tls() | .Status() | .Other(): 0
    stdout.print(reason(() => connecting(net, host=\"127.0.0.1\", port=port)?) == f\"`127.0.0.1` refused a connection on port {port}\")
";

#[test]
fn the_real_network_works_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("real_network", REAL_NETWORK)
}

/// `bytes` over TCP and UDP natively, against what was pinned: what isn't text, reads of none and
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
                stdout.print(data, len(data), data[1])
                decode_value = conn.read(1, timeout=2s)?.decode() except InvalidValue(): \"\"
                stdout.print(decode_value)
                stdout.print(len(conn.read(0)?))
                client.close()
                stdout.print(len(conn.read(4, timeout=2s)?))
    with net.udp(port=0)? as first:
        with net.udp()? as second:
            first.send_to(\"ping\", f\"127.0.0.1:{second.port}\")?
            packet = second.receive(timeout=2s)?
            decode_value2 = packet.data.decode() except InvalidValue(): \"\"
            stdout.print(decode_value2, packet.sender.endswith(str(first.port)))
            second.send_to(bytes([1, 2, 3]), packet.sender)?
            answer = first.receive(timeout=2s)?
            stdout.print(answer.data, answer.data[0])
            _ = first.receive(timeout=20ms) except waited:
                stdout.print(waited.message())
                answer
            first.send_to(\"x\", \"no-such-host.invalid:9\") except unsent:
                stdout.print(unsent.message())
            first.send_to(\"last\", f\"127.0.0.1:{second.port}\")?
            for packet in second:
                decode_value3 = packet.data.decode() except InvalidValue(): \"\"
                stdout.print(decode_value3)
                second.close()

def main(net: Net, stdout: Stdout):
    mut talked_fine = True
    talk(net, stdout) except talked:
        stdout.print(talked.message())
        talked_fine = False
    if talked_fine:
        stdout.print(\"done\")
    bound(net, port=99999) except .NotFound() | .Refused() | .TimedOut() | .Closed | .Denied() | .Tls() | .Status() | .Other():
        pass
";

#[test]
fn bytes_and_packets_cross_the_real_network_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("network_bytes", NETWORK_BYTES)
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
    return Response(status=200, body=request.path.encode())

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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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

/// A native server gives a whole request, its head and body, one `timeout`, so a client sending a
/// body a byte at a time is cut off once it passes rather than held for as long as it keeps sending.
#[test]
fn a_native_server_cuts_off_a_request_sent_a_byte_at_a_time() -> std::io::Result<()> {
    use std::io::{ErrorKind, Read, Write};
    if !ready() {
        return Ok(());
    }
    let source = r"from std.net import Request, Response

def respond(request: Request) -> Response:
    return Response(status=200, body=request.path.encode())

def main(net: Net, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=1s)
";
    let dir = folder("http_server_trickle")?;
    let program = dir.join("serve.fen");
    std::fs::write(&program, source)?;
    let (program, executable) = (program.to_string_lossy().into_owned(), dir.join("serve"));
    let built = fennec(&["build", "--llvm", &program, "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut server = Running(Command::new(&executable).stdout(std::process::Stdio::piped()).spawn()?);
    let port: u16 = server.first_line()?.trim().parse().map_err(|_| ErrorKind::InvalidData)?;
    let mut trickle = std::net::TcpStream::connect(("127.0.0.1", port))?;
    trickle.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
    trickle.write_all(b"POST /slow HTTP/1.1\r\nContent-Length: 1000\r\n\r\n")?;
    // A byte of the body each tenth of a second, each well within the timeout, for up to 20
    // seconds: with only each read timed, each read gets its byte and this never ends.
    let started = std::time::Instant::now();
    let mut heard = Vec::new();
    let mut closed = false;
    while started.elapsed() < std::time::Duration::from_secs(20) {
        if trickle.write_all(b"x").is_err() {
            closed = true;
            break;
        }
        let mut buffer = [0; 512];
        match trickle.read(&mut buffer) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(read) => heard.extend_from_slice(&buffer[..read]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => {
                closed = true;
                break;
            }
        }
    }
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    assert!(closed, "the server still held a request sent a byte at a time after 20 seconds");
    assert!(!String::from_utf8_lossy(&heard).contains("200 OK"), "{}", String::from_utf8_lossy(&heard));
    Ok(())
}

/// How many writes a run asked of its connections, from what it said under `FENNEC_COUNTS`.
fn connection_writes(said: &str) -> Option<u64> {
    let line = said.lines().find(|line| line.starts_with("copies "))?;
    let mut words = line.split(' ');
    words.by_ref().find(|word| *word == "connection_writes")?;
    words.next()?.parse().ok()
}

/// How many bytes a run allocated in all, from what it said under `FENNEC_COUNTS`.
#[cfg(unix)]
fn allocated_bytes(said: &str) -> Option<u64> {
    let line = said.lines().find(|line| line.starts_with("copies "))?;
    let mut words = line.split(' ');
    words.by_ref().find(|word| *word == "allocated_bytes")?;
    words.next()?.parse().ok()
}

/// A server reads a body that comes in many pieces into one buffer with room to grow, so a 10 MB
/// upload allocates a few times its size, where joining each piece onto what came before with
/// `+` copied all of it again for each piece: about 840 MB. The work is counted, not timed. It stops
/// the server with SIGTERM, so it runs where there are signals.
#[test]
#[cfg(unix)]
fn a_served_body_read_in_many_pieces_is_copied_about_once() -> std::io::Result<()> {
    use std::io::{BufRead, Read, Write};
    if !ready() {
        return Ok(());
    }
    let dir = folder("served_body_once")?;
    let server = built_in(
        &dir,
        "serve",
        r#"from std.net import Request, Response

def respond(request: Request) -> Response:
    return Response(status=200, body=f"{len(request.body)}".encode())

def main(net: Net, signals: Signals, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=60s, until=signals, grace=2s)
"#,
    )?;
    let mut serving = Running(
        Command::new(&server)
            .env("FENNEC_COUNTS", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    );
    let mut printed = std::io::BufReader::new(serving.0.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?);
    let mut port = String::new();
    printed.read_line(&mut port)?;
    let port: u16 = port.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let size = 10_000_000_usize;
    let mut client = std::net::TcpStream::connect(("127.0.0.1", port))?;
    client.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
    client.write_all(
        format!("POST /up HTTP/1.1\r\ncontent-length: {size}\r\nconnection: close\r\n\r\n").as_bytes(),
    )?;
    // In pieces of 16 KiB, so the server reads it in hundreds.
    for piece in vec![b'x'; size].chunks(16 * 1024) {
        client.write_all(piece)?;
    }
    let mut answer = String::new();
    client.read_to_string(&mut answer)?;
    assert!(answer.ends_with("10000000"), "{answer}");
    terminate(&serving)?;
    let mut said = String::new();
    serving.0.stderr.take().ok_or(std::io::ErrorKind::BrokenPipe)?.read_to_string(&mut said)?;
    std::fs::remove_dir_all(&dir)?;
    let allocated = allocated_bytes(&said).unwrap_or(u64::MAX);
    assert!(allocated < 8 * 10_000_000, "a 10 MB body allocated {allocated} bytes: {said}");
    Ok(())
}

/// `source` built natively in `dir` as `name`.
fn built_in(dir: &Path, name: &str, source: &str) -> std::io::Result<PathBuf> {
    let program = dir.join(format!("{name}.fen"));
    std::fs::write(&program, source)?;
    let executable = dir.join(name);
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    Ok(executable)
}

/// A response's head and body go out in one write, so it goes in one packet: written apart, the
/// body waits for the client to acknowledge the head, which it may hold back for 40 ms. Counted
/// under `FENNEC_COUNTS`: three responses, two with a body, are three writes.
#[cfg(unix)]
#[test]
fn a_response_goes_out_in_one_write() -> std::io::Result<()> {
    use std::io::{BufRead, Read, Write};
    if !ready() {
        return Ok(());
    }
    let dir = folder("one_write_response")?;
    let server = built_in(
        &dir,
        "serve",
        r#"from std.net import Request, Response

def respond(request: Request) -> Response:
    match request.path:
        case "/missing":
            return Response(status=404)
        case _:
            return Response(status=200, body=f"at {request.path}".encode())

def main(net: Net, signals: Signals, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=60s, until=signals, grace=2s)
"#,
    )?;
    let mut serving = Running(
        Command::new(&server)
            .env("FENNEC_COUNTS", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    );
    let mut printed = std::io::BufReader::new(serving.0.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?);
    let mut port = String::new();
    printed.read_line(&mut port)?;
    let port: u16 = port.trim().parse().map_err(|_| std::io::ErrorKind::InvalidData)?;
    let mut client = std::net::TcpStream::connect(("127.0.0.1", port))?;
    client.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
    client.write_all(
        b"GET /a HTTP/1.1\r\n\r\nGET /missing HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\nconnection: close\r\n\r\n",
    )?;
    let mut answers = String::new();
    client.read_to_string(&mut answers)?;
    assert_eq!(answers.matches("HTTP/1.1 ").count(), 3, "{answers}");
    terminate(&serving)?;
    let mut said = String::new();
    serving.0.stderr.take().ok_or(std::io::ErrorKind::BrokenPipe)?.read_to_string(&mut said)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(connection_writes(&said), Some(3), "{said}");
    Ok(())
}

/// A request's head and body go out in one write too: two posts with a body are two writes.
#[test]
fn a_request_goes_out_in_one_write() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    // A server here, which answers each post once it's all come, and closes.
    let answering = std::thread::spawn(move || -> std::io::Result<()> {
        for _ in 0..2 {
            let (mut conn, _) = listener.accept()?;
            conn.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
            let mut heard = Vec::new();
            let mut buffer = [0; 4096];
            while !String::from_utf8_lossy(&heard).ends_with("\r\n\r\ndata") {
                let got = conn.read(&mut buffer)?;
                if got == 0 {
                    break;
                }
                heard.extend_from_slice(buffer.get(..got).unwrap_or_default());
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")?;
        }
        Ok(())
    });
    let dir = folder("one_write_request")?;
    let poster = built_in(
        &dir,
        "post",
        r#"from std.net import NetError

def main(net: Net, args: Args, stdout: Stdout) fails NetError:
    port = int(args[0]) except InvalidValue(): 0
    for path in ["/a", "/b"]:
        answer = net.post(f"http://127.0.0.1:{port}{path}", body="data", timeout=10s)?
        stdout.print(answer.status, answer.text())
"#,
    )?;
    let ran = Command::new(&poster).arg(port.to_string()).env("FENNEC_COUNTS", "1").output_within()?;
    answering.join().map_err(|_| std::io::Error::other("the answering thread panicked"))??;
    std::fs::remove_dir_all(&dir)?;
    let said = String::from_utf8_lossy(&ran.stderr);
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "200 ok\n200 ok\n", "{said}");
    assert_eq!(connection_writes(&said), Some(2), "{said}");
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
            return Response(status=200, body=f"hello, {name}".encode())
        case "/echo":
            return Response(status=201, headers={"x-method": request.method}, body=request.body)
        case _:
            return Response(status=404)

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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    return Response(status=200, body=request.path.encode())

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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    return Response(status=200, body=request.path.encode())

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
    stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
        "struct Person:\n    name: str\n\ndef rename(person: shared Person, first: str, second: str):\n    lock person as ref current:\n        current.name = f\"{first}{second}\"\n\ndef main(stdout: Stdout):\n    shared person = Person(name=\"a\")\n    done = [at for at in parallel(range(2))]\n    rename(person, first=\"a name that is long enough \", second=\"to need a block of its own\")\n    lock person as held:\n        stdout.print(len(done), held.name)\n",
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
        return Response(status=200, body=f"{current.count} {current.last} {current.names}".encode())

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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    conn.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    let deadline = std::time::Instant::now() + crate::executables::WALL_DEADLINE;
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
    stdout.print(answer.status, answer.ok(), answer.text(), answer.headers["x-why"], len(answer.body))
    answer = net.post("http://127.0.0.1:{port}/b", body="data")?
    stdout.print(answer.status, answer.ok(), answer.text(), answer.headers["x-why"])
    answer = net.request("put", "http://127.0.0.1:{port}/c", body="bytes".encode())?
    stdout.print(answer.status, answer.text())

def main(net: Net, stdout: Stdout):
    ask(net, stdout) except error:
        stdout.print(error.message())
    _ = net.get("http://127.0.0.1:1/") except refused:
        stdout.print(refused.message())
        Response(status=0)
    _ = net.get("not a url") except not_a_url:
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
    send(net, stdout) except error:
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
    let deadline = std::time::Instant::now() + crate::executables::WALL_DEADLINE;
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
fn a_built_program_prints_what_it_should() -> std::io::Result<()> {
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
    stdout.print([Tag(name=\"a\")], {\"key\": 1}, len(args))
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
    return \"nothing\"

def main(net: Net, stdout: Stdout):
    items = [i for i in range(16)]
    lines = [fetch(net, {port}).expect(\"it answers\") for i in parallel(items)]
    stdout.print(len(lines), lines[0], lines[15])
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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
        stream.write_all(format!("{line}\n").as_bytes())?;
        let mut back = String::new();
        let _ = std::io::BufReader::new(stream).read_line(&mut back);
        Ok(back)
    };
    let answers: Vec<String> =
        ["a", "skip", "b", "stop"].into_iter().map(ask).collect::<std::io::Result<_>>()?;
    assert_eq!(answers, ["a\n", "", "b\n", ""]);
    // After `break`, the loop ends, and the program goes on to its end and stops by itself.
    let deadline = std::time::Instant::now() + crate::executables::WALL_DEADLINE;
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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
        let Ok(line) = lines.recv_timeout(crate::executables::WALL_DEADLINE) else { break };
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
    return x

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
    ping.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    return big + n * 1000

def answer(line: str) -> str:
    words = [f\"{line} {at}\" for at in range(8)]
    seen = {word: len(word) for word in words}
    said = f\"{words} {len(seen)}\"
    return f\"{overflowed(len(line))} {said}\"

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
                stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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

/// A generic `with` source and a generic function's copy hold the types they're given
/// themselves, never their text: a program's own `Delivery`, which the checker holds under a
/// name a program can't write, a field named by a keyword, and nested generics come through
/// unchanged.
#[test]
fn a_generic_source_and_copy_carry_any_type_the_checker_holds_through_unchanged() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "held_any_type",
            r#"# The library has a `Delivery` too: this one is the program's own.
struct Delivery:
    parcel: str
    if: int

def held[T](value: T) -> with T:
    lend value

def first[T](items: list[T]) -> T:
    return items[0]

def main(stdout: Stdout):
    with held(Delivery(parcel="box", if=3)) as delivery:
        stdout.print(delivery.parcel, delivery.if)
    nested: dict[str, list[(int, Delivery)]] = {"a": [(1, Delivery(parcel="x", if=1))]}
    with held(nested) as kept:
        stdout.print(len(kept["a"]), kept["a"][0][1].if)
    stdout.print(first([Delivery(parcel="y", if=2)]).parcel)
    stdout.print(first([[(1, Delivery(parcel="z", if=4))]])[0][1].parcel)
"#
        )?,
        "box 3\n1 1\ny\nz\n"
    );
    Ok(())
}

/// A program's own types named like the library's, `Delivery`, `Duration` and `Match`, go through
/// a launched task's inbox and a `with` of two channels. Each is checked under a name no source
/// can write, and the source the checker writes in for a `with launch` and for a channel's type
/// names it that way too; the library's own `Duration`, which `stop_after` takes, stays the
/// library's. A channel of one once kept the checker going round until it ran out of memory.
#[test]
fn a_program_type_named_like_a_library_type_goes_through_inboxes_and_channels() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds(
        "renamed_inbox",
        r#"from std.concurrent import Closed, Crashed, Inbox, channel

struct Delivery:
    user: int

struct Match:
    at: int

enum Duration:
    Short
    Long(minutes: int)

def quiet(inbox: Inbox[Delivery], stdout: Stdout):
    mut total = 0
    for delivery in inbox:
        total += delivery.user
    stdout.print(total)
    return

def timed(inbox: Inbox[Duration], stdout: Stdout):
    for length in inbox:
        match length:
            case .Short:
                stdout.print("short")
            case .Long(minutes):
                stdout.print("long", minutes)
    return

def main(stdout: Stdout) fails Closed | Crashed:
    with launch((inbox) => quiet(inbox, stdout), inbox_size=4)? as first:
        first.send(Delivery(user=1))?
        first.send(Delivery(user=2))?
    with launch((inbox) => timed(inbox, stdout), stop_after=5s, inbox_size=4)? as second:
        second.send(.Short)?
        second.send(.Long(minutes=3))?
    with channel[Delivery](size=4) as deliveries, channel[list[Match]](size=2) as matches:
        deliveries.send(Delivery(user=5))?
        matches.send([Match(at=7), Match(at=8)])?
        delivery = deliveries.receive() else return
        found = matches.receive() else return
        stdout.print(delivery.user, len(found), found[1].at)
    return
"#,
        "3\nshort\nlong 3\n5 2 8\n",
    )
}

/// Checking a channel of a program's own type named like a library type takes the memory checking
/// anything small does. The checker once went round and round on it, checking the whole program
/// again a dozen times a second, past 2 GB in seconds, because the source it wrote in for the
/// channel's type couldn't be parsed; a check that never settles can't come back unseen.
#[test]
fn checking_a_channel_of_a_type_named_like_the_librarys_settles() -> std::io::Result<()> {
    let dir = folder("renamed_channel_check")?;
    let program = dir.join("renamed.fen");
    std::fs::write(
        &program,
        "from std.concurrent import Closed, channel\n\nstruct Match:\n    at: int\n\n\
         def main(stdout: Stdout) fails Closed:\n    with channel[Match](size=2) as matches:\n\
         \x20       matches.send(Match(at=7))?\n        found = matches.receive() else return\n\
         \x20       stdout.print(found.at)\n    return\n",
    )?;
    runtime_built();
    let fennec = Path::new(env!("CARGO_BIN_EXE_fennec"));
    let (checked, peak) = peak_memory_of(fennec, &["check", &program.to_string_lossy()], &dir)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(checked.status.success(), "{}", String::from_utf8_lossy(&checked.stderr));
    if let Some(peak) = measured(peak) {
        assert!(peak < 400 << 20, "checking it held {} MB", peak >> 20);
    }
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
    mut received = 0
    for n in inbox:
        received += n
    stdout.print(\"sum\", received)

def main(stdout: Stdout) fails Closed | Crashed | Muted | Timeout:
    with launch((inbox) => audio(inbox, stdout)?, stop_after=5s, inbox_size=4)? as sounds:
        sounds.send(.Play(name=\"jump\", volume=0.8))?
        sounds.send(.StopAll)?
    with launch(() => tick(stdout))?:
        pass
    loud(stdout) except too_loud:
        stdout.print(\"failed:\", too_loud)
    crashing(stdout) except crashed:
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

/// Launches whose messages and errors are of types a text form can't carry, for the test below.
const LAUNCHED_ANY_TYPE: &str = "from std.concurrent import Closed, Crashed, Inbox, Timeout

# The library has a `Delivery` too: this one is the program's own.
struct Delivery:
    parcel: str
    if: int

error Lost:
    where: str

error Late:
    by: int

def deliver(inbox: Inbox[Delivery], stdout: Stdout) fails Lost | Late:
    for delivery in inbox:
        if delivery.if > 9: fail Lost(where=delivery.parcel)
        stdout.print(f\"{delivery.parcel} {delivery.if}\")

def tally(inbox: Inbox[dict[str, list[(int, Delivery)]]], stdout: Stdout):
    for batch in inbox:
        for name, items in batch.items():
            stdout.print(name, len(items))

def main(stdout: Stdout) fails Closed | Crashed | Lost | Late | Timeout:
    with launch((inbox) => deliver(inbox, stdout)?, inbox_size=2)? as deliveries:
        deliveries.send(Delivery(parcel=\"box\", if=3))?
    with launch((inbox) => tally(inbox, stdout))? as batches:
        batches.send({\"a\": [(1, Delivery(parcel=\"x\", if=1)), (2, Delivery(parcel=\"y\", if=2))]})?
    with launch((inbox) => deliver(inbox, stdout)?)? as more:
        more.send(Delivery(parcel=\"far\", if=12))?
";

/// What a `with launch` is written in as holds the checked types themselves, never their text:
/// a program's own type named like a library one, which the checker holds under a name a
/// program can't write, a field named by a keyword, a message of nested generics, and a task that
/// fails with a union of errors all come through unchanged. Read back as text, the first didn't
/// build.
#[test]
fn a_launch_carries_any_type_the_checker_holds_through_unchanged() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("launched-any-type")?;
    let program = dir.join("shadow.fen");
    std::fs::write(&program, LAUNCHED_ANY_TYPE)?;
    let executable = dir.join("shadow");
    let mut command = crate::executables::fennec_command();
    command.args(["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()]);
    let built = command
        .env("FENNEC_HOME", &dir)
        .env_remove("FENNEC_THREADS_RUNTIME")
        .output_before(FENNEC_DEADLINE)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).current_dir(&dir).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "box 3\na 2\n");
    assert!(String::from_utf8_lossy(&ran.stderr).starts_with("error: `main` failed: Lost(where=\"far\")\n"));
    assert_eq!(ran.status.code(), Some(1));
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
        boxes.send(Box[T](value=value)) except Closed(): return fallback
        got = boxes.receive() else return fallback
        return got.value
    return fallback

def main(stdout: Stdout):
    stdout.print(boxed(3, fallback=0), boxed("fox", fallback=""), boxed([1, 2], fallback=[]))
"#,
    )?;
    assert_eq!(printed, "3 fox [1, 2]\n");
    Ok(())
}

/// A channel's messages cross it as the runtime holds them, of each kind: a number, a string long
/// enough for a block, a struct held in place, and a list. A `receive_within` or `send_within`
/// that waits past its time fails as `Timeout`.
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
        stdout.print(ch.receive() else fail Closed(), ch.receive() else fail Closed())

def signs(stdout: Stdout) fails Closed | Timeout:
    with channel[int](size=2) as ints:
        ints.send(-7)?
        stdout.print(ints.receive() else fail Closed())
    with channel[float](size=2) as floats:
        floats.send(-1.5)?
        stdout.print(floats.receive() else fail Closed())
    with channel[u64](size=2) as big:
        half: u64 = 9223372036854775807
        big.send(half + half + 1)?
        stdout.print(big.receive() else fail Closed())
    with channel[f32](size=2) as small:
        tenth: f32 = -0.1
        small.send(tenth)?
        stdout.print(small.receive() else fail Closed())
    with channel[int](size=1) as waits:
        got: int? = waits.receive_within(Duration(nanos=-5)) except problem:
            stdout.print(\"at once:\", problem.waited)
            None
        if got is not None:
            stdout.print(\"got\", got)

def words(stdout: Stdout) fails Closed | Timeout:
    with channel[str](size=4) as ch:
        ch.send(\"hi\")?
        ch.send(\"a string long enough to live in a block of its own\")?
        short = ch.receive() else fail Closed()
        long = ch.receive() else fail Closed()
        stdout.print(short, len(long))

def jobs(stdout: Stdout) fails Closed | Timeout:
    with channel[Job](size=2) as ch:
        ch.send(Job(name=\"first\", weight=3))?
        got = ch.receive() else fail Closed()
        stdout.print(got.name, got.weight)

def lists(stdout: Stdout) fails Closed | Timeout:
    with channel[list[float]](size=1) as ch:
        ch.send([1.5, 2.5])?
        stdout.print(ch.receive() else fail Closed())

def waits(stdout: Stdout) fails Closed | Timeout:
    with channel[int](size=1) as ch:
        got: int? = ch.receive_within(Duration(nanos=2_000_000)) except problem:
            stdout.print(\"empty:\", problem.waited)
            None
        if got is not None:
            stdout.print(\"got\", got)
        ch.send(1)?
        ch.send_within(2, Duration(nanos=2_000_000)) except full:
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
        "7 8\n-7\n-1.5\n18446744073709551615\n-0.1\nat once: -5ns\nhi 50\nfirst 3\n[1.5, 2.5]\nempty: 2ms\nfull: Timeout(waited=Duration(nanos=2000000))\n"
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
    return line

def serve(net: Net, stdout: Stdout) -> int fails NetError | Refused:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            conn.write(f\"{checked(line)?}\\n\")?
    return 0

def main(net: Net, stdout: Stdout):
    _ = serve(net, stdout) except error:
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
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
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
    let deadline = std::time::Instant::now() + crate::executables::WALL_DEADLINE;
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
/// std's collections among them, so a program reading and writing them builds natively.
#[test]
fn json_and_toml_of_plain_types_go_through_std_in_fennec() -> std::io::Result<()> {
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
    got_value = json.loads[list[Spot]](text) except ParseError(): []
    stdout.print(text, got_value)
    got_value2 = toml.loads[Spot](toml.dumps(Spot(x=2, tags=[]))) except ParseError(): Spot(x=0, tags=[])
    stdout.print(got_value2)
    board = Board(cells=Grid.of([[1, 2]]), scores=SortedMap.of([(\"b\", 2), (\"a\", 1)]), seen=SortedSet.of([3, 1]), queue=Deque.of([\"go\"]))
    saved = json.dumps(board)
    got_value3 = json.loads[Board](saved) except ParseError(): board
    stdout.print(saved, json.dumps(got_value3) == saved)
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

/// A tuple holding a program's own type saves and loads: written alone in `loads`' brackets, as
/// `json.loads[(int, Parcel)]`, which is one type, not two; in a list; and in a tuple in a dict,
/// through JSON and `std.pack`.
#[test]
fn tuples_holding_a_programs_types_save_and_load_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "tuples_of_program_types",
            r#"from std import json, pack

struct Parcel:
    name: str

struct Shipment:
    legs: list[(int, Parcel)]
    by_name: dict[str, (Parcel, list[(int, Parcel)])]

def main(stdout: Stdout):
    one = json.loads[(int, Parcel)]("[1, {\"name\": \"x\"}]") except ParseError(): return
    stdout.print(one[1].name)
    shipment = Shipment(legs=[(1, Parcel(name="a"))], by_name={"k": (Parcel(name="b"), [(2, Parcel(name="c"))])})
    text = json.dumps(shipment)
    stdout.print(text)
    back = json.loads[Shipment](text) except ParseError(): return
    stdout.print(back == shipment)
    packed = pack.loads[Shipment](pack.dumps(shipment)) except ParseError(): return
    stdout.print(packed == shipment)
    pairs = json.loads[list[(Parcel, int)]]("[[{\"name\": \"d\"}, 4]]") except ParseError(): return
    stdout.print(pairs[0][0].name, pairs[0][1])
"#
        )?,
        "x\n{\"legs\": [[1, {\"name\": \"a\"}]], \"by_name\": {\"k\": [{\"name\": \"b\"}, [[2, {\"name\": \"c\"}]]]}}\nTrue\nTrue\nd 4\n"
    );
    Ok(())
}

/// The `serialize` and `deserialize` written for a program's types hold their names, types and
/// fields themselves, never their text: fields named by keywords, in a struct and in an enum's
/// case, a name given with `@serialize` that has quotes in it, a program's own `Delivery`, a
/// union, and tuples inside dicts all save and load back, through JSON and `std.pack`. Read back
/// as text, none of it built.
#[test]
fn saving_carries_any_name_type_and_field_through_unchanged() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "saving_any_name",
            r#"from std import json, pack

struct Circle:
    radius: int

struct Square:
    side: int

enum Move:
    Stay
    Step(if: int, for: str)

# The library has a `Delivery` too: this one is the program's own.
struct Delivery:
    if: int
    @serialize(name="the \"label\"")
    label: str
    sizes: dict[str, list[(int, str)]]
    shape: Circle | Square
    moves: list[Move]
    for = "all"
    note: str? = None

def main(stdout: Stdout):
    delivery = Delivery(if=3, label="box", sizes={"a": [(1, "one")]}, shape=Square(side=2), moves=[Move.Stay, Move.Step(if=1, for="x")])
    text = json.dumps(delivery)
    stdout.print(text)
    back = json.loads[Delivery](text) except ParseError(): return
    stdout.print(back == delivery, back.if, back.for, back.label, back.sizes["a"][0][1])
    packed = pack.loads[Delivery](pack.dumps(delivery)) except ParseError(): return
    stdout.print(packed == delivery)
    _ = json.loads[Delivery]("{\"if\": 1}") except why:
        stdout.print(why)
        return
"#
        )?,
        "{\"if\": 3, \"the \\\"label\\\"\": \"box\", \"sizes\": {\"a\": [[1, \"one\"]]}, \"shape\": {\"Square\": {\"side\": 2}}, \
         \"moves\": [\"Stay\", {\"Step\": {\"if\": 1, \"for\": \"x\"}}], \"for\": \"all\", \"note\": null}\n\
         True 3 all box one\nTrue\nParseError(message=\"`the \\\"label\\\"` is missing\", line=0, column=0)\n"
    );
    Ok(())
}

/// The functions that save and load a tuple hold its types themselves, never their text: tuples
/// inside lists of tuples of dicts, and an optional list inside a tuple, save and load back, and a
/// wrong one is refused naming the tuple as it's written.
#[test]
fn tuples_of_nested_generics_save_and_load_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "tuples_nested",
            r#"from std import json

struct Route:
    legs: list[(int, list[(str, dict[str, int])])]
    corner: (int, (str, list[int]?))

def main(stdout: Stdout):
    route = Route(legs=[(1, [("a", {"x": 2})])], corner=(3, ("b", None)))
    text = json.dumps(route)
    stdout.print(text)
    back = json.loads[Route](text) except ParseError(): return
    stdout.print(back == route)
    _ = json.loads[Route]("{\"legs\": [[1]], \"corner\": [1, [\"b\", null]]}") except why:
        stdout.print(why)
        return
"#
        )?,
        "{\"legs\": [[1, [[\"a\", {\"x\": 2}]]]], \"corner\": [3, [\"b\", null]]}\nTrue\n\
         ParseError(message=\"`legs[0]` should be a (int, list[(str, dict[str, int])]), but it's a list\", line=0, column=0)\n"
    );
    Ok(())
}

/// A tuple saves as an array of its items, as in Python's `json.dumps`, and loads back, through
/// std's readers and writers in Fennec: in a list, in a field, inside
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
    got_value = json.loads[Path](text) except ParseError(): Path(steps=[], at=(0, (0, 0)))
    stdout.print(got_value)
    orders = [Order.Hold, Order.Move(to=(2, 3))]
    orders_back = json.loads[list[Order]](json.dumps(orders)) except ParseError(): []
    stdout.print(json.dumps(orders), orders_back == orders)
    corners = {(0, 0): \"start\", (3, 4): \"end\"}
    corners_back = json.loads[dict[(int, int), str]](json.dumps(corners)) except ParseError(): {}
    stdout.print(json.dumps(corners), corners_back == corners)
    mut by_side = SortedMap[(int, int), str]()
    by_side[(2, 0)] = \"b\"
    by_side[(1, 0)] = \"a\"
    stdout.print(json.dumps(by_side))
    stdout.print(json.dumps((1, None, \"x\")))
    got_value2 = json.loads[list[(int, int)]](\"[[1, 2, 3]]\") except ParseError(): [(0, 0)]
    stdout.print(got_value2)
    got_value3 = toml.loads[Path](toml.dumps(path)) except ParseError(): path
    stdout.print((got_value3) == path)
    got_value4 = pack.loads[Path](pack.dumps(path)) except ParseError(): Path(steps=[], at=(0, (0, 0)))
    stdout.print((got_value4) == path)
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
    return None

def main(stdout: Stdout):
    marks: list[(Spot, int?)] = [(Spot(x=1), counted(2)), (Spot(x=3), counted(0))]
    text = json.dumps(marks)
    stdout.print(text)
    marks_back = json.loads[list[(Spot, int?)]](text) except ParseError(): []
    stdout.print(marks_back == marks)
    held = shapes.Held(pair=(Spot(x=5), (counted(0), \"a\")))
    saved = json.dumps(held)
    held_back = json.loads[shapes.Held](saved) except ParseError(): held
    stdout.print(saved, held_back == held)
    packed_back = pack.loads[shapes.Held](pack.dumps(held)) except ParseError(): shapes.Held(pair=(Spot(x=0), (counted(1), \"\")))
    stdout.print(packed_back == held)
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
    stdout.print(pairs, len(saved) > 0, len(pack.dumps(pairs)) > len(saved))
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
    back = pack.loads[list[(int, str)]](pack.dumps(pairs)) except ParseError(): []
    stdout.print(back, back == pairs)
    got_value = pack.loads[list[(int, str)]](pack.dumps(empty)) except ParseError(): pairs
    stdout.print((got_value) == empty)
    got_value2 = pack.loads[list[(int, str)]](pack.dumps([1, 2])) except ParseError(): empty
    stdout.print((got_value2) == empty)
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

/// Native JSON and TOML write and read what was pinned, and fail with the same messages: a
/// field left out, a value of the wrong type, a key no field has, inside and outside, and text
/// that isn't JSON.
#[test]
fn native_json_and_toml_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    _ = run() except why:
        return why.message
    return "read"

def main(stdout: Stdout):
    ash = Unit(name="Ash \"the\" fox\n", level=3, kind=.Owl(wise=True), pet=None, at=Spot(x=1), tags=["a", "é"], stats={"hp": 10})
    text = json.dumps(ash)
    stdout.print(text)
    stdout.print(json.dumps([ash, ash], indent=2))
    back = json.loads[Unit](text) except ParseError(): ash
    stdout.print(back == ash, json.dumps(back) == text)
    stdout.print(said(() => json.loads[Unit]("{\"name\": \"a\"}")?))
    stdout.print(said(() => json.loads[Unit]("{\"name\": 1, \"level\": 2}")?))
    stdout.print(said(() => json.loads[Unit]("{\"name\": \"a\", \"at\": {\"x\": 1, \"z\": 2}, \"extra\": 1}")?))
    stdout.print(said(() => json.loads[Unit]("[1, 2")?))
    stdout.print(said(() => json.loads[list[int]]("[1, \"two\"]")?))
    got_value = json.loads[Spot]("{\"x\": 1, \"x\": 2}") except ParseError(): Spot(x=0)
    stdout.print(got_value)
    spot_text = toml.dumps(Spot(x=4))
    stdout.print(spot_text)
    got_value2 = toml.loads[Spot](spot_text) except ParseError(): Spot(x=0)
    stdout.print(got_value2)
    stdout.print(said(() => toml.loads[Spot]("x = \"a\"")?))
"#,
    )
}

/// Two known differences between native JSON and what was pinned for it, both only in what a
/// failed read says: a key given twice whose first value doesn't fit fails natively, where the
/// pinned outcome keeps the last value; and with two fields of the wrong type, native code names
/// the first in the text, where the pinned outcome names the first the type declares.
#[test]
fn json_fails_on_a_repeated_key_that_misfits_and_names_the_first_wrong_field_in_the_text()
-> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let ((ours, _, _), (wanted, _, _)) = both(
        "native_json_differences",
        r#"from std import json

struct Spot:
    x: int
    y = 2.5

def said(run: () -> Spot fails ParseError) -> str:
    _ = run() except why:
        return why.message
    return "read"

def main(stdout: Stdout):
    stdout.print(said(() => json.loads[Spot]("{\"x\": \"one\", \"x\": 2}")?))
    stdout.print(said(() => json.loads[Spot]("{\"y\": \"no\", \"x\": \"one\"}")?))
"#,
    )?;
    assert_eq!(ours, "`x` should be an int, but it's a str\n`y` should be a float, but it's a str\n");
    assert_eq!(wanted, "read\n`x` should be an int, but it's a str\n");
    Ok(())
}

/// A generic struct is shown by the name it's written with, never with its type arguments:
/// `Holder(item=3)`, not `Holder[int](item=3)`. One held in place took its layout's name, which
/// has the arguments, while one in a block, or inside a list or a dict, was shown without. Here
/// it holds an int, a string, a float, another generic struct and an array, alone and inside a
/// list, a dict, a tuple and an optional, through `print`, an f-string and `str()`.
#[test]
fn a_generic_struct_is_shown_without_its_type_arguments() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("generic_shown", GENERIC_SHOWN, GENERIC_SHOWN_PRINTS)
}

const GENERIC_SHOWN: &str = r#"struct Holder[T]:
    item: T

struct Pair[A, B]:
    left: A
    right: B

def main(stdout: Stdout):
    stdout.print(Holder(item=3))
    stdout.print(Holder(item="x"))
    stdout.print(Holder(item=2.5))
    stdout.print([Holder(item=1)])
    stdout.print(Holder(item=Holder(item=4)))
    stdout.print(Pair(left=1, right="one"))
    stdout.print({"a": Holder(item=1)}, (Holder(item=1), 2))
    maybe: Holder[int]? = Holder(item=9)
    stdout.print(maybe)
    number = Holder(item=3)
    text = Holder(item="x")
    stdout.print(f"{number} {text}", str(number), str(Holder(item=2.5)), str(text))
    grid: array[int, 2] = [1, 2]
    stdout.print(grid, [grid], Holder(item=grid))
    return
"#;

const GENERIC_SHOWN_PRINTS: &str = r#"Holder(item=3)
Holder(item="x")
Holder(item=2.5)
[Holder(item=1)]
Holder(item=Holder(item=4))
Pair(left=1, right="one")
{"a": Holder(item=1)} (Holder(item=1), 2)
Holder(item=9)
Holder(item=3) Holder(item="x") Holder(item=3) Holder(item=2.5) Holder(item="x")
[1, 2] [[1, 2]] Holder(item=[1, 2])
"#;

/// A generic enum is shown the same way, by the name it's written with: `Choice.Picked(value=7)`,
/// never `Choice[int].Picked(value=7)`. Its cases hold an int, a string, a float and another
/// generic enum, one has two type parameters, and one case holds nothing; alone, inside a generic
/// struct, a list, a dict, a tuple and an optional; through `print`, an f-string, `str()` and a
/// failed `assert`.
#[test]
fn a_generic_enum_is_shown_without_its_type_arguments() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("generic_enum_shown", GENERIC_ENUM_SHOWN, GENERIC_ENUM_SHOWN_PRINTS)?;
    let dir = folder("generic_enum_assert")?;
    let (program, executable) = (dir.join("generic_enum_assert.fen"), dir.join("generic_enum_assert"));
    std::fs::write(&program, GENERIC_ENUM_ASSERT)?;
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (printed, said, code) = outcome(&Command::new(&executable).output_within()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!((printed.as_str(), code), ("", Some(1)), "{said}");
    assert!(
        said.contains("the left side is Choice.Picked(value=3) and the right side is Choice.Picked(value=4)"),
        "{said}"
    );
    Ok(())
}

const GENERIC_ENUM_SHOWN: &str = r#"enum Choice[T]:
    Picked(value: T)
    Nothing

enum Either[A, B]:
    Left(value: A)
    Right(value: B)

struct Holder[T]:
    item: T

def none_of_ints() -> Choice[int]:
    return .Nothing

def main(stdout: Stdout):
    number = Choice.Picked(value=7)
    text = Choice.Picked(value="fox")
    ratio = Choice.Picked(value=2.5)
    nothing = none_of_ints()
    stdout.print(number, text, ratio, nothing)
    stdout.print(Choice.Picked(value=Choice.Picked(value=4)), Holder(item=Choice.Picked(value=1)))
    left: Either[int, str] = Either[int, str].Left(value=3)
    right: Either[int, str] = Either[int, str].Right(value="a text long enough for a block of its own")
    stdout.print(left, right)
    stdout.print([number, nothing], {"a": number}, (number, 2))
    maybe: Choice[int]? = number
    stdout.print(maybe)
    stdout.print(f"{number} {text} {nothing}", str(number), str(text), str(nothing))
    return
"#;

const GENERIC_ENUM_SHOWN_PRINTS: &str = r#"Choice.Picked(value=7) Choice.Picked(value="fox") Choice.Picked(value=2.5) Choice.Nothing
Choice.Picked(value=Choice.Picked(value=4)) Holder(item=Choice.Picked(value=1))
Either.Left(value=3) Either.Right(value="a text long enough for a block of its own")
[Choice.Picked(value=7), Choice.Nothing] {"a": Choice.Picked(value=7)} (Choice.Picked(value=7), 2)
Choice.Picked(value=7)
Choice.Picked(value=7) Choice.Picked(value="fox") Choice.Nothing Choice.Picked(value=7) Choice.Picked(value="fox") Choice.Nothing
"#;

const GENERIC_ENUM_ASSERT: &str = r#"enum Choice[T]:
    Picked(value: T)
    Nothing

def main(stdout: Stdout, args: Args):
    # No arguments are given, so this is 0, but the compiler can't know it.
    shift = len(args)
    assert Choice.Picked(value=3 + shift) == Choice.Picked(value=4)
    stdout.print("not reached")
    return
"#;

/// A failed `assert` says both sides the same way, with no type arguments.
#[test]
fn a_failed_assert_shows_a_generic_struct_without_its_type_arguments() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("generic_assert")?;
    let (program, executable) = (dir.join("generic_assert.fen"), dir.join("generic_assert"));
    std::fs::write(&program, GENERIC_ASSERT)?;
    let built =
        fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (printed, said, code) = outcome(&Command::new(&executable).output_within()?);
    std::fs::remove_dir_all(&dir)?;
    assert_eq!((printed.as_str(), code), ("", Some(1)), "{said}");
    assert!(said.contains("the left side is Holder(item=3) and the right side is Holder(item=4)"), "{said}");
    Ok(())
}

const GENERIC_ASSERT: &str = r#"struct Holder[T]:
    item: T

def main(stdout: Stdout, args: Args):
    # No arguments are given, so this is 0, but the compiler can't know it.
    shift = len(args)
    assert Holder(item=3 + shift) == Holder(item=4)
    stdout.print("not reached")
    return
"#;

/// A list of a union's values, whose members are generic or tuples, prints each value as itself,
/// from a literal or from fields, in `print` and in an f-string.
#[test]
fn lists_of_unions_with_generic_members_print_their_values() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
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
        return len(text)

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") except:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        return len(text) * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") except:
            pass

type Store = Lite | Postgres

struct Mem:
    size: int

    def run(self, text: str) -> int fails Nope:
        return len(text) * 3

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
    return ""

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
        return f"lite {self.name}"

struct Pg:
    host: str

    def label(self) -> str:
        return f"pg {self.host}"

struct Mem:
    size: int

    def label(self) -> str:
        return f"mem {self.size}"

type Three = Lite | Pg | Mem

def which(store: Three) -> str:
    if store is Lite:
        return "lite"
    if store is Pg:
        return f"pg {store.host}"
    return f"mem {store.size}"

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
    return store.label()

def two(store: Pg | Mem) -> str:
    if store is Pg:
        return store.host
    return which(store)

def passed(store: Three) -> str:
    if store is Lite:
        return "lite"
    return two(store)

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
        return len(text)

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") except:
            pass

struct Mem:
    size: int

    def run(self, text: str) -> int fails Nope:
        return len(text) * 3

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log=f"mem{self.size}") except:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        return len(text) * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") except:
            pass

type Store = Lite | Postgres

type Three = Lite | Postgres | Mem

struct Holder:
    db: Three

def saved(store: Store) -> str fails Nope:
    with store.transaction()? as tx:
        count = store.run("abc")?
        return f"{tx.log} {count}"
    return ""

def three(store: Three) -> str fails Nope:
    mut seen = ""
    with store.transaction()? as tx:
        seen = tx.log
    with store.transaction()? as tx:
        seen = f"{seen}+{tx.log}"
    return seen

def held(holder: Holder) -> int fails Nope:
    db = holder.db
    with db.transaction()? as tx:
        return len(tx.log)
    return 0

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
    return (given.expose_secret(), True)

def size(held: Box[int] | int) -> int:
    if held is int:
        return held
    return held.value * 10

def main(stdout: Stdout):
    stdout.print(text_of("plain"), text_of(Secret.of("hidden"))[1])
    both: list[Box[int] | int] = [Box(value=4), 2]
    stdout.print(size(both[0]), size(both[1]))
"#,
        "(\"plain\", False) True\n40 2\n",
    )
}

/// A known difference: native code writes a field holding a union tagged with its member, as
/// the language says, where the pinned outcome left the tag out.
#[test]
fn json_tags_a_field_holding_a_union_with_its_member() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let ((ours, _, _), (wanted, _, _)) = both(
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
    assert_eq!(wanted, "{\"main\": {\"radius\": 1.5}}\n");
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
        back = json.loads[Blob](text) except ParseError(): Blob(data=bytes([]))
        stdout.print(text, back.data == blob.data)
    stdout.print(toml.dumps(Blob(data="hello".encode())))
    _ = json.loads[Blob]("{\"data\": \"ab=c\"}") except bad:
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
    // How it ended goes in too: a program that stops with nothing said, as a crash of the process
    // itself does on Windows, says only its code.
    assert!(ran.status.success(), "{}: {}", ran.status, String::from_utf8_lossy(&ran.stderr));
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
    _ = run() except ParseError():
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
    found = sql.rows_as[Entry](rows) except .Constraint() | .Busy() | .Column() | .Failed():
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
    let (_shower_folder, shower) = shows_its_arguments("process_template_shower")?;
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
    _ = process.run(t"printf {upload}") except refused:
        stdout.print(refused.message())
        shown
    path = "-odd"
    stdout.print(process.run(t"SHOWER -- {path}")?.stdout.strip())
    _ = narrow.run(t"cat") except error:
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
/// A program that prints its arguments, with the folder it's in, which goes when that's dropped.
fn shows_its_arguments(name: &str) -> std::io::Result<(fennec_temp::Temp, String)> {
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
    Ok((dir, path))
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
    _ = run() except InvalidValue():
        return "refused"
    return "loaded"

def said_toml(text: str) -> str:
    _ = toml.loads[dict[str, str]](text) except ParseError():
        return "refused"
    return "loaded"

def main(stdout: Stdout):
    stdout.print(said(() => base64.decode("aGk=")?), said(() => base64.decode("aGl=")?), said(() => base64.decode("aA==")?), said(() => base64.decode("aB==")?))
    stdout.print(Path("..").parent(), Path("a/..").parent(), Path("a/b").parent())
    root = Path("static")
    for asked in ["/a/b.txt", "a.txt", "/../secret", "a/../../b", "a\x00b", "a\\b"]:
        found = root.inside(asked) except InvalidValue():
            stdout.print("refused")
            continue
        stdout.print(found)
    stdout.print(said_toml("d = 2024-02-29\n"), said_toml("d = 2023-02-29\n"), said_toml("d = 2024-02-30\n"), said_toml("d = 2024-04-31\n"))
    _ = csv.loads("a;;b\n", delimiter=";;") except ParseError():
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
    text = csv.dumps(rows) except InvalidValue(): return "refused"
    read = csv.loads(text) except ParseError():
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
    rows = csv.loads("﻿﻿a,b\n﻿c") except ParseError():
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
        "from std.net import NetError\n\ndef said(net: Net) -> str:\n    answer = net.get(\"http://127.0.0.1:{port}/\", timeout=10s) except .NotFound() | .Refused() | .TimedOut() | .Closed | .Denied() | .Tls() | .Status() | .Other():\n        return \"refused\"\n    return answer.text()\n\ndef main(net: Net, stdout: Stdout):\n    stdout.print(said(net), said(net), said(net))\n"
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
        "def main(net: Net, stdout: Stdout) fails NetError:\n    answer = net.get(\"http://127.0.0.1:{port}/\")?\n    stdout.print(len(answer.headers))\n    stdout.print(answer.headers[\"x-tag\"])\n    stdout.print(answer.set_cookies)\n"
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
    let place = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-sqlite-nan-{}.db", std::process::id())),
    );
    let path = place.to_string_lossy().into_owned();
    // In a Fennec string, a Windows path's `\` starts an escape, so each is written twice.
    let written = path.replace('\\', "\\\\");
    let source = format!(
        "from std import sqlite\nfrom std.sql import SqlError\n\ndef main(fs: Fs, stdout: Stdout) fails SqlError:\n    with sqlite.open(fs, \"{written}\")? as db:\n        _ = db.execute(t\"create table m (x real)\")?\n        nan = float(\"nan\") except InvalidValue(): 0.0\n        _ = db.execute(t\"insert into m values ({{nan}})\") except refused:\n            stdout.print(refused.message())\n            0\n        one = 1.5\n        stdout.print(db.execute(t\"insert into m values ({{one}})\")?)\n"
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
        found = url.parse(text) except error:
            stdout.print("refused", text, error.message)
            continue
        stdout.print(str(found) == text, str(found))
    for text in ["http://a@b@c/", "http://good.com%2f.evil.com/", "http://a%40b/"]:
        found = url.parse(text) except InvalidValue():
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
    return Secret.of("hun" + "ter2")

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
    let place =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-fs-safety-{}", std::process::id())));
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
    let place =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-fs-only-{}", std::process::id())));
    std::fs::create_dir_all(place.join("inside"))?;
    std::fs::create_dir_all(place.join("outside"))?;
    std::fs::write(place.join("outside/secret.txt"), "secret")?;
    std::fs::write(place.join("inside/plain.txt"), "plain")?;
    std::os::unix::fs::symlink(place.join("outside"), place.join("inside/door"))?;
    let folder = place.to_string_lossy().into_owned();
    let source = format!(
        "def read_out(fs: Fs, path: str) -> str:\n    text = fs.read_text(path) except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other():\n        return \"refused\"\n    return text\n\ndef main(fs: Fs, stdout: Stdout):\n    narrow = fs.only(\"{folder}/inside\")\n    stdout.print(read_out(narrow, path=\"{folder}/inside/door/secret.txt\"))\n    stdout.print(read_out(narrow, path=\"{folder}/inside/plain.txt\"))\n    narrow.write_text(\"{folder}/inside/door/new.txt\", \"x\") except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other():\n        stdout.print(\"refused\")\n        return\n    stdout.print(\"wrote\")\n"
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
            return "no connection came free"
        case Constraint(reason=reason) | Busy(reason=reason) | Failed(reason=reason):
            return reason
        case Column(name=name, reason=reason):
            return f"column {name}: {reason}"

def run(port: int, weak: bool, net: Net, entropy: Entropy) fails SqlError | Timeout:
    with postgres.connect(net, entropy, f"postgres://u@127.0.0.1:{port}/d", password=Secret.of("pw"), allow_weak_sign_in=weak, pool_size=1, tls=False)? as db:
        with db.transaction()? as tx:
            _ = tx.execute(t"select 1")?

def main(stdout: Stdout, net: Net, entropy: Entropy):
    for at, port in enumerate(PORTS):
        _ = run(port, weak=at >= STRICT, net=net, entropy=entropy) except error:
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
    message = reader.next() except .Constraint() | .Busy() | .Column() | .Failed():
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
    found = postgres.rows_of(["flag"], types=[16], rows=rows) except .Constraint() | .Busy() | .Column() | .Failed():
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
    let first = "from std import pack\n\nstruct Entry:\n    path: str\n    size: int\n\ndef main(fs: Fs, stdout: Stdout) fails IoError:\n    data = pack.dumps(Entry(path=\"a.txt\", size=12))\n    fs.write_bytes(\"entry.pack\", data)?\n    stdout.print(len(data))\n";
    let long = ran(first)?;
    let updated = in_kit(&["update"])?;
    let said = String::from_utf8_lossy(&updated.stdout).into_owned();
    let lock = std::fs::read_to_string(kit.join("pack.lock"))?;
    let short = ran(first)?;
    let next = "from std import pack\n\nstruct Entry:\n    path: str\n    size: float\n    tags = list[str]()\n\ndef main(fs: Fs, stdout: Stdout) fails IoError:\n    found = pack.loads[Entry](fs.read_bytes(\"entry.pack\")?) except error:\n        stdout.print(error.message)\n        return\n    stdout.print(found.path, found.size, found.tags)\n";
    let read = ran(next)?;
    // A version that can't load what was locked says so as it's checked, without failing.
    let breaking = "from std import pack\n\nstruct Entry:\n    path: str\n    size: str\n    rank: int\n    note = \"\"\n\ndef main(stdout: Stdout):\n    stdout.print(len(pack.dumps(Entry(path=\"a\", size=\"1\", rank=2))))\n";
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
    let place =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-iter-fail-{}", std::process::id())));
    std::fs::create_dir_all(place.join("folder"))?;
    std::fs::write(place.join("bad.txt"), [97, 10, 255, 10, 98])?;
    std::fs::write(place.join("good.txt"), "one\ntwo\n")?;
    // In a Fennec string, a Windows path's `\` starts an escape, so each is written twice.
    let folder = place.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        "def chunked(fs: Fs, path: str) -> int fails IoError:\n    mut total = 0\n    with fs.open(path)? as file:\n        for chunk in file.chunks(4)?:\n            total += len(chunk)\n    return total\n\ndef lined(fs: Fs, path: str) -> int fails IoError:\n    mut count = 0\n    with fs.open(path)? as file:\n        for line in file.lines()?:\n            count += len(line) + 1\n    return count\n\ndef said(run: () -> int fails IoError) -> str:\n    n = run() except .NotFound() | .PermissionDenied() | .AlreadyExists() | .Other():\n        return \"failed\"\n    return f\"read {{n}}\"\n\ndef main(fs: Fs, stdout: Stdout):\n    stdout.print(said(() => chunked(fs, path=\"{folder}/folder\")?), said(() => chunked(fs, path=\"{folder}/good.txt\")?))\n    stdout.print(said(() => lined(fs, path=\"{folder}/bad.txt\")?), said(() => lined(fs, path=\"{folder}/good.txt\")?))\n"
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
    found = postgres.settings(url) except error:
        return error.message()
    return f"{found.user} {found.host} {found.port} {found.database}"

def twice(url: Secret[str], net: Net, entropy: Entropy) fails SqlError:
    with postgres.connect(net, entropy, url, password=Secret.of("other"))? as _:
        pass

def main(stdout: Stdout, env: Env, net: Net, entropy: Entropy):
    written = env.get("FENNEC_DATABASE_URL") else "postgres://ann:hunter2@db.example/shop"
    stdout.print(described(written))
    stdout.print(described(Secret.of(written)))
    twice(Secret.of(written), net=net, entropy=entropy) except error:
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
        "from std import postgres\nfrom std.crypto import Secret\nfrom std.sql import SqlError\n\ndef run(port: int, net: Net, entropy: Entropy) fails SqlError:\n    with postgres.connect(net, entropy, f\"postgres://u@localhost:{{port}}/d\", password=Secret.of(\"pw\"))? as _:\n        pass\n\ndef main(stdout: Stdout, net: Net, entropy: Entropy):\n    for port in [{refused}, {injected}]:\n        _ = run(port, net=net, entropy=entropy) except error:\n            stdout.print(error.message())\n            continue\n        stdout.print(\"went through\")\n"
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
    _ = run() except error:
        return error.message
    return "loaded"

def changed(data: bytes, at: int, to: int) -> bytes:
    mut items = list[int]()
    for b in range(len(data)):
        items.append(to if b == at else data[b])
    return bytes(items)

## Where the value starts: after the header, and the layout saved with it.
def body(data: bytes) -> int:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    return at + 1 + (n | (data[at] << shift))

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
    back = pack.loads[list[Unit]](data) except ParseError(): []
    stdout.print(len(data), back == units, back[1].name, back[1].at.x, back[1].tags, back[0].items[0].weight)
    got_value = pack.loads[float](pack.dumps(-0.1)) except ParseError(): 0.0
    got_value2 = pack.loads[str](pack.dumps("")) except ParseError(): "x"
    stdout.print(got_value, got_value2)
    stdout.print(said(() => pack.loads[list[Place]](data)?))
    stdout.print(said(() => pack.loads[list[Unit]](data[0:len(data) - 1])?))
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
    return (seed * 1103515245 + 12345) % 2147483648

def tried(data: bytes, limits: Limits) -> int:
    _ = pack.loads[Node](data, limits=limits) except ParseError():
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
    return at + 1 + (n | (data[at] << shift))

def main(stdout: Stdout):
    tail = Node(name="tail", shape=.Dot, data=bytes([0, 255]), weights=[], names={})
    node = Node(
        name="héad", shape=.Box(width=3, height=-4), data=bytes([1, 2, 3]), weights=[0.5, -2.0], names={"a": 1}, next=tail
    )
    good = pack.dumps(node)
    small = Limits(depth=8, length=64)
    stdout.print(len(good), tried(good, Limits()), tried(good, small))
    # Every truncation fails.
    mut loaded = 0
    for size in range(len(good)):
        loaded += tried(good[0:size], Limits())
    stdout.print("truncated", loaded)
    mut items = list[int]()
    for at in range(len(good)):
        items.append(good[at])
    # Every byte changed, one at a time, to bytes from a seed.
    mut seed = 7
    loaded = 0
    mut runs = 0
    for at in range(len(good)):
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
        return Code(text=src.text()?)

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
    return [node.name] + names(rest)

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() except error:
        return error.message
    return "loaded"

## What `data` loads as, as an `After`.
def show_after(data: bytes, stdout: Stdout):
    found = pack.loads[After](data) except error:
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
    stdout.print(len(data), len(first))

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
    for place in range(start, len(data)):
        items.append(data[place])
    return bytes(items)
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
    stdout.print("stream", len(data), "one value alone", len(one), "a hundred alone", len(one) * 100)
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
    for at in range(len(data)):
        fed.feed(data[at:at + 1])
        while True:
            _ = fed.next[Event]()? else break
            got += 1
    stdout.print("fed", got)
    # Cut off, bytes that are all there are fail.
    mut cut = Unpacker.of(data[0:len(data) - 1])
    mut read = 0
    mut failed = ""
    while True:
        found = cut.next[Event]() except error:
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
    stdout.print(len(first), len(second))
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
    return out

## Bytes that ask for `count` items that each take no bytes, from `start`, the header and a layout.
def asking(start: bytes, count: int) -> bytes:
    return start + bytes(varint(count))

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() except error:
        return error.message
    return "loaded"

def main(stdout: Stdout):
    empty = pack.dumps([Hollow()])
    start = empty[0:len(empty) - 1]
    # The same shape: a list of what saves nothing.
    stdout.print(said(() => pack.loads[list[Hollow]](asking(start, 2_000_000))?))
    # A layout written by hand, of structs with no fields, loaded by name into a bigger struct.
    layout = "l S 0".encode()
    named = bytes([70, 80, 75, 50, 1, 2, 3, 4, 5, 6, 7, 8, len(layout)]) + layout
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
    filled = bytes([70, 80, 75, 50, 1, 2, 3, 4, 5, 6, 7, 8, len(one_byte)]) + one_byte + bytes(varint(100)) + bytes([1 for _ in range(100)])
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
        return made

struct Scene:
    shapes: list[Shape]

def said[T](run: () -> T fails ParseError) -> str:
    _ = run() except error:
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
    return at + 1 + (n | (data[at] << shift))

def main(stdout: Stdout):
    scene = Scene(shapes=[Shape(kind=0, circle=Circle(radius=1.5)), Shape(kind=1, square=Square(side=2.0)), Shape(count=7)])
    text = json.dumps(scene)
    stdout.print(text)
    back = json.loads[Scene](text) except ParseError(): Scene(shapes=[])
    stdout.print(json.dumps(back) == text)
    written = toml.dumps(scene)
    stdout.print(written)
    again = toml.loads[Scene](written) except ParseError(): Scene(shapes=[])
    stdout.print(json.dumps(again) == text)
    data = pack.dumps(scene)
    packed = pack.loads[Scene](data) except ParseError(): Scene(shapes=[])
    stdout.print(json.dumps(packed) == text)
    stdout.print(said(() => json.loads[Scene]("{\"shapes\": [{\"Triangle\": {}}]}")?))
    stdout.print(said(() => json.loads[Scene]("{\"shapes\": [5]}")?))
    stdout.print(said(() => toml.loads[Scene]("[[shapes]]\nTriangle = 1\n")?))
    mut items = list[int]()
    for at in range(len(data)):
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
    _ = run() except error:
        return error.message
    return "loaded"

def main(stdout: Stdout):
    scene = Scene(main=Square(side=2.0), count="many", shapes=[Circle(radius=1.5), 7], named={"a": Circle(radius=2.0)})
    empty = Scene(main=Circle(radius=0.0), shapes=[], named={})
    text = json.dumps(scene)
    stdout.print(text)
    got_value = json.loads[Scene](text) except ParseError(): empty
    stdout.print((got_value) == scene)
    stdout.print(toml.dumps(scene))
    got_value2 = toml.loads[Scene](toml.dumps(scene)) except ParseError(): empty
    stdout.print((got_value2) == scene)
    got_value3 = pack.loads[Scene](pack.dumps(scene)) except ParseError(): empty
    stdout.print((got_value3) == scene)
    log = Log(events=[Event.Spawn(Circle(radius=0.5)), Event.Quit], picked=4)
    stdout.print(json.dumps(log))
    got_value4 = json.loads[Log](json.dumps(log)) except ParseError(): Log(events=[])
    stdout.print((got_value4) == log)
    got_value5 = pack.loads[Log](pack.dumps(log)) except ParseError(): Log(events=[])
    stdout.print((got_value5) == log)
    got_value6 = pack.loads[Log](pack.dumps(Log(events=[]))) except ParseError(): log
    stdout.print((got_value6).picked is None)
    # A default that's one of a union's members, and what doesn't load.
    got_value7 = json.loads[Scene]("{\"main\": {\"Circle\": {\"radius\": 1.0}}, \"shapes\": [], \"named\": {}}") except ParseError(): scene
    stdout.print((got_value7).count)
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
    _ = run() except error:
        return error.message
    return "loaded"

def changed(data: bytes, at: int, to: int) -> bytes:
    mut items = list[int]()
    for b in range(len(data)):
        items.append(to if b == at else data[b])
    return bytes(items)

## Where the value starts: after the header, and the layout saved with it.
def body(data: bytes) -> int:
    mut at = 12
    mut n = 0
    mut shift = 0
    while data[at] >= 128:
        n = n | ((data[at] & 127) << shift)
        shift += 7
        at += 1
    return at + 1 + (n | (data[at] << shift))

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
    back = pack.loads[World](data) except ParseError(): World()
    stdout.print(len(data), len(back.units), back.last is None, pack.dumps(back) == data)
    for id in back.selected:
        unit = back.units[id]
        target = unit.target
        stdout.print(unit.name, back.units[target].name if target is not None else "-")
    # Slot for slot: the same slots and generations, and the Id whose item has gone still gone.
    exact = pack.dumps(world, exact=True)
    mut same = pack.loads[World](exact) except ParseError(): World()
    stdout.print(len(exact), exact[3], len(same.units), same.units[same.selected[1]].name)
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
        for size in range(len(good)):
            if said(() => pack.loads[World](good[0:size])?) == "loaded":
                cut += 1
        mut changes = 0
        for at in range(len(good)):
            for _ in range(30):
                seed = (seed * 1103515245 + 12345) % 2147483648
                loaded: World? = pack.loads[World](changed(good, at=at, to=(seed >> 16) & 255)) except ParseError():
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

/// A `Buffer` popped empty, or inserted into past its end, fails with a buffer's own message,
/// not as a list does.
#[test]
fn an_empty_buffer_fails_to_pop_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "empty_buffer",
        "def main(stdout: Stdout):\n    mut kept = Buffer[str]()\n    kept.push(\"fox\")\n    stdout.print(kept.pop())\n    stdout.print(kept.pop())\n",
    )?;
    same_message_as_pinned(
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
    return False

def main(stdout: Stdout, net: Net):
    up = listening(net) except .NotFound() | .Refused() | .TimedOut() | .Closed | .Denied() | .Tls() | .Status() | .Other():
        stdout.print(\"failed\")
        return
    stdout.print(\"listening\", up)
";
    same_as_pinned("net_kind", source)
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
    return len(copy_of[at % 2].tags)

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
            return "L(" + name + ")"
        case .Branch(label, children):
            mut text = "B(" + label
            for child in children:
                text += " " + show(child)
            return text + ")"

def last_use(given: Unit) -> Unit:
    mut u = given
    u = u
    return u

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
    stdout.print(show(t), u, p, xs, ys, len(other), len(junk))
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
fn borrowed_reads_match_what_was_pinned() -> std::io::Result<()> {
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
        return self.items[at].weight

    def names(ref self, ref into: list[str]):
        for at in range(len(self.items)):
            item = self.items[at]
            if self.weight_of(at) > 1:
                for tag in item.tags:
                    into.append(tag)
            into.append(item.name)

    def first_tags(self) -> list[str]:
        return self.items[0].tags

    def relabel(ref self):
        tags = self.items[0].tags
        self.items[0] = Item(name="new", tags=["x"], weight=9)
        self.log.append(tags[0])

def size(tree: Tree) -> int:
    match tree:
        case .Leaf:
            return 1
        case .Node(left, right):
            return 1 + size(left) + size(right)

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
    same_as_pinned("borrowed", source)?;
    let counts = counts_of("borrowed", source)?;
    assert!(counts.starts_with("copies 0 "), "{counts}");
    Ok(())
}

/// `list.insert` at the front, the middle and the end, of numbers and of strings, then past the
/// end, which fails.
#[test]
fn a_list_insert_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "list_insert",
        "def main(stdout: Stdout):\n    mut ns = [2, 4]\n    ns.insert(0, 1)\n    ns.insert(2, 3)\n    ns.insert(4, 5)\n    mut words = [\"b\"]\n    words.insert(0, \"a\")\n    words.insert(2, \"c\")\n    stdout.print(ns, words)\n",
    )?;
    same_message_as_pinned(
        "list_insert_past",
        "def main(stdout: Stdout):\n    mut ns = [1]\n    ns.insert(3, 2)\n    stdout.print(ns)\n",
    )
}

#[test]
fn an_optional_of_a_block_allocates_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "def find(names: list[str], wanted: str) -> str?:\n    for name in names:\n        if name == wanted: return name\n    return None\n\nstruct Seen:\n    last: str?\n\ndef main(stdout: Stdout):\n    names = [\"fox\", \"kit\", \"den\"]\n    mut seen = Seen(last=None)\n    mut hits = 0\n    for at in range(1000):\n        found = find(names, names[at % 3])\n        if found is not None:\n            hits += 1\n        seen.last = found\n    stdout.print(hits, seen.last, find(names, \"owl\"))\n";
    same_as_pinned("optional", source)?;
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
fn a_release_build_optimized_as_a_whole_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "struct Point:\n    x: int\n    y: int\n\n    def moved(self, by: int) -> Point:\n        return Point(x=self.x + by, y=self.y - by)\n\ndef count_up(limit: int) -> Iterator[int]:\n    mut at = 0\n    while at < limit:\n        yield at\n        at += 1\n\ndef main(stdout: Stdout):\n    mut points = [Point(x=i, y=i * 2) for i in range(5)]\n    mut seen: dict[str, int] = {}\n    for point in points:\n        moved = point.moved(3)\n        seen[f\"{moved.x}\"] = moved.y\n    stdout.print(len(seen), sum(n for n in count_up(10)), points[4].moved(1).y)\n    stdout.print([1, 2][5])\n";
    let (ours, wanted) = both_built("release", source, &["--llvm", "--release"])?;
    assert_eq!((ours.0, ours.2), (wanted.0, wanted.2));
    assert_eq!(ours.1.lines().next(), wanted.1.lines().next());
    Ok(())
}

/// Floats have one equality, order and hash everywhere: `0.0` is `-0.0`, and NaN comes after
/// everything and is the same as any other NaN, whatever its sign. So this is held to what that
/// decision says, as pinned.
#[test]
fn floats_compare_and_hash_as_decided() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = r"struct Pair:
    x: float
    y: float

def same(a: float, b: float) -> bool:
    return a == b

def different(a: float, b: float) -> bool:
    return a != b

def less(a: float, b: float) -> bool:
    return a < b

def most(a: float, b: float) -> bool:
    return a <= b

def more(a: float, b: float) -> bool:
    return a > b

def least(a: float, b: float) -> bool:
    return a >= b

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
    let pinned_outcome = pinned_outcome("floats_ordered_as_decided", source)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, pinned_outcome);
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
    let source = "def main(stdout: Stdout):\n    counts = {word: len(word) for word in \"a fox ran\".split()}\n    total = sum(n * 2 for n in parallel([1, 2, 3]))\n    stdout.print(counts, total)\n";
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

/// Floats inside an enum's case and a struct's fields compare field by field as Fennec compares
/// floats, with no value read whole: NaN equals NaN, `-0.0` equals `0.0`, another case is unequal,
/// and a block field goes by what it holds. Keys found equal hash alike, so a dict keeps one.
#[test]
fn floats_in_cases_and_fields_compare_and_hash_as_floats_do() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "float_fields_equal",
        include_str!("fixtures/float_fields_equal.fen"),
        "True True False False True\nTrue True False False\nTrue False False\n3 nan again zero again\n1 2 False\n",
    )
}

/// `in`, `index` and `remove` on a list of strings find an item by its text, held in its header or
/// in a block, with nothing made to compare: empty, 8, 23 and 24 bytes, long, and past ASCII.
#[test]
fn a_list_of_strings_finds_its_items_by_text_however_held() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "string_list_in",
        include_str!("fixtures/string_list_in.fen"),
        "True True 0 False\nTrue True 1 False\nTrue True 2 False\nTrue True 3 False\nTrue True 4 False\n\
         True True 5 False\nTrue True 6 False\nFalse False False False\n5 False False True\n",
    )
}

/// Enums held in blocks, as one holding itself is, compare by case first, with no call for another
/// case, and the same case compares its fields: a string, a list, a float, itself and none. `!=`
/// is the opposite, a value is equal to itself, and a module constant compares as one made here.
#[test]
fn enums_in_blocks_compare_by_case_then_fields() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "boxed_enum_equal",
        include_str!("fixtures/boxed_enum_equal.fen"),
        "False True False True False False False True\nTrue False False True False False False True\n\
         False False True True False False False True\nFalse False False False False False False True\n\
         False False False True True False False True\nFalse False False True False True False True\n\
         False False False True False False True True\nTrue True True\n",
    )
}

/// `in` and `not in` on a list or set written out of constants, strings or ints, which only that
/// test reads, compare with each constant and make nothing, giving what a made one would.
#[test]
fn looking_in_constants_written_out_gives_what_a_made_collection_does() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "in_constants",
        include_str!("fixtures/in_constants.fen"),
        "def True False\nde False False\ndeff False False\n True False\n\
         a keyword much longer than eight bytes True False\na keyword much longer than eight bytes! False False\n\
         else True False\nreturn True False\n-> True\n= False\n<= True\n>=> False\n[0, 1, 4, 6, 8, 9, 10, 12, 14]\n",
    )
}

/// Floats have one equality and hash everywhere: `0.0` is `-0.0`, and NaN is the same as any other
/// NaN, whatever its sign. So this is held to what that decision says, as pinned.
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
    return a == b

def different(a: float, b: float) -> bool:
    return a != b

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
    let pinned_outcome = pinned_outcome("floats_as_decided", source)?;
    std::fs::remove_dir_all(&dir)?;
    assert_eq!(ours, pinned_outcome);
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
fn float_keys_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "float_keys",
        r#"from std.math import sqrt

def main(stdout: Stdout):
    mut floats = {1.5, 2.5}
    stdout.print(floats, 1.5 in floats, 3.5 in floats, len(floats), sorted(floats), sorted({"b", "a"}))
    floats.add(1.5)
    floats.add(3.5)
    floats.discard(2.5)
    stdout.print(floats, len(floats), list(floats), sum(x for x in floats))
    zero = 0.0
    mut zeros = {zero}
    negative = -zero
    stdout.print(negative in zeros, zero in zeros)
    zeros.add(negative)
    stdout.print(zeros, len(zeros))
    mut signed = {negative}
    signed.add(zero)
    stdout.print(signed, len(signed), zero in signed)
    nan = 1e300 * 1e300 - 1e300 * 1e300
    mut nans = {nan}
    nans.add(nan)
    nans.add(-nan)
    stdout.print(len(nans), nan in nans, -nan in nans, 1.0 in nans)
    mut named = {1.5: "a", 2.5: "b"}
    named[1.5] = "c"
    named[0.0] = "z"
    stdout.print(named, named[1.5], named[negative], named.get(9.5), named.keys(), named.values())
    stdout.print(named.pop(2.5), named, {1.5, 2.5} == {2.5, 1.5})
    mut counts: dict[float, int] = {}
    for at in range(100):
        key = float(at % 7) / 2.0
        counts[key] = (counts.get(key) else 0) + 1
    stdout.print(len(counts), counts[0.0], counts[3.0])
"#,
    )
}

/// Keys that hold strings or floats go by what they hold, not by their words.
#[test]
fn keys_holding_strings_and_floats_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected(
        "keys_by_value",
        r#"struct Named:
    id: int
    name: str

enum Tag:
    Plain
    Label(text: str)

def joined(a: str, b: str) -> str:
    return a + b

def main(stdout: Stdout):
    mut pairs = {(1, "one"): [1], (2, "a long string in its own block"): [2]}
    long = joined("a long string ", b="in its own block")
    stdout.print(pairs[(1, "one")], pairs[(2, long)], (2, long) in pairs, (3, long) in pairs)
    pairs[(2, long)] = [9]
    pairs[(3, joined("th", b="ree"))] = [3]
    kept = pairs
    pairs[(4, "four")] = []
    stdout.print(pairs, len(pairs), len(kept), pairs.get((3, "three")), pairs.get((9, "no")))
    stdout.print(pairs.pop((1, "one")), pairs.keys(), kept == pairs, kept == kept)
    for key, value in pairs.items():
        stdout.print(key, value)
    mut people = {Named(id=1, name="ann"): 1}
    people[Named(id=2, name=joined("b", b="ob"))] = 2
    stdout.print(people[Named(id=2, name="bob")], Named(id=1, name="ann") in people, len(people))
    mut tags = {Tag.Plain, Tag.Label(text="x"), Tag.Label(text="x")}
    tags.add(Tag.Label(text=joined("y", b="z")))
    tags.discard(Tag.Label(text="x"))
    stdout.print(tags, len(tags), Tag.Label(text="yz") in tags, Tag.Plain in tags)
    zero = 0.0
    mut floats = {(zero, 1): "a"}
    floats[(-zero, 1)] = "b"
    floats[(1.5, 2)] = "c"
    stdout.print(floats, len(floats), floats[(-zero, 1)])
    mut grown: dict[(int, str), int] = {}
    for at in range(200):
        grown[(at % 20, f"key {at % 20}")] = at
    stdout.print(len(grown), grown[(7, "key 7")], grown[(19, "key 19")])
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
    stdout.print(len(zeros), {zero, 1.0} == {minus, 1.0})
    mut nans = {nan}
    nans.add(-nan)
    stdout.print(len(nans), -nan in nans)
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
    return big - big

def counted() -> int:
    other = -nan()
    seen = {Point(x=0.0, y=1.0), Point(x=-0.0, y=1.0), Point(x=nan(), y=2.0), Point(x=other, y=2.0)}
    return len(seen)

SEEN = counted()

def main(stdout: Stdout):
    missing = nan()
    other = -missing
    mut names = {Point(x=0.0, y=1.0): "zero"}
    names[Point(x=-0.0, y=1.0)] = "negative zero"
    names[Point(x=missing, y=2.0)] = "nan"
    names[Point(x=other, y=2.0)] = "nan again"
    stdout.print(len(names), names[Point(x=0.0, y=1.0)], names[Point(x=missing, y=2.0)])
    stdout.print(Point(x=-0.0, y=1.0) in names, Point(x=0.5, y=1.0) in names)
    small = {Small(at=f32(0.0)), Small(at=f32(-0.0)), Small(at=f32(missing)), Small(at=f32(other))}
    readings = {Reading.Level(value=-0.0): 1, Reading.Level(value=0.0): 2, Reading.Off: 3}
    marks: set[Mark] = {Point(x=0.0, y=0.0), Point(x=-0.0, y=-0.0), "a"}
    stdout.print(len(small), len(readings), readings[Reading.Level(value=0.0)], len(marks))
    stdout.print(SEEN, counted())
    zero = f32(0.0)
    stdout.print(Reading32.At(value=zero) == .At(value=-zero), Small(at=zero) == Small(at=-zero))
    stdout.print(Reading.Level(value=0.0) == .Level(value=-0.0), Reading32.At(value=f32(missing)) == .At(value=f32(other)))
    sized = {Reading32.At(value=zero): 1, Reading32.At(value=-zero): 2, Reading32.At(value=f32(missing)): 3, Reading32.At(value=f32(other)): 4}
    stdout.print(len(sized), sized[Reading32.At(value=zero)], len({f32(0.0), f32(-0.0), f32(missing), f32(other)}))
"#,
        "2 negative zero nan again\nTrue False\n2 2 2 2\n2 2\nTrue True\nTrue True\n2 2 2\n",
    )
}

/// `==`, `+` and ordering on lists of each kind of item, and `==` on dicts and sets.
#[test]
fn operators_on_collections_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "operators_on_collections",
        r#"struct Spot:
    x: int
    y: str

def maybe(on: bool) -> Spot?:
    if on:
        return Spot(x=1, y="a")
    return None

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

/// A tuple of plainer items written or kept where one with optional items goes, as
/// `(1, Parcel(name="x"))` where `(int, Parcel?)` goes: alone, in a list, in a list in a dict's
/// value, in a list of lists, and a tuple kept in a name first. Each item becomes the optional it
/// goes to, in both builds, with nothing left held.
#[test]
fn a_tuple_widens_to_its_optional_items_wherever_it_goes() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "tuples_widened",
        r#"struct Parcel:
    name: str

def main(stdout: Stdout):
    batch: dict[str, list[(int, Parcel?)]] = {"a": [(1, Parcel(name="x")), (2, None)]}
    pairs: list[(int, Parcel?)] = [(3, Parcel(name="y"))]
    one: (int, Parcel?) = (4, Parcel(name="z"))
    nested: list[list[(str, int?)]] = [[("a", 1)], [("b", None)]]
    numbers: dict[str, list[int?]] = {"n": [1]}
    kept = (5, Parcel(name="w"))
    widened: (int, Parcel?) = kept
    stdout.print(batch, pairs, one)
    stdout.print(nested, numbers, widened, kept)
    return
"#,
        "{\"a\": [(1, Parcel(name=\"x\")), (2, None)]} [(3, Parcel(name=\"y\"))] (4, Parcel(name=\"z\"))\n\
         [[(\"a\", 1)], [(\"b\", None)]] {\"n\": [1]} (5, Parcel(name=\"w\")) (5, Parcel(name=\"w\"))\n",
    )
}

#[test]
fn tuples_and_structs_compare_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
/// looks from never changes which that is: the `Id`s it gives are as pinned.
#[test]
fn an_id_maps_new_items_go_in_its_first_free_slot() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(first, second, third, fourth, fifth, len(names))
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
fn reversing_and_removing_from_tables_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(names.values(), len(names), copied.values(), len(copied), names[first], names[third])
    fourth = names.add("vex")
    stdout.print(names.values(), len(names), names[fourth])
    names.remove_where(name => False)
    stdout.print(len(names))
    names.remove_where(name => True)
    stdout.print(names.values(), len(names), names.get(first))
    mut units = Table[Unit]()
    weak = units.add(Unit(name="weak", hp=1))
    strong = units.add(Unit(name="strong", hp=9))
    units.remove_where(unit => unit.hp < 5)
    stdout.print(units.values(), units.get(weak), units[strong].name)
    mut numbers = Table[int]()
    _ = numbers.add(1)
    _ = numbers.add(2)
    numbers.remove_where(n => n % 2 == 0)
    stdout.print(numbers.values(), len(numbers))
    numbers.clear()
    stdout.print(numbers.values(), len(numbers))
    gone = numbers.add(5)
    stdout.print(numbers.values(), len(numbers), numbers[gone])
    mut cleared = names
    _ = cleared.add("again")
    cleared.clear()
    stdout.print(len(cleared), len(names), cleared.values())
    _ = numbers.remove(gone)
    stdout.print(len(numbers))
"#,
    )
}

/// `Table.modify` changes an item where it is through a closure with a `ref` parameter, and a
/// `Table`'s items change their fields as a list's do.
#[test]
fn changing_what_a_table_holds_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "table_changes",
        r#"from std.collections import Id, Table

struct Enemy:
    name: str
    health: int
    tags: list[str]

    def take_damage(ref self, amount: int) -> int:
        self.health = self.health - amount
        self.tags.append("hit")
        return self.health

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
    stdout.print(world.enemies.modify(near, foe => foe.take_damage(1)), len(world.enemies))
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
    stdout.print(nodes[root].value, len(nodes[root].children), len(copied[root].children))
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
    return sum_of

def named(units: Table[Unit]) -> str:
    mut parts = []
    for id, unit in units.items():
        parts.append(f"{unit.name}:{len(unit.tags)}:{units[id].name}")
    for unit in units.values():
        parts.append(unit.name)
    return ",".join(parts)

def main(stdout: Stdout):
    mut numbers = Table[int]()
    ids = [numbers.add(n) for n in range(8)]
    stdout.print(total(numbers))
    _ = numbers.remove(ids[0])
    _ = numbers.remove(ids[3])
    _ = numbers.remove(ids[4])
    _ = numbers.remove(ids[7])
    stdout.print(total(numbers), len(numbers))
    _ = numbers.add(100)
    stdout.print(total(numbers), len(numbers))

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
    stdout.print(len(pairs))

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
    same_as_pinned("walking_a_map", WALKING_A_MAP)
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
    return found

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
    stdout.print(total, len(world.enemies))
"#;

#[test]
fn walking_a_tables_slots_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned("walking_slots", WALKING_SLOTS)
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

/// Builds `source` with these options and runs it under `FENNEC_COUNTS`: what it printed, and
/// the line saying what it copied and what it still held at the end.
fn printed_and_counts(dir: &Path, name: &str, how: &[&str]) -> std::io::Result<(String, String)> {
    let (program, executable) = (dir.join(format!("{name}.fen")), dir.join(name));
    let (program, output) =
        (program.to_string_lossy().into_owned(), executable.to_string_lossy().into_owned());
    let args: Vec<&str> =
        ["build"].into_iter().chain(how.iter().copied()).chain([program.as_str(), "-o", &output]).collect();
    let built = fennec(&args, dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();
    let counts = said.lines().find(|line| line.starts_with("copies ")).unwrap_or_default().to_owned();
    Ok((String::from_utf8_lossy(&ran.stdout).into_owned(), counts))
}

/// `ref` in a loop never copies, items that hold blocks of their own included: a dict, a list
/// and a string. A `Table` gone through as a local, a parameter or a field, or by `enumerate`,
/// a list in a field, an item changed through `units[id]`, and one copied out with `mut`,
/// changed and put back where it was: each item is taken out, changed where it is and put back,
/// in either build. Each used to copy the whole item, its dict shared, every time round.
#[test]
fn a_ref_loop_over_items_holding_blocks_copies_nothing() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("ref_loops_holding_blocks")?;
    std::fs::write(dir.join("ref_loops_holding_blocks.fen"), REF_LOOPS_HOLDING_BLOCKS)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "ref_loops_holding_blocks", how)?;
        assert_eq!(printed, "24110 1000 3225 2000\n", "{how:?}");
        assert!(counts.starts_with("copies 0 ") && counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const REF_LOOPS_HOLDING_BLOCKS: &str = r#"from std.collections import Id, Table

struct Unit:
    name: str
    hp: int
    bag: dict[str, int]
    tags: list[str]

struct World:
    units = Table[Unit]()
    queue = list[Unit]()

def heal(ref units: Table[Unit]):
    for ref unit in units:
        unit.hp += 1
        unit.bag["herb"] = (unit.bag.get("herb") else 0) + 1
    return

def march(ref world: World):
    for at, ref unit in enumerate(world.units):
        unit.hp += at
        unit.tags.append("m")
        _ = unit.tags.pop()
    for ref unit in world.queue:
        unit.hp -= 1
        unit.bag["wait"] = (unit.bag.get("wait") else 0) + 1
    return

def hurt(ref units: Table[Unit], id: Id[Unit], by: int):
    mut unit = units[id]
    unit.hp -= by
    if unit.hp <= 0:
        unit.hp = 100
        unit.bag["bone"] = (unit.bag.get("bone") else 0) + 1
    units[id] = unit
    return

def wait(ref queue: list[Unit], at: int):
    mut unit = queue[at]
    unit.hp += 1
    unit.bag["wait"] = (unit.bag.get("wait") else 0) + 1
    queue[at] = unit
    return

def main(stdout: Stdout):
    mut world = World()
    mut ids = list[Id[Unit]]()
    for n in range(50):
        ids.append(world.units.add(Unit(name=f"fox{n}", hp=n, bag={"gold": n}, tags=["a"])))
        world.queue.append(Unit(name=f"kit{n}", hp=n, bag={"gold": n}, tags=["b"]))
    for _ in range(20):
        heal(ref world.units)
        march(ref world)
        for ref unit in world.units:
            unit.bag["gold"] = (unit.bag.get("gold") else 0) + 2
        for id in ids:
            world.units[id].hp -= 1
        for at, id in enumerate(ids):
            hurt(ref world.units, id, by=at % 7)
            wait(ref world.queue, at)
    mut hp = 0
    mut herbs = 0
    mut gold = 0
    for unit in world.units:
        hp += unit.hp
        herbs += unit.bag.get("herb") else 0
        gold += unit.bag.get("gold") else 0
    mut waited = 0
    for unit in world.queue:
        hp += unit.hp
        waited += unit.bag.get("wait") else 0
    stdout.print(hp, herbs, gold, waited)
    return
"#;

/// An item taken out of a `Table` by `for ref` goes back however the loop ends. A failure
/// halfway keeps what was changed, up to the item that failed, and leaves the rest as they
/// were. A crash halfway under `lock ... as ref` puts every item back as it was. A copy of
/// the `Table` made before the loop stays as it was, its items' dicts included.
#[test]
fn a_table_left_halfway_through_a_ref_loop_is_whole() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("ref_loop_left_halfway")?;
    std::fs::write(dir.join("ref_loop_left_halfway.fen"), REF_LOOP_LEFT_HALFWAY)?;
    let rows = "fox0 10 1 0\nfox1 20 1 0\nfox2 30 1 0\nfox3 30 0 0\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "ref_loop_left_halfway", how)?;
        assert_eq!(printed, format!("too hot at 30\n{rows}before 60 4\ncrashed\n{rows}"), "{how:?}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const REF_LOOP_LEFT_HALFWAY: &str = r#"from std.collections import Id, Table
from std.concurrent import Crashed

error TooHot:
    at: int

struct Unit:
    name: str
    hp: int
    bag: dict[str, int]

struct World:
    units = Table[Unit]()

def heat(ref units: Table[Unit], limit: int) fails TooHot:
    for ref unit in units:
        unit.hp += 10
        unit.bag["heat"] = (unit.bag.get("heat") else 0) + 1
        if unit.hp > limit: fail TooHot(at=unit.hp)
    return

def burn(world: shared World, limit: int):
    lock world as ref held:
        for ref unit in held.units:
            unit.hp += 100
            unit.bag["burn"] = 1
            # A crash halfway: the lock puts back everything it changed.
            assert unit.hp < limit
    return

def burning(world: shared World) fails Crashed:
    with launch(() => burn(world, limit=125))?:
        pass
    return

def show(stdout: Stdout, units: Table[Unit]):
    for unit in units:
        stdout.print(unit.name, unit.hp, unit.bag.get("heat") else 0, unit.bag.get("burn") else 0)
    return

def main(stdout: Stdout):
    mut units = Table[Unit]()
    for n in range(4):
        _ = units.add(Unit(name=f"fox{n}", hp=n * 10, bag={"gold": n}))
    before = units
    heat(ref units, limit=25) except why:
        case TooHot(at):
            stdout.print("too hot at", at)
    show(stdout, units)
    stdout.print("before", sum(unit.hp for unit in before), sum(len(unit.bag) for unit in before))
    shared world = World(units=units)
    burning(world) except why:
        case Crashed():
            stdout.print("crashed")
    lock world as held:
        show(stdout, held.units)
    return
"#;

/// An item taken out of a `Table` to change in place never changes a copy of the `Table`: it's
/// made alone before the item is taken. A copy kept as a local, as a field of a copied struct,
/// by a caller passing it as `ref`, and by another task, of items holding a dict and of plain
/// ones, and through `units[id]`: each keeps what it had.
#[test]
fn a_table_copied_before_its_items_change_keeps_them() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("table_copied_before")?;
    std::fs::write(dir.join("table_copied_before.fen"), TABLE_COPIED_BEFORE)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, _) = printed_and_counts(&dir, "table_copied_before", how)?;
        assert_eq!(
            printed,
            "local 0 0\nlocal 1 1\nlocal 2 2\nchanged 100 7\nchanged 101 7\nchanged 102 7\nplain 0 0\nplain 1 1\nplain 2 2\nfield 0 0\nfield 1 1\nfield 2 2\nparam 0 0\nparam 1 1\nparam 2 2\ntask 0 0\ntask 1 1\ntask 2 2\nafter 100 7\nafter 101 7\nafter 102 7\nby id 0 0\nby id 1 1\nby id 2 2\nby id changed 100 7\nby id changed 101 7\nby id changed 102 7\n",
            "{how:?}"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const TABLE_COPIED_BEFORE: &str = r#"from std.collections import Id, Table
from std.concurrent import Closed, Crashed, Inbox

struct Heavy:
    x: int
    bag: dict[str, int]

struct Plain:
    x: int
    y: int

struct World:
    units: Table[Heavy]

def heavy_table() -> Table[Heavy]:
    mut table = Table[Heavy]()
    for i in range(3):
        _ = table.add(Heavy(x=i, bag={"gold": i}))
    return table

def bump(ref units: Table[Heavy]):
    for ref item in units:
        item.x += 100
        item.bag["gold"] = 7
    return

def bump_world(ref world: World):
    for ref item in world.units:
        item.x += 100
        item.bag["gold"] = 7
    return

def show(stdout: Stdout, name: str, units: Table[Heavy]):
    for item in units:
        stdout.print(name, item.x, item.bag["gold"])
    return

def holder(inbox: Inbox[int], kept: Table[Heavy], stdout: Stdout):
    for _ in inbox:
        show(stdout, "task", kept)
    return

def main(stdout: Stdout) fails Closed | Crashed:
    # A local copied before the loop.
    mut table = heavy_table()
    backup = table
    for ref item in table:
        item.x += 100
        item.bag["gold"] = 7
    show(stdout, "local", backup)
    show(stdout, "changed", table)
    # Plain items, the same.
    mut plain = Table[Plain]()
    for i in range(3):
        _ = plain.add(Plain(x=i, y=i))
    plain_backup = plain
    for ref item in plain:
        item.x += 100
    for item in plain_backup:
        stdout.print("plain", item.x, item.y)
    # A field of a struct that was copied.
    mut world = World(units=heavy_table())
    world_backup = world
    bump_world(ref world)
    show(stdout, "field", world_backup.units)
    # Behind a `ref` parameter, the caller keeping a copy.
    mut units = heavy_table()
    kept = units
    bump(ref units)
    show(stdout, "param", kept)
    # A copy another task holds while the loop runs.
    mut shared_out = heavy_table()
    held = shared_out
    with launch((inbox) => holder(inbox, held, stdout))? as go:
        bump(ref shared_out)
        go.send(1)?
    show(stdout, "after", shared_out)
    # Changed through `units[id]`, a copy kept first.
    mut by_id = Table[Heavy]()
    mut ids = list[Id[Heavy]]()
    for i in range(3):
        ids.append(by_id.add(Heavy(x=i, bag={"gold": i})))
    by_id_backup = by_id
    for id in ids:
        by_id[id].x += 100
        by_id[id].bag["gold"] = 7
    show(stdout, "by id", by_id_backup)
    show(stdout, "by id changed", by_id)
    return
"#;

/// `mut unit = units[id]`, changed, then `units[id] = unit`, for a `Table` and a list, takes the
/// item out rather than copying it, and still means what a copy means. A copy of the collection
/// kept first keeps what it had. A failure between taking the item and putting it back loses the
/// change, as a copy's would be lost, and leaves the item as it was. A crash halfway under
/// `lock ... as ref` puts back every item.
#[test]
fn an_item_copied_out_and_put_back_means_a_copy() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("copied_out_put_back")?;
    std::fs::write(dir.join("copied_out_put_back.fen"), COPIED_OUT_PUT_BACK)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "copied_out_put_back", how)?;
        assert_eq!(
            printed, "(988, 213) 790\n(950, 190) 190\nrefused too much\n(988, 213)\ncrashed\n(988, 213)\n",
            "{how:?}"
        );
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const COPIED_OUT_PUT_BACK: &str = r#"from std.collections import Id, Table
from std.concurrent import Crashed

error Refused:
    why: str

struct Unit:
    hp: int
    bag: dict[str, int]
    tags: list[str]

struct World:
    units = Table[Unit]()
    queue = list[Unit]()

def hurt(ref units: Table[Unit], id: Id[Unit], by: int):
    mut unit = units[id]
    unit.hp -= by
    if unit.hp <= 0:
        unit.hp = 100
        unit.bag["bone"] = (unit.bag.get("bone") else 0) + 1
    unit.tags.append("hurt")
    _ = unit.tags.pop()
    units[id] = unit
    return

def wait(ref queue: list[Unit], at: int):
    mut unit = queue[at]
    unit.hp += 1
    unit.bag["wait"] = (unit.bag.get("wait") else 0) + 1
    queue[at] = unit
    return

def checked(by: int) -> int fails Refused:
    if by > 50: fail Refused(why="too much")
    return by

# A failure between taking the item and putting it back: the change was made to a copy, so
# it's lost, and the `Table` keeps the item as it was.
def hurt_checked(ref units: Table[Unit], id: Id[Unit], by: int) fails Refused:
    mut unit = units[id]
    unit.hp -= 1
    unit.hp -= checked(by)?
    units[id] = unit
    return

def burn(world: shared World, ids: list[Id[Unit]]):
    lock world as ref held:
        for id in ids:
            mut unit = held.units[id]
            unit.hp += 1000
            unit.bag["burn"] = 1
            held.units[id] = unit
            # A crash halfway: the lock puts back everything it changed.
            assert unit.hp < 1050
    return

def burning(world: shared World, ids: list[Id[Unit]]) fails Crashed:
    with launch(() => burn(world, ids))?:
        pass
    return

def totals(units: Table[Unit]) -> (int, int):
    mut hp = 0
    mut items = 0
    for unit in units:
        hp += unit.hp
        items += sum(unit.bag.values())
    return (hp, items)

def main(stdout: Stdout):
    mut world = World()
    mut ids = list[Id[Unit]]()
    for n in range(20):
        ids.append(world.units.add(Unit(hp=n * 5, bag={"gold": n}, tags=["a"])))
        world.queue.append(Unit(hp=n, bag={"gold": n}, tags=["b"]))
    kept = world
    for round in range(30):
        for at, id in enumerate(ids):
            hurt(ref world.units, id, by=at % 7 + round % 3)
            wait(ref world.queue, at)
    stdout.print(totals(world.units), sum(unit.hp for unit in world.queue))
    stdout.print(totals(kept.units), sum(unit.hp for unit in kept.queue))
    hurt_checked(ref world.units, ids[3], by=60) except why:
        case Refused(why):
            stdout.print("refused", why)
    stdout.print(totals(world.units))
    shared burnt = world
    burning(burnt, ids) except why:
        case Crashed():
            stdout.print("crashed")
    lock burnt as held:
        stdout.print(totals(held.units))
    return
"#;

/// The same for a dict's value: a plain struct, a struct holding a dict, and a list. Each goes
/// back into its own entry, from a dict, and a value's own block, made alone first, so copies of
/// the dict kept before keep what they had, and a failure between loses the change. Growing a
/// list held in a dict this way used to copy the whole list each time, which made it quadratic:
/// now only the copies kept are copied.
#[test]
fn a_dict_value_copied_out_and_put_back_means_a_copy() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("dict_value_put_back")?;
    std::fs::write(dir.join("dict_value_put_back.fen"), DICT_VALUE_PUT_BACK)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "dict_value_put_back", how)?;
        assert_eq!(printed, "301 3 302 305 301 44851\n1 2 5 1\nrefused no\n1\n", "{how:?}");
        assert!(count(&counts, "copies") < Some(20) && counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const DICT_VALUE_PUT_BACK: &str = r#"error Refused:
    why: str

struct Plain:
    x: int
    y: int

struct Heavy:
    x: int
    bag: dict[str, int]

def bump_plain(ref d: dict[str, Plain], key: str):
    mut item = d[key]
    item.x += 1
    d[key] = item
    return

def bump_heavy(ref d: dict[str, Heavy], key: str):
    mut item = d[key]
    item.x += 1
    item.bag["n"] = (item.bag.get("n") else 0) + 1
    d[key] = item
    return

def grow(ref d: dict[str, list[int]], key: str, n: int):
    mut items = d[key]
    items.append(n)
    d[key] = items
    return

def checked(n: int) -> int fails Refused:
    if n > 5: fail Refused(why="no")
    return n

def bump_checked(ref d: dict[str, Heavy], key: str, n: int) fails Refused:
    mut item = d[key]
    item.x += 1
    item.x += checked(n)?
    d[key] = item
    return

def main(stdout: Stdout):
    mut plain = {"a": Plain(x=1, y=2), "b": Plain(x=3, y=4)}
    mut heavy = {"a": Heavy(x=1, bag={"n": 0}), "b": Heavy(x=2, bag={"n": 5})}
    mut lists = {"a": [1], "b": [2]}
    kept_plain = plain
    kept_heavy = heavy
    kept_lists = lists
    for n in range(300):
        bump_plain(ref plain, "a")
        bump_heavy(ref heavy, "b")
        grow(ref lists, "a", n)
    stdout.print(
        plain["a"].x,
        plain["b"].x,
        heavy["b"].x,
        heavy["b"].bag["n"],
        len(lists["a"]),
        sum(lists["a"]),
    )
    stdout.print(
        kept_plain["a"].x,
        kept_heavy["b"].x,
        kept_heavy["b"].bag["n"],
        len(kept_lists["a"]),
    )
    bump_checked(ref heavy, "a", n=9) except why:
        case Refused(why):
            stdout.print("refused", why)
    stdout.print(heavy["a"].x)
    return
"#;

/// An array's `for ref` takes each item out and changes it where it is, as a list's does, for
/// items holding a dict and a list: going round twenty times copies no more than going round
/// once, where each time round used to copy every item. A copy of the array kept first, as a
/// local or in a copied struct, keeps what it had, and a failure halfway keeps the changes up to
/// the item that failed.
#[test]
fn an_arrays_ref_loop_changes_its_items_in_place() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("array_ref_loop")?;
    std::fs::write(dir.join("array_ref_loop.fen"), ARRAY_REF_LOOP)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "array_ref_loop", how)?;
        assert_eq!(
            printed,
            "kept 0 0 1\nkept 1 1 1\nkept 2 2 1\nkept 3 3 1\nbumped 10 100 2\nbumped 11 101 2\nbumped 12 102 2\nbumped 13 103 2\ngrid kept 0 0 1\ngrid kept 1 1 1\ngrid kept 2 2 1\ngrid kept 3 3 1\ngrid 10 100 1\ngrid 11 101 1\ngrid 12 102 1\ngrid 13 103 1\nrefused at 1012\nafter 1010 100 2\nafter 1011 101 2\nafter 1012 102 2\nafter 13 103 2\n",
            "{how:?}"
        );
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    let rounds = |times: u32| -> std::io::Result<Option<u64>> {
        let name = format!("array_rounds_{times}");
        let source = ARRAY_REF_LOOP.replace(
            "    bump(ref cells)\n",
            &format!("    for _ in range({times}):\n        bump(ref cells)\n"),
        );
        std::fs::write(dir.join(format!("{name}.fen")), source)?;
        let (_, counts) = printed_and_counts(&dir, &name, &["--llvm"])?;
        Ok(count(&counts, "copies"))
    };
    let (once, often) = (rounds(1)?, rounds(20)?);
    assert!(once.is_some() && once == often, "{once:?} against {often:?}");
    // `cells[at].x += 1`, and an item copied out with `mut` and put back, take it out too: eighty
    // of each copy only what the items first shared, and what the copy kept first shares.
    std::fs::write(dir.join("array_item_forms.fen"), ARRAY_ITEM_FORMS)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "array_item_forms", how)?;
        assert_eq!(printed, "8080 86 8\n0 6 4\n", "{how:?}");
        assert!(count(&counts, "copies") < Some(30) && counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const ARRAY_ITEM_FORMS: &str = r#"struct Cell:
    x = 0
    bag = dict[str, int]()

def poke(ref cells: array[Cell, 4], at: int):
    cells[at].x += 1
    cells[at].bag["n"] = (cells[at].bag.get("n") else 0) + 1
    return

def swap_out(ref cells: array[Cell, 4], at: int):
    mut cell = cells[at]
    cell.x += 100
    cell.bag["m"] = 1
    cells[at] = cell
    return

def main(stdout: Stdout):
    mut cells = array[Cell, 4]()
    for at in range(4):
        cells[at].bag["n"] = at
    kept = cells
    for _ in range(20):
        for at in range(4):
            poke(ref cells, at)
            swap_out(ref cells, at)
    stdout.print(
        sum(cell.x for cell in cells),
        sum(cell.bag["n"] for cell in cells),
        sum(len(cell.bag) for cell in cells),
    )
    stdout.print(
        sum(cell.x for cell in kept),
        sum(cell.bag["n"] for cell in kept),
        sum(len(cell.bag) for cell in kept),
    )
    return
"#;

const ARRAY_REF_LOOP: &str = r#"error Refused:
    at: int

struct Cell:
    x = 0
    bag = dict[str, int]()
    tags = list[str]()

struct Grid:
    cells = array[Cell, 4]()

def fill(ref cells: array[Cell, 4]):
    for at, ref cell in enumerate(cells):
        cell.x = at
        cell.bag["n"] = at
        cell.tags.append(f"t{at}")
    return

def bump(ref cells: array[Cell, 4]):
    for ref cell in cells:
        cell.x += 10
        cell.bag["n"] = (cell.bag.get("n") else 0) + 100
        cell.tags.append("b")
    return

def bump_grid(ref grid: Grid):
    for ref cell in grid.cells:
        cell.x += 10
        cell.bag["n"] = (cell.bag.get("n") else 0) + 100
    return

def bump_until(ref cells: array[Cell, 4], limit: int) fails Refused:
    for ref cell in cells:
        cell.x += 1000
        if cell.x > limit: fail Refused(at=cell.x)
    return

def show(stdout: Stdout, name: str, cells: array[Cell, 4]):
    for cell in cells:
        stdout.print(name, cell.x, cell.bag.get("n") else -1, len(cell.tags))
    return

def main(stdout: Stdout):
    mut cells = array[Cell, 4]()
    fill(ref cells)
    kept = cells
    bump(ref cells)
    show(stdout, "kept", kept)
    show(stdout, "bumped", cells)
    mut grid = Grid()
    fill(ref grid.cells)
    grid_kept = grid
    bump_grid(ref grid)
    show(stdout, "grid kept", grid_kept.cells)
    show(stdout, "grid", grid.cells)
    bump_until(ref cells, limit=1011) except why:
        case Refused(at):
            stdout.print("refused at", at)
    show(stdout, "after", cells)
    return
"#;

/// A dict's value and a `Table`'s item are places, as a list's item is: `held[key].x += 1`,
/// `held[key].bag[k] = v`, `held[key].append(v)`, `held[key].tags.append(v)`, `heal(ref
/// held[key])`, and the same through `units[id]`, each change the value where it is, with the
/// dict or `Table` and the value's block made alone first. Each used to copy the whole value,
/// and growing a list held in a dict copied all of it each time, which was quadratic. Now only
/// the copies kept first are copied, and they keep what they had.
#[test]
fn a_dict_value_and_a_table_item_are_places() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("value_places")?;
    for (name, source, wanted) in [
        ("dict_value_places", DICT_VALUE_PLACES, "301 301 299 301 44850\n1 1 0 1\n"),
        ("dict_value_fields", DICT_VALUE_FIELDS, "200 19900 202 0 2\n"),
        ("table_item_places", TABLE_ITEM_PLACES, "21802 12\n"),
    ] {
        std::fs::write(dir.join(format!("{name}.fen")), source)?;
        for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
            let (printed, counts) = printed_and_counts(&dir, name, how)?;
            assert_eq!(printed, wanted, "{name} {how:?}");
            assert!(
                count(&counts, "copies") < Some(20) && counts.ends_with(" live 0"),
                "{name} {how:?}: {counts}"
            );
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const DICT_VALUE_PLACES: &str = r#"struct Plain:
    x: int
    y: int

struct Heavy:
    x: int
    bag: dict[str, int]

def plain(ref held: dict[str, Plain], key: str):
    held[key].x += 1
    return

def heavy(ref held: dict[str, Heavy], key: str, n: int):
    held[key].x += 1
    held[key].bag["gold"] = n
    return

def grow(ref held: dict[str, list[int]], key: str, n: int):
    held[key].append(n)
    return

def main(stdout: Stdout):
    mut p = {"a": Plain(x=1, y=2)}
    mut h = {"a": Heavy(x=1, bag={"gold": 0})}
    mut g = {"a": [0]}
    kept_p = p
    kept_h = h
    kept_g = g
    for n in range(300):
        plain(ref p, "a")
        heavy(ref h, "a", n=n)
        grow(ref g, "a", n=n)
    stdout.print(p["a"].x, h["a"].x, h["a"].bag["gold"], len(g["a"]), sum(g["a"]))
    stdout.print(kept_p["a"].x, kept_h["a"].x, kept_h["a"].bag["gold"], len(kept_g["a"]))
    return
"#;

const DICT_VALUE_FIELDS: &str = r#"struct Unit:
    hp: int
    tags: list[int]

def heal(ref unit: Unit, by: int):
    unit.hp += by
    return

def main(stdout: Stdout):
    mut held = {"a": Unit(hp=1, tags=[]), "b": Unit(hp=2, tags=[])}
    kept = held
    for round in range(200):
        held["a"].tags.append(round)
        heal(ref held["b"], by=1)
    stdout.print(
        len(held["a"].tags),
        sum(held["a"].tags),
        held["b"].hp,
        len(kept["a"].tags),
        kept["b"].hp,
    )
    return
"#;

const TABLE_ITEM_PLACES: &str = r#"from std.collections import Id, Table

struct Unit:
    hp: int
    bag: dict[str, int]
    tags: list[int]

def heal(ref unit: Unit, by: int):
    unit.hp += by
    unit.tags.append(by)
    return

def main(stdout: Stdout):
    mut units = Table[Unit]()
    mut ids = list[Id[Unit]]()
    for n in range(4):
        ids.append(units.add(Unit(hp=n, bag={"gold": n}, tags=[])))
    kept = units
    for round in range(100):
        for id in ids:
            units[id].bag["gold"] = round
            units[id].tags.append(round)
            heal(ref units[id], by=1)
    mut total = 0
    for unit in units:
        total += unit.hp + unit.bag["gold"] + len(unit.tags) + sum(unit.tags)
    mut kept_total = 0
    for unit in kept:
        kept_total += unit.hp + unit.bag["gold"] + len(unit.tags)
    stdout.print(total, kept_total)
    return
"#;

/// A loop over a dict's keys or values alone, where each is a tuple, reads the tuple's own parts.
/// The backend took `value[0]` and `value[1]` there for the parts of the pair a walk over
/// `items()` never makes, and read the entry's key and value: `value[0]` of a `(int, int)` value
/// gave the dict's key, with no error, and a tuple holding a `str` didn't build. Here keys and
/// values that are tuples, of ints, of a string and of a list, are walked by `keys()`,
/// `values()`, `items()` and a `Table`'s `values()`, and every part is its own.
#[test]
fn a_tuple_walked_as_a_dicts_key_or_value_has_its_own_parts() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds("walked_tuples", WALKED_TUPLES, WALKED_TUPLES_PRINTS)?;
    assert!(counts_of("walked_tuples_counts", WALKED_TUPLES)?.ends_with(" live 0"));
    Ok(())
}

/// The same for every other way a walked item's parts are read: comprehensions and `sum` over a
/// dict's keys, values and items, a `for` that takes each tuple apart, `enumerate`, `zip`, a
/// generator that yields them, `parallel`, and a `Table`'s items and values. Before the fix a
/// comprehension or a `sum` over `values()`, `for left, right in by_int.values()` and
/// `for number, name in by_pair.keys()` each read the entry's key and value, and a `Table`'s
/// `values()` read its id's words.
#[test]
fn a_walked_tuples_parts_are_its_own_however_the_walk_is_written() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    prints_in_both_builds("walked_tuples_every_way", WALKED_EVERY_WAY, WALKED_EVERY_WAY_PRINTS)?;
    assert!(counts_of("walked_tuples_every_way_counts", WALKED_EVERY_WAY)?.ends_with(" live 0"));
    Ok(())
}

const WALKED_EVERY_WAY: &str = r#"# Every other way a walked item's parts are read: comprehensions and `sum` over a dict's keys,
# values and items, `enumerate`, `zip`, a `for` that takes each tuple apart, a generator that
# yields them, `parallel`, and a `Table`'s items. Each key and value is a tuple, so a part read
# from the wrong place shows.

from std.collections import Table

def pair(at: int) -> (int, str):
    return (at, f"key number {at}, long enough for a block")

def each_value(by_int: dict[int, (int, int)]) -> Iterator[(int, int)]:
    for value in by_int.values():
        yield value

def each_key(by_pair: dict[(int, str), (int, int)]) -> Iterator[(int, str)]:
    for key in by_pair.keys():
        yield key

def each_item(by_pair: dict[(int, str), (int, int)]) -> Iterator[((int, str), (int, int))]:
    for item in by_pair.items():
        yield item

def spread(value: (int, int)) -> int:
    return value[1] - value[0]

def main(stdout: Stdout):
    mut by_int: dict[int, (int, int)] = {}
    by_int[1] = (30, 40)
    by_int[2] = (50, 65)
    mut by_pair: dict[(int, str), (int, int)] = {}
    by_pair[pair(3)] = (5, 6)
    by_pair[pair(4)] = (7, 9)
    # Comprehensions and `sum`.
    stdout.print([value[0] for value in by_int.values()], [value[1] for value in by_int.values()])
    stdout.print([key[0] for key in by_pair.keys()], [len(key[1]) for key in by_pair.keys()])
    stdout.print([key[0] * 100 + value[1] for key, value in by_pair.items()])
    stdout.print([item[0][0] * 100 + item[1][0] for item in by_pair.items()])
    stdout.print(sum(value[0] for value in by_int.values()), sum(value[1] for value in by_int.values()))
    stdout.print(sum(key[0] for key in by_pair.keys()), sum(value[1] for key, value in by_pair.items()))
    # A `for` that takes each tuple apart.
    mut lefts = 0
    mut rights = 0
    for left, right in by_int.values():
        lefts += left
        rights += right
    mut numbers = 0
    mut sizes = 0
    for number, name in by_pair.keys():
        numbers += number
        sizes += len(name)
    stdout.print(lefts, rights, numbers, sizes)
    # `enumerate` and `zip` over lists of tuples.
    tuples = [(10, 11), (20, 22)]
    names = [pair(5), pair(6)]
    mut counted = 0
    for at, value in enumerate(tuples):
        counted += at * 1000 + value[0] * 10 + value[1]
    mut numbered = 0
    for numbered_pair in enumerate(tuples):
        numbered += numbered_pair[0] * 1000 + numbered_pair[1][0] * 10 + numbered_pair[1][1]
    mut zipped = 0
    for value, name in zip(tuples, names):
        zipped += value[0] * 10 + value[1] + name[0] * 1000 + len(name[1])
    mut whole = 0
    for both in zip(tuples, names):
        whole += both[0][0] * 10 + both[0][1] + both[1][0] * 1000 + len(both[1][1])
    stdout.print(counted, numbered, zipped, whole)
    # A generator that yields them.
    stdout.print([value[0] for value in each_value(by_int)], [value[1] for value in each_value(by_int)])
    stdout.print([key[0] for key in each_key(by_pair)], [len(key[1]) for key in each_key(by_pair)])
    stdout.print([item[0][0] * 100 + item[1][1] for item in each_item(by_pair)])
    # `parallel`, over a dict's values and over a list of tuples.
    stdout.print([spread(value) for value in parallel(by_int.values())])
    stdout.print([value[0] + value[1] for value in parallel(tuples)])
    # A `Table`'s items and values.
    mut table = Table[(int, str)]()
    _ = table.add(pair(1))
    _ = table.add(pair(2))
    stdout.print([value[0] for value in table.values()], [len(value[1]) for value in table.values()])
    stdout.print([value[0] for id, value in table.items()], sum(len(value[1]) for id, value in table.items()))
    return
"#;

const WALKED_EVERY_WAY_PRINTS: &str = r"[30, 50] [40, 65]
[3, 4] [37, 37]
[306, 409]
[305, 407]
80 105
7 15
80 105 7 74
1333 1333 11407 11407
[30, 50] [40, 65]
[3, 4] [37, 37]
[306, 409]
[10, 15]
[21, 42]
[1, 2] [37, 37]
[1, 2] 74
";

const WALKED_TUPLES: &str = r#"# A loop over a dict's keys or values alone, where each is a tuple: its parts are the tuple's own,
# not the entry's key and value. Beside them, a loop over its items, whose pair's parts are.

from std.collections import Table

def pair(at: int) -> (int, str):
    return (at, f"key number {at}, long enough for a block")

def main(stdout: Stdout):
    mut by_pair: dict[(int, str), int] = {}
    by_pair[pair(7)] = 100
    by_pair[pair(8)] = 200
    mut firsts = 0
    mut sizes = 0
    mut last = ""
    for key in by_pair.keys():
        firsts += key[0]
        sizes += len(key[1])
        last = key[1]
    stdout.print(firsts, sizes, last)
    mut by_int: dict[int, (int, int)] = {}
    by_int[1] = (30, 40)
    by_int[2] = (50, 60)
    mut lefts = 0
    mut rights = 0
    for value in by_int.values():
        lefts += value[0]
        rights += value[1]
    stdout.print(lefts, rights)
    mut by_name: dict[str, (str, list[int])] = {}
    by_name["one"] = ("a first name long enough for a block", [1, 2])
    by_name["two"] = ("a second name long enough for a block", [3])
    mut names = list[str]()
    mut counts = 0
    for value in by_name.values():
        names.append(value[0])
        counts += len(value[1])
    stdout.print(names, counts)
    mut key_sizes = 0
    for key in by_name.keys():
        key_sizes += len(key)
    stdout.print(key_sizes)
    # A walk over items: the pair's parts are the entry's key and value, each a tuple here.
    mut both: dict[(int, str), (int, int)] = {}
    both[pair(3)] = (5, 6)
    both[pair(4)] = (7, 8)
    mut seen = list[str]()
    mut added = 0
    for key, value in both.items():
        seen.append(key[1])
        added += key[0] * 100 + value[0] * 10 + value[1]
    stdout.print(seen, added)
    for item in both.items():
        stdout.print(item[0][0], item[0][1], item[1][0], item[1][1])
    # A `Table`'s values, each a tuple.
    mut table = Table[(int, str)]()
    _ = table.add(pair(1))
    _ = table.add(pair(2))
    mut table_firsts = 0
    mut table_sizes = 0
    for value in table.values():
        table_firsts += value[0]
        table_sizes += len(value[1])
    stdout.print(table_firsts, table_sizes)
    return
"#;

const WALKED_TUPLES_PRINTS: &str = r#"15 74 key number 8, long enough for a block
80 100
["a first name long enough for a block", "a second name long enough for a block"] 3
6
["key number 3, long enough for a block", "key number 4, long enough for a block"] 834
3 key number 3, long enough for a block 5 6
4 key number 4, long enough for a block 7 8
3 74
"#;

/// A dict's `values()`, `keys()` and `items()`, gone through by a `for`, a comprehension or a
/// `sum`, are read where they are, its entries walked in order past those removed, rather than
/// made as a list each time: two hundred rounds of each make nothing more than one. `sum` over
/// one is the loop it stands for. The same goes for a dict reached through an item a loop over
/// a `Table` reads.
#[test]
fn a_dicts_views_are_walked_where_they_are() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("dict_views_walked")?;
    std::fs::write(dir.join("dict_views_walked.fen"), DICT_VIEWS_WALKED)?;
    std::fs::write(dir.join("dict_in_items.fen"), DICT_IN_ITEMS)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "dict_views_walked", how)?;
        assert_eq!(printed, DICT_VIEWS_PRINT, "{how:?}");
        assert!(count(&counts, "allocations") < Some(60) && counts.ends_with(" live 0"), "{how:?}: {counts}");
        let (printed, counts) = printed_and_counts(&dir, "dict_in_items", how)?;
        assert_eq!(printed, "515000\n", "{how:?}");
        assert!(count(&counts, "allocations") < Some(400), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const DICT_VIEWS_PRINT: &str = r#"671400 42
[123, 129, 132, 135, 138, 141, 144]
["k1", "k2", "k3"] [("k1", 3), ("k2", 6)]
kitden 12
0.875 6
0 []
"#;

const DICT_VIEWS_WALKED: &str = r#"struct Unit:
    hp: int
    name: str

def main(stdout: Stdout):
    mut d = dict[str, int]()
    for n in range(50):
        d[f"k{n}"] = n * 3
    for n in range(0, 50, 7):
        _ = d.pop(f"k{n}")
    mut total = 0
    for _ in range(200):
        total += sum(d.values())
        for key in d.keys():
            total += len(key)
        for key, value in d.items():
            total += len(key) * value % 7
        for value in d.values():
            total -= value % 3
    stdout.print(total, len(d))
    stdout.print([value for value in d.values() if value > 120])
    stdout.print([key for key in d.keys()][0:3], [pair for pair in d.items()][0:2])
    mut units = {"a": Unit(hp=3, name="fox"), "b": Unit(hp=5, name="kit")}
    _ = units.pop("a")
    units["c"] = Unit(hp=7, name="den")
    mut names = ""
    for unit in units.values():
        names += unit.name
    stdout.print(names, sum(unit.hp for unit in units.values()))
    floats = {1: 0.5, 2: 0.25, 3: 0.125}
    stdout.print(sum(floats.values()), sum(floats.keys()))
    empty = dict[str, int]()
    stdout.print(sum(empty.values()), [k for k in empty.keys()])
    return
"#;

const DICT_IN_ITEMS: &str = r#"from std.collections import Table

struct E:
    n: int
    inventory: dict[str, int]

def carried(es: Table[E]) -> int:
    mut total = 0
    for e in es:
        total += sum(e.inventory.values())
    return total

def main(stdout: Stdout):
    mut es = Table[E]()
    for n in range(100):
        _ = es.add(E(n=n, inventory={"gold": n, "arrow": 2}))
    mut t = 0
    for _ in range(100):
        t += carried(es)
    stdout.print(t)
    return
"#;

/// `text = text + piece` and `text = f"{text}..."` grow the text where it is, as `text += piece`
/// does, rather than copying it into a new one each time, which made building a text that way
/// quadratic. A copy kept first keeps what it had, and adding a text to itself doubles it.
#[test]
fn text_added_to_itself_grows_where_it_is() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("text_grown")?;
    std::fs::write(dir.join("text_grown.fen"), TEXT_GROWN)?;
    std::fs::write(dir.join("text_chained.fen"), TEXT_CHAINED)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "text_grown", how)?;
        assert_eq!(printed, TEXT_GROWN_PRINTS, "{how:?}");
        assert!(
            count(&counts, "allocations") < Some(200) && counts.ends_with(" live 0"),
            "{how:?}: {counts}"
        );
        // `route = route + "/" + part` grows by each piece in turn, unless a later piece reads the
        // text itself, which would then see it grown already.
        let (printed, _) = printed_and_counts(&dir, "text_chained", how)?;
        assert_eq!(printed, "GET /a!/1!/b!xGET /a!/1!/b! GET /a!/1!/b! ab-ab qq!\n", "{how:?}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const TEXT_GROWN_PRINTS: &str = r"4001 4000 ab! 4001 x0,1,2,3,4,5 ababab 64 a 0 1,000 2,000
";

/// A field's text grown by what a call makes, `self.text += str(value)`, grows where it is: the
/// call can't change the field, so the field is read after it, its only owner as it's added to.
/// It once read the field first, so the text was copied every time, quadratically. A call given
/// the field's holder by `ref`, or a method on it, still sees the field as it was before.
#[test]
fn a_field_grown_by_a_calls_text_grows_where_it_is() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("field_grown_by_call")?;
    std::fs::write(dir.join("field_grown_by_call.fen"), FIELD_GROWN_BY_CALL)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "field_grown_by_call", how)?;
        assert_eq!(printed, "7000\na? <5:1><6:2>\n", "{how:?}");
        assert!(
            count(&counts, "allocations") < Some(200) && counts.ends_with(" live 0"),
            "{how:?}: {counts}"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A field grown by a call that fails keeps what it had: the call runs before the field is read,
/// so a failure out of it leaves the field as the last `+=` that finished left it, and what the
/// calls print comes out in order.
#[test]
fn a_field_grown_by_a_call_that_fails_keeps_what_it_had() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "field_grown_by_failing_call",
        r#"error Bad:
    why: str

struct Writer:
    text = "start"
    total = 0

def risky(value: int, stdout: Stdout) -> str fails Bad:
    stdout.print("called", value)
    if value > 2: fail Bad(why="too big")
    return str(value)

def grow(ref out: Writer, value: int, stdout: Stdout) fails Bad:
    out.text += risky(value, stdout)?
    out.text += "," + risky(value + 1, stdout)?
    return

def main(stdout: Stdout):
    mut out = Writer()
    for value in [1, 2]:
        grow(ref out, value, stdout) except Bad(why):
            stdout.print("failed", why)
        stdout.print(out.text)
    got = risky(5, stdout) except Bad(why): f"<{why}>"
    out.text += got
    stdout.print(out.text)
    return
"#,
        "called 1\ncalled 2\nstart1,2\ncalled 2\ncalled 3\nfailed too big\nstart1,22\ncalled 5\nstart1,22<too big>\n",
    )
}

const FIELD_GROWN_BY_CALL: &str = r#"struct Writer:
    text = ""
    n = 0

    def int(ref self, value: int):
        self.text += str(value)

    def pair(ref self, a: int, b: int):
        self.text += "," + str(a) + str(b)

    def counted(ref self, value: int):
        self.n += 1
        self.text += self.label(value)

    def label(self, value: int) -> str:
        return f"<{value}:{self.n}>"

def bump(ref writer: Writer) -> str:
    writer.text += "!"
    return "?"

def main(stdout: Stdout):
    mut out = Writer()
    for i in range(4000):
        out.int(i % 10)
    for i in range(1000):
        out.pair(i % 3, i % 7)
    stdout.print(len(out.text))
    mut other = Writer()
    other.text = "a"
    other.text += bump(ref other)
    mut third = Writer()
    third.counted(5)
    third.counted(6)
    stdout.print(other.text, third.text)
    return
"#;

/// `text += "," + f()?` that fails leaves the text as it was, in a field, a `ref` parameter, and
/// `text = text + "," + f()?`: the pieces aren't added one at a time when a later one can fail,
/// since those before it would be in already.
#[test]
fn a_text_grown_by_pieces_one_of_which_fails_keeps_what_it_had() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "pieces_failing",
        r#"error Bad:
    why: str

struct Writer:
    text = "start"

def risky(value: int, stdout: Stdout) -> str fails Bad:
    stdout.print("called", value)
    if value > 2: fail Bad(why="too big")
    return str(value)

def grow(ref out: Writer, value: int, stdout: Stdout) fails Bad:
    out.text += "," + risky(value, stdout)? + "!"
    return

def grow_text(ref text: str, value: int, stdout: Stdout) fails Bad:
    text += "," + risky(value, stdout)?
    return

def rebuilt(ref text: str, value: int, stdout: Stdout) fails Bad:
    text = text + "," + risky(value, stdout)?
    return

def main(stdout: Stdout):
    mut out = Writer()
    mut text = "t"
    mut other = "o"
    for value in [2, 3]:
        grow(ref out, value, stdout) except Bad(why):
            stdout.print("failed", why)
        grow_text(ref text, value, stdout) except Bad(why):
            stdout.print("failed", why)
        rebuilt(ref other, value, stdout) except Bad(why):
            stdout.print("failed", why)
        stdout.print(out.text, text, other)
    return
"#,
        "called 2\ncalled 2\ncalled 2\nstart,2! t,2 o,2\n\
         called 3\nfailed too big\ncalled 3\nfailed too big\ncalled 3\nfailed too big\n\
         start,2! t,2 o,2\n",
    )
}

/// A failure in the first, a middle or the last piece of a `+=` leaves what's added to exactly as
/// it was: a text field, a `ref` text, `x = x + a? + b? + c?` and a list field, each failing at
/// each piece in turn, then with none failing, which adds every piece. Nothing is left held.
#[test]
fn a_failure_in_any_piece_of_a_sum_added_on_leaves_the_target_as_it_was() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("pieces_failing_anywhere")?;
    std::fs::write(dir.join("pieces_failing_anywhere.fen"), PIECES_FAILING_ANYWHERE)?;
    let failed = |at: u32| {
        let what = ["piece", "piece", "piece", "items"].map(|kind| format!("failed {kind} {at}\n")).concat();
        format!("{what}{at} t u v [0]\n")
    };
    let wanted = format!("{}{}{}0 t1-2-3 u1-2-3 v123 [0, 1, 2, 3]\n", failed(1), failed(2), failed(3));
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "pieces_failing_anywhere", how)?;
        assert_eq!(printed, wanted, "{how:?}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const PIECES_FAILING_ANYWHERE: &str = r#"error Bad:
    why: str

struct Box:
    text = "t"
    items = [0]

def piece(n: int, fails_at: int) -> str fails Bad:
    if n == fails_at: fail Bad(why=f"piece {n}")
    return str(n)

def items(n: int, fails_at: int) -> list[int] fails Bad:
    if n == fails_at: fail Bad(why=f"items {n}")
    return [n]

def grow_field(ref box: Box, fails_at: int) fails Bad:
    box.text += piece(1, fails_at=fails_at)? + "-" + piece(2, fails_at=fails_at)? + "-" + piece(3, fails_at=fails_at)?
    return

def grow_text(ref text: str, fails_at: int) fails Bad:
    text += piece(1, fails_at=fails_at)? + "-" + piece(2, fails_at=fails_at)? + "-" + piece(3, fails_at=fails_at)?
    return

def rebuild_text(ref text: str, fails_at: int) fails Bad:
    text = text + piece(1, fails_at=fails_at)? + piece(2, fails_at=fails_at)? + piece(3, fails_at=fails_at)?
    return

def grow_items(ref box: Box, fails_at: int) fails Bad:
    box.items += items(1, fails_at=fails_at)? + items(2, fails_at=fails_at)? + items(3, fails_at=fails_at)?
    return

def main(stdout: Stdout):
    for fails_at in [1, 2, 3, 0]:
        mut box = Box()
        mut text = "u"
        mut other = "v"
        grow_field(ref box, fails_at=fails_at) except Bad(why):
            stdout.print("failed", why)
        grow_text(ref text, fails_at=fails_at) except Bad(why):
            stdout.print("failed", why)
        rebuild_text(ref other, fails_at=fails_at) except Bad(why):
            stdout.print("failed", why)
        grow_items(ref box, fails_at=fails_at) except Bad(why):
            stdout.print("failed", why)
        stdout.print(fails_at, box.text, text, other, box.items)
    return
"#;

const TEXT_CHAINED: &str = r#"def main(stdout: Stdout):
    mut route = "GET "
    for part in ["a", "1", "b"]:
        route = route + "/" + part + "!"
    kept = route
    route = route + "x" + route
    mut n = 0
    mut text = "ab"
    text = f"{text}-{text}"
    mut other = "q"
    other = other + f"{other}!"
    stdout.print(route, kept, text, other)
    return
"#;

const TEXT_GROWN: &str = r#"def main(stdout: Stdout):
    mut text = ""
    for n in range(2000):
        text = text + "ab"
    mut formatted = "x"
    for n in range(2000):
        formatted = f"{formatted}{n % 10},"
    kept = text
    text = text + "!"
    mut doubled = "ab"
    for _ in range(5):
        doubled = doubled + doubled
    mut spaced = "a"
    for n in range(3):
        spaced = f"{spaced} {n * 1000:,}"
    stdout.print(
        len(text),
        len(kept),
        text[-3:],
        len(formatted),
        formatted[0:12],
        doubled[0:6],
        len(doubled),
        spaced,
    )
    return
"#;

/// A struct made with a list nothing reads again takes the list's owner over, so the list has
/// one owner and changing it through the struct copies nothing: a list built just for a struct
/// used to be copied, with its items, the first time anything changed it. A list that's read
/// again, made with in a loop, or given to two fields keeps its own owner, so it's copied where
/// it changes, as a copy has to be.
#[test]
fn a_struct_made_with_a_list_built_for_it_takes_it_over() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("made_with_a_list")?;
    std::fs::write(dir.join("made_with_a_list.fen"), MADE_WITH_A_LIST)?;
    std::fs::write(dir.join("list_built_for_it.fen"), LIST_BUILT_FOR_IT)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "made_with_a_list", how)?;
        assert_eq!(printed, MADE_WITH_A_LIST_PRINTS, "{how:?}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
        let (printed, counts) = printed_and_counts(&dir, "list_built_for_it", how)?;
        assert_eq!(printed, "95\n", "{how:?}");
        assert!(counts.starts_with("copies 0 ") && counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const MADE_WITH_A_LIST_PRINTS: &str = r"[1, 2, 9] [1, 2, 3]
[[5, 0], [5, 10, 1], [5, 10, 11, 2]] [5, 10, 11, 12]
[7, 8] [7] [7]
[0, 1, 2, 3, 100] [4]
";

const MADE_WITH_A_LIST: &str = r"struct W:
    es: list[int]
    more: list[int]

def build(n: int) -> W:
    mut made = list[int]()
    for i in range(n):
        made.append(i)
    return W(es=made, more=[n])

def main(stdout: Stdout):
    mut xs = [1, 2]
    mut w = W(es=xs, more=[0])
    xs.append(3)
    w.es.append(9)
    stdout.print(w.es, xs)
    mut kept = list[W]()
    mut ys = [5]
    for round in range(3):
        mut v = W(es=ys, more=[round])
        v.es.append(round)
        kept.append(v)
        ys.append(10 + round)
    stdout.print([k.es for k in kept], ys)
    same = [7]
    mut twice = W(es=same, more=same)
    twice.es.append(8)
    stdout.print(twice.es, twice.more, same)
    mut b = build(4)
    b.es.append(100)
    stdout.print(b.es, b.more)
    return
";

const LIST_BUILT_FOR_IT: &str = r#"struct E:
    x: int
    d: dict[str, int]

struct W:
    es: list[E]

def bump(ref w: W):
    for ref e in w.es:
        e.x += 1
    return

def main(stdout: Stdout):
    mut w = W(es=[E(x=i, d={"a": 1}) for i in range(10)])
    for _ in range(5):
        bump(ref w)
    stdout.print(sum(e.x for e in w.es))
    return
"#;

/// `a + b` of texts too long to join in a header, or held in blocks, is made as one block at
/// the size of both, each copied in once, with neither boxed into a block of its own first: a
/// short text held in its header was copied into a block, joined and let go of. It gives what it
/// gave, for texts of every length and of characters past ASCII.
#[test]
fn texts_joined_past_a_header_are_made_once() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("texts_joined")?;
    std::fs::write(dir.join("texts_joined.fen"), TEXTS_JOINED)?;
    std::fs::write(dir.join("texts_added_in_turn.fen"), TEXTS_ADDED_IN_TURN)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "texts_joined", how)?;
        assert_eq!(printed, TEXTS_JOINED_PRINT, "{how:?}");
        // `text += a + b + c` adds each piece in turn, to a name or a field, unless a piece reads
        // what the text is reached from.
        let (printed, _) = printed_and_counts(&dir, "texts_added_in_turn", how)?;
        assert_eq!(printed, "\"a\": <\">\"bb\": <\"> xyzx pqrs [\"\", \"mn\"]\n", "{how:?}");
        assert!(count(&counts, "allocations") < Some(90) && counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// `a + b` gives a text equal to the same text made any other way, at every length either side of
/// what a header holds: 22, 23, 24 and 25 bytes, empty either side, and characters of two and
/// three bytes where 23 bytes end. Each joined text is compared with the same text from a list,
/// is a dict's key found by that text, and says its length in characters. The texts come out of
/// lists, so each join is made where it runs, and every block is freed after, with none left.
#[test]
fn a_join_at_every_length_past_a_header_equals_the_same_text_made_another_way() -> std::io::Result<()> {
    use std::fmt::Write as _;
    if !ready() {
        return Ok(());
    }
    let (e11, d7) = ("é".repeat(11), "日".repeat(7));
    let rows: Vec<(String, String)> = vec![
        (String::new(), String::new()),
        (String::new(), "x".to_owned()),
        ("a".repeat(11), "b".repeat(11)),
        ("a".repeat(11), "b".repeat(12)),
        ("a".repeat(12), "b".repeat(12)),
        ("a".repeat(12), "b".repeat(13)),
        (String::new(), "c".repeat(24)),
        ("d".repeat(24), String::new()),
        (e11.clone(), "a".to_owned()),
        ("é".repeat(6), "é".repeat(6)),
        (d7.clone(), "é".to_owned()),
        (d7.clone(), "日".to_owned()),
        (format!("{d7}é"), "日".to_owned()),
        ("a\"b{c}".to_owned(), "\\".repeat(20)),
        ("x".repeat(40), "y".repeat(40)),
    ];
    let lefts: Vec<String> = rows.iter().map(|(left, _)| fennec_literal(left)).collect();
    let rights: Vec<String> = rows.iter().map(|(_, right)| fennec_literal(right)).collect();
    let wanted: Vec<String> =
        rows.iter().map(|(left, right)| fennec_literal(&format!("{left}{right}"))).collect();
    let source = format!(
        r#"def main(stdout: Stdout):
    lefts = [{}]
    rights = [{}]
    wanted = [{}]
    mut seen = dict[str, int]()
    for at in range(len(lefts)):
        joined = lefts[at] + rights[at]
        seen[joined] = at
        same = "same" if joined == wanted[at] else "differs"
        stdout.print(f"{{len(joined)}} {{same}} {{joined}}")
    for text in wanted:
        stdout.print(seen.get(text) else -1)
    return
"#,
        lefts.join(", "),
        rights.join(", "),
        wanted.join(", ")
    );
    let mut expected = String::new();
    let mut last = std::collections::HashMap::new();
    for (at, (left, right)) in rows.iter().enumerate() {
        let joined = format!("{left}{right}");
        let _ = writeln!(expected, "{} same {joined}", joined.chars().count());
        last.insert(joined, at);
    }
    for (left, right) in &rows {
        let found = last.get(&format!("{left}{right}")).copied().unwrap_or(usize::MAX);
        let _ = writeln!(expected, "{found}");
    }
    let dir = folder("joins_past_a_header")?;
    let program = dir.join("joins_past_a_header.fen");
    std::fs::write(&program, &source)?;
    prints_with_nothing_left(&dir, &program, &expected, &["1"])
}

const TEXTS_ADDED_IN_TURN: &str = r#"struct Writer:
    text = ""
    parts = ["", ""]

    def field(ref self, name: str):
        self.text += "\"" + name + "\": "
        self.text += "<" + self.text[0:1] + ">"
        return

def main(stdout: Stdout):
    mut w = Writer()
    w.field("a")
    w.field("bb")
    mut t = "x"
    t += "y" + "z" + t
    mut u = "p"
    u += "q" + ("r" + "s")
    w.parts[1] += "m" + "n"
    stdout.print(w.text, t, u, w.parts)
    return
"#;

const TEXTS_JOINED_PRINT: &str = r"abab aba string long enough to be held in a block of its own a string long enough to be held in a block of its ownab a string long enough to be held in a block of its owna string long enough to be held in a block of its own
55 56 é日本é日本 18
é日本 own 56 12
165 29aé日
";

const TEXTS_JOINED: &str = r#"def main(stdout: Stdout):
    short = "ab"
    long_one = "a string long enough to be held in a block of its own"
    wide = "é日本"
    stdout.print(short + short, short + long_one, long_one + short, long_one + long_one)
    stdout.print(
        len(short + long_one),
        len(wide + long_one),
        wide + wide,
        len(wide + wide + wide + wide + wide + wide),
    )
    joined = wide + long_one
    stdout.print(joined[0:3], joined[-3:], len(joined), joined.find("long") else -1)
    mut parts = list[str]()
    for n in range(30):
        parts.append(f"{n}" + long_one[0:n % 7] + wide[0:n % 3])
    stdout.print(len("".join(parts)), parts[29])
    return
"#;

/// `text += piece` with a piece held in its header and no room left in the text's block makes
/// the joined text natively, in a block with room to double, letting the old text go, as the
/// runtime grows one: the piece used to be boxed into a block of its own to hand to the runtime,
/// and let go of again. A copy kept first keeps what it had, and adding a text to itself doubles.
#[test]
fn text_grown_past_its_room_takes_no_box() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("text_grown_past_room")?;
    std::fs::write(dir.join("text_grown_past_room.fen"), TEXT_GROWN_PAST_ROOM)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "text_grown_past_room", how)?;
        assert_eq!(printed, TEXT_GROWN_PAST_ROOM_PRINT, "{how:?}");
        assert!(count(&counts, "allocations") < Some(30) && counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const TEXT_GROWN_PAST_ROOM_PRINT: &str = r"132 130 é日本 né HTTP/1.1
445 7é8é9é -1
64 !!si
";

const TEXT_GROWN_PAST_ROOM: &str = r#"def main(stdout: Stdout):
    long_one = "a string long enough to be held in a block of its own"
    mut out = f"HTTP/1.1 {200} OK and some more words\r\n"
    out += "Content-Type: x\r\n"
    out += f"Content-Length: {len(long_one)}\r\n\r\n"
    out += long_one
    out += "é"
    kept = out
    out += "日本"
    stdout.print(len(out), len(kept), out[-3:], kept[-2:], out[0:8])
    mut built = "start of a text that does not fit in a header"
    for n in range(200):
        built += f"{n % 10}é"
    stdout.print(len(built), built[-6:], built.find("9é9") else -1)
    mut same = "sixteen chars!!!sixteen chars!!!"
    same += same
    stdout.print(len(same), same[30:34])
    return
"#;

/// An item copied out with `mut` and put back by a key read again goes back only where it came
/// from: when what the key is read from changes between, by `swap`, `reverse`, an item set or a
/// call given it by `ref`, the item is copied, as the source says, rather than taken out. Taking
/// it out then put it in another item's place and left the first with no list. A `Table`, a list
/// and a dict, each of the four ways, print what a copy gives.
#[test]
fn an_item_put_back_by_a_key_that_moved_is_a_copy() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("put_back_moved_key")?;
    std::fs::write(dir.join("put_back_moved_key.fen"), PUT_BACK_MOVED_KEY)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "put_back_moved_key", how)?;
        assert_eq!(printed, PUT_BACK_MOVED_KEY_PRINT, "{how:?}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const PUT_BACK_MOVED_KEY_PRINT: &str = r#"table 0 [(0, [0]), (100, [0, 5]), (2, [2])]
list 0 [(0, [0]), (100, [0, 7]), (2, [2])]
dict 0 [("a", 0, [0]), ("b", 100, [0, 9]), ("c", 2, [2])]
table 1 [(0, [0]), (1, [1]), (100, [0, 5])]
list 1 [(0, [0]), (1, [1]), (100, [0, 7])]
dict 1 [("a", 0, [0]), ("b", 1, [1]), ("c", 100, [0, 9])]
table 2 [(0, [0]), (1, [1]), (100, [0, 5])]
list 2 [(0, [0]), (1, [1]), (100, [0, 7])]
dict 2 [("a", 0, [0]), ("b", 1, [1]), ("c", 100, [0, 9])]
table 3 [(0, [0]), (1, [1]), (100, [0, 5])]
list 3 [(0, [0]), (1, [1]), (100, [0, 7])]
dict 3 [("a", 0, [0]), ("b", 1, [1]), ("c", 100, [0, 9])]
"#;

const PUT_BACK_MOVED_KEY: &str = r#"from std.collections import Id, Table

struct Heavy:
    x: int
    names: list[int]

def shuffle(ref keys: list[Id[Heavy]]):
    keys.reverse()
    return

def shuffle_at(ref keys: list[int]):
    keys.reverse()
    return

def shuffle_names(ref keys: list[str]):
    keys.reverse()
    return

def table_case(stdout: Stdout, how: int):
    mut table = Table[Heavy]()
    mut ids = list[Id[Heavy]]()
    for i in range(3):
        ids.append(table.add(Heavy(x=i, names=[i])))
    mut item = table[ids[0]]
    item.x += 100
    item.names.append(5)
    if how == 0:
        ids.swap(0, 1)
    elif how == 1:
        ids.reverse()
    elif how == 2:
        ids[0] = ids[2]
    else:
        shuffle(ref ids)
    table[ids[0]] = item
    stdout.print("table", how, [(h.x, h.names) for h in table.values()])
    return

def list_case(stdout: Stdout, how: int):
    mut items = [Heavy(x=i, names=[i]) for i in range(3)]
    mut places = [0, 1, 2]
    mut item = items[places[0]]
    item.x += 100
    item.names.append(7)
    if how == 0:
        places.swap(0, 1)
    elif how == 1:
        places.reverse()
    elif how == 2:
        places[0] = 2
    else:
        shuffle_at(ref places)
    items[places[0]] = item
    stdout.print("list", how, [(h.x, h.names) for h in items])
    return

def dict_case(stdout: Stdout, how: int):
    mut held = {"a": Heavy(x=0, names=[0]), "b": Heavy(x=1, names=[1]), "c": Heavy(x=2, names=[2])}
    mut keys = ["a", "b", "c"]
    mut item = held[keys[0]]
    item.x += 100
    item.names.append(9)
    if how == 0:
        keys.swap(0, 1)
    elif how == 1:
        keys.reverse()
    elif how == 2:
        keys[0] = "c"
    else:
        shuffle_names(ref keys)
    held[keys[0]] = item
    stdout.print("dict", how, [(k, held[k].x, held[k].names) for k in sorted(held.keys())])
    return

def main(stdout: Stdout):
    for how in range(4):
        table_case(stdout, how=how)
        list_case(stdout, how=how)
        dict_case(stdout, how=how)
    return
"#;

/// A loop over a `Table` that only reads its items' fields borrows each from its slot, rather than
/// giving it an owner and letting go again each time round, when nothing in the loop changes the
/// `Table`. One that keeps the item, as `held.append(unit)` does, gives it an owner: the item kept
/// stays as it was when the `Table`'s is changed after. Through a parameter, a local and a field.
#[test]
fn a_tables_items_only_read_in_a_loop_are_borrowed() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("table_items_borrowed")?;
    std::fs::write(dir.join("table_items_borrowed.fen"), TABLE_ITEMS_BORROWED)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (printed, counts) = printed_and_counts(&dir, "table_items_borrowed", how)?;
        assert_eq!(printed, "259000 2450 u1 changed 50 0\n", "{how:?}");
        assert!(counts.ends_with(" live 0"), "{how:?}: {counts}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const TABLE_ITEMS_BORROWED: &str = r#"from std.collections import Id, Table

struct Unit:
    hp: int
    bag: dict[str, int]
    name: str

struct World:
    units: Table[Unit]

def total(units: Table[Unit]) -> int:
    mut sum_hp = 0
    for unit in units:
        sum_hp += unit.hp + len(unit.name) + (unit.bag.get("g") else 0)
    return sum_hp

def main(stdout: Stdout):
    mut units = Table[Unit]()
    mut ids = list[Id[Unit]]()
    for n in range(50):
        ids.append(units.add(Unit(hp=n, bag={"g": n}, name=f"u{n}")))
    mut t = 0
    for _ in range(100):
        t += total(units)
    kept = list[Unit]()
    mut seen = 0
    for unit in units:
        seen += unit.hp
    mut world = World(units=units)
    for unit in world.units:
        seen += unit.hp
    mut held = list[Unit]()
    for unit in units:
        held.append(unit)
    units[ids[1]].name = "changed"
    stdout.print(t, seen, held[1].name, units[ids[1]].name, len(held), len(kept))
    return
"#;

#[test]
fn id_maps_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(units[a].health, len(units), units.get(b) is None)
    gone = units.remove(b)
    stdout.print(units.get(b) is None, len(units), a == a, a == b)
    stdout.print(a in units, b in units, a not in units, b not in units)
    c = units.add(Unit(name="den", health=7))
    stdout.print(b in units, c in units)
    ids: list[Id[Unit]] = units.ids()
    for id, unit in units.items():
        stdout.print(unit.name, unit.health)
    stdout.print(len(ids), len(units.values()), c == b)
    if gone is not None:
        stdout.print(gone.name)
"#,
    )?;
    same_message_as_pinned(
        "stale",
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut names = Table[str]()\n    fox = names.add(\"fox\")\n    _ = names.remove(fox)\n    stdout.print(names[fox])\n",
    )
}

/// An `Id` finds nothing in a `Table` other than the one that gave it out, though a copy of that
/// one, holding the same items, is the same `Table`.
#[test]
fn ids_from_another_table_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "other_table",
        r#"from std.collections import Id, Table

def main(stdout: Stdout):
    mut ours = Table[str]()
    mut theirs = Table[str]()
    fox = ours.add("fox")
    kit = theirs.add("kit")
    stdout.print(fox == kit, ours.get(kit) is None, kit in ours, fox in theirs)
    stdout.print(theirs.remove(fox) is None, len(theirs), ours[fox])
    mut copied = ours
    copied[fox] = "den"
    stdout.print(copied[fox], ours[fox], fox in copied)
    held: list[Id[str]] = ours.ids()
    for id, name in theirs.items():
        stdout.print(name, id in theirs, id in ours, held[0] in ours)
"#,
    )?;
    same_message_as_pinned(
        "other_table_stop",
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut ours = Table[str]()\n    mut theirs = Table[str]()\n    _ = ours.add(\"fox\")\n    kit = theirs.add(\"kit\")\n    stdout.print(ours[kit])\n",
    )?;
    same_message_as_pinned(
        "other_table_store",
        "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut ours = Table[str]()\n    mut theirs = Table[str]()\n    _ = ours.add(\"fox\")\n    kit = theirs.add(\"kit\")\n    ours[kit] = \"den\"\n    stdout.print(len(ours))\n",
    )?;
    // Saved, an `Id` has to be from the `Table` saved with it.
    same_message_as_pinned(
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
fn tuple_slices_and_enumerated_text_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
fn slices_as_dict_keys_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "slices",
        r#"def main(stdout: Stdout):
    text = "abcabcabd"
    mut counts: dict[str, int] = {}
    for at in range(len(text) - 1):
        piece = text[at:at + 2]
        counts[piece] = (counts.get(piece) else 0) + 1
    for size in [1, 3]:
        for at in range(len(text)):
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

/// A slice as a dict's key, set, read with `get`, tested with `in`, popped and added to a set,
/// for values of every kind a dict holds: a string, an int, a float, a bool, a list, a struct, a
/// struct holding a list, and a dict. Each does what the same key does made another way, by an
/// f-string, in either build. A string value used to be stored by a slice key as if it were a
/// block, which LLVM refused, as `names[line[:colon]] = line[colon + 2:]` in a parse of headers.
#[test]
fn a_slice_as_a_key_works_for_every_kind_of_value() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("slice_keys_every_value")?;
    let ran = |name: &str, source: &str, how: &[&str]| -> std::io::Result<(String, String)> {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        let executable = dir.join(name);
        let mut args = vec!["build"];
        args.extend_from_slice(how);
        let (program, output) =
            (program.to_string_lossy().into_owned(), executable.to_string_lossy().into_owned());
        args.extend([program.as_str(), "-o", output.as_str()]);
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{name} {how:?}: {}", String::from_utf8_lossy(&built.stderr));
        let run = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
        assert!(run.status.success(), "{name} {how:?}: {}", String::from_utf8_lossy(&run.stderr));
        Ok((
            String::from_utf8_lossy(&run.stdout).into_owned(),
            String::from_utf8_lossy(&run.stderr).into_owned(),
        ))
    };
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (sliced, counts) = ran("sliced", SLICED_KEYS, how)?;
        let (made, _) = ran("made", MADE_KEYS, how)?;
        assert_eq!(sliced, made, "{how:?}");
        assert!(
            counts.lines().any(|line| line.starts_with("copies ") && line.ends_with(" live 0")),
            "{how:?}: {counts}"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const SLICED_KEYS: &str = r#"struct Point:
    x: int
    y: int

struct Holder:
    items: list[int]

def main(stdout: Stdout):
    lines = ["ab: one", "cd: two", "ab: three", "ef: four"]
    mut texts = dict[str, str]()
    mut counts = dict[str, int]()
    mut floats = dict[str, float]()
    mut flags = dict[str, bool]()
    mut lists = dict[str, list[str]]()
    mut points = dict[str, Point]()
    mut holders = dict[str, Holder]()
    mut nested = dict[str, dict[str, int]]()
    mut seen = set[str]()
    for line in lines:
        colon = line.find(":") else 0
        texts[line[:colon]] = line[colon + 2:]
        counts[line[:colon]] = (counts.get(line[:colon]) else 0) + 1
        floats[line[:colon]] = 0.5
        flags[line[:colon]] = line[:colon] in counts
        lists[line[:colon]] = [line[colon + 2:]]
        points[line[:colon]] = Point(x=colon, y=len(line))
        holders[line[:colon]] = Holder(items=[colon])
        nested[line[:colon]] = {"n": colon}
        seen.add(line[:colon])
        stdout.print(
            texts.get(line[:colon]) else "-",
            line[:colon] in texts,
            line[:colon] in seen,
            counts[line[:colon]],
        )
    first = lines[0]
    gone = texts.pop(first[0:2])
    stdout.print(gone, texts, counts, floats, flags, lists)
    stdout.print(
        [(k, p.x, p.y) for k, p in points.items()],
        [len(holders[k].items) for k in sorted(holders.keys())],
    )
    stdout.print(nested, sorted(seen), first[0:2] in seen, first[0:2] in texts)
    seen.discard(first[0:2])
    stdout.print(sorted(seen))
    return
"#;

const MADE_KEYS: &str = r#"struct Point:
    x: int
    y: int

struct Holder:
    items: list[int]

def main(stdout: Stdout):
    lines = ["ab: one", "cd: two", "ab: three", "ef: four"]
    mut texts = dict[str, str]()
    mut counts = dict[str, int]()
    mut floats = dict[str, float]()
    mut flags = dict[str, bool]()
    mut lists = dict[str, list[str]]()
    mut points = dict[str, Point]()
    mut holders = dict[str, Holder]()
    mut nested = dict[str, dict[str, int]]()
    mut seen = set[str]()
    for line in lines:
        colon = line.find(":") else 0
        texts[f"{line[:colon]}"] = line[colon + 2:]
        counts[f"{line[:colon]}"] = (counts.get(f"{line[:colon]}") else 0) + 1
        floats[f"{line[:colon]}"] = 0.5
        flags[f"{line[:colon]}"] = f"{line[:colon]}" in counts
        lists[f"{line[:colon]}"] = [line[colon + 2:]]
        points[f"{line[:colon]}"] = Point(x=colon, y=len(line))
        holders[f"{line[:colon]}"] = Holder(items=[colon])
        nested[f"{line[:colon]}"] = {"n": colon}
        seen.add(f"{line[:colon]}")
        stdout.print(
            texts.get(f"{line[:colon]}") else "-",
            f"{line[:colon]}" in texts,
            f"{line[:colon]}" in seen,
            counts[f"{line[:colon]}"],
        )
    first = lines[0]
    gone = texts.pop(first[0:2])
    stdout.print(gone, texts, counts, floats, flags, lists)
    stdout.print(
        [(k, p.x, p.y) for k, p in points.items()],
        [len(holders[k].items) for k in sorted(holders.keys())],
    )
    stdout.print(nested, sorted(seen), first[0:2] in seen, first[0:2] in texts)
    seen.discard(first[0:2])
    stdout.print(sorted(seen))
    return
"#;

/// A struct kept in place that holds a list, read out of a dict by `values()` or `items()`, or out
/// of a list by `enumerate` or `zip`, is copied with an owner of its own for what it holds. The
/// copy used to share the list with no owner of its own, so it read freed memory once the dict
/// let the list go: the first value's list read as `None`, and printing them could crash.
#[test]
fn a_struct_copied_out_of_a_dict_owns_what_it_holds() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("struct_values_owned")?;
    let program = dir.join("struct_values_owned.fen");
    std::fs::write(&program, STRUCT_VALUES_OWNED)?;
    let executable = dir.join("struct_values_owned");
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let mut args = vec!["build"];
        args.extend_from_slice(how);
        let (source, output) =
            (program.to_string_lossy().into_owned(), executable.to_string_lossy().into_owned());
        args.extend([source.as_str(), "-o", output.as_str()]);
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{how:?}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).env("FENNEC_COUNTS", "1").output_within()?;
        assert!(ran.status.success(), "{how:?}: {}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), STRUCT_VALUES_PRINT, "{how:?}");
        let said = String::from_utf8_lossy(&ran.stderr);
        assert!(
            said.lines().any(|line| line.starts_with("copies ") && line.ends_with(" live 0")),
            "{how:?}: {said}"
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const STRUCT_VALUES_PRINT: &str = r"[7] [7] [[7], [7]]
[7] [[7], [7]]
[7]
[7] [7]
[Holder(items=[1, 2], n=2)]
";

const STRUCT_VALUES_OWNED: &str = r#"struct Holder:
    items: list[int]
    n: int

def main(stdout: Stdout):
    mut h = dict[str, Holder]()
    for line in ["ab", "cd"]:
        h[line] = Holder(items=[7], n=1)
    vals = h.values()
    stdout.print(vals[0].items, vals[1].items, [x.items for x in h.values()])
    pairs = h.items()
    stdout.print(pairs[0][1].items, [p[1].items for p in pairs])
    numbered = enumerate(vals)
    stdout.print(numbered[1][1].items)
    zipped = zip(vals, vals)
    stdout.print(zipped[0][0].items, zipped[1][1].items)
    mut by_int = dict[int, Holder]()
    by_int[0] = Holder(items=[1, 2], n=2)
    stdout.print(by_int.values())
    return
"#;

/// All of `std.crypto`, of text and of bytes.
#[test]
fn crypto_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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

/// A BLAKE3 key that isn't one says so, as pinned.
#[test]
fn a_bad_crypto_key_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
fn std_calls_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    return n

def outer(n: int) -> int fails Wrapped:
    return inner(n)? from Wrapped(why="inner failed")

## What `run` read, or what its failure says.
def shown(run: () -> Tool fails ParseError) -> str:
    read = run() except error:
        return error.message
    return f"{read}"

def main(stdout: Stdout):
    stdout.print(crypto.sha256("abc"), crypto.blake3("abc"))
    stdout.print(crypto.keyed_blake3("0000000000000000000000000000000000000000000000000000000000000000", "abc") else "no")
    stdout.print(crypto.equal("a", "a"), crypto.equal("a", "b"))
    tool = Tool(main_weapon="bow", uses=3)
    stdout.print(json.dumps(tool, names=.CamelCase), json.dumps(tool, names=.KebabCase))
    stdout.print(toml.dumps(tool) else "no")
    stdout.print(shown(() => toml.loads[Tool]("main_weapon = \"axe\"\nuses = 2\n")?))
    stdout.print(shown(() => toml.loads[Tool]("not toml")?))
    outer_value = outer(1) except Wrapped(): -1
    outer_value2 = outer(2) except Wrapped(): -1
    stdout.print(outer_value, outer_value2)
"#,
    )
}

#[test]
fn std_pieces_the_proof_program_uses_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    // Lists of lists and `dict.pop`.
    same_as_pinned(
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
    stdout.print(grid, copy_of, sum_of, len(grid[2]))
    mut counts: dict[str, int] = {"a": 1, "b": 2}
    gone = counts.pop("a")
    stdout.print(gone, len(counts), "a" in counts)
"#,
    )?;
    // std's `PriorityQueue`, which compares tuples and pops from a dict.
    same_as_pinned(
        "queue",
        r#"from std.collections import PriorityQueue

def main(stdout: Stdout):
    mut queue: PriorityQueue[str, int] = PriorityQueue()
    for n in [5, 1, 4, 2, 3]:
        queue.push(f"job{n}", n)
    stdout.print(queue.peek() else "none", len(queue))
    while len(queue) > 0:
        stdout.print(queue.pop() else "none")
"#,
    )?;
    // f-strings of bools, tuples, plain structs and enums, and `list.pop(at)`.
    same_as_pinned(
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
    stdout.print(f"{first}", len(spots))
"#,
    )?;
    // std's `Random`, drawn as pinned, ending in its failure.
    same_message_as_pinned(
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
    same_as_pinned(
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
    return b.items

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
        let (ours, wanted) = both_built(name, source, how)?;
        assert_eq!(ours, wanted);
        let (ours, wanted) = both_built(&format!("{name}_shifted"), shifted, how)?;
        assert_eq!(ours, wanted);
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
                  if name == \"\": fail Missing(name=\"nobody\")\n    return len(name)\n\n\
                  def look(name: str) -> int fails Missing:\n    return find(name)? + 1\n\n\
                  def main(stdout: Stdout) fails Missing:\n    stdout.print(look(\"\")?)\n";
    let bouncing = "def ping(n: int) -> int:\n    return pong(n + 1)\n\ndef pong(n: int) -> int:\n    return ping(n + 1)\n\n\
                    def main(stdout: Stdout):\n    stdout.print(ping(0))\n";
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let (_, said, code) = both_built_unpinned("raised_chain", raised, how)?;
        assert_eq!(code, Some(1));
        // A release names its files from where it was built, never the builder's folders.
        let at = if how.contains(&"--release") { "" } else { "<dir>/" };
        assert!(said.contains(&format!(" --> {at}raised_chain.fen:5:20\n")), "{how:?}: {said}");
        assert!(
            said.ends_with(&format!(
                "  = in `find`\n  = called from `look` at {at}raised_chain.fen:9:12\n  \
                 = called from `main` at {at}raised_chain.fen:12:18\n"
            )),
            "{how:?}: {said}"
        );
        let (_, said, _) = both_built_unpinned("bouncing_chain", bouncing, how)?;
        let calls = said.lines().filter(|line| line.contains("called from")).count();
        assert_eq!(calls, 2 * 16, "{how:?}: {said}");
        assert!(said.contains("  = ... more calls\n"), "{how:?}: {said}");
        assert!(
            said.ends_with(&format!("  = called from `main` at {at}bouncing_chain.fen:8:18\n")),
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
fn failures_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let print_after = "\n\ndef main(stdout: Stdout):\n    stdout.print(\"before\")\n";
    same_as_pinned(
        "overflow",
        &format!(
            "def f(n: int) -> int:\n    return n * 9223372036854775807{print_after}    stdout.print(f(3))\n"
        ),
    )?;
    same_as_pinned(
        "zero",
        &format!("def f(n: int) -> int:\n    return 10 // n{print_after}    stdout.print(f(0))\n"),
    )?;
    same_as_pinned(
        "outside",
        &format!("def f(n: int) -> int:\n    return [1, 2][n]{print_after}    stdout.print(f(2))\n"),
    )?;
    same_message_as_pinned(
        "before",
        &format!(
            "def f(n: int) -> int:\n    mut a = [1, 2]\n    a[n] = 3\n    return a[0]{print_after}    stdout.print(f(-3))\n"
        ),
    )?;
    // A format Fennec doesn't have is caught when the program's checked, before anything runs.
    let dir = folder("spec")?;
    let program = dir.join("spec.fen");
    std::fs::write(
        &program,
        format!("def f(n: int) -> int:\n    return n{print_after}    stdout.print(f\"{{f(1):z}}\")\n"),
    )?;
    let output = dir.join("spec").to_string_lossy().into_owned();
    let built = fennec(&["build", "--llvm", &program.to_string_lossy(), "-o", &output], &dir)?;
    assert!(!built.status.success());
    assert!(String::from_utf8_lossy(&built.stderr).contains("Fennec has no format `z`"));
    std::fs::remove_dir_all(&dir)?;
    same_as_pinned(
        "deep",
        &format!("def f(n: int) -> int:\n    return f(n + 1) + 1{print_after}    stdout.print(f(0))\n"),
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
            "{line}def big(n: int) -> int:\n    mut total = n\n    for i in range(n):\n        total = total + i * 2 + 1\n        total = total + big_two(i)\n    return total\n\ndef big_two(n: int) -> int:\n    if n > 1: return big_two(n - 1) + 3\n    return n * 3 + 5 + n * 7 + n * 9 + n * 11 + n * 13 + n * 15 + n * 17 + n * 19 + n * 21 + n * 23 + n * 25 + n * 27 + n * 29 + n * 31 + n * 33\n\ndef main(stdout: Stdout):\n    stdout.print(big(3))\n"
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
fn strips_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "strips",
        r#"def main(stdout: Stdout):
    for s in ["", "   ", "x", " x", "x ", "\t a b \n", "\x0b\x0cq\r", "héllo  ", "  ünï  ", "abc", "  long text with spaces inside   "]:
        stdout.print(f"[{s.strip()}] [{s.lstrip()}] [{s.rstrip()}] {len(s.strip())}")
    mut total = 0
    for i in range(1000):
        line = f"  {i} " if i % 2 == 0 else f"{i}"
        total += len(line.strip())
    stdout.print(total)
"#,
    )
}

/// `upper`, `lower`, `replace`, `find` and `split`, which run the runtime's own code.
#[test]
fn string_methods_the_runtime_runs_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "string_methods",
        r#"def main(stdout: Stdout):
    words = ["Hello", "wörld", "", "MiXeD 123"]
    for w in words:
        stdout.print(w.upper(), w.lower(), w.replace("l", "L"), w.find("l"), w.find(""))
    stdout.print("a b  c".split(), "a,b,,c".split(","), "".split(), "xyx".split("y"))
"#,
    )
}

/// `split` hands the runtime its text and separator as their headers: each inline or in a
/// block, with none, and with an empty one, which fails as the method says.
#[test]
fn a_split_reads_its_text_and_separator_however_they_are_held() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "split_headers",
        r#"def main(stdout: Stdout):
    long_separator = " and then, after all of that, "
    long_text = "first and then, after all of that, second and then, after all of that, third"
    stdout.print("a b".split(" "), long_text.split(long_separator), "a b".split(long_separator))
    stdout.print(long_text.split(), "  spaced   out  ".split(), "".split(), "x".split("x"))
    empty = len(long_text) - len(long_text)
    stdout.print(long_text[:empty].split(","), "short".split(long_text[:empty] + ","))
    stdout.print("a,b".split("a,b,c"[:empty + 1]))
    return
"#,
        "[\"a\", \"b\"] [\"first\", \"second\", \"third\"] [\"a b\"]\n\
         [\"first\", \"and\", \"then,\", \"after\", \"all\", \"of\", \"that,\", \"second\", \"and\", \"then,\", \"after\", \"all\", \"of\", \"that,\", \"third\"] [\"spaced\", \"out\"] [] [\"\", \"\"]\n\
         [\"\"] [\"short\"]\n\
         [\"\", \",b\"]\n",
    )?;
    // An empty separator, made where it runs, fails as the method says.
    let dir = folder("split_empty_separator")?;
    let (program, executable) = (dir.join("empty.fen"), dir.join("empty"));
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    text = \"a,b\"\n    stdout.print(text.split(text[:len(text) - 3]))\n",
    )?;
    let built = fennec(&["build", &program.to_string_lossy(), "-o", &executable.to_string_lossy()], &dir)?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&executable).output_within()?;
    std::fs::remove_dir_all(&dir)?;
    assert!(!ran.status.success());
    assert_eq!(
        String::from_utf8_lossy(&ran.stderr).lines().next(),
        Some("error: `split` was called with the wrong arguments")
    );
    Ok(())
}

/// What a list of parts prints as in `a_split_gives_what_rust_gives_at_every_edge`: each part's
/// length in characters and its text, between bars.
fn parts_shown<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts.map(|part| format!("{}:{part}", part.chars().count())).collect::<Vec<_>>().join("|")
}

/// A Fennec string literal of `text`.
fn fennec_literal(text: &str) -> String {
    let escaped: String = text
        .chars()
        .map(|character| match character {
            '"' => "\\\"".to_owned(),
            '\\' => "\\\\".to_owned(),
            '\n' => "\\n".to_owned(),
            '\t' => "\\t".to_owned(),
            '\r' => "\\r".to_owned(),
            other => other.to_string(),
        })
        .collect();
    format!("\"{escaped}\"")
}

/// `split` gives what Rust's `str::split` and `split_whitespace` give, on rows chosen for where it
/// could go wrong: empty parts, separators at both ends, separators of one to nine bytes, ones
/// that overlap themselves, ones that end at, start at and lie across a word of the text, parts
/// of exactly 23 bytes, which are held inline, and of 24, which take a block, of ASCII and of
/// two-byte characters, and whitespace of ASCII and beyond it. The texts and separators come out
/// of lists, so each split is the runtime's. Every list and part is freed after, with no block
/// left.
#[test]
fn a_split_gives_what_rust_gives_at_every_edge() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let (a23, b24) = ("a".repeat(23), "b".repeat(24));
    let (e23, e24) = (format!("{}a", "é".repeat(11)), "é".repeat(12));
    let separated: Vec<(String, &str)> = vec![
        ("a,,b".to_owned(), ","),
        (",,".to_owned(), ","),
        (String::new(), ","),
        (",a,b,".to_owned(), ","),
        (" x ".to_owned(), " "),
        ("x".to_owned(), "x"),
        ("aéb€éc".to_owned(), "é"),
        ("é".to_owned(), "é"),
        ("日本語本".to_owned(), "本"),
        ("→a→→b→".to_owned(), "→"),
        ("a, b,, c".to_owned(), ", "),
        (format!("{a23},{b24},{e23},{e24},"), ","),
        (format!("{a23}é{b24}é{e23}"), "é"),
        (format!(",{e24},,{a23}"), ","),
        (format!("{a23}{b24}"), "ab"),
        (b24.clone(), b24.as_str()),
        ("ab".to_owned(), "abc"),
        ("a\tb\nc".to_owned(), "\n"),
        ("x,y,z,w,v,u,t,s,r,q,p,o,n,m,l,k".to_owned(), ","),
        // Separators of two to eight bytes are found a word at a time, and of nine as a `str`.
        ("GET /a HTTP/1.1\r\nHost: fox\r\nAccept: */*\r\n\r\nbody".to_owned(), "\r\n"),
        ("\r\n\r\n".to_owned(), "\r\n"),
        ("aaa".to_owned(), "aa"),
        ("aaaa".to_owned(), "aa"),
        ("abababab".to_owned(), "abab"),
        // A separator ending at, starting at and lying across the end of the text's first word.
        ("123456--rest".to_owned(), "--"),
        ("12345678--rest".to_owned(), "--"),
        ("1234567--rest".to_owned(), "--"),
        ("one12345678two12345678three".to_owned(), "12345678"),
        ("one123456789two123456789three".to_owned(), "123456789"),
        ("one1234567".to_owned(), "12345678"),
        (format!("{a23}::{b24}::{e23}::{e24}::"), "::"),
        (format!("{e24}→é{a23}→é"), "→é"),
        ("a\r\nb\rc\nd\r".to_owned(), "\r\n"),
    ];
    let spaced: Vec<String> = vec![
        "  a\tb\nc  ".to_owned(),
        "a\u{3000}b  c\u{a0}d".to_owned(),
        format!(" {a23} {b24}\t{e23}\n{e24} "),
        String::new(),
        " \t\n ".to_owned(),
    ];
    let texts: Vec<String> = separated.iter().map(|(text, _)| fennec_literal(text)).collect();
    let separators: Vec<String> = separated.iter().map(|(_, separator)| fennec_literal(separator)).collect();
    let spaced_texts: Vec<String> = spaced.iter().map(|text| fennec_literal(text)).collect();
    let source = format!(
        r#"def shown(parts: list[str]) -> str:
    mut said = list[str]()
    for part in parts:
        said.append(f"{{len(part)}}:{{part}}")
    return "|".join(said)

def main(stdout: Stdout):
    texts = [{}]
    separators = [{}]
    for at in range(len(texts)):
        stdout.print(shown(texts[at].split(separators[at])))
    for text in [{}]:
        stdout.print(shown(text.split()))
    return
"#,
        texts.join(", "),
        separators.join(", "),
        spaced_texts.join(", ")
    );
    let mut expected = String::new();
    for (text, separator) in &separated {
        expected.push_str(&parts_shown(text.split(*separator)));
        expected.push('\n');
    }
    for text in &spaced {
        expected.push_str(&parts_shown(text.split_whitespace()));
        expected.push('\n');
    }
    let dir = folder("split_edges")?;
    let program = dir.join("split_edges.fen");
    std::fs::write(&program, &source)?;
    prints_with_nothing_left(&dir, &program, &expected, &["1"])
}

/// A value a million and three million deep, a chain of structs each in the last one's field, is
/// compared, written as text and copied for a task, on the main thread and in a launched task,
/// with every block freed after. The runtime compared and wrote a value by calling itself for
/// each block it held, so `==` at three million deep and `str` at one million ended the program
/// with a signal and no message, and in a task, on its smaller stack, far sooner. Fable's review of
/// the drop worklist found both.
#[test]
fn a_value_millions_deep_is_compared_written_and_sent() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("deep_values")?;
    let program = dir.join("deep_values.fen");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deep_values.fen"), &program)?;
    let wanted = "1000000 True False 20888916 True True 1000000\n\
                  in a task: True False 20888916 True True\n\
                  3000000 True False 64888916 True True 3000000\n\
                  in a task: True False 64888916 True True\n";
    prints_with_nothing_left(&dir, &program, wanted, &["4"])
}

/// A dict's key that holds blocks is hashed by what it holds, which the runtime reads into a tree
/// with a call for each level. Twenty thousand levels are hashed, on the main thread and in a
/// task. A million stop the program with exit code 1, saying the value is nested too deep, where
/// they ended it with a signal and no message.
#[test]
fn a_key_nested_too_deep_to_hash_stops_the_program_saying_so() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let fixture =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deep_key.fen"))?;
    let dir = folder("deep_key")?;
    for (depth, in_task) in [("20_000", false), ("20_000", true), ("1_000_000", false), ("1_000_000", true)] {
        let source = fixture
            .replace("DEPTH = 20_000", &format!("DEPTH = {depth}"))
            .replace("IN_TASK = False", if in_task { "IN_TASK = True" } else { "IN_TASK = False" });
        let (program, executable) = (dir.join("deep_key.fen"), dir.join("deep_key"));
        std::fs::write(&program, source)?;
        let built = fennec(
            &["build", "--release", &program.to_string_lossy(), "-o", &executable.to_string_lossy()],
            &dir,
        )?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&executable).env("FENNEC_THREADS", "4").output_within()?;
        let (printed, said) = (String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
        let case = format!("{depth} deep, in a task: {in_task}");
        if depth == "20_000" {
            assert!(ran.status.success(), "{case}: {said}");
            assert_eq!(printed, "7\n", "{case}");
        } else {
            assert_eq!(ran.status.code(), Some(1), "{case}: {said}");
            assert_eq!(printed, "", "{case}");
            assert!(
                said.starts_with("error: a value is nested too deep to be a dict's key or a set's item"),
                "{case}: {said}"
            );
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// `removesuffix` and `removeprefix` give what Rust's `strip_suffix` and `strip_prefix` do, or the
/// text as it is: with an empty text or affix, an affix as long as the text or longer, one found
/// only elsewhere, one twice over, characters of two and three bytes, and results either side of
/// what a header holds, from texts of 23 to 27 bytes. Each result is compared with the same text
/// made another way, is a dict's key found by it, and says its length in characters. The texts
/// come out of lists, so each is cut where it runs, and no block is left after.
#[test]
fn an_affix_removed_gives_what_rust_gives_at_every_edge() -> std::io::Result<()> {
    use std::fmt::Write as _;
    if !ready() {
        return Ok(());
    }
    let (a23, e12) = ("a".repeat(23), "é".repeat(12));
    let rows: Vec<(String, &str)> = vec![
        (String::new(), ""),
        (String::new(), "ms"),
        ("ms".to_owned(), ""),
        ("ms".to_owned(), "ms"),
        ("s".to_owned(), "ms"),
        ("42ms".to_owned(), "ms"),
        ("42msms".to_owned(), "ms"),
        ("ms42".to_owned(), "ms"),
        ("café".to_owned(), "é"),
        ("caf".to_owned(), "é"),
        ("日本語".to_owned(), "本語"),
        ("日本語".to_owned(), "日本"),
        (format!("{a23}ms"), "ms"),
        (format!("{a23}bms"), "ms"),
        (format!("{a23}bbms"), "bms"),
        (format!("{e12}é"), "é"),
        (format!("ms{a23}b"), "ms"),
    ];
    let texts: Vec<String> = rows.iter().map(|(text, _)| fennec_literal(text)).collect();
    let affixes: Vec<String> = rows.iter().map(|(_, affix)| fennec_literal(affix)).collect();
    let source = format!(
        r#"def main(stdout: Stdout):
    texts = [{}]
    affixes = [{}]
    mut seen = dict[str, int]()
    for at in range(len(texts)):
        without_suffix = texts[at].removesuffix(affixes[at])
        without_prefix = texts[at].removeprefix(affixes[at])
        seen[without_suffix] = at
        stdout.print(f"{{len(without_suffix)}}:{{without_suffix}}|{{len(without_prefix)}}:{{without_prefix}}")
    for text in [{}]:
        stdout.print(seen.get(text) else -1)
    return
"#,
        texts.join(", "),
        affixes.join(", "),
        rows.iter()
            .map(|(text, affix)| fennec_literal(text.strip_suffix(affix).unwrap_or(text)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut expected = String::new();
    let mut last = std::collections::HashMap::new();
    for (at, (text, affix)) in rows.iter().enumerate() {
        let (suffixless, prefixless) =
            (text.strip_suffix(affix).unwrap_or(text), text.strip_prefix(affix).unwrap_or(text));
        expected.push_str(&parts_shown([suffixless].into_iter()));
        expected.push('|');
        expected.push_str(&parts_shown([prefixless].into_iter()));
        expected.push('\n');
        last.insert(suffixless.to_owned(), at);
    }
    for (text, affix) in &rows {
        let found = last.get(text.strip_suffix(affix).unwrap_or(text)).copied().unwrap_or(usize::MAX);
        let _ = writeln!(expected, "{found}");
    }
    let dir = folder("affixes_removed")?;
    let program = dir.join("affixes_removed.fen");
    std::fs::write(&program, &source)?;
    prints_with_nothing_left(&dir, &program, &expected, &["1"])
}

/// `startswith` and `endswith` compare a text's bytes with its part's where both are, inline or
/// in a block: with an empty part, a part as long as the text or longer, and characters of two
/// bytes either side of where they'd meet.
#[test]
fn a_texts_start_and_end_are_compared_where_they_are() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "text_affixes",
        r#"def main(stdout: Stdout):
    texts = ["", "a", "abc", "café", "été long enough to need a block of its own", "a text that's long enough to be held in a block"]
    parts = ["", "a", "c", "abc", "abcd", "é", "été", "own", "block", "café", "a text that's long enough to be held in a block", "x"]
    for text in texts:
        mut said = list[str]()
        for part in parts:
            starts = "1" if text.startswith(part) else "0"
            ends = "1" if text.endswith(part) else "0"
            said.append(starts + ends)
        stdout.print(" ".join(said))
    return
"#,
        "11 00 00 00 00 00 00 00 00 00 00 00\n11 11 00 00 00 00 00 00 00 00 00 00\n11 10 01 11 00 00 00 00 00 00 00 00\n11 00 10 00 00 01 00 00 00 11 00 00\n11 00 00 00 00 10 10 01 00 00 00 00\n11 10 00 00 00 00 00 00 01 00 11 00\n",
    )
}

/// `find` hands the runtime its text and part as their headers, inline or in a block, and gives
/// back where the part starts in characters: an empty part, two-byte characters before it, the
/// whole text, and a part that isn't there.
#[test]
fn a_find_reads_its_text_and_part_however_they_are_held() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "find_headers",
        r#"def main(stdout: Stdout):
    texts = ["", "a: b", "café: au lait", "a header line long enough for a block: its value", "été, then a long tail past twenty-three bytes: été"]
    parts = ["", ": ", "a", "é", "été", "its value", "a header line long enough for a block: its value", "absent"]
    for text in texts:
        mut said = list[str]()
        for part in parts:
            at = text.find(part) else -1
            said.append(str(at))
        stdout.print(" ".join(said))
    return
"#,
        "0 -1 -1 -1 -1 -1 -1 -1\n0 1 0 -1 -1 -1 -1 -1\n0 4 1 3 -1 -1 -1 -1\n0 37 0 -1 -1 39 0 -1\n0 45 10 0 0 -1 -1 -1\n",
    )
}

/// Strings let go of where nothing takes them, and a `str?` from a dict of strings.
#[test]
fn strings_let_go_of_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "strings_let_go_of",
        r#"def main(stdout: Stdout):
    mut parts = ["a", "a much longer string than a header holds", "b", "c"]
    _ = parts.pop()
    _ = parts.pop(0)
    parts.remove_where((s) => len(s) > 10)
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
fn string_keys_in_place_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(len(counts), counts[""], counts["abc"], counts[held], counts[letters[:23]], counts[letters[:24]])
    stdout.print(counts.pop("the"), counts.pop(letters[:24]), len(counts))
    counts["the"] = 7
    counts[letters[:24]] = 8
    stdout.print(counts["the"], counts[letters[:24]], len(counts), list(counts.keys())[-2:])
    mut order: list[str] = []
    for key in counts.keys():
        if len(key) < 3:
            order.append(key)
    stdout.print(order, counts.get("missing") else -1)
    mut seen: set[str] = {"dog"}
    for word in ["fox", "fox", "a much longer word than a header holds", "dog"]:
        seen.add(word)
    stdout.print(len(seen), "fox" in seen, "cat" in seen)
"#,
    )
}

/// New string keys a dict nothing else holds gets, from slices and whole strings: added in
/// native code while short and the dict has room, and by the runtime when long, the dict is
/// full or shared, or the text isn't ASCII. Every way gives what was pinned.
#[test]
fn new_string_keys_go_in_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "new_string_keys",
        r#"def main(stdout: Stdout):
    dna = "acgtacgtttgacca" * 300
    for size in [1, 3, 12, 23, 24, 30]:
        mut counts: dict[str, int] = {}
        for at in range(len(dna) - size + 1):
            piece = dna[at:at + size]
            counts[piece] = (counts.get(piece) else 0) + 1
        mut best = 0
        for key, value in counts.items():
            best = max(best, value)
        stdout.print(size, len(counts), best, counts.get(dna[:size]) else -1)
    mut words: dict[str, float] = {}
    for at in range(200):
        words[f"w{at % 37}"] = float(at) * 0.5
        words[f"a much longer key than a header holds {at % 5}"] = 1.0
    stdout.print(len(words), words["w3"], sorted(words.keys())[:4])
    mut seen: set[str] = set()
    for at in range(100):
        seen.add(dna[at:at + 4])
    stdout.print(len(seen), "acgt" in seen)
    mut shared: dict[str, int] = {"x": 1}
    copy = shared
    for at in range(50):
        shared[dna[at:at + 2]] = at
    stdout.print(len(shared), len(copy), copy)
    mut flags: dict[str, bool] = {}
    for at in range(40):
        flags[f"{at % 9}"] = at % 2 == 0
    stdout.print(flags)
    uni = "éacgtü" * 20
    mut by: dict[str, int] = {}
    for at in range(len(uni) - 3):
        by[uni[at:at + 3]] = at
    stdout.print(len(by), by["acg"])
"#,
    )
}

/// String keys of every length up to past what a header holds, from slices at every place in a
/// block's text and from whole strings: a short one is compared by its header's words, its last
/// part word read as the bytes ending where it ends, and both ways find the same entries.
#[test]
fn string_keys_of_every_length_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "string_key_lengths",
        r#"def main(stdout: Stdout):
    text = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ"
    mut by_slice: dict[str, int] = {}
    mut by_word: dict[str, int] = {}
    for size in range(31):
        for at in range(len(text) - size + 1):
            piece = text[at:at + size]
            by_slice[piece] = (by_slice.get(piece) else 0) + 1
            word = "".join([text[i] for i in range(at, at + size)])
            by_word[word] = (by_word.get(word) else 0) + 1
    mut agree = 0
    for key, count in by_slice.items():
        if (by_word.get(key) else -1) == count:
            agree += 1
    stdout.print(len(by_slice), len(by_word), agree, by_slice.get("") else -1, by_slice.get(text[3:20]) else -1)
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
fn a_missing_string_key_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "missing_string_key",
        r#"def main(stdout: Stdout):
    counts: dict[str, int] = {"fox": 1}
    stdout.print(counts["a key long enough to live in a block"])
"#,
    )
}

/// A float sum over a generator, whose start the checker writes in by name, as an int's isn't.
#[test]
fn float_sums_over_generators_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
fn a_list_taken_eight_items_a_turn_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "eight_a_turn",
        r"def eights(items: list[float]) -> float:
    n = len(items)
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
    return ((a0 + a1) + (a2 + a3)) + ((a4 + a5) + (a6 + a7))

def past(items: list[float]) -> float:
    mut at = 0
    mut total = 0.0
    while at + 2 <= len(items):
        total += items[at] + items[at + 2]
        at += 2
    return total

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
fn short_copies_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
    stdout.print(grown, len(grown))
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
    return .Word(text)

def describe(token: Token) -> str:
    match token:
        case .Blank:
            return "blank"
        case .Word(text):
            return f"word {text}"
        case .Num(value):
            return f"num {value}"
        case .Many(items, label):
            return f"{label}: {len(items)} items, first {items[0]}"

def replace(ref token: Token, text: str):
    token = .Word(text)

def pick(at: int) -> Token?:
    if at == 0:
        return None
    if at == 1:
        return .Word("a string long enough that it can't be held inline")
    if at == 2:
        return .Word("short")
    return .Many([at], "label")

def someone(at: int) -> Person?:
    if at == 0:
        return None
    return Person("somebody with a name long enough to be a block of its own", at)

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
    stdout.print(len(list_of), describe(list_of[0]), describe(list_of[50]), describe(popped))
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
    return at

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    # Failures holding them.
    for at in range(3):
        value = get(at) except failure:
            stdout.print("err", failure)
            continue
        stdout.print("ok", value)
    # JSON of them.
    d = Doc(.Word(long), [Token.Blank, Token.Num(2), Token.Word("x")], long)
    text = json.dumps(d)
    stdout.print(text)
    back = json.loads[Doc](text) except ParseError(): Doc(.Blank, [], "")
    stdout.print(back == d, back)
    token = json.loads[Token](json.dumps(Token.Word(long))) except ParseError(): .Blank
    stdout.print(token)
    tokens = json.loads[list[Token]](json.dumps(d.tokens)) except ParseError(): []
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
            return "null"
        case .Flag(flag):
            return "true" if flag else "false"
        case .Number(number):
            return str(number)
        case .Text(text):
            return f"'{text}'"
        case .Items(values):
            return ",".join([printed(item) for item in values])
        case .Fields(names, values):
            mut parts = []
            for at, name in enumerate(names):
                parts.append(f"{name}={printed(values[at])}")
            return ";".join(parts)

def document(depth: int) -> Json:
    if depth == 0:
        return .Number(7)
    return .Fields(
        ["a much longer name than fits in a header", "b"],
        [Json.Items([document(depth - 1), Json.Null, Json.Flag(True)]), Json.Text("é")],
    )

def count(tree: Tree) -> int:
    mut total = 1
    for child in tree.children:
        total += count(child)
    return total

def deep(depth: int) -> Tree:
    mut tree = Tree("leaf", [])
    for at in range(depth):
        tree = Tree("node", [tree])
    return tree

def depth_of(tree: Tree) -> int:
    mut held = tree
    mut seen = 0
    while len(held.children) > 0:
        held = held.children[0]
        seen += 1
    return seen

def total(expr: Expr) -> int:
    match expr:
        case .Number(value):
            return value
        case .Add(parts):
            return sum(total(part) for part in parts)
        case .Named(name, value):
            return len(name) + sum(total(part) for part in value.parts)

def changed(value: Json) -> Json:
    match value:
        case .Items(values):
            mut more = values
            more.append(Json.Null)
            return .Items(more)
        case _:
            return value

def main(stdout: Stdout):
    made = document(3)
    stdout.print(printed(made))
    held = made
    other = changed(made)
    stdout.print(printed(held) == printed(made), len(printed(other)))
    stdout.print(len(printed(changed(.Items([made, made])))))
    stdout.print(made == held, made == .Null, Json.Number(1) == .Number(1))
    stdout.print(Json.Text("a") == .Text("a"), Json.Items([Json.Null]) == .Items([Json.Null]))

    mut items = [Json.Null, Json.Text("in a list"), Json.Items([Json.Number(1)])]
    copied = items
    items.append(document(1))
    items[0] = .Text("replaced")
    last = items.pop() else return
    stdout.print(len(items), len(copied), printed(items[0]), printed(copied[0]), printed(last))
    stdout.print(items)

    mut tree = Tree("root", [])
    tree.add("left")
    tree.add("right")
    tree.children[0].add("under")
    kept = tree
    tree.children[1].add("more")
    tree.children[1].children[0].add("deeper")
    stdout.print(count(tree), count(kept), tree.children[1].children[0].children[0].label)
    stdout.print(tree == kept, len(kept.children[1].children))
    stdout.print(depth_of(deep(20000)))

    root = Folder("root", [File("f", Holder(1, [Folder("sub", [File("g", Holder(2, []))])]))])
    stdout.print(root.files[0].inside.folders[0].files[0].inside.size, root.files[0].name)

    expr = Expr.Add([Expr.Number(1), Expr.Named("two", Wrapped([Expr.Number(2)])), Expr.Add([])])
    stdout.print(total(expr))
"#;

#[test]
fn types_holding_themselves_through_a_list_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned("through_lists", THROUGH_LISTS)
}

/// What's in place goes with the block holding it, so freeing a deep one needs no stack of its
/// own and leaves nothing behind.
#[test]
fn a_type_holding_itself_through_a_list_allocates_no_block_of_its_own() -> std::io::Result<()> {
    if !ready() {
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
        "{SHARED_LEAVES_TREE}def main(stdout: Stdout):\n    mut leaves = []\n    for _ in range(1000):\n        leaves.append(Tree.Leaf)\n    stdout.print(len(leaves), count(build(0)))\n"
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

/// Native blocks come from the allocator the runtime they're linked with uses: mimalloc in a plain
/// build, and the system's under either sanitizer, whose runtime leaves mimalloc out. Under the
/// thread sanitizer they once still asked for mimalloc, and such a program didn't link. A build's
/// LLVM text is kept and read, so this holds wherever the sanitized link itself can't run.
#[test]
fn native_blocks_come_from_the_allocator_each_sanitizer_links() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("native_allocator")?;
    let program = dir.join("tree.fen");
    std::fs::write(
        &program,
        format!("{SHARED_LEAVES_TREE}def main(stdout: Stdout):\n    stdout.print(count(build(5)))\n"),
    )?;
    for (sanitize, wanted, not) in [
        (None, "@mi_malloc(", "@malloc("),
        (Some("address"), "@malloc(", "@mi_malloc("),
        (Some("thread"), "@malloc(", "@mi_malloc("),
    ] {
        let home = dir.join(format!("home-{}", sanitize.unwrap_or("plain")));
        let mut command = crate::executables::fennec_command();
        command
            .args(["build", "--llvm", &program.to_string_lossy(), "-o", &dir.join("tree").to_string_lossy()])
            .env("FENNEC_HOME", &home)
            .env("FENNEC_LLVM_KEEP", "1")
            .env("FENNEC_NO_CACHE", "1")
            .env_remove("FENNEC_SANITIZE");
        if let Some(sanitize) = sanitize {
            command.env("FENNEC_SANITIZE", sanitize);
        }
        // Linking may fail where the sanitizer's runtime isn't there; the text is what's read.
        command.output_before(FENNEC_DEADLINE)?;
        let mut declared = String::new();
        let mut folders = vec![home.join("work")];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|ext| ext == "ll") {
                    let text = std::fs::read_to_string(&path)?;
                    declared.extend(
                        text.lines()
                            .filter(|line| line.starts_with("declare"))
                            .map(|line| format!("{line}\n")),
                    );
                }
            }
        }
        let shown = sanitize.unwrap_or("no sanitizer");
        assert!(declared.contains(wanted), "{shown}: native blocks don't come from {wanted}\n{declared}");
        assert!(!declared.contains(not), "{shown}: native blocks come from {not}\n{declared}");
    }
    std::fs::remove_dir_all(&dir)?;
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
    return .Pair(left=build(depth - 1, tag=tag * 2), right=build(depth - 1, tag=tag * 2 + 1), label=f"p{tag}")

def size(tree: Named) -> int:
    match tree:
        case .Leaf(name):
            return len(name)
        case .Pair(left, right, label):
            return size(left) + size(right) + len(label)
        case .Empty:
            return 1

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
    return .Pair(left=build(depth - 1, tag=tag * 2), right=build(depth - 1, tag=tag * 2 + 1), title=f"t{tag}")

def size(doc: Doc) -> int:
    match doc:
        case .Words(names, counts):
            mut more = names
            more.append("x")
            return sum(len(name) for name in more) + sum(counts) + len(names)
        case .Pair(left, right, title):
            return size(left) + size(right) + len(title)
        case .Empty:
            return 1

def main(stdout: Stdout):
    rounds = 3
    total = sum(size(build(10, tag=n)) for n in range(rounds))
    kept = build(3, tag=7)
    stdout.print(total, size(kept))
"#;

/// The shared block behaves as a block of its own would: compared, matched, printed, held in
/// lists and dicts, and made and let go of on other threads.
#[test]
fn cases_with_no_fields_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned("shared_leaves", &format!("{SHARED_LEAVES_TREE}{SHARED_LEAVES_MAIN}"))
}

const SHARED_LEAVES_TREE: &str = "enum Tree:
    Leaf
    Node(left: Tree, right: Tree)

def build(depth: int) -> Tree:
    if depth == 0: return .Leaf
    return .Node(left=build(depth - 1), right=build(depth - 1))

def count(tree: Tree) -> int:
    match tree:
        case .Leaf:
            return 1
        case .Node(left, right):
            return 1 + count(left) + count(right)

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
    stdout.print(len(leaves), Tree.Leaf == build(0))
"#;

#[test]
fn enums_holding_blocks_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned("enums_holding_blocks", ENUMS_HOLDING_BLOCKS)?;
    same_as_pinned("enums_in_json_and_results", ENUMS_IN_JSON_AND_FAILURES)
}

/// Unions held in place, each with its member's case first: in lists and fields, optional,
/// narrowed by `is`, read through methods every member has, compared, and copied when changed,
/// with members that hold strings and lists.
#[test]
fn unions_holding_blocks_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("unions_holding_blocks", UNIONS_HOLDING_BLOCKS)
}

/// A member given where a builtin takes its union: a dict's key to `get`, `pop` and `in`, a
/// set's item to `add` and `in`, and a list's item to `append`.
#[test]
fn members_given_to_builtins_taking_a_union_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("members_given_to_builtins", MEMBERS_GIVEN_TO_BUILTINS)
}

/// A template string builds natively and runs as pinned: its text, its values of a
/// union, each value's expression and format spec, a `LiteralStr` picked by a conditional and
/// joined with `+`, and a template with no values.
#[test]
fn template_strings_build_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("templates", TEMPLATES)
}

const TEMPLATES: &str = r#"type SqlValue = int | str

def placeholders(query: Template[SqlValue]) -> str:
    mut sent = ""
    for at, text in enumerate(query.strings):
        sent += text
        if at < len(query.values):
            value = query.values[at]
            if value is int:
                sent += f"<int {value}>"
            else:
                sent += f"<str {value}>"
    return sent

def order(up: bool) -> LiteralStr:
    return "asc" if up else "desc"

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

/// A generic enum's cases are made natively: worked out from their arguments, with their types
/// written as `Choice[int].Nothing`, from a function's return type, and in a list, and a `match`
/// reads each case's field as the type it is there.
#[test]
fn a_generic_enums_cases_are_made_and_matched_natively() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    assert_eq!(
        printed_by(
            "generic_enum_cases",
            r#"enum Choice[T]:
    Picked(value: T)
    Nothing

def make(name: str) -> Choice[str]:
    if name == "": return Choice.Nothing
    return .Picked(value=name)

def shown(choice: Choice[int]) -> str:
    match choice:
        case .Picked(value):
            return f"picked {value + 1}"
        case .Nothing:
            return "nothing"

def main(stdout: Stdout):
    stdout.print(shown(Choice.Picked(value=7)), shown(Choice[int].Nothing))
    stdout.print(make("fox"), make(""))
    numbers = [Choice.Picked(value=1), Choice.Nothing, Choice.Picked(value=3)]
    stdout.print(numbers)
    stdout.print(Choice[float].Nothing, Choice[str].Picked(value="x"))
"#
        )?,
        "picked 8 nothing\nChoice.Picked(value=\"fox\") Choice.Nothing\n\
         [Choice.Picked(value=1), Choice.Nothing, Choice.Picked(value=3)]\nChoice.Nothing Choice.Picked(value=\"x\")\n"
    );
    Ok(())
}

/// A generic type built where one of it is expected takes its types from there: a `Box[int |
/// str]` built from an int or a str, in a parameter, a name's type and a list of them.
#[test]
fn generic_types_built_where_one_is_expected_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("generics_expected", GENERICS_EXPECTED)
}

/// A generic function whose type parameter is only in an `Iterator[T]` it's given works out
/// what `T` is from the iterator. It once couldn't, so the caller didn't build natively.
#[test]
fn a_generic_over_an_iterator_takes_its_type_from_it() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "generic_iterator",
        r#"def count[T](items: Iterator[T]) -> int:
    mut n = 0
    for _ in items:
        n += 1
    return n

def last_of[T](items: Iterator[T], otherwise: T) -> T:
    mut last = otherwise
    for item in items:
        last = item
    return last

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
    return f"str {value}"

def count(held: Held[int | str]) -> str:
    return f"{len(held.items)} {show(held.items[1])} {len(held.plain)}"

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
    stdout.print(len(counts), counts)
    mut shapes: list[Shape] = []
    shapes.append(Circle(radius=1.0))
    shapes.append(Square(side=2.0))
    stdout.print(len(shapes))
    mut seen: set[int | str] = {1}
    seen.add("a")
    seen.add(2)
    seen.add("a")
    stdout.print(len(seen), "a" in seen, 3 in seen)
"#;

const UNIONS_HOLDING_BLOCKS: &str = r#"struct Circle:
    radius: float

    def area(self) -> float:
        return 3.0 * self.radius * self.radius

    def name(self) -> str:
        return "circle"

struct Square:
    side: float

    def area(self) -> float:
        return self.side * self.side

    def name(self) -> str:
        return "square"

struct Label:
    text: str
    tags: list[str]

    def area(self) -> float:
        return float(len(self.text))

    def name(self) -> str:
        return f"label {self.text}"

type Shape = Circle | Square | Label

struct Holder:
    shape: Shape
    note: int | str

def describe(value: int | str) -> str:
    if value is int: return f"int {value}"
    return f"str {value.upper()}"

def measure(value: float | int) -> str:
    if value is float: return f"float {value}"
    return f"int {value}"

def biggest(shapes: list[Shape]) -> Shape:
    mut best = shapes[0]
    for shape in shapes:
        if shape.area() > best.area():
            best = shape
    return best

def maybe(at: int, shapes: list[Shape]) -> Shape?:
    if at < len(shapes):
        return shapes[at]
    return None

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
            letters += len(shape.tags) + len(shape.text)
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
        "struct Circle:\n    radius: float\n\nstruct Square:\n    side: float\n\ndef area(shape: Circle | Square) -> float:\n    if shape is Circle: return 3.0 * shape.radius * shape.radius\n    return shape.side * shape.side\n\ndef main(stdout: Stdout):\n    mut shapes: list[Circle | Square] = []\n    for i in range(1000):\n        if i % 2 == 0:\n            shapes.append(Circle(radius=float(i % 7)))\n        else:\n            shapes.append(Square(side=float(i % 5)))\n    mut total = 0.0\n    for shape in shapes:\n        total += area(shape)\n    stdout.print(total)\n",
    )?;
    assert!(count(&counts, "allocations") < Some(30), "{counts}");
    assert_eq!(count(&counts, "copies"), Some(0), "{counts}");
    Ok(())
}

/// A `parallel` call whose body may wait goes to the runtime's `_waiting` entry points, and
/// runs as one whose body only computes does: here a loop, a comprehension and a sum whose
/// ranges sleep, beside ones that don't.
/// A `parallel` comprehension whose item can fail, like `[save(fs.read(path)?)? for path in
/// parallel(paths)]`, builds natively, and fails as pinned: what it fails with has
/// no way out of the runtime's ranges yet, so its items are made in order.
#[test]
fn a_parallel_comprehension_whose_item_can_fail_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "parallel_tries",
        r#"def doubled(text: str) -> int fails InvalidValue:
    return int(text)? * 2

def main(stdout: Stdout) fails InvalidValue:
    texts = ["1", "2", "3"]
    stdout.print([doubled(t)? for t in parallel(texts)], sum(int(t)? for t in parallel(texts)))
    stdout.print([int(t)? for t in parallel(["4", "x"])])
"#,
    )
}

#[test]
fn parallel_bodies_that_wait_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "parallel_waiting",
        r"from std.time import Clock

def rest(clock: Clock, x: int) -> int:
    clock.sleep(1ms)
    return x * 10

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

/// Native blocks made in a launched task and in `parallel`'s workers are freed there: chains
/// through the first field, a block holding the one below it twice, and one subtree under two
/// roots, at 1, 4 and 18 threads, each printing the same with nothing left held, native blocks
/// counted, and holding little memory. A native block's shape word carries no thread's number, so
/// on any thread but the first each let-go was handed to the runtime, which knows no such block,
/// and none was ever freed: 80 chains of 100,000 held over a gigabyte.
#[test]
fn native_blocks_made_in_tasks_and_workers_are_freed_there() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for (name, source, wanted, how) in [
        ("native_in_tasks", NATIVE_IN_TASKS, "done 1999980 999\n", &["--llvm", "--release"][..]),
        ("native_shared", NATIVE_SHARED, "done 3600000 3000\n", &["--llvm"][..]),
        ("native_shared", NATIVE_SHARED, "done 3600000 3000\n", &["--llvm", "--release"][..]),
    ] {
        let dir = folder(name)?;
        let (program, executable) = (dir.join(format!("{name}.fen")), dir.join(name));
        std::fs::write(&program, source)?;
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.to_str().unwrap_or_default(), "-o", executable.to_str().unwrap_or_default()])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        for threads in ["1", "4", "18"] {
            let set = [("FENNEC_COUNTS", "1"), ("FENNEC_THREADS", threads)];
            let (ran, peak) = peak_memory_with(&executable, &[], &set, &dir)?;
            let said = String::from_utf8_lossy(&ran.stderr).into_owned();
            assert_eq!(String::from_utf8_lossy(&ran.stdout), wanted, "{name} {how:?} at {threads}: {said}");
            assert!(
                said.lines().any(|line| line.ends_with(" live 0")),
                "{name} {how:?} at {threads}: {said}"
            );
            // Freed as they go, the chains hold 6 to 74 MB here, by how many threads hold one at
            // once; never freed, 600 MB to 1.5 GB. A sanitizer's own share is no part of that.
            if let Some(peak) = measured(peak)
                && std::env::var_os("FENNEC_SANITIZE").is_none()
            {
                assert!(
                    peak < 300 * 1024 * 1024,
                    "{name} {how:?} at {threads} held {} MB",
                    peak / (1024 * 1024)
                );
            }
        }
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

const NATIVE_IN_TASKS: &str = r#"from std.concurrent import Crashed

enum Plain:
    End
    Two(left: Plain, right: Plain, n: int)

def plain_chain(depth: int) -> Plain:
    mut made = Plain.End
    for at in range(depth):
        made = .Two(left=made, right=.End, n=at)
    return made

def dropped(depth: int) -> int:
    plain = plain_chain(depth)
    match plain:
        case .Two(_, _, n):
            return n
        case _:
            return 0

def in_a_task(depth: int) fails Crashed:
    with launch(() => dropped(depth)) as task:
        pass

def main(stdout: Stdout):
    for round in range(60):
        in_a_task(100_000) except Crashed():
            stdout.print("the task crashed")
    mut sizes = [100_000 for _ in range(20)]
    for ref size in parallel(sizes):
        size = dropped(size)
    stdout.print("done", sum(sizes), dropped(1_000))
    return
"#;

const NATIVE_SHARED: &str = r#"from std.concurrent import Crashed

enum Plain:
    End
    Two(left: Plain, right: Plain, n: int)

def plain_chain(depth: int) -> Plain:
    mut made = Plain.End
    for at in range(depth):
        made = .Two(left=made, right=.End, n=at)
    return made

## A chain whose every block holds the one below it twice.
def doubled(depth: int) -> Plain:
    mut made = Plain.End
    for at in range(depth):
        made = .Two(left=made, right=made, n=at)
    return made

def top(made: Plain) -> int:
    match made:
        case .Two(_, _, n):
            return n
        case _:
            return -1

## One subtree under two roots, each let go of in turn, the subtree last held by the second.
def two_roots(depth: int) -> int:
    under = plain_chain(depth)
    first = Plain.Two(left=under, right=.End, n=1)
    second = Plain.Two(left=.End, right=under, n=2)
    return top(first) + top(second) + top(under)

def worked(depth: int) -> int:
    return top(plain_chain(depth)) + top(doubled(depth)) + two_roots(depth)

def in_a_task(depth: int) fails Crashed:
    with launch(() => worked(depth)) as task:
        pass

def main(stdout: Stdout):
    for round in range(40):
        in_a_task(50_000) except Crashed():
            stdout.print("the task crashed")
    mut sizes = [50_000 for _ in range(24)]
    for ref size in parallel(sizes):
        size = worked(size)
    stdout.print("done", sum(sizes), worked(1_000))
    return
"#;

/// Chains 200,000 deep let go of at 1, 4 and 18 threads, in dev and release, nothing left held:
/// enums of native blocks through their first field, one holding strings and a list where no
/// other thread runs and one holding numbers in a task and in `parallel`'s workers, and enums
/// through a list and a dict. A native drop recursing through `left` ran 200,000 calls deep,
/// past the thread sanitizer's stack, which then said a list was freed twice and hung.
#[test]
fn deep_chains_are_let_go_of_anywhere_with_no_stack() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    for (name, source, wanted) in [
        ("deep_threadless", DEEP_THREADLESS, "200032 35\n"),
        ("deep_threaded", DEEP_THREADED, "[599997, 2997, 599997] 599997\n"),
    ] {
        let dir = folder(name)?;
        let (program, executable) = (dir.join(format!("{name}.fen")), dir.join(name));
        std::fs::write(&program, source)?;
        for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
            let args: Vec<&str> = ["build"]
                .into_iter()
                .chain(how.iter().copied())
                .chain([program.to_str().unwrap_or_default(), "-o", executable.to_str().unwrap_or_default()])
                .collect();
            let built = fennec(&args, &dir)?;
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            for threads in ["1", "4", "18"] {
                let ran = Command::new(&executable)
                    .env("FENNEC_COUNTS", "1")
                    .env("FENNEC_THREADS", threads)
                    .output_within()?;
                let said = String::from_utf8_lossy(&ran.stderr).into_owned();
                assert_eq!(
                    String::from_utf8_lossy(&ran.stdout),
                    wanted,
                    "{name} {how:?} at {threads}: {said}"
                );
                assert!(
                    said.lines().any(|line| line.ends_with(" live 0")),
                    "{name} {how:?} at {threads}: {said}"
                );
            }
        }
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

const DEEP_THREADLESS: &str = r#"enum Plain:
    End
    Two(left: Plain, right: Plain, n: int)

enum Named:
    Leaf(name: str)
    Pair(left: Named, right: Named, label: str, tags: list[str])

def plain_chain(depth: int) -> Plain:
    mut made = Plain.End
    for at in range(depth):
        made = .Two(left=made, right=.End, n=at)
    return made

def named_chain(depth: int) -> Named:
    mut named = Named.Leaf(name="a leaf long enough for a block of its own")
    for at in range(depth):
        named = .Pair(left=named, right=.Leaf(name=f"<{at}>"), label="a label long enough for a block", tags=["t", f"tag {at}"])
    return named

def dropped(depth: int) -> int:
    plain = plain_chain(depth)
    named = named_chain(depth)
    mut total = 0
    match plain:
        case .Two(_, _, n):
            total += n
        case _:
            pass
    match named:
        case .Pair(_, _, label, tags):
            total += len(label) + len(tags)
        case _:
            pass
    return total

def main(stdout: Stdout):
    stdout.print(dropped(200_000), dropped(3))
    return
"#;

const DEEP_THREADED: &str = r#"from std.concurrent import Crashed

enum Plain:
    End
    Two(left: Plain, right: Plain, n: int)

enum Tree:
    Leaf(n: int)
    Branch(kids: list[Tree], n: int)

enum Map:
    Leaf(n: int)
    Branch(kids: dict[int, Map], n: int)

def plain_chain(depth: int) -> Plain:
    mut made = Plain.End
    for at in range(depth):
        made = .Two(left=made, right=.End, n=at)
    return made

def tree_chain(depth: int) -> Tree:
    mut made = Tree.Leaf(n=0)
    for at in range(depth):
        made = .Branch(kids=[made], n=at)
    return made

def map_chain(depth: int) -> Map:
    mut made = Map.Leaf(n=0)
    for at in range(depth):
        made = .Branch(kids={at: made}, n=at)
    return made

def dropped(depth: int) -> int:
    plain = plain_chain(depth)
    tree = tree_chain(depth)
    map = map_chain(depth)
    mut total = 0
    match plain:
        case .Two(_, _, n):
            total += n
        case _:
            pass
    match tree:
        case .Branch(_, n):
            total += n
        case _:
            pass
    match map:
        case .Branch(_, n):
            total += n
        case _:
            pass
    return total

def in_a_task(depth: int) fails Crashed:
    with launch(() => dropped(depth)) as task:
        pass

def main(stdout: Stdout):
    in_a_task(200_000) except Crashed():
        stdout.print("the task crashed")
    mut sizes = [200_000, 1_000, 200_000]
    for ref size in parallel(sizes):
        size = dropped(size)
    stdout.print(sizes, dropped(200_000))
    return
"#;

/// `parallel` puts items back by their case, letting go of what each item held.
#[test]
fn parallel_loops_over_enums_holding_blocks_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "parallel_enums",
        r#"enum Token:
    Blank
    Word(text: str)
    Many(items: list[int], label: str)

def describe(token: Token) -> str:
    match token:
        case .Blank:
            return "blank"
        case .Word(text):
            return text
        case .Many(items, label):
            return f"{label} {len(items)}"

def main(stdout: Stdout):
    long = "a string long enough that it can't be held inline"
    mut tokens = [Token.Word(long), Token.Blank, Token.Many([1, 2], long), Token.Word("short")]
    for ref item in parallel(tokens):
        item = .Word("changed, and long enough that it can't be held inline")
    stdout.print(tokens)
    stdout.print([describe(item) for item in parallel(tokens)])
    mut more = [Token.Word(long + "!") for at in range(200)]
    for ref item in parallel(more):
        if len(describe(item)) > 10:
            item = .Blank
    stdout.print(more[0], more[199], sum(len(describe(item)) for item in parallel(more)))
"#,
    )
}

/// A value passed `mut`, with a copy or another name for it passed without: that one keeps what it held.
#[test]
fn a_value_passed_both_ways_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "passed_both_ways",
        r#"struct Bag:
    label: str
    count: int

def both(items: list[int], ref others: list[int]) -> int:
    others.append(9)
    others[0] = 7
    return len(items) * 10 + items[0]

def bags(bag: Bag, ref other: Bag) -> str:
    other.count = 5
    other.label = "a label long enough to be held in a block"
    return f"{bag.label} {bag.count}"

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
    same_message_as_pinned(
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
        "def main(stdout: Stdout):\n    stdout.print(list[int](), len(list[int]()))\n",
    )?;
    assert_eq!(printed, "[] 0\n");
    let printed = printed_by(
        "typed_empty_in_a_with",
        "def opened() -> with int:\n    lend 1\n\ndef main(stdout: Stdout):\n    with opened() as n:\n        stdout.print(list[int](), n)\n",
    )?;
    assert_eq!(printed, "[] 1\n");
    let printed = printed_by(
        "typed_empties",
        "EMPTY = list[int]()\n\ndef main(stdout: Stdout):\n    mut d = dict[str, int]()\n    d[\"a\"] = 1\n    s = set[str]()\n    stdout.print(EMPTY, d, s, len([1]) > len(list[int]()))\n",
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
    same_as_pinned(
        "pop_or_none",
        r#"struct Unit:
    name: str

struct Squad:
    units: list[Unit]
    rows: list[list[int]]

def picked(at: int) -> int:
    return at % 2

def drained(ref items: list[str]) -> int:
    mut count = 0
    while True:
        item = items.pop() else break
        count += len(item)
    return count

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
    same_as_pinned(
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
        total += len(person.name)
        set_person(ref person, keep)
        total += len(person.name) + len(keep.name)
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
        total += len(gone.name) + len(first.name) + len(people) + len(tokens)
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
    stdout.print(len(by_person))
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
    let source = "def slow() -> int:\n    mut total = 0\n    for i in range(2000000):\n        total += i\n    return total\n\n\
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
            "def site() -> EmbeddedFolder:\n    return embed_folder(\"site\")\n\n\
             def main(stdout: Stdout):\n    mut total = 0\n    for i in range({calls}):\n        \
             files = site()\n        total += len(files) + len(files.get(\"index.html\") else bytes())\n    \
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
    let source = "WAIT = 2s\n\ndef waited(n: int) -> int:\n    return int(WAIT.as_millis()) + n\n\n\
                  def main(stdout: Stdout):\n    mut totals = [0, 1, 2, 3]\n    for ref t in parallel(totals):\n        \
                  t = waited(t)\n    stdout.print(totals)\n";
    assert_eq!(printed_by("struct_constant_ahead", source)?, "[2000, 2001, 2002, 2003]\n");
    Ok(())
}

/// A constant struct held in place, holding a list and a text, read by `parallel`'s workers: the
/// blocks it holds live for the whole run, as a constant held in a block does, so no worker's
/// count of them races another's and none is left held at the end, at 1, 4 and 18 threads, in dev
/// and release. They were never marked, and the list was left held.
#[test]
fn a_constant_struct_in_place_keeps_its_blocks_for_the_whole_run() -> std::io::Result<()> {
    if !ready() || !crate::executables::threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("constant_blocks_kept")?;
    let (program, executable) = (dir.join("kept.fen"), dir.join("kept"));
    std::fs::write(&program, CONSTANT_BLOCKS_KEPT)?;
    for how in [&["--llvm"][..], &["--llvm", "--release"][..]] {
        let args: Vec<&str> = ["build"]
            .into_iter()
            .chain(how.iter().copied())
            .chain([program.to_str().unwrap_or_default(), "-o", executable.to_str().unwrap_or_default()])
            .collect();
        let built = fennec(&args, &dir)?;
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        for threads in ["1", "4", "18"] {
            let ran = Command::new(&executable)
                .env("FENNEC_COUNTS", "1")
                .env("FENNEC_THREADS", threads)
                .output_within()?;
            let said = String::from_utf8_lossy(&ran.stderr).into_owned();
            assert_eq!(
                String::from_utf8_lossy(&ran.stdout),
                "97399 Settings(id=0, title=\"a title long enough for a block of its own\", limits=[1, 2, 3])\n",
                "{how:?} at {threads}: {said}"
            );
            assert!(said.lines().any(|line| line.ends_with(" live 0")), "{how:?} at {threads}: {said}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const CONSTANT_BLOCKS_KEPT: &str = r#"struct Settings:
    id: int
    title: str
    limits: list[int]

SETTINGS = Settings(id=0, title="a title long enough for a block of its own", limits=[1, 2, 3])

def score(n: int) -> int:
    settings = SETTINGS
    return settings.limits[n % 3] + len(settings.title) + n

def main(stdout: Stdout):
    mut values = [n for n in range(400)]
    for ref value in parallel(values):
        value = score(value)
    stdout.print(sum(values), SETTINGS)
    return
"#;

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
LENGTHS = {word: len(word) for word in WORDS}
ORIGIN = Point(x=0, tags=["start"])
NAMED = Shape.Named(words=["n"])
TEXT = f"{WORDS[0]} and a long tail of text"

def changed() -> list[str]:
    mut mine = WORDS
    mine.append("c")
    mine[0] = "z"
    return mine

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
            stdout.print(len(words))
        case .Dot:
            pass
    stdout.print(NAMED, TEXT, len(TEXT))
"#;
    same_as_pinned("constant_reads", source)
}

/// Conversions and small builtins: floats to ints, `PI`, `range` and `reversed` as lists, and a
/// dict's or set's keys as a list.
#[test]
fn conversions_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "conversions",
        r#"from std.math import PI, ceil, floor

def main(stdout: Stdout):
    for x in [2.5, -2.5, 3.5, 0.49, -0.51, 7.0]:
        stdout.print(int(x), round(x), floor(x), ceil(x), abs(x))
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

/// `len(x)` and `abs(x)` call the value's own method: a list's, a string's, a dict's, a
/// comprehension's, a number's and a type's own `def len` and `def abs`.
#[test]
fn len_and_abs_call_the_values_own_method() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "len_and_abs",
        r#"struct Bag:
    items: list[int]

    def len(self) -> int:
        return len(self.items)

struct Turn:
    by: int

    def abs(self) -> Turn:
        return Turn(by=abs(self.by))

def main(stdout: Stdout):
    numbers = [1, 2, 3]
    bag = Bag(items=numbers)
    stdout.print(len(numbers), len("héllo"), len({1: 2}), len(bag), len([n for n in numbers if n > 1]))
    stdout.print(abs(-3), abs(-2.5), abs(Turn(by=-4)).by, abs(len(numbers) - 10))
"#,
        "3 5 1 3 2\n3 2.5 4 7\n",
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
    return n

def coded(n: int) -> int fails Coded:
    if n > 1: fail Coded(code=n)
    return n

def held(n: int) -> int fails Held:
    if n > 1: fail Held(items=[n, n], table={"k": n})
    return n

def check(n: int) fails Short:
    if n > 3: fail Short(why="the end")

"#;

/// A failing `main` says what was pinned, at the same place, after what it printed, and
/// the program ends with the same code.
#[test]
fn a_failing_main_ends_as_pinned() -> std::io::Result<()> {
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
        let pinned_outcome = pinned_outcome_of(&program, &format!("failing_main_{name}"))?;
        assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome, "{name}");
        assert_eq!(ran.status.code(), Some(1), "{name}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn errors_of_several_types_go_up_and_show_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "errors_of_several_types",
        include_str!("fixtures/errors_of_several_types.fen"),
    )
}

/// An error's notes natively: none to start with, each `add_note` changing only that error, not
/// a copy taken before, `==` and printing passing them over, a union of errors noting the error
/// it holds, and a failure reaching `main` or `expect` showing them under it. All as pinned, for
/// an error with cases as for one without.
#[test]
fn notes_go_with_errors_as_pinned() -> std::io::Result<()> {
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
        let pinned_outcome = pinned_outcome_of(&program, &format!("error_notes_{name}"))?;
        assert_eq!(apart_from(&dir, outcome(&ran)), pinned_outcome, "{name}");
        let said = String::from_utf8_lossy(&ran.stderr);
        assert!(said.contains(note), "{name}: {said}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A function that says `fails A | B`, with errors that have cases among them.
#[test]
fn failing_several_ways_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds("fails_several_ways", include_str!("fixtures/fails_several_ways.fen"))
}

/// A failed `assert` says all that was pinned, each side as it's shown.
#[test]
fn a_failed_assert_shows_each_side() -> std::io::Result<()> {
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
    return None

def named(present: bool) -> str?:
    if present:
        return \"hi\"
    return None
";
    let conditions = [
        "1 + 1 == 3",
        "2 > 3",
        "2.5 <= 1.5",
        "True != True",
        "\"a\\nb\" == \"c\\\"d\"",
        "len([1]) >= 2",
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

/// `_ = check(n) except why:` on a call that gives nothing but can fail keeps nothing and runs
/// the call, as the statement `check(n) except why:` does.
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
        _ = check(n) except why:
            stdout.print(why.why)
            continue
        stdout.print("fine", n)
    check(5) except why:
        stdout.print("again", why.why)
"#;

/// A failed `assert value fails Pattern` says what really happened, natively: what the call failed
/// with instead, or what it gave, and stops the program as a failed `assert` does.
#[test]
fn a_failed_assert_that_something_fails_says_what_happened() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let setup = "error Oops:\n    line: int\n\nstruct Config:\n    size: int\n\ndef load(text: str) -> Config fails Oops:\n    if text == \"bad\":\n        fail Oops(line=4)\n    return Config(size=len(text))\n";
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

/// A shift by a count that isn't one fails as pinned.
#[test]
fn a_bad_shift_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "bad_shift",
        "def main(stdout: Stdout):
    for count in [1, 64]:
        stdout.print(1 << count)
",
    )
}

/// A float outside an int's range doesn't become one.
#[test]
fn a_float_too_big_for_an_int_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
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
    let (ours, wanted) = both("a_loop_tests_what_its_body_gave_a_name", source)?;
    let held = pinned_outcome("a_loop_tests_what_its_body_gave_a_name", source)?;
    assert_eq!(ours, held);
    assert_eq!(wanted, held);
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
    let (ours, wanted) = both("a_loop_tests_what_a_path_through_its_body_gave_a_name", source)?;
    let held = pinned_outcome("a_loop_tests_what_a_path_through_its_body_gave_a_name", source)?;
    assert_eq!(ours, held);
    assert_eq!(wanted, held);
    Ok(())
}

/// The other way round, a name that starts with a value and is given None. Held only to its
/// outcome in `tests/fixtures/outcomes`, as the one in `tests/fixtures/pinned` stops when it's
/// given None: #572.
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
    assert_eq!(ours, pinned_outcome("a_loop_tests_a_name_its_body_gave_none", source)?);
    Ok(())
}

/// Items given where a list of something wider goes, each made to fit.
#[test]
fn numbers_and_strings_go_in_lists_of_optionals() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "optional_items",
        r#"struct Holder:
    values: list[int?]

def counted(values: list[int?]) -> int:
    mut found = 0
    for value in values:
        if value is not None:
            found += value
    return found

def made() -> list[str?]:
    return ["given back", None]

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
fn none_in_a_tuple_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
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
        return len(text)

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="lite") except:
            pass

struct Postgres:
    host: str

    def run(self, text: str) -> int fails Nope:
        return len(text) * 2

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="pg") except:
            pass

struct Mem:
    size: int

    def run(self, text: str) -> int fails Nope:
        return len(text) * 3

    def transaction(self) -> with Transaction fails Nope:
        lend Transaction(log="mem") except:
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
    return ""

def main(stdout: Stdout) fails Nope:
    stdout.print(saved(Lite(name="a"))?, saved(Postgres(host="b"))?, saved(Mem(size=4))?)
"#,
        "lite 3 a pg 6 b mem 9 4\n",
    )
}

/// A value of a union like `int | str`, read from a field or a list's item, prints and formats as
/// itself, a string member unquoted: by `print`, `str` and an f-string.
/// Inside a list it's shown quoted, as any string in a list is.
#[test]
fn a_unions_value_from_a_field_or_an_item_prints_as_itself() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
        "union_text",
        r#"struct Box[T]:
    value: T

def main(stdout: Stdout):
    held: int | str = "ash"
    boxed: Box[int | str] = Box(value="ash")
    items: list[int | str] = ["ash", 1]
    stdout.print(held, boxed.value, items[0], items[1], items)
    stdout.print(f"{boxed.value} {items[0]}", str(items[0]), len(str(boxed.value)))
    numbers: Box[int | str] = Box(value=3)
    floats: Box[float | str] = Box(value=1.5)
    flags: list[bool | str] = [True, "no"]
    lists: Box[list[int] | str] = Box(value=[1, 2])
    stdout.print(numbers.value, floats.value, flags[0], flags[1], lists.value, flags)
    stdout.print(f"{numbers.value}|{floats.value}|{flags[1]}|{lists.value}", str(lists.value))
"#,
    )
}

/// A name narrowed to `None`, like `x` after `x = None`, prints and formats as `None`. LLVM
/// refused it once (#572).
#[test]
fn a_name_narrowed_to_none_prints_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_expected_in_both_builds(
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
fn optional_strings_in_lists_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "optional_strings_in_lists",
        r#"def first(words: list[str]) -> str?:
    if len(words) > 0:
        return words[0]
    return None

def main(stdout: Stdout):
    mut found = []
    found.append(first(["short"]))
    found.append(first([]))
    found.append(first(["a string long enough that it has a block of its own"]))
    stdout.print(found, len(found))
    for item in found:
        stdout.print(item else "missing")
"#,
    )
}

/// An error and a struct holding a `str?`, each held as `X?` in another struct, compared, printed and
/// handed to a task and back: one layout, so a field and a local hold each the same way, where the
/// first layout held them in place and the finished one as blocks. The counts are as pinned: the
/// copies and blocks made converting between the two ways are gone.
#[test]
fn an_error_and_a_struct_holding_a_str_optional_are_held_one_way_everywhere() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    prints_in_both_builds(
        "optional_texts_one_way",
        OPTIONAL_TEXTS_ONE_WAY,
        "Inner(label=\"a label long enough for a block 1\", note=None) True Lost(place=\"a place long enough for a block 2\", hint=None) True\nInner(label=\"a label long enough for a block 3\", note=None) None Lost(place=\"a place long enough for a block 2\", hint=None) True True 3\n",
    )?;
    assert_eq!(
        counts_of("optional_texts_one_way_counts", OPTIONAL_TEXTS_ONE_WAY)?,
        "copies 29 copied_bytes 1968 largest_copy 88 allocations 94 allocated_bytes 6440 live 0"
    );
    Ok(())
}

const OPTIONAL_TEXTS_ONE_WAY: &str = r#"error Lost:
    place: str
    hint: str?

struct Inner:
    label: str
    note: str?

struct Holder:
    maybe: Inner?
    why: Lost?
    count: int

def made(n: int) -> Holder:
    inner = Inner(label=f"a label long enough for a block {n}", note=f"a note long enough for a block {n}" if n % 2 == 0 else None)
    lost = Lost(place=f"a place long enough for a block {n}", hint=None if n % 2 == 0 else f"a hint long enough for a block {n}")
    return Holder(maybe=inner, why=lost, count=n)

def put(out: shared list[Holder], holder: Holder):
    lock out as ref held:
        held.append(holder)
    return

def main(stdout: Stdout):
    noise = [made(n + 10) for n in range(3)]
    holders = [made(n) for n in range(4)]
    first = holders[1].maybe else return
    stdout.print(first, first == made(1).maybe, holders[2].why, holders[2].why == made(2).why)
    shared out = list[Holder]()
    for at in [3, 2]:
        given = holders[at]
        with launch(() => put(out, given)):
            pass
    lock out as held:
        kept = held[0].maybe else return
        stdout.print(kept, kept.note, held[1].why, held[0] == holders[3], held[1] == holders[2], len(noise))
    return
"#;

/// `==` and `!=` on an optional string, against a string, another optional string or `None`:
/// short and long strings, and `None`, whose header is all zero, on either side.
#[test]
fn optional_strings_compare_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "optional_strings_compare",
        r#"def first(words: list[str]) -> str?:
    if len(words) > 0:
        return words[0]
    return None

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
fn an_optional_of_an_optional_is_the_optional_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "optional_of_optional",
        r#"from std.collections import SortedMap

def first[T](items: list[T]) -> T?:
    if len(items) > 0:
        return items[0]
    return None

def words() -> list[str?]:
    return ["kit", None]

def numbers() -> list[int?]:
    return [None, 3]

def main(stdout: Stdout):
    stdout.print(first(words()), first(words()[1:]) is None, first(numbers()) is None)
    mut map = SortedMap[int, str?]()
    map[1] = None
    map[2] = "kit"
    stdout.print(map.get(1) is None, map.get(2), map.get(3) is None)
    stdout.print(map.remove(2), map.remove(1) is None, len(map))
"#,
    )
}

/// `?` in `main`, which then can fail: going on when nothing failed.
#[test]
fn a_main_giving_back_a_result_matches_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "main_gives_back",
        r#"def number(text: str) -> int fails InvalidValue:
    return int(text)?

def main(stdout: Stdout):
    stdout.print(number("12")?)
    stdout.print("still here")
"#,
    )
}

/// A function that can fail in more than one way, however its error is then read.
#[test]
fn sets_of_errors_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "error_sets",
        r#"error NotFound:
    name: str

error TooWeak:
    level: int

error Plain:
    code: int

def weak(level: int) -> int fails TooWeak:
    if level < 5: fail TooWeak(level)
    return level

def named(name: str) -> str fails NotFound:
    if name == "": fail NotFound(name)
    return name

def challenge(name: str, level: int) -> str fails NotFound | TooWeak:
    who = named(name)?
    power = weak(level)?
    return f"{who} at {power}"

def three(name: str, level: int, code: int) -> str fails NotFound | Plain | TooWeak:
    if code > 0: fail Plain(code)
    return challenge(name, level)?

## Whether `challenge` fails with `error`, compared with the failure on the left.
def fails_with(name: str, level: int, error: NotFound | TooWeak) -> bool:
    _ = challenge(name, level) except why:
        return why == error
    return False

## The same, compared with the failure on the right.
def fails_by(name: str, level: int, error: NotFound | TooWeak) -> bool:
    _ = challenge(name, level) except why:
        return error == why
    return False

def main(stdout: Stdout):
    for name, level in [("ash", 7), ("kit", 1), ("", 9)]:
        text = challenge(name, level) except why:
            case TooWeak(level):
                stdout.print("weak", level)
                continue
            case _:
                error = why
                stdout.print("other", error, f"{error}")
                continue
        stdout.print(text)
    for code in [0, 3]:
        text = three("", level=1, code=code) except why:
            case Plain(code):
                stdout.print("plain", code)
                continue
            case NotFound(name):
                stdout.print("not found", len(name))
                continue
            case _:
                error = why
                stdout.print("other", error)
                continue
        stdout.print(text)
    kit = three("kit", level=1, code=0) except error:
        f"{error}"
    stdout.print(kit)
    stdout.print(fails_with("kit", 1, error=TooWeak(level=1)))
    stdout.print(fails_with("kit", 1, error=TooWeak(level=2)))
    stdout.print(fails_by("kit", 1, error=TooWeak(level=1)))
    stdout.print(not fails_with("kit", 1, error=TooWeak(level=1)))
    stdout.print(fails_with("ash", 7, error=TooWeak(level=1)))
    stdout.print(fails_with("", 1, error=NotFound(name="")))
    found = challenge("kit", 9) except NotFound() | TooWeak(): "failed"
    missing = challenge("", 9) except NotFound() | TooWeak(): "failed"
    stdout.print(found, missing)
"#,
    )
}

/// An error from a set that nothing handles ends the program as pinned.
#[test]
fn an_unhandled_error_of_a_set_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "error_set_unhandled",
        "error NotFound:
    name: str

error TooWeak:
    level: int

def pick(level: int) -> int fails NotFound | TooWeak:
    if level < 0: fail NotFound(\"x\")
    if level < 5: fail TooWeak(level)
    return level

def main(stdout: Stdout) -> int:
    stdout.print(pick(9)?)
    stdout.print(pick(1)?)
    return 0
",
    )
}

/// `?` in `main` on an error fails the program, saying so at `main`.
#[test]
fn a_main_giving_back_an_error_fails_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "main_gives_back_an_error",
        r#"def number(text: str) -> int fails InvalidValue:
    return int(text)?

def main(stdout: Stdout):
    stdout.print("first")
    stdout.print(number("twelve")?)
    stdout.print("never")
"#,
    )
}

/// A failure in a function with nothing else in it that can fail still says what went wrong.
#[test]
fn failing_calls_alone_in_a_function_fail_as_pinned() -> std::io::Result<()> {
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
        same_message_as_pinned(
            name,
            &format!(
                "from std.math import ceil, floor

def main(stdout: Stdout):
    empty: list[int] = []
    stdout.print(len(empty))
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
        if self.at >= len(self.parts):
            fail ParseError(message="out of words", line=0, column=self.at)
        self.at += 1
        return self.parts[self.at - 1]

    def int(ref self) -> int fails ParseError:
        word = self.next()?
        n = int(word) except InvalidValue():
            fail ParseError(message=f"`{word}` should be an int", line=0, column=self.at)
        return n

    def float(ref self) -> float fails ParseError:
        word = self.next()?
        x = float(word) except InvalidValue():
            fail ParseError(message=f"`{word}` should be a float", line=0, column=self.at)
        return x

    def bool(ref self) -> bool fails ParseError:
        return self.next()? == "yes"

    def text(ref self) -> str fails ParseError:
        return self.next()?

    def bytes(ref self) -> bytes fails ParseError:
        return self.next()?.encode()

    def at_item(self, at: int, message: str) -> ParseError:
        return ParseError(message=f"item {at} {message}", line=0, column=0)

    def here(self, message: str) -> ParseError:
        return ParseError(message=message, line=0, column=0)

    def saw_id(ref self, kind: str, position: int):
        pass

    def saw_table(ref self, kind: str, count: int):
        pass

    def tag(ref self, kind: str) -> int:
        return 0

    def none_next(ref self) -> bool:
        return self.at < len(self.parts) and self.parts[self.at] == "-"

    def exact(self) -> bool:
        return False

    def ordered(ref self, name: str) -> bool fails ParseError:
        return False

    def end_ordered(ref self):
        pass

    def map_count(ref self) -> int fails ParseError:
        return -1

    def end_map(ref self):
        pass

    def items(ref self, name: str, count: int) fails ParseError:
        pass

    def item[T: Deserializable](ref self) -> T fails ParseError:
        return T.deserialize(ref self)?

    def end_items(ref self) fails ParseError:
        pass

    def none(ref self) -> bool fails ParseError:
        if self.at < len(self.parts) and self.parts[self.at] == "-":
            self.at += 1
            return True
        return False

    def value[T: Deserializable](ref self) -> T fails ParseError:
        return T.deserialize(ref self)?

    def sequence[T: Deserializable](ref self) -> list[T] fails ParseError:
        mut found = list[T]()
        for _ in range(self.int()?):
            found.append(T.deserialize(ref self)?)
        return found

    def map[K: Deserializable, V: Deserializable](ref self) -> list[(K, V)] fails ParseError:
        mut found = list[(K, V)]()
        for _ in range(self.int()?):
            key = K.deserialize(ref self)?
            found.append((key, V.deserialize(ref self)?))
        return found

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
        return self.place

    def missing(self, name: str) -> ParseError:
        return ParseError(message=f"`{name}` is missing", line=0, column=self.at)

    def rule(self, message: str) -> ParseError:
        return ParseError(message=f"in the text, {message}", line=0, column=self.at)

    def enum_case(ref self, name: str, cases: list[(str, bool)]) -> int fails ParseError:
        word = self.next()?
        mut at = 0
        for case_name, _ in cases:
            if case_name == word:
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
    return src.value[Unit]()?

def shown(text: str) -> str:
    unit = read(text) except error:
        return f"failed: {error.message} at {error.column}"
    orders = " ".join([str(order) for order in unit.orders])
    pet = unit.pet else "no pet"
    return f"{unit.name} {unit.health} {pet} {unit.pace} {unit.ready} {unit.tags} {len(unit.seen)} {unit.scores} {orders}"

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
    pair = src.value[Pair[int]]() except ParseError(): Pair(left=0, right=0)
    skipped = read("Unit name ash end") except ParseError(): Unit(name="none", pet=None)
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
                  struct Part:\n    size = 3\n\n    def make() -> Self:\n        return Self(size=4)\n\n    \
                  def measure(self) -> int:\n        return self.size\n\n\
                  struct Other:\n    size = 8\n\n    def make() -> Self:\n        return Self(size=9)\n\n    \
                  def measure(self) -> int:\n        return self.size\n\n\
                  def build[T: Made]() -> T:\n    return T.make()\n\n\
                  def named[T: Made](text: str) -> str:\n    made = build[T]()\n    return f\"{text} {made.measure()}\"\n\n\
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

/// `T.make()` calls the function of the type each copy of a generic stands for.
#[test]
fn a_type_parameter_calls_its_types_function() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let source = "interface Made:\n    def make() -> Self\n\n\
                  struct Part:\n    x = 1\n\n    def make() -> Self:\n        return Self(x=7)\n\n\
                  struct Other:\n    name = \"\"\n\n    def make() -> Self:\n        return Self(name=\"other\")\n\n\
                  def build[T: Made]() -> T:\n    return T.make()\n\n\
                  def twice[T: Made]() -> list[T]:\n    return [build[T](), T.make()]\n\n\
                  def main(stdout: Stdout):\n    stdout.print(build[Part]().x, build[Other]().name)\n    \
                  made: Part = build()\n    stdout.print(made.x, len(twice[Other]()))\n";
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
fn small_calls_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "small_calls",
        r#"from std.math import cos, sin

struct Money:
    cents: int

    def <(self, other: Money) -> bool:
        return self.cents > other.cents

struct Spot:
    x: int
    y: int

def main(stdout: Stdout):
    stdout.print(max(1, 3, 2), min(1, 3, 2), max(1.5, -0.5), min(-0.0, 0.0), max("a", "b long enough for a block"))
    stdout.print(max([3, 1, 3]), min([3, 1, 1]), max([0.5, 2.5]), min(["pear", "apple"]), max([False, True]))
    stdout.print(max([(1, "b"), (2, "a"), (2, "b")]), min([(1, 2.5), (1, 1.5)]))
    stdout.print(max(["aa", "bbb", "ccc"], key=word => len(word)), min(["aa", "bbb", "c"], key=word => len(word)))
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
    same_as_expected_in_both_builds(
        "fails_only_in_max",
        r#"def top(values: list[int]) -> int:
    return max(values)

def smallest(values: list[int]) -> int:
    return min(values)

def longest(words: list[str]) -> str:
    return max(words, key=word => len(word))

def shortest(words: list[str]) -> str:
    return min(words, key=word => len(word))

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

/// `max` of nothing, and removing what isn't there, fail as pinned.
#[test]
fn small_calls_fail_as_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_message_as_pinned(
        "max_of_nothing",
        "def main(stdout: Stdout):
    mut none = [1]
    _ = none.pop()
    stdout.print(max(none))
",
    )?;
    same_message_as_pinned(
        "remove_missing",
        "def main(stdout: Stdout):
    mut some = [1]
    some.remove(2)
    stdout.print(some)
",
    )
}

/// Texts for `float.parse`, each with the number it reads as, written as a program writes it:
/// `inf`, `-inf` and `nan` for those, and nothing for text that isn't a float.
fn float_rows() -> Vec<(String, Option<String>)> {
    let padded = format!("{}2.5", " ".repeat(30));
    let padded_long = format!("{}2.5{}", " ".repeat(50), " ".repeat(5));
    let (inline_23, block_24) = (format!("1.{}", "0".repeat(21)), format!("1.{}", "0".repeat(22)));
    let digits_long = format!("0.{}5", "0".repeat(45));
    let floats = [
        ("1.5", "1.5"),
        (".5", "0.5"),
        ("5.", "5.0"),
        ("-2e3", "-2000.0"),
        (" 1.5 ", "1.5"),
        ("\t42\n", "42.0"),
        ("+7", "7.0"),
        ("0", "0.0"),
        ("-0", "-0.0"),
        ("1234.56", "1234.56"),
        ("000001.250", "1.25"),
        ("1E3", "1000.0"),
        (padded.as_str(), "2.5"),
        (padded_long.as_str(), "2.5"),
        (inline_23.as_str(), "1.0"),
        (block_24.as_str(), "1.0"),
        (digits_long.as_str(), digits_long.as_str()),
        ("1.5\u{a0}", "1.5"),
        ("\u{3000}8.25", "8.25"),
        ("inf", "inf"),
        ("-inf", "-inf"),
        ("Infinity", "inf"),
        ("1e400", "inf"),
        ("-1e400", "-inf"),
        ("nan", "nan"),
        ("NaN", "nan"),
    ];
    let others = [
        "",
        " ",
        "abc",
        "1.5x",
        "x1.5",
        "1,5",
        "1 5",
        "--1",
        "1e",
        ".",
        "\u{663}.\u{665}",
        "0x10",
        "a text that isn't a float and is long enough to be past forty bytes",
    ];
    let floats = floats.iter().map(|(text, wanted)| ((*text).to_owned(), Some((*wanted).to_owned())));
    floats.chain(others.iter().map(|text| ((*text).to_owned(), None))).collect()
}

/// What the program over `float_rows` prints: `same` for each float, and for other text the
/// message it fails with. Each row is checked against what Rust reads, so the table can't drift
/// from it.
fn floats_printed(rows: &[(String, Option<String>)]) -> String {
    use std::fmt::Write as _;
    let mut printed = String::new();
    for (text, wanted) in rows {
        let read = text.trim().parse::<f64>().ok();
        let same = match (wanted.as_deref(), read) {
            (None, None) => true,
            (Some("nan"), Some(read)) => read.is_nan(),
            (Some(wanted), Some(read)) => {
                wanted.parse::<f64>().is_ok_and(|wanted| wanted.total_cmp(&read).is_eq())
            }
            _ => false,
        };
        assert!(same, "{text:?} reads as {read:?} in Rust, and the table says {wanted:?}");
        let count = text.chars().count();
        if wanted.is_some() {
            printed.push_str("same\n");
        } else if count <= 40 {
            let _ = writeln!(printed, "{text:?} isn't a float");
        } else {
            let start: String = text.chars().take(20).collect();
            let _ = writeln!(printed, "\"{}…\" ({count} characters) isn't a float", start.escape_debug());
        }
    }
    printed
}

/// A program that reads each of `float_rows` with `float.parse` and compares it with the same
/// number written in the program, which the compiler reads.
fn floats_program(rows: &[(String, Option<String>)]) -> String {
    let texts: Vec<String> = rows.iter().map(|(text, _)| fennec_literal(text)).collect();
    let kinds: Vec<String> = rows
        .iter()
        .map(|(_, wanted)| match wanted.as_deref() {
            Some(special @ ("inf" | "-inf" | "nan")) => format!("\"{special}\""),
            Some(_) => "\"number\"".to_owned(),
            None => "\"none\"".to_owned(),
        })
        .collect();
    let numbers: Vec<&str> = rows
        .iter()
        .map(|(_, wanted)| match wanted.as_deref() {
            Some("inf" | "-inf" | "nan") | None => "0.0",
            Some(number) => number,
        })
        .collect();
    format!(
        r#"def same(read: float, kind: str, wanted: float) -> bool:
    if kind == "nan":
        return read.is_nan()
    if kind == "inf":
        return read > 1.0e308
    if kind == "-inf":
        return read < -1.0e308
    return kind == "number" and read == wanted

def main(stdout: Stdout):
    texts = [{}]
    kinds = [{}]
    wanted = [{}]
    for at in range(len(texts)):
        read = float.parse(texts[at]) except error:
            stdout.print(error.message)
            continue
        stdout.print("same" if same(read, kind=kinds[at], wanted=wanted[at]) else f"differs: {{read}}")
    return
"#,
        texts.join(", "),
        kinds.join(", "),
        numbers.join(", ")
    )
}

/// `float.parse` reads what Rust's `str::parse::<f64>` reads from the trimmed text, and fails on
/// what it fails on, saying what it was given: with and without a sign, a point and an exponent,
/// space around it of ASCII and beyond, `inf` and `nan` however they're cased, a number too big to
/// hold, text held inline at 23 bytes and in a block at 24, text past the 40 bytes read in place,
/// and what isn't a float at all, digits that aren't ASCII among it. Each float read is compared
/// with the same number written in the program, which the compiler reads. The text is read where
/// it is, by its header, and only text that isn't a float is made a block, for its message, so
/// no block is left after.
#[test]
fn a_float_read_from_text_is_what_rust_reads_at_every_edge() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let rows = float_rows();
    let dir = folder("floats_read")?;
    let program = dir.join("floats_read.fen");
    std::fs::write(&program, floats_program(&rows))?;
    prints_with_nothing_left(&dir, &program, &floats_printed(&rows), &["1"])
}

/// Numbers read from text, with what fails being a value the program holds; and lists and sets of
/// a string's characters and a list's items.
#[test]
fn numbers_and_sets_from_text_match_what_was_pinned() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    same_as_pinned(
        "numbers_from_text",
        r#"from std.re import Regex

def add(a: str, b: str) -> int fails InvalidValue:
    return int(a)? + int(b)?

def main(stdout: Stdout):
    for text in ["42", " -7 ", "4x2", "", "99999999999999999999"]:
        n = int(text) except error:
            stdout.print(error.message)
            continue
        stdout.print(n)
    for text in ["2.5", " 1e3 ", "nope", "a \"quoted\" value long enough for a block"]:
        x = float(text) except error:
            stdout.print(error.message)
            continue
        stdout.print(x)
    int_value = int("12") except InvalidValue(): 0
    int_value2 = int("x") except InvalidValue(): -1
    float_value = float("0.5") except InvalidValue(): 0.0
    float_value2 = float("y") except InvalidValue(): -1.5
    stdout.print(int_value, int_value2, float_value, float_value2)
    add_value = add("1", b="2") except InvalidValue(): -1
    add_value2 = add("1", b="two") except InvalidValue(): -1
    stdout.print(add_value, add_value2)
    for pattern in ["[0-9]+", "[0-9+"]:
        regex = Regex(pattern) except error:
            stdout.print(error.message)
            continue
        stdout.print(regex.pattern)
    stdout.print(list("héllo"), list(""), len(set("hello")), "l" in set("hello"), set(""))
    stdout.print(set([3, 1, 3, 2, 1]), set(["b", "a", "b", "a key long enough for a block", "a key long enough for a block"]))
    stdout.print(len(set([(1, 2), (1, 2), (3, 4)])))
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

/// What a compiled program does natively, a compiled test does with its fakes: a sleep passes no
/// time, and nothing runs.
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
    assert len(process.ran()) == 1
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
/// failed for there too.
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
/// through LLVM, rather than running again another way.
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
    return int(text)?

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
