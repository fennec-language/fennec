//! What gets fuzzed: each target takes an input, and gives back what's wrong if something is.
//! A crash, a hang or running out of memory counts as well, without a target doing anything.

use std::fmt::Write as _;
use std::path::Path;

/// A part of Fennec to fuzz.
pub(crate) struct Target {
    pub(crate) name: &'static str,
    /// Where to find inputs to start from, under the repository.
    corpus: &'static [&'static str],
    /// What the target's inputs are made of.
    pub(crate) tokens: &'static [&'static str],
    pub(crate) run: fn(&[u8]) -> Result<(), String>,
}

const PROGRAMS: [&str; 5] =
    ["examples", "examples/errors", "bench/kernels", "selfhost/src", "crates/syntax/src/stdlib"];

const SYNTAX: [&str; 40] = [
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ":",
    ",",
    ".",
    "?",
    "\n",
    "\n    ",
    "\t",
    "\r\n",
    "\"",
    "\\",
    "f\"{",
    "}\"",
    "r\"",
    "def ",
    "struct ",
    "enum ",
    "match ",
    "case ",
    "if ",
    "for ",
    "while ",
    "with ",
    "test \"",
    "bench \"",
    "=> ",
    "-> ",
    "mut ",
    "from ",
    "import ",
    "not ",
    "- ",
    "#",
    "é🦊",
    "99999999999999999999",
];

const JSON: [&str; 16] =
    ["{", "}", "[", "]", ":", ",", "\"", "\\", "\\u", "\\ud83e", "null", "true", "-", "1e999", "0.", "é🦊"];

const TOML: [&str; 17] = [
    "[",
    "]",
    "[[",
    "]]",
    "=",
    ".",
    "\"",
    "'''",
    "\"\"\"",
    "\\",
    "\\u",
    "#",
    "\n",
    "1979-05-27",
    "0x",
    "inf",
    "é🦊",
];

const PATTERNS: [&str; 20] = [
    "(", ")", "(?:", "[", "]", "{", "}", "{1000}", "*", "+", "?", "|", "^", "$", "\\", "\\d", "\\p", ".",
    "a", "é🦊",
];

const BYTES: [&str; 8] = ["\u{7f}", "é", "🦊", "\u{800}", "\u{10ffff}", "\0", "a", "\u{fffd}"];

const ANYTHING: [&str; 4] = ["\0", "\u{7f}", "é", "🦊"];

pub(crate) const TARGETS: [Target; 9] = [
    Target { name: "lexer", corpus: &PROGRAMS, tokens: &SYNTAX, run: lexer },
    Target { name: "parser", corpus: &PROGRAMS, tokens: &SYNTAX, run: parser },
    Target { name: "formatter", corpus: &PROGRAMS, tokens: &SYNTAX, run: formatter },
    Target { name: "checker", corpus: &PROGRAMS, tokens: &SYNTAX, run: checker },
    Target { name: "json", corpus: &["crates/fuzz/seeds/json"], tokens: &JSON, run: json },
    Target { name: "toml", corpus: &["crates/fuzz/seeds/toml"], tokens: &TOML, run: toml },
    Target { name: "regex", corpus: &["crates/fuzz/seeds/regex"], tokens: &PATTERNS, run: regex },
    Target { name: "jit", corpus: &["crates/fuzz/seeds/jit"], tokens: &ANYTHING, run: crate::engines::jit },
    Target {
        name: "runtime",
        corpus: &["crates/fuzz/seeds/abi"],
        tokens: &ANYTHING,
        run: crate::engines::runtime,
    },
];

/// The UTF-8 reader is fuzzed with the bytes it's given, so it has its own way to be run.
pub(crate) const UTF8: Target =
    Target { name: "utf8", corpus: &["crates/fuzz/seeds/utf8"], tokens: &BYTES, run: utf8 };

pub(crate) fn find(name: &str) -> Option<&'static Target> {
    TARGETS.iter().chain(std::iter::once(&UTF8)).find(|target| target.name == name)
}

impl Target {
    /// The files to start from, in an order that doesn't depend on the machine.
    pub(crate) fn corpus(&self, root: &Path) -> Vec<Vec<u8>> {
        let mut paths = Vec::new();
        for folder in self.corpus {
            for entry in std::fs::read_dir(root.join(folder)).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_none_or(|extension| extension != "out") {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        paths.iter().filter_map(|path| std::fs::read(path).ok()).collect()
    }
}

fn text(input: &[u8]) -> String {
    String::from_utf8_lossy(input).into_owned()
}

#[expect(clippy::unnecessary_wraps, reason = "every target has the same signature")]
fn lexer(input: &[u8]) -> Result<(), String> {
    let _ = fennec_syntax::lex_source(&text(input));
    Ok(())
}

#[expect(clippy::unnecessary_wraps, reason = "every target has the same signature")]
fn parser(input: &[u8]) -> Result<(), String> {
    let _ = fennec_syntax::parse(&text(input));
    Ok(())
}

/// Formatting keeps a program's meaning, and doing it twice changes nothing.
fn formatter(input: &[u8]) -> Result<(), String> {
    let source = text(input);
    let Ok(before) = fennec_syntax::parse(&source) else { return Ok(()) };
    let Ok(formatted) = fennec_fmt::format(&source) else {
        return Err("a program that parses didn't format".to_owned());
    };
    let after =
        fennec_syntax::parse(&formatted).map_err(|err| format!("formatted, it doesn't parse: {err:?}"))?;
    if meaning(before) != meaning(after) {
        return Err("formatting changed what the program means".to_owned());
    }
    if fennec_fmt::format(&formatted).as_ref() != Ok(&formatted) {
        return Err("formatting twice changed something".to_owned());
    }
    Ok(())
}

/// A module's tree without positions, in a form that formatting leaves alone. Formatting sorts
/// imports, and their order means nothing.
fn meaning(mut module: fennec_syntax::Module) -> String {
    let (imports, others): (Vec<_>, Vec<_>) =
        module.items.drain(..).partition(|item| matches!(item, fennec_syntax::Item::Import(_)));
    let mut imports: Vec<String> = imports
        .into_iter()
        .filter_map(|item| match item {
            fennec_syntax::Item::Import(mut import) => {
                let mut bound: Vec<_> = import.names.drain(..).zip(import.aliases.drain(..)).collect();
                bound.sort();
                (import.names, import.aliases) = bound.into_iter().unzip();
                let one = fennec_syntax::Module { items: vec![fennec_syntax::Item::Import(import)] };
                Some(fennec_fmt::without_spans(&one))
            }
            _ => None,
        })
        .collect();
    imports.sort();
    module.items = others;
    format!("{imports:?}\n{}", fennec_fmt::without_spans(&module))
}

/// The checker, and what's lowered from what it passes.
fn checker(input: &[u8]) -> Result<(), String> {
    let Ok(module) = fennec_syntax::parse(&text(input)) else { return Ok(()) };
    let Ok(signatures) = fennec_check::check(&module) else { return Ok(()) };
    let program = fennec_ir::lower(&module, &signatures);
    fennec_ir::validate(&program).map_err(|problems| format!("the IR isn't valid: {problems:?}"))?;
    let _ = fennec_ir::costs(&program);
    let _ = fennec_build::test_keys(&module);
    Ok(())
}

/// A string literal holding `text`, whatever's in it.
fn literal(text: &str) -> String {
    let mut written = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => written.push_str("\\\""),
            '\\' => written.push_str("\\\\"),
            '{' => written.push_str("\\{"),
            '}' => written.push_str("\\}"),
            ch if u32::from(ch) < 0x20 || ch == '\u{7f}' => {
                let _ = write!(written, "\\x{:02x}", u32::from(ch));
            }
            ch => written.push(ch),
        }
    }
    written.push('"');
    written
}

/// Runs a program in the interpreter, where failing is fine and only breaking isn't.
fn interpreted(source: &str) -> Result<(), String> {
    let module =
        fennec_syntax::parse(source).map_err(|err| format!("the harness wrote bad code: {err:?}"))?;
    let _ = fennec_interp::run(&module, &mut std::io::sink());
    Ok(())
}

fn json(input: &[u8]) -> Result<(), String> {
    let source = format!(
        "from std.json import loads\n\ndef main(stdout: Stdout):\n    stdout.print(loads({}))\n",
        literal(&text(input))
    );
    interpreted(&source)
}

fn toml(input: &[u8]) -> Result<(), String> {
    let source = format!(
        "from std.toml import loads\n\ndef main(stdout: Stdout):\n    stdout.print(loads({}))\n",
        literal(&text(input))
    );
    interpreted(&source)
}

/// A regular expression against a text. The first line of the input is the pattern.
#[expect(clippy::unnecessary_wraps, reason = "every target has the same signature")]
fn regex(input: &[u8]) -> Result<(), String> {
    let whole = text(input);
    let (pattern, subject) = whole.split_once('\n').unwrap_or((&whole, "aaaa bbbb é🦊 1234"));
    let Ok(regex) = fennec_re::Regex::new(pattern) else { return Ok(()) };
    let _ = regex.find_all(subject);
    Ok(())
}

/// `bytes.decode()` on the input's bytes, which must agree with what the language promises:
/// it reads valid UTF-8 and fails on anything else.
fn utf8(input: &[u8]) -> Result<(), String> {
    let items: Vec<String> = input.iter().take(256).map(u8::to_string).collect();
    let source =
        format!("def main(stdout: Stdout):\n    stdout.print(bytes([{}]).decode())\n", items.join(", "));
    interpreted(&source)
}

#[cfg(test)]
mod tests {
    use super::{TARGETS, UTF8, find, literal};

    #[test]
    fn every_target_can_be_found_by_name() {
        for target in TARGETS.iter().chain([&UTF8]) {
            assert_eq!(find(target.name).map(|found| found.name), Some(target.name));
        }
        assert!(find("no such target").is_none());
    }

    #[test]
    fn a_literal_holds_anything_and_parses() {
        for text in ["plain", "\"quoted\"", "back\\slash", "{braces}", "line\nbreak", "\0\u{7f}é🦊"] {
            let source = format!("x = {}\n", literal(text));
            assert!(fennec_syntax::parse(&source).is_ok(), "{text:?}");
        }
    }

    #[test]
    fn a_program_that_can_be_formatted_passes() {
        let target = find("formatter").unwrap();
        assert_eq!((target.run)(b"def f() -> int:\n    1\n"), Ok(()));
        assert_eq!((target.run)(b"not a program ((("), Ok(()));
    }
}
