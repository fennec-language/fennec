//! The `csv` target: `std.csv`, built natively, held to the rules its own documentation gives,
//! which a reader here follows: RFC 4180's quoting, lines ending in `\n`, `\r\n` or `\r`, blank
//! lines that aren't rows, and a byte order mark at the start passed over. What `csv.loads` reads
//! is compared with what that reader reads, and what it can't read has to fail.
//!
//! Then the input is taken as rows, split at `\u{1e}` and fields at `\u{1f}`, and those rows go
//! through `csv.dumps` and back through `csv.loads`, which has to give the same rows. Split that
//! way, every row has a field, so `dumps` never has a reason to refuse them.

use crate::json_rules::{Json, parse};

/// The program that reads the input as CSV, then writes its rows and reads them back.
pub(crate) const READER: &str = "\
from std import csv, json

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    loaded: list[list[str]]? = csv.loads(text) except ParseError(): None
    stdout.print(\"err\" if loaded is None else \"ok \" + json.dumps(loaded))
    rows = [part.split(\"\\u001f\") for part in text.split(\"\\u001e\")]
    written: str? = csv.dumps(rows) except InvalidValue(): None
    if written is None:
        stdout.print(\"unwritten\")
    else:
        back: list[list[str]]? = csv.loads(written) except ParseError(): None
        stdout.print(\"err\" if back is None else \"ok \" + json.dumps(back))
    stdout.print(json.dumps(rows))
";

/// What the rules say `text` reads as, or None when it isn't CSV.
fn rows(text: &str) -> Option<Vec<Vec<String>>> {
    let mut chars = text.strip_prefix('\u{feff}').unwrap_or(text).chars().peekable();
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    // Whether this field was in quotes, so it's a field even when it's empty.
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if field.is_empty() && !quoted => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '"' if chars.peek() == Some(&'"') => {
                            chars.next();
                            field.push('"');
                        }
                        '"' => break,
                        c => field.push(c),
                    }
                }
                if !matches!(chars.peek(), None | Some(',' | '\n' | '\r')) {
                    return None;
                }
            }
            '"' => return None,
            ',' => {
                row.push(std::mem::take(&mut field));
                quoted = false;
            }
            '\n' | '\r' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if !row.is_empty() || !field.is_empty() || quoted {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                quoted = false;
            }
            c => field.push(c),
        }
    }
    if !row.is_empty() || !field.is_empty() || quoted {
        row.push(field);
        rows.push(row);
    }
    Some(rows)
}

/// Rows written as JSON, read back.
fn as_rows(json: &Json) -> Option<Vec<Vec<String>>> {
    let Json::Array(rows) = json else { return None };
    rows.iter()
        .map(|row| {
            let Json::Array(fields) = row else { return None };
            fields
                .iter()
                .map(|field| if let Json::Text(text) = field { Some(text.clone()) } else { None })
                .collect()
        })
        .collect()
}

/// Rows printed as JSON.
fn printed_rows(text: &str) -> Option<Vec<Vec<String>>> {
    as_rows(&parse(text).ok()?)
}

/// The target: what `csv.loads` and `csv.dumps` do with the input, against the rules.
pub(crate) fn csv(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("csv", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    let [read, written, given] = lines.as_slice() else {
        return Err(format!("the reader printed something else:\n{printed}"));
    };
    match (rows(&text), read.strip_prefix("ok ")) {
        (None, Some(read)) => return Err(format!("{text:?} isn't CSV, but `loads` read {read}")),
        (Some(wanted), None) => return Err(format!("{text:?} is CSV, but `loads` failed: it's {wanted:?}")),
        (Some(wanted), Some(read)) if printed_rows(read).as_ref() != Some(&wanted) => {
            return Err(format!("{text:?} is {wanted:?}, but `loads` read {read}"));
        }
        _ => {}
    }
    let given = printed_rows(given).ok_or_else(|| format!("the reader printed {given:?}"))?;
    // Rows split from text always have a field each, which CSV can write.
    if *written == "unwritten" {
        return Err(format!("`dumps` refused {given:?}"));
    }
    match written.strip_prefix("ok ").map(printed_rows) {
        None => Err(format!("`dumps` of {given:?} wrote what `loads` can't read")),
        Some(Some(back)) if back == given => Ok(()),
        Some(back) => Err(format!("`dumps` of {given:?} reads back as {back:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::rows;

    fn of(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter().map(|row| row.iter().map(|field| (*field).to_owned()).collect()).collect()
    }

    #[test]
    fn the_rules_read_csv_as_the_documentation_says() {
        assert_eq!(
            rows("a,b\r\nc,\"d,\"\"e\"\"\"\n\n\"\"\r"),
            Some(of(&[&["a", "b"], &["c", "d,\"e\""], &[""]]))
        );
        assert_eq!(rows("\u{feff}a,\n,"), Some(of(&[&["a", ""], &["", ""]])));
        assert_eq!(rows("a\"b"), None);
        assert_eq!(rows("\"a\"b"), None);
        assert_eq!(rows("\"a"), None);
        assert_eq!(rows("\"a\nb\""), Some(of(&[&["a\nb"]])));
    }
}
