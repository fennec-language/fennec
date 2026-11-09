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
        match values.next[Scene]():
            case Ok(found):
                if found is None:
                    return read
                read += 1
            case Err(_):
                return -1 - read
    read

";

/// The program that reads the file it's given as each of `TYPES`, a line each, then as a stream.
pub(crate) fn reader() -> String {
    let mut source = DECLARED.to_owned();
    source.push_str(
        "def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:\n    data = fs.read_bytes(args[0])?\n    \
         longer = data + bytes([0])\n    mut shorter = data\n    if data.len() > 0:\n        \
         shorter = data[0:data.len() - 1]\n",
    );
    for ty in TYPES {
        let _ = write!(
            source,
            "    match pack.loads[{ty}](data):\n        case Ok(value):\n            mut same = False\n            \
             match pack.loads[{ty}](pack.dumps(value)):\n                case Ok(back):\n                    \
             same = back == value\n                case Err(_):\n                    pass\n            \
             mut grown = \"err\"\n            match pack.loads[{ty}](longer):\n                case Ok(_):\n                    \
             grown = \"ok\"\n                case Err(_):\n                    pass\n            \
             mut cut = \"err\"\n            match pack.loads[{ty}](shorter):\n                case Ok(_):\n                    \
             cut = \"ok\"\n                case Err(_):\n                    pass\n            \
             stdout.print(\"ok\", same, grown, cut)\n        case Err(_):\n            stdout.print(\"err\")\n",
        );
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
