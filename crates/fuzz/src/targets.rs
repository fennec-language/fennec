//! What gets fuzzed: each target takes an input, and gives back what's wrong if something is.
//! A crash, a hang or running out of memory counts as well, without a target doing anything.

use std::path::Path;
use std::time::Duration;

use crate::supervise::STUCK;

/// A part of Fennec to fuzz.
pub(crate) struct Target {
    pub(crate) name: &'static str,
    /// Where to find inputs to start from, under the repository.
    corpus: &'static [&'static str],
    /// What the target's inputs are made of.
    pub(crate) tokens: &'static [&'static str],
    pub(crate) run: fn(&[u8]) -> Result<(), String>,
    /// How long one input may take before it counts as stuck.
    pub(crate) stuck: Duration,
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

/// What HTTP/1.1 requests give meaning to.
const HTTP: [&str; 16] = [
    "\r\n",
    "\n",
    "\r",
    " ",
    ":",
    "GET / HTTP/1.1\r\n",
    "HTTP/1.0",
    "Content-Length: ",
    "Transfer-Encoding: chunked\r\n",
    "Connection: close\r\n",
    "Host: a\r\n",
    "\t",
    "HEAD ",
    "?a=1&b=%20+",
    "99999999999999999999",
    "é🦊",
];

/// What a Postgres server's messages are made of: their type bytes, lengths and counts.
const POSTGRES: [&str; 12] = [
    "R",
    "T",
    "D",
    "Z",
    "E",
    "C",
    "\0",
    "\0\0\0\u{4}",
    "\0\0\0\u{8}",
    "\u{ff}\u{ff}",
    "\u{ff}\u{ff}\u{ff}\u{ff}",
    "\0\u{1}",
];

/// What URLs give meaning to.
const URL: [&str; 16] =
    ["://", "//", "/", ":", "@", "?", "#", "&", "=", "%", "%2F", "%ff", "+", "[::1]", ":65536", "é🦊"];

/// What CSV gives meaning to, and the separators the `csv` target splits rows at.
const CSV: [&str; 10] = [",", "\"", "\"\"", "\n", "\r", "\r\n", "\u{feff}", "\u{1e}", "\u{1f}", "é🦊"];

/// Bytes `std.pack` gives meaning to: its headers, small and large varints, and text.
const PACKED: [&str; 10] = [
    "FPK2",
    "FPKS",
    "\0",
    "\u{1}",
    "\u{7f}",
    "\u{ff}",
    "\u{80}\u{80}\u{80}\u{80}\u{1}",
    "\u{ff}\u{ff}\u{ff}\u{ff}\u{f}",
    "é",
    "Spot",
];

/// What a request's path gives meaning to: its segments, escapes, a query, and the routes' own
/// segments, so inputs reach past the first.
const PATHS: [&str; 14] =
    ["/", "//", "?", "%", "%2F", "%2f", "%ff", "%e9", "notes", "new", "tags", "files", "a", "é"];

/// What a page and a link give meaning to.
const PAGE: [&str; 16] = [
    "<",
    ">",
    "\"",
    "'",
    "&",
    "&#",
    "javascript:",
    "JavaScript:",
    "https:",
    "mailto:",
    ":",
    "/",
    "\t",
    "\n",
    "\u{1}",
    "</script>",
];

const ANYTHING: [&str; 4] = ["\0", "\u{7f}", "é", "🦊"];

/// What numbers and UUIDs are written with, and what's near enough to be mistaken for them:
/// signs, exponents, the ends of each size, separators, other bases, space that isn't ASCII,
/// and digits that aren't.
const NUMBERS: [&str; 24] = [
    "0",
    "9",
    "-",
    "+",
    ".",
    "e",
    "E-",
    "_",
    " ",
    "\t",
    "\n",
    "0x",
    "inf",
    "NaN",
    "127",
    "-128",
    "65535",
    "4294967296",
    "9223372036854775808",
    "18446744073709551616",
    "0f8fad5b-d9cb-469f-a165-70867728950e",
    "\u{a0}",
    "\u{663}",
    "é🦊",
];

/// What RFC 3339 times are written with, and the edges of what `Time` holds.
const TIMES: [&str; 20] = [
    "2026-11-06T21:55:56Z",
    "1678-01-01T00:00:00+01:00",
    "2261-12-31T23:59:59.999999999-23:59",
    "T",
    "t",
    " ",
    "-",
    ":",
    "+",
    ".",
    "Z",
    "z",
    "0",
    "9",
    "60",
    "02-29",
    "23:59",
    "123456789012",
    "\u{663}",
    "é🦊",
];

/// What base64 is written with, in both alphabets, and its padding.
const BASE64: [&str; 14] = ["=", "==", "+", "/", "-", "_", "A", "Q", "AA", "aGk", "\n", " ", "%", "é🦊"];

pub(crate) const TARGETS: [Target; 27] = [
    Target { name: "lexer", corpus: &PROGRAMS, tokens: &SYNTAX, run: lexer, stuck: STUCK },
    Target { name: "parser", corpus: &PROGRAMS, tokens: &SYNTAX, run: parser, stuck: STUCK },
    Target { name: "formatter", corpus: &PROGRAMS, tokens: &SYNTAX, run: formatter, stuck: STUCK },
    Target { name: "checker", corpus: &PROGRAMS, tokens: &SYNTAX, run: checker, stuck: STUCK },
    Target {
        name: "lengths",
        corpus: &["crates/fuzz/seeds/programs", "crates/fuzz/seeds/llvm"],
        tokens: &ANYTHING,
        run: lengths,
        stuck: STUCK,
    },
    Target {
        name: "ranges",
        corpus: &["crates/fuzz/seeds/programs", "crates/fuzz/seeds/llvm"],
        tokens: &ANYTHING,
        run: ranges,
        stuck: STUCK,
    },
    Target { name: "json", corpus: &["crates/fuzz/seeds/json"], tokens: &JSON, run: json, stuck: STUCK },
    Target {
        name: "json-rules",
        corpus: &["crates/fuzz/seeds/json"],
        tokens: &JSON,
        run: crate::json_rules::json_rules,
        stuck: STUCK,
    },
    Target {
        name: "csv",
        corpus: &["crates/fuzz/seeds/csv"],
        tokens: &CSV,
        run: crate::csv_rules::csv,
        stuck: STUCK,
    },
    Target {
        name: "pack",
        corpus: &["crates/fuzz/seeds/pack"],
        tokens: &PACKED,
        run: crate::pack_rules::pack,
        stuck: STUCK,
    },
    Target { name: "toml", corpus: &["crates/fuzz/seeds/toml"], tokens: &TOML, run: toml, stuck: STUCK },
    Target {
        name: "toml-rules",
        corpus: &["crates/fuzz/seeds/toml"],
        tokens: &TOML,
        run: crate::toml_rules::toml,
        stuck: STUCK,
    },
    Target {
        name: "http-server",
        corpus: &["crates/fuzz/seeds/http"],
        tokens: &HTTP,
        run: crate::http_rules::http_server,
        stuck: STUCK,
    },
    Target {
        name: "http-sessions",
        corpus: &["crates/fuzz/seeds/http-sessions"],
        tokens: &HTTP,
        run: crate::http_sessions::http_sessions,
        stuck: STUCK,
    },
    Target {
        name: "http-splits",
        corpus: &["crates/fuzz/seeds/http"],
        tokens: &HTTP,
        run: crate::http_sessions::http_splits,
        stuck: STUCK,
    },
    Target {
        name: "http-client",
        corpus: &["crates/fuzz/seeds/http-client"],
        tokens: &HTTP,
        run: crate::http_rules::http_client,
        stuck: STUCK,
    },
    Target {
        name: "http-client-scripts",
        corpus: &["crates/fuzz/seeds/http-client-scripts"],
        tokens: &HTTP,
        run: crate::http_client_scripts::http_client_scripts,
        stuck: STUCK,
    },
    Target {
        name: "router",
        corpus: &["crates/fuzz/seeds/router"],
        tokens: &PATHS,
        run: crate::router_rules::router,
        stuck: STUCK,
    },
    Target {
        name: "html",
        corpus: &["crates/fuzz/seeds/html"],
        tokens: &PAGE,
        run: crate::html_rules::html,
        stuck: STUCK,
    },
    Target {
        name: "postgres",
        corpus: &["crates/fuzz/seeds/postgres"],
        tokens: &POSTGRES,
        run: crate::postgres_rules::postgres,
        stuck: STUCK,
    },
    Target {
        name: "regex",
        corpus: &["crates/fuzz/seeds/regex"],
        tokens: &PATTERNS,
        run: regex,
        stuck: STUCK,
    },
    Target {
        name: "url",
        corpus: &["crates/fuzz/seeds/url"],
        tokens: &URL,
        run: crate::url_rules::url,
        stuck: STUCK,
    },
    Target {
        name: "runtime",
        corpus: &["crates/fuzz/seeds/abi"],
        tokens: &ANYTHING,
        run: crate::engines::runtime,
        stuck: STUCK,
    },
    Target {
        name: "numbers",
        corpus: &["crates/fuzz/seeds/numbers"],
        tokens: &NUMBERS,
        run: crate::numbers_rules::numbers,
        stuck: STUCK,
    },
    Target {
        name: "time",
        corpus: &["crates/fuzz/seeds/time"],
        tokens: &TIMES,
        run: crate::time_rules::time,
        stuck: STUCK,
    },
    Target {
        name: "base64",
        corpus: &["crates/fuzz/seeds/base64"],
        tokens: &BASE64,
        run: crate::base64_rules::base64,
        stuck: STUCK,
    },
    Target {
        name: "parked",
        corpus: &["crates/fuzz/seeds/parked"],
        tokens: &ANYTHING,
        run: crate::parked::parked,
        stuck: STUCK,
    },
];

/// Programs built through LLVM, a dev build against a `--release` build and what's pinned, which
/// runs only when named, since it builds every input twice.
pub(crate) const LLVM: Target = Target {
    name: "llvm",
    corpus: &["crates/fuzz/seeds/llvm"],
    tokens: &ANYTHING,
    run: crate::llvm::llvm,
    // It runs `opt`, `llc` and the linker for every input, which a busy machine slows down.
    stuck: Duration::from_secs(60),
};

/// The targets, besides `llvm`, that run their input through a program built natively.
pub(crate) const NATIVE: [&str; 19] = [
    "base64",
    "csv",
    "html",
    "http-client",
    "http-client-scripts",
    "http-server",
    "http-sessions",
    "http-splits",
    "json",
    "json-rules",
    "numbers",
    "pack",
    "postgres",
    "router",
    "time",
    "toml",
    "toml-rules",
    "url",
    "utf8",
];

/// The UTF-8 reader is fuzzed with the bytes it's given, so it has its own way to be run.
pub(crate) const UTF8: Target =
    Target { name: "utf8", corpus: &["crates/fuzz/seeds/utf8"], tokens: &BYTES, run: utf8, stuck: STUCK };

/// Random, well-typed programs, each built both ways and run by the reference evaluator, which
/// runs only when named, since it builds every input twice.
pub(crate) const RANDOM_PROGRAMS: Target = Target {
    name: "programs",
    corpus: &["crates/fuzz/seeds/programs"],
    tokens: &ANYTHING,
    run: crate::programs::programs,
    stuck: Duration::from_secs(60),
};

/// Tasks and a server's rounds that print and make way, wait with timeouts, take jobs from one
/// queue, send into one channel from several tasks, run `parallel` calls cut short, take locks and
/// deadlock, recorded on 1 and 4 threads and each recording replayed on 1, 4 and 16, which runs only
/// when named, since it builds every input once and runs it 8 times: 2 recordings and 6 replays.
pub(crate) const REPLAY: Target = Target {
    name: "replay",
    corpus: &["crates/fuzz/seeds/replay"],
    tokens: &ANYTHING,
    run: crate::replay_rules::replay,
    stuck: Duration::from_secs(60),
};

/// A program putting a new file in place of an old one, killed partway, which must leave the old
/// file or the new one, never a torn one.
pub(crate) const KILLED_WRITES: Target = Target {
    name: "killed-writes",
    corpus: &["crates/fuzz/seeds/killed-writes"],
    tokens: &ANYTHING,
    run: crate::killed_writes::killed_writes,
    stuck: Duration::from_secs(60),
};

/// A program writing files whole and through `with` blocks while the runtime's fault hook fails
/// its work on the disk partway, which must leave what the model says, never a torn file. It runs
/// only when named, and needs a runtime built with the `faults` feature (`FENNEC_FAULTED`).
pub(crate) const FAULT_WRITES: Target = Target {
    name: "fault-writes",
    corpus: &["crates/fuzz/seeds/fault-writes"],
    tokens: &ANYTHING,
    run: crate::fault_writes::fault_writes,
    stuck: Duration::from_secs(120),
};

/// A program edited a piece at a time, each version checked and built reusing what the one before
/// it kept, which has to make what checking and building it from nothing makes.
pub(crate) const EDITS: Target = Target {
    name: "edits",
    corpus: &["crates/fuzz/seeds/edits"],
    tokens: &ANYTHING,
    run: crate::edits::edits,
    stuck: Duration::from_secs(120),
};

pub(crate) fn find(name: &str) -> Option<&'static Target> {
    TARGETS
        .iter()
        .chain([&UTF8, &LLVM, &RANDOM_PROGRAMS, &REPLAY, &KILLED_WRITES, &FAULT_WRITES, &EDITS])
        .find(|target| target.name == name)
}

impl Target {
    /// The files to start from, in an order that doesn't depend on the machine.
    pub(crate) fn corpus(&self, root: &Path) -> Vec<Vec<u8>> {
        let mut paths = Vec::new();
        for folder in self.corpus {
            for entry in std::fs::read_dir(root.join(folder)).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_file() && !crate::llvm::is_pinned_output(&path) {
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

/// The parser, and what it parses comes back from its bytes as it was.
fn parser(input: &[u8]) -> Result<(), String> {
    let Ok(module) = fennec_syntax::parse(&text(input)) else { return Ok(()) };
    comes_back("the module", &module)
}

/// `value` written as bytes reads back the same, and writes the same bytes again: what a build
/// sends to another process is what that process builds from.
fn comes_back<T>(what: &str, value: &T) -> Result<(), String>
where
    T: fennec_syntax::codec::Encode + fennec_syntax::codec::Decode + PartialEq,
{
    use fennec_syntax::codec::{Reader, Writer};
    let mut to = Writer::default();
    value.encode(&mut to);
    let bytes = to.into_bytes();
    let mut from = Reader::new(&bytes);
    let Some(back) = T::decode(&mut from) else { return Err(format!("{what} didn't read back")) };
    if !from.is_done() {
        return Err(format!("{what} read back with bytes left over"));
    }
    if back != *value {
        return Err(format!("{what} read back differently"));
    }
    let mut again = Writer::default();
    back.encode(&mut again);
    if again.into_bytes() != bytes {
        return Err(format!("{what} read back wrote other bytes"));
    }
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
                let one = fennec_syntax::Module {
                    items: vec![fennec_syntax::Item::Import(import)],
                    ..Default::default()
                };
                Some(fennec_fmt::without_spans(&one))
            }
            _ => None,
        })
        .collect();
    imports.sort();
    module.items = others;
    format!("{imports:?}\n{}", fennec_fmt::without_spans(&module))
}

/// The checker, what it finds coming back from its bytes as it was, and what's lowered from
/// what it passes.
fn checker(input: &[u8]) -> Result<(), String> {
    let Ok(module) = fennec_syntax::parse(&text(input)) else { return Ok(()) };
    let Ok(signatures) = fennec_check::check(&module) else { return Ok(()) };
    comes_back("what the check found", &signatures)?;
    let program = fennec_ir::lower(&module, &signatures);
    fennec_ir::validate(&program).map_err(|problems| format!("the IR isn't valid: {problems:?}"))?;
    let _ = fennec_ir::costs(&program);
    let _ = fennec_build::test_keys(&module);
    Ok(())
}

/// What's known of list lengths across calls holds while a program runs in the IR's evaluator:
/// each fact is checked on the way into each function and out of it. The program is one of the
/// two kinds the bytes can write, by their length: a random well-typed one, or one the `llvm`
/// target builds.
fn lengths(input: &[u8]) -> Result<(), String> {
    let source = if input.len().is_multiple_of(2) {
        crate::programs::written(input)
    } else {
        crate::generate::program(input)
    };
    let Ok(module) = fennec_syntax::parse(&source) else { return Ok(()) };
    let Ok(signatures) = fennec_check::check(&module) else { return Ok(()) };
    let program = fennec_ir::lower(&module, &signatures);
    let lengths = fennec_ir::lengths::Lengths::of(&program);
    if lengths.params().next().is_none() {
        return Ok(());
    }
    let ran = fennec_ir::eval::check_lengths(&program, &lengths, LENGTH_STEPS);
    match ran.broken {
        Some(broken) => Err(format!("{broken}\n--- program\n{source}")),
        None => Ok(()),
    }
}

/// How far the `lengths` target runs each program.
const LENGTH_STEPS: u64 = 100_000;

/// What's known of ints' and floats' values holds while a program runs in the IR's evaluator:
/// each value written and each parameter given holds what's known of it, and each `//`, `%`, `*`
/// and list index said not negative, `int(x)` said to fit and comparison said not NaN has
/// operands that are. The program is one of the two kinds the bytes can write, as for `lengths`.
fn ranges(input: &[u8]) -> Result<(), String> {
    let source = if input.len().is_multiple_of(2) {
        crate::programs::written(input)
    } else {
        crate::generate::program(input)
    };
    let Ok(module) = fennec_syntax::parse(&source) else { return Ok(()) };
    let Ok(signatures) = fennec_check::check(&module) else { return Ok(()) };
    let program = fennec_ir::lower(&module, &signatures);
    let ranges = fennec_ir::ranges::Ranges::of(&program);
    let ran = fennec_ir::eval::check_ranges(&program, &ranges, LENGTH_STEPS);
    match ran.broken {
        Some(broken) => Err(format!("{broken}\n--- program\n{source}")),
        None => Ok(()),
    }
}

/// The standard library's JSON reader, built natively, on any text.
fn json(input: &[u8]) -> Result<(), String> {
    crate::llvm::read_natively("json", text(input).as_bytes()).map(|_| ())
}

/// The standard library's TOML reader, built natively, on any text.
fn toml(input: &[u8]) -> Result<(), String> {
    crate::llvm::read_natively("toml", text(input).as_bytes()).map(|_| ())
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

/// `bytes.decode()` on the input's bytes, built natively, which must agree with what the
/// language promises: it reads valid UTF-8 and fails on anything else.
fn utf8(input: &[u8]) -> Result<(), String> {
    let input = input.get(..256).unwrap_or(input);
    let Some(printed) = crate::llvm::read_natively("utf8", input)? else { return Ok(()) };
    let read = printed.starts_with(b"Ok(");
    if read != std::str::from_utf8(input).is_ok() {
        let said = if read { "read" } else { "refused" };
        return Err(format!("`decode` {said} {input:?}: {}", String::from_utf8_lossy(&printed)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LLVM, TARGETS, UTF8, find};

    #[test]
    fn every_target_can_be_found_by_name() {
        for target in TARGETS.iter().chain([&UTF8, &LLVM]) {
            assert_eq!(find(target.name).map(|found| found.name), Some(target.name));
        }
        assert!(find("no such target").is_none());
    }

    #[test]
    fn a_program_that_can_be_formatted_passes() {
        let target = find("formatter").unwrap();
        assert_eq!((target.run)(b"def f() -> int:\n    1\n"), Ok(()));
        assert_eq!((target.run)(b"not a program ((("), Ok(()));
    }
}
