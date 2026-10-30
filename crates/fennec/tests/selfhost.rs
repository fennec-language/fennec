//! The self-hosted front end in `selfhost/` lexes and parses every example, every error
//! example, the standard library's Fennec sources and its own source exactly as the Rust front
//! end does, with the interpreter and with the JIT.

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
    (
        "float_forms",
        "x = 1.5e3 + 2E-2 + 1e9_9 + 3.0 + 1e20 + 1.25 + 0.00001 + 1e-4 + 1e16 + 1e999\ny = 1.x\n",
    ),
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

/// Small sources that reach the parser's error paths, and one that uses every construct.
const PARSE_CASES: &[(&str, &str)] = &[
    ("top_level_statement", "x = 1\n"),
    ("noalloc_struct", "noalloc struct A:\n    x: int\n"),
    ("missing_block", "def f():\nx\n"),
    ("needs_else", "def f():\n    a if b\n"),
    ("field_needs_type", "struct A:\n    x\n"),
    ("param_needs_type", "def f(x):\n    pass\n"),
    ("mut_needs_name", "def f():\n    mut a.b = 1\n"),
    ("not_assignable", "def f():\n    f() = 1\n"),
    ("tuple_augmented", "def f():\n    a, b += 1\n"),
    ("optional_attr_assigned", "def f():\n    a?.b = 1\n"),
    ("try_block", "def f():\n    try:\n        pass\n"),
    ("try_as_name", "def f():\n    try = 1\n    raise.x\n"),
    ("reserved_param", "def f(extern: int):\n    pass\n"),
    ("reserved_value", "def f():\n    x = unsafe\n"),
    ("keyword_value", "def f():\n    x = while\n"),
    ("expected_case", "def f():\n    match x:\n        y\n"),
    ("bad_pattern", "def f():\n    match x:\n        case +: 1\n"),
    ("bad_negative_pattern", "def f():\n    match x:\n        case -a: 1\n"),
    ("keyword_pattern", "def f():\n    match x:\n        case if: 1\n"),
    ("empty_fstring_code", "def f():\n    x = f\"{ }\"\n"),
    ("fstring_two_values", "def f():\n    x = f\"{a b}\"\n"),
    ("fstring_lex_error", "def f():\n    x = f\"{a $}\"\n"),
    ("empty_index", "def f():\n    x[]\n"),
    ("loop_needs_in", "def f():\n    for a b:\n        pass\n"),
    ("from_needs_import", "from std.math Vec3\n"),
    ("bad_function_name", "def (x: int):\n    pass\n"),
    ("bad_index_def", "struct A:\n    def [x](self):\n        pass\n"),
    ("unclosed_call", "def f():\n    g(1, 2\n"),
    ("line_goes_on", "def f():\n    x = 1 2\n"),
    ("dedent_at_top", "def f():\n        x\n    y\n"),
    (
        "everything",
        "import std.math
from std.game import Id, IdMap
export type Grid = list[list[int?]]
type Pair = (int, str)

interface Shape:
    def area(self) -> float
    pass

struct Box[T, U: Shape]:
    width = 1.0 where width > 0.0
    items: list[T] = []
    pass

    def +(self, other: Box) -> Box:
        self
    def [](self, at: int):
        self.items[at]
    def []=(mut self, at: int, value: T):
        self.items[at] = value
    def ==(self, other: Box):
        True
    def //(self, n: int):
        n

enum Shape2:
    Circle(radius: float = 1.0)
    Point
    pass

error Oops:
    reason: str

noalloc def step(mut world: World, dt: float) -> (int, str)?:
    for mut enemy in world.enemies:
        enemy.x += enemy.v * dt
    for a, b in pairs: pass
    while not done and (x < y <= z or x is not None):
        x -= 1
        x *= 2
        x /= 3
        break
    if a: b
    elif c: d
    else: e
    with arena() as mut scratch:
        continue
    with lock():
        pass
    def inner(x: int) -> int:
        return x ** 2 ** -3
    y = [x for x in xs if x > 0]
    s = {x for x in xs}
    d = {k: v for k, v in items}
    e = {}
    f = {1, 2,}
    g = {1: 2, 3: 4,}
    h = (1,)
    i = ()
    j = sum(x * x for x in xs)
    k = (items[1:], items[:2], items[:], items[1:2], items[-1])
    l = a?.b?.c else 0
    m = load(path)? from Context(path)
    n = value? if ok else - - 1
    o = x => x + 1
    p = (a, b) => a * b
    q = () => 0
    r = f(mut x, y=2) if False else g(-1 % 2 // 3)
    t = 3 in xs and 4 not in ys or not True
    u = .Circle(radius=2.0).area()
    match = 1
    v = f(match) + match.x + (match, match)[0]
    w = match shape:
        case .Circle(r): r
        case Rect(w, h): w * h
        case (1, -2, -3.5, \"s\", True, False, None, _): 0
        case Ok(value):
            value
        case name: name
    z = [
        1,
        2,
    ]
    assert z == [1, 2]
    yield 1
    fail Oops(\"no\")
    return

test \"boxes add\":
    assert Box() + Box() == Box()

test \"any list\" for items: list[int], n: int:
    assert len(items) >= 0
",
    ),
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
        .chain(LEX_CASES.iter().map(|(name, source)| (format!("lex_{name}"), *source)))
        .chain(PARSE_CASES.iter().map(|(name, source)| (format!("parse_{name}"), *source)));
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

#[test]
fn selfhost_parser_matches_the_rust_parser() {
    if let Err(difference) = compare("tree", fennec_syntax::dump::tree) {
        panic!("{difference}");
    }
}
