//! The `pack` target: `std.pack`'s reader, built natively, on any bytes. Nothing reads its format
//! but itself, so it's held to what `std.pack` says of itself: bytes are read as each of `TYPES`,
//! and whatever reads at all saves again to bytes that read back as the same value, while the
//! same bytes with one more byte, or one fewer, fail to read. A stream of values read through
//! `Unpacker` may end or fail anywhere, but never crash. The supervisor and `read_natively`
//! catch crashes, getting stuck and holding far more memory than the bytes could need.

use std::fmt::Write as _;

/// The types bytes are read as.
const TYPES: [&str; 8] =
    ["int", "str", "float", "list[int]", "list[str?]", "dict[str, list[float]]", "list[(int, str)]", "Scene"];

/// The types `Scene` is made of, and how a stream of them is read.
const DECLARED: &str = "\
from std import pack
from std.pack import Unpacker

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
    for (n, ty) in TYPES.iter().enumerate() {
        let _ = write!(
            source,
            "def read_as_{n}(data: bytes, longer: bytes, shorter: bytes) -> str:\n    \
             value = pack.loads[{ty}](data) else:\n        return \"err\"\n    \
             back: {ty}? = pack.loads[{ty}](pack.dumps(value)) else None\n    \
             same = back is not None and back == value\n    \
             grown = \"ok\" if failure_of(() => pack.loads[{ty}](longer)) is None else \"err\"\n    \
             cut = \"ok\" if failure_of(() => pack.loads[{ty}](shorter)) is None else \"err\"\n    \
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
    for (ty, line) in TYPES.iter().zip(&lines) {
        let Some(read) = line.strip_prefix("ok ") else { continue };
        let said: Vec<&str> = read.split(' ').collect();
        let wrong = match said.as_slice() {
            ["False", ..] => "saved again, it doesn't read back as the same value",
            [_, "ok", ..] => "with a byte more after it, it still reads",
            [_, _, "ok"] if !input.is_empty() => "with its last byte left off, it still reads",
            [_, _, _] => continue,
            _ => "the reader printed something else",
        };
        return Err(format!("{input:?} read as {ty}: {wrong} ({line})"));
    }
    Ok(())
}
