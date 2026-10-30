//! The self-hosted front end in `selfhost/` lexes every example, every error example, the
//! standard library's Fennec sources and its own source exactly as the Rust front end does,
//! with the interpreter and with the JIT.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Small sources that reach the lexer's error paths and edge cases.
const LEX_CASES: &[(&str, &str)] = &[
    ("unterminated", "def main(stdout: Stdout):\n    x = \"abc\n"),
    ("unterminated_at_end", "x = \"abc"),
    ("unterminated_escape", "x = \"abc\\"),
    ("bad_escape", "x = \"a\\qb\"\n"),
    ("fstring_unterminated", "x = f\"{name\"\n"),
    ("fstring_open_brace", "x = f\"{name\n"),
    ("fstring_lone_close", "x = f\"a}b\"\n"),
    ("fstring_parts", "x = f\"{{a}} {b=} {c:>8.2f} {d[1]:,} {f(x)[0]} \\t\\{ {g({1: 2})} {h:}\"\n"),
    ("fstring_quote_inside", "x = f\"{f(\\\"x\\\")}\"\n"),
    ("fstring_empty", "x = f\"\"\ny = f\"{}\"\n"),
    ("unexpected", "x = 1 ! 2\n"),
    ("unexpected_dollar", "x = $\n"),
    ("unexpected_wide", "x = é\n"),
    ("wide_then_error", "# 🦊 and €\ns = \"héllo\"\nt = f\"é{s}\"\nu = $\n"),
    ("int_too_big", "x = 99999999999999999999\n"),
    ("int_largest", "x = 9_223_372_036_854_775_807\ny = 1_000\n"),
    ("float_forms", "x = 1.5e3 + 2E-2 + 1e9_9 + 3.0 + 1e20 + 1.25\ny = 1.x\n"),
    ("float_bad", "x = 1e\n"),
    ("float_bad_sign", "x = 2e+_\n"),
    ("dedent_mismatch", "if x:\n        a\n    b\n"),
    ("tab_inside", "x =\t1\n"),
    ("tab_in_indent", "if x:\n  \ty\n"),
    ("crlf", "a = 1\r\nif a:\r\n    b = 2\r\n"),
    ("blank_and_comment_lines", "def f():\n    a\n\n      # a comment\n  \n    b\n# end\n"),
    ("no_final_newline", "def f():\n    if x:\n        y"),
    ("brackets", "x = [\n    1,\n        2,\n]\ny = (a\n)\n"),
    ("closing_too_many", "x = )]\ny\n"),
    ("operators", "a -> b => c == d != e <= f >= g // h ** i += j -= k *= l /= m ? n . o % p\n"),
    (
        "keywords",
        "def if elif else while for in return break continue pass mut and or not is\nTrue False None fail yield with as import from export noalloc assert extern unsafe match case\n",
    ),
    ("empty", ""),
    ("only_comments", "# nothing\n   # here\n"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sources_in(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Every file the two front ends are compared on. Sources that don't live in files, like the
/// standard library's, are written into `work`.
fn corpus(work: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = sources_in(&repo().join("examples"))?;
    paths.extend(sources_in(&repo().join("examples/errors"))?);
    paths.extend(sources_in(&repo().join("selfhost/src"))?);
    let generated = fennec_syntax::PRELUDE
        .iter()
        .enumerate()
        .map(|(at, source)| (format!("prelude_{at}"), *source))
        .chain(LEX_CASES.iter().map(|(name, source)| (format!("lex_{name}"), *source)));
    for (name, source) in generated {
        let path = work.join(format!("{name}.fen"));
        fs::write(&path, source)?;
        paths.push(path);
    }
    Ok(paths)
}

/// Runs the self-hosted front end on every file in `corpus` with `mode`, returning what it
/// printed. It reads its work from `inputs.txt` in the folder it runs in.
fn selfhost(work: &Path, mode: &str, corpus: &[PathBuf], engine: &[&str]) -> Result<String, String> {
    let mut inputs = String::new();
    for path in corpus {
        let _ = writeln!(inputs, "{mode} {}", path.display());
    }
    fs::write(work.join("inputs.txt"), inputs).map_err(|err| err.to_string())?;
    let output = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("run")
        .args(engine)
        .arg(repo().join("selfhost"))
        .current_dir(work)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "the self-hosted front end failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| err.to_string())
}

/// Splits the self-hosted output into one dump per file, in order.
fn dumps(output: &str) -> Vec<(String, String)> {
    let mut dumps: Vec<(String, String)> = Vec::new();
    for line in output.split_inclusive('\n') {
        if let Some(path) = line.strip_prefix("== ") {
            dumps.push((path.trim_end().to_owned(), String::new()));
        } else if let Some((_, dump)) = dumps.last_mut() {
            dump.push_str(line);
        }
    }
    dumps
}

/// Runs the self-hosted front end over the corpus with both engines, and checks each file's
/// dump against `rust_dump`. Returns the first difference.
fn compare(mode: &str, rust_dump: fn(&str) -> String) -> Result<(), String> {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("selfhost-{mode}"));
    fs::create_dir_all(&work).map_err(|err| err.to_string())?;
    let corpus = corpus(&work).map_err(|err| err.to_string())?;
    let interpreted = selfhost(&work, mode, &corpus, &[])?;
    let dumps = dumps(&interpreted);
    if dumps.len() != corpus.len() {
        return Err(format!("the self-hosted front end dumped {} of {} files", dumps.len(), corpus.len()));
    }
    for (path, (shown, dump)) in corpus.iter().zip(&dumps) {
        let source = fs::read_to_string(path).map_err(|err| err.to_string())?;
        let expected = rust_dump(&source);
        if *dump != expected {
            let line = dump.lines().zip(expected.lines()).position(|(a, b)| a != b).map_or(0, |at| at + 1);
            return Err(format!(
                "{mode} differ for {shown} at line {line}\nself-hosted:\n{dump}\nrust:\n{expected}"
            ));
        }
    }
    let native = selfhost(&work, mode, &corpus, &["--jit"])?;
    if native != interpreted {
        return Err(format!("the JIT and the interpreter disagree on the self-hosted {mode}"));
    }
    Ok(())
}

#[test]
fn selfhost_lexer_matches_the_rust_lexer() {
    if let Err(difference) = compare("tokens", fennec_syntax::dump::tokens) {
        panic!("{difference}");
    }
}
