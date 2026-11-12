//! `fennec fix --migrate --rust-strings`: the Fennec programs written in the string literals of
//! Rust files, mostly tests, moved to the forms of 2026-11-11 as `.fen` files are. Each literal
//! holding a `def` is read as the program it spells, migrated on its own, and only its changed
//! lines are written back, in the literal's own quoting and indentation, so the rest of each
//! test reads as it did.

use std::path::Path;
use std::process::ExitCode;

/// How a literal is written: `r#"..."#` with its number of `#`s, or `"..."` with escapes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Quoting {
    Raw(usize),
    Plain {
        /// Whether its lines break where its text does, rather than with `\n`.
        multiline: bool,
    },
}

/// A string literal in a Rust file: what it says, and for each byte of that, where it's written.
#[derive(Debug)]
struct Literal {
    quoting: Quoting,
    text: String,
    /// The byte in the file where each byte of `text` is written, and one past the last.
    at: Vec<usize>,
}

/// The byte at `at`, or nothing past the end.
fn byte(bytes: &[u8], at: usize) -> u8 {
    bytes.get(at).copied().unwrap_or(0)
}

/// The text from `at` on, or nothing where that isn't a char's start.
fn from(src: &str, at: usize) -> &str {
    src.get(at..).unwrap_or_default()
}

/// The string literals in a Rust source, skipping comments, chars and byte strings.
fn literals(src: &str) -> Vec<Literal> {
    let bytes = src.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = from(src, i);
        let before = i.checked_sub(1).map_or(b' ', |at| byte(bytes, at));
        let after_word = !(before.is_ascii_alphanumeric() || before == b'_');
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with('\'') {
            // A char literal, as `'"'` or `'\''`; a lifetime is one quote, passed over.
            i += char_literal(rest).unwrap_or(1);
        } else if let Some(hashes) = raw_start(rest).filter(|_| after_word) {
            let open = i + 2 + hashes;
            let close = format!("\"{}", "#".repeat(hashes));
            let Some(end) = from(src, open).find(&close).map(|at| open + at) else { break };
            found.push(Literal {
                quoting: Quoting::Raw(hashes),
                text: src.get(open..end).unwrap_or_default().to_owned(),
                at: (open..=end).collect(),
            });
            i = end + close.len();
        } else if after_word && rest.starts_with("b\"") {
            i += 2;
            while i < bytes.len() && byte(bytes, i) != b'"' {
                i += if byte(bytes, i) == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if rest.starts_with('"') {
            let (literal, end) = plain(src, i + 1);
            found.push(literal);
            i = end + 1;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    found
}

/// How many `#`s a raw string starting here has, when one does.
fn raw_start(rest: &str) -> Option<usize> {
    let after = rest.strip_prefix('r')?;
    let hashes = after.bytes().take_while(|byte| *byte == b'#').count();
    (after.as_bytes().get(hashes) == Some(&b'"')).then_some(hashes)
}

/// The length of a char literal starting here, when it is one.
fn char_literal(rest: &str) -> Option<usize> {
    let mut chars = rest.char_indices().skip(1);
    let (_, first) = chars.next()?;
    let (at, close) = match (first, chars.next()?) {
        ('\\', (_, 'u')) => {
            let end = rest.find('}')?;
            (end + 1, from(rest, end + 1).chars().next()?)
        }
        ('\\', _) => chars.next()?,
        (_, next) => next,
    };
    (close == '\'').then_some(at + 1)
}

/// A plain literal whose text starts at `start`, unescaped, and where its closing quote is.
fn plain(src: &str, start: usize) -> (Literal, usize) {
    let bytes = src.as_bytes();
    let (mut text, mut at, mut multiline) = (String::new(), Vec::new(), false);
    let mut i = start;
    while i < bytes.len() && byte(bytes, i) != b'"' {
        if byte(bytes, i) == b'\\' && i + 1 < bytes.len() {
            let escaped = byte(bytes, i + 1);
            let mut push = |c: char| {
                at.extend(std::iter::repeat_n(i, c.len_utf8()));
                text.push(c);
            };
            match escaped {
                b'n' => push('\n'),
                b't' => push('\t'),
                b'r' => push('\r'),
                b'0' => push('\0'),
                b'u' => {
                    let end = from(src, i).find('}').map_or(i + 2, |close| i + close);
                    let digits = src.get(i + 3..end).unwrap_or_default();
                    let code = u32::from_str_radix(digits, 16).ok().and_then(char::from_u32);
                    push(code.unwrap_or('\u{fffd}'));
                    i = end.saturating_sub(1);
                }
                b'\n' => {
                    // A line continuation: the break and the next line's indent aren't text.
                    i += 2;
                    while i < bytes.len() && matches!(byte(bytes, i), b' ' | b'\t' | b'\n') {
                        i += 1;
                    }
                    continue;
                }
                _ => push(char::from(escaped)),
            }
            i += 2;
        } else {
            let c = from(src, i).chars().next().unwrap_or('\u{fffd}');
            multiline |= c == '\n';
            at.extend(std::iter::repeat_n(i, c.len_utf8()));
            text.push(c);
            i += c.len_utf8();
        }
    }
    at.push(i);
    (Literal { quoting: Quoting::Plain { multiline }, text, at }, i)
}

/// A program as a literal spells it: with a first line break dropped and the indent every line
/// shares taken off, as the tests' `dedent` reads one. Gives back the program, the indent, and
/// the byte of the literal's text each of its lines starts at.
fn as_program(text: &str) -> (String, String, Vec<usize>) {
    let skip = usize::from(text.starts_with('\n'));
    let body = from(text, skip);
    let indent = body
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.strip_suffix(line.trim_start()).unwrap_or_default())
        .min_by_key(|indent| indent.len())
        .unwrap_or_default()
        .to_owned();
    let (mut program, mut starts, mut at) = (String::new(), Vec::new(), skip);
    for (number, line) in body.split('\n').enumerate() {
        if number > 0 {
            program.push('\n');
        }
        starts.push(at);
        program.push_str(line.strip_prefix(indent.as_str()).unwrap_or(line.trim_start()));
        at += line.len() + 1;
    }
    (program, indent, starts)
}

/// The edits that turn the literal's program `old` into `new`, as (from, to, text) on the Rust
/// file: whole changed lines, written back in the literal's quoting with its indent.
fn edits(literal: &Literal, old: &str, new: &str) -> Option<Vec<(usize, usize, String)>> {
    let (_, indent, starts) = as_program(&literal.text);
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let end_of = |line: usize| -> usize { starts.get(line).copied().unwrap_or(literal.text.len() + 1) };
    let mut found = Vec::new();
    for (from, to, written) in hunks(&old_lines, &new_lines) {
        // Each old line with its line break, the last one's to the literal's end.
        let start = *literal.at.get(end_of(from))?;
        let text_end = end_of(to).min(literal.text.len());
        let end = *literal.at.get(text_end)?;
        let mut replaced = String::new();
        for line in new_lines.get(written).unwrap_or_default() {
            if !line.is_empty() {
                replaced.push_str(&indent);
            }
            replaced.push_str(line);
            replaced.push('\n');
        }
        // The literal's last line has no break after it.
        if to >= old_lines.len() && !literal.text.ends_with('\n') {
            replaced.pop();
        }
        found.push((start, end, quoted(literal.quoting, &replaced)?));
    }
    Some(found)
}

/// The runs of lines that differ: (first old line, one past the last, the new lines instead).
fn hunks(old: &[&str], new: &[&str]) -> Vec<(usize, usize, std::ops::Range<usize>)> {
    // The longest common run of lines from each pair of lines on, row by row.
    let (n, m) = (old.len(), new.len());
    let mut longest = vec![0_usize; (n + 1) * (m + 1)];
    let cell = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let length = if old.get(i) == new.get(j) {
                longest.get(cell(i + 1, j + 1)).copied().unwrap_or(0) + 1
            } else {
                let down = longest.get(cell(i + 1, j)).copied().unwrap_or(0);
                down.max(longest.get(cell(i, j + 1)).copied().unwrap_or(0))
            };
            if let Some(slot) = longest.get_mut(cell(i, j)) {
                *slot = length;
            }
        }
    }
    let at = |i: usize, j: usize| longest.get(cell(i, j)).copied().unwrap_or(0);
    let (mut i, mut j, mut found) = (0, 0, Vec::new());
    let mut open: Option<(usize, usize)> = None;
    while i < n || j < m {
        if i < n && j < m && old.get(i) == new.get(j) {
            if let Some((from, written)) = open.take() {
                found.push((from, i, written..j));
            }
            i += 1;
            j += 1;
            continue;
        }
        open.get_or_insert((i, j));
        if j < m && (i == n || at(i, j + 1) >= at(i + 1, j)) {
            j += 1;
        } else {
            i += 1;
        }
    }
    if let Some((from, written)) = open {
        found.push((from, n, written..m));
    }
    found
}

/// `text` written inside a literal quoted as `quoting`, or nothing when it can't be.
fn quoted(quoting: Quoting, text: &str) -> Option<String> {
    match quoting {
        Quoting::Raw(hashes) => {
            (!text.contains(&format!("\"{}", "#".repeat(hashes)))).then(|| text.to_owned())
        }
        Quoting::Plain { multiline } => {
            let mut out = String::new();
            for c in text.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' if !multiline => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    _ => out.push(c),
                }
            }
            Some(out)
        }
    }
}

/// A Rust source with the programs in its literals migrated by `migrate`, which is given each
/// program and gives back its new text, or nothing when it's left as it is.
fn migrated(src: &str, migrate: &mut dyn FnMut(&str) -> Option<String>) -> Option<String> {
    let mut all = Vec::new();
    for literal in literals(src).iter().filter(|literal| literal.text.contains("def ")) {
        let (program, ..) = as_program(&literal.text);
        let Some(new) = migrate(&program).filter(|new| *new != program) else { continue };
        if let Some(found) = edits(literal, &program, &new) {
            all.extend(found);
        }
    }
    if all.is_empty() {
        return None;
    }
    all.sort_by_key(|(start, ..)| *start);
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for (start, end, text) in all {
        out.push_str(src.get(at..start)?);
        out.push_str(&text);
        at = end;
    }
    out.push_str(src.get(at..)?);
    Some(out)
}

/// `fennec fix --migrate --rust-strings <files>`: each Rust file's Fennec programs, migrated
/// one at a time in a scratch folder.
pub(crate) fn migrate_files(files: &[&str]) -> ExitCode {
    let scratch = std::env::temp_dir().join(format!("fennec-rust-strings-{}", std::process::id()));
    if std::fs::create_dir_all(&scratch).is_err() {
        eprintln!("error: couldn't make a scratch folder");
        return ExitCode::FAILURE;
    }
    let code = migrate_in(files, &scratch);
    // A scratch folder left behind only takes a little room.
    let _ = std::fs::remove_dir_all(&scratch);
    code
}

fn migrate_in(files: &[&str], scratch: &Path) -> ExitCode {
    let program = scratch.join("program.fen");
    for file in files {
        let Ok(src) = std::fs::read_to_string(file) else {
            eprintln!("error: couldn't read {file}");
            return ExitCode::FAILURE;
        };
        let mut migrate = |text: &str| -> Option<String> {
            std::fs::write(&program, text).ok()?;
            crate::fix::migrate_quietly(&program.to_string_lossy());
            std::fs::read_to_string(&program).ok()
        };
        if let Some(new) = migrated(&src, &mut migrate) {
            if std::fs::write(file, new).is_err() {
                eprintln!("error: couldn't write {file}");
                return ExitCode::FAILURE;
            }
            println!("fixed {file}");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `migrated` writes when each program's last line gets `return` and `else as` reads
    /// `except`, standing in for the real migration.
    fn by_hand(src: &str) -> String {
        let mut migrate = |text: &str| -> Option<String> {
            Some(text.replace("    1 + 1", "    return 1 + 1").replace("else as why:", "except why:"))
        };
        migrated(src, &mut migrate).unwrap_or_else(|| src.to_owned())
    }

    #[test]
    fn a_raw_string_keeps_its_hashes_quotes_and_indent() {
        let src =
            "let p = r#\"\n        def two() -> int:\n            1 + 1\n\n        x = \"q\"\n    \"#;\n";
        assert_eq!(by_hand(src), src.replace("            1 + 1", "            return 1 + 1"));
    }

    #[test]
    fn a_plain_string_keeps_its_escapes_and_continuations() {
        let src = "let p = \"def two() -> int:\\n    \\\n    1 + 1\\n\\ndef main(stdout: Stdout):\\n    stdout.print(\\\"a\\\\b\\\")\\n\";\n";
        let out = by_hand(src);
        assert!(out.contains("return 1 + 1\\n"), "{out}");
        assert!(out.contains("stdout.print(\\\"a\\\\b\\\")\\n\";"), "{out}");
        assert!(out.starts_with("let p = \"def two() -> int:\\n"), "{out}");
    }

    #[test]
    fn a_plain_string_over_several_lines_keeps_its_line_breaks() {
        let src = "let p = \"def two() -> int:\n    1 + 1\n\";\n";
        assert_eq!(by_hand(src), "let p = \"def two() -> int:\n    return 1 + 1\n\";\n");
    }

    #[test]
    fn a_line_that_goes_away_takes_its_break_with_it() {
        let src = "let p = r\"def f() -> int:\n    x = g() else as why:\n        match why:\n            case _:\n                return 0\n    return x\n\";\n";
        let mut migrate = |text: &str| -> Option<String> {
            Some(text.replace(
                "else as why:\n        match why:\n            case _:\n                return 0",
                "except why:\n        case _:\n            return 0",
            ))
        };
        let out = migrated(src, &mut migrate).unwrap_or_default();
        assert_eq!(
            out,
            "let p = r\"def f() -> int:\n    x = g() except why:\n        case _:\n            return 0\n    return x\n\";\n"
        );
    }

    #[test]
    fn chars_comments_and_byte_strings_are_passed_over() {
        let src =
            "let q = '\"'; // \"def x\"\nlet b = b\"def y\"; let s = \"def two() -> int:\\n    1 + 1\";\n";
        assert_eq!(by_hand(src), src.replace("    1 + 1\";", "    return 1 + 1\";"));
    }

    #[test]
    fn a_literal_with_nothing_to_migrate_is_left_as_it_is() {
        let src = "let p = r#\"def two() -> int:\n    return 2\n\"#;\n";
        assert_eq!(by_hand(src), src);
    }
}
