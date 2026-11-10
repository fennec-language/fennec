//! The `url` target: `std.url`, built natively, on any text, held to what it says of itself. A
//! URL it reads prints as text that reads back as the same parts, and prints the same again; and
//! `quote` and `unquote`, and `encode_query` and `decode_query`, undo each other.

/// The program that checks each of those for the input, a line each: the check's name and `True`
/// when it holds.
pub(crate) const READER: &str = "\
from std import url
from std.url import Url

def same(a: Url, b: Url) -> bool:
    a.scheme == b.scheme and a.user == b.user and a.host == b.host and a.port == b.port and a.path == b.path and a.query == b.query and a.fragment == b.fragment

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    found: Url? = url.parse(text) else None
    if found is None:
        stdout.print(\"reads-back\", True)
        stdout.print(\"prints-the-same\", True)
    else:
        written = f\"{found}\"
        again: Url? = url.parse(written) else None
        if again is None:
            stdout.print(\"reads-back\", False, written)
            stdout.print(\"prints-the-same\", False, written)
        else:
            stdout.print(\"reads-back\", same(found, again), written)
            stdout.print(\"prints-the-same\", f\"{again}\" == written, written)
    stdout.print(\"unquotes\", url.unquote(url.quote(text)) == text)
    stdout.print(\"unquotes-kept\", url.unquote(url.quote(text, keep=\"/:@%+\")) == text or text.find(\"%\") is not None)
    query = {text: text, \"b\": text + \"&\"}
    stdout.print(\"decodes\", url.decode_query(url.encode_query(query)) == query or text == \"\")
";

/// The checks `READER` makes, in order.
const CHECKS: [&str; 5] = ["reads-back", "prints-the-same", "unquotes", "unquotes-kept", "decodes"];

/// The target: each check the reader makes holds.
pub(crate) fn url(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("url", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != CHECKS.len() {
        return Err(format!("the reader printed something else:\n{printed}"));
    }
    for (check, line) in CHECKS.iter().zip(&lines) {
        let held = line.strip_prefix(check).and_then(|rest| rest.strip_prefix(' '));
        if !held.is_some_and(|rest| rest.starts_with("True")) {
            return Err(format!("{text:?}: {line}"));
        }
    }
    Ok(())
}
