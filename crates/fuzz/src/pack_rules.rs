//! The `pack` target: `std.pack`'s reader, built natively, on any bytes. Nothing reads its format
//! but itself, so it's held to what `std.pack` says of itself: bytes are read as each of `TYPES`,
//! and whatever reads at all saves again to bytes that read back as the same value, while the
//! same bytes with one more byte, or one fewer, fail to read. A stream of values read through
//! `Unpacker` may end or fail anywhere, but never crash. The supervisor and `read_natively`
//! catch crashes, getting stuck and holding far more memory than the bytes could need.
//!
//! `Node` is also read within `SMALL` limits, so bytes reach the limits' own failures: one that
//! nests deeper than 4, or holds a text longer than 64 bytes, has to fail, never crash or overflow
//! the stack. The seeds for it in `seeds/pack`: `node_70.bin`, a chain 70 deep, past both the
//! default limit of 64 and `SMALL`'s 4; and `node_3.bin`, 3 deep, inside both, so it reads and
//! saves back the same either way.

use std::fmt::Write as _;

/// The types bytes are read as, each with the limits it's read within: the default's, or `SMALL`.
const TYPES: [(&str, &str); 14] = [
    ("int", ""),
    ("str", ""),
    ("float", ""),
    ("list[int]", ""),
    ("list[str?]", ""),
    ("dict[str, list[float]]", ""),
    ("list[(int, str)]", ""),
    ("Scene", ""),
    ("bytes", ""),
    ("bool", ""),
    ("Choice", ""),
    ("Duration", ""),
    ("Node", ""),
    ("Node", ", limits=SMALL"),
];

/// The types `Scene` is made of, and how a stream of them is read.
const DECLARED: &str = "\
from std import pack
from std.pack import Limits, Unpacker
from std.time import Duration

struct Spot:
    x: int
    name: str
    tags = list[str]()

enum Shape:
    Dot
    Ring(radius: float)

struct Scene:
    spots: list[Spot]
    shape: Shape
    note: str? = None

## A union, held in a field so its type can be written optional.
struct Choice:
    value: int | str

## A chain as deep as its bytes say, to reach the depth limit.
struct Node:
    name: str
    next: Node? = None

## Limits small enough for bytes to pass them: 4 deep, and 64 bytes a text.
SMALL = Limits(depth=4, length=64)

def streamed(data: bytes) -> int:
    mut read = 0
    mut values = Unpacker.of(data)
    while read < 1000:
        found = values.next[Scene]() else as _:
            return -1 - read
        if found is None:
            return read
        read += 1
    read

";

/// The program that reads the file it's given as each of `TYPES`, a line each, then as a stream.
/// The `n`th type is read by its own function, `read_as_n`.
pub(crate) fn reader() -> String {
    let mut source = DECLARED.to_owned();
    for (n, (ty, limits)) in TYPES.iter().enumerate() {
        let _ = write!(
            source,
            "def read_as_{n}(data: bytes, longer: bytes, shorter: bytes) -> str:\n    \
             value = pack.loads[{ty}](data{limits}) else:\n        return \"err\"\n    \
             back: {ty}? = pack.loads[{ty}](pack.dumps(value){limits}) else None\n    \
             same = back is not None and back == value\n    \
             grown_back: {ty}? = pack.loads[{ty}](longer{limits}) else None\n    \
             grown = \"ok\" if grown_back is not None else \"err\"\n    \
             cut_back: {ty}? = pack.loads[{ty}](shorter{limits}) else None\n    \
             cut = \"ok\" if cut_back is not None else \"err\"\n    \
             f\"ok {{same}} {{grown}} {{cut}}\"\n\n",
        );
    }
    source.push_str(
        "def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:\n    data = fs.read_bytes(args[0])?\n    \
         longer = data + bytes([0])\n    mut shorter = data\n    if data.len() > 0:\n        \
         shorter = data[0:data.len() - 1]\n",
    );
    for n in 0..TYPES.len() {
        let _ = writeln!(source, "    stdout.print(read_as_{n}(data, longer=longer, shorter=shorter))");
    }
    source.push_str("    stdout.print(\"stream\", streamed(data))\n");
    source
}

/// The target: what `std.pack` says of itself, for the input's bytes.
pub(crate) fn pack(input: &[u8]) -> Result<(), String> {
    let Some(printed) = crate::llvm::read_natively("pack", input)? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != TYPES.len() + 1 || !lines.last().is_some_and(|last| last.starts_with("stream ")) {
        return Err(format!("the reader printed something else than a line for each type:\n{printed}"));
    }
    for ((ty, limits), line) in TYPES.iter().zip(&lines) {
        let Some(read) = line.strip_prefix("ok ") else { continue };
        let said: Vec<&str> = read.split(' ').collect();
        let wrong = match said.as_slice() {
            ["False", ..] => "saved again, it doesn't read back as the same value",
            [_, "ok", ..] => "with a byte more after it, it still reads",
            [_, _, "ok"] if !input.is_empty() => "with its last byte left off, it still reads",
            [_, _, _] => continue,
            _ => "the reader printed something else",
        };
        let within = if limits.is_empty() { "" } else { " within SMALL" };
        return Err(format!("{input:?} read as {ty}{within}: {wrong} ({line})"));
    }
    Ok(())
}
