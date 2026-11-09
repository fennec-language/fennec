//! The `toml-rules` target: `std.toml`, built natively, on any text. Whatever it reads as each
//! of `TYPES` it writes with `toml.dumps`, and that text has to read back as the same value. The
//! TOML spec's own list of what must fail is checked once, in `fails_what_the_spec_refuses`.

use std::fmt::Write as _;

/// The types text is read as.
const TYPES: [&str; 5] =
    ["dict[str, str]", "dict[str, int]", "dict[str, list[float]]", "dict[str, dict[str, str]]", "Settings"];

/// The program that reads the input as each of `TYPES`, a line each, saying whether what it read
/// writes and reads back as itself.
pub(crate) fn reader() -> String {
    let mut source = "\
from std import toml

struct Window:
    title: str
    size = 0
    scale = 1.0

struct Settings:
    name: str
    on = False
    tags = list[str]()
    window = Window(title=\"main\")
    limits = dict[str, int]()

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
"
    .to_owned();
    for ty in TYPES {
        let _ = write!(
            source,
            "    match toml.loads[{ty}](text):\n        case Ok(value):\n            written = toml.dumps(value)\n            \
             match toml.loads[{ty}](written):\n                case Ok(back):\n                    \
             stdout.print(\"ok\", back == value)\n                case Err(_):\n                    \
             stdout.print(\"ok unreadable\")\n        case Err(_):\n            stdout.print(\"err\")\n"
        );
    }
    source
}

/// The target: what's read writes and reads back as itself.
pub(crate) fn toml(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("toml-rules", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != TYPES.len() {
        return Err(format!("the reader printed something else:\n{printed}"));
    }
    for (ty, line) in TYPES.iter().zip(&lines) {
        match *line {
            "err" | "ok True" => {}
            "ok unreadable" => {
                return Err(format!("{text:?} read as {ty}, but what `dumps` wrote doesn't read"));
            }
            _ => {
                return Err(format!("{text:?} read as {ty}, but what `dumps` wrote reads as something else"));
            }
        }
    }
    Ok(())
}
