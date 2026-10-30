//! The self-hosted front end in `selfhost/` lexes and parses every example, every error
//! example, the standard library's Fennec sources and its own source exactly as the Rust front
//! end does, with the interpreter and with the JIT.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Small sources that reach the lexer's error paths and edge cases.
const LEX_CASES: &[(&str, &str)] = &[
    ("unterminated", "def main(stdout: Stdout):\n    x = \"abc\n"),
    ("unterminated_at_end", "x = \"abc"),
    ("unterminated_escape", "x = \"abc\\"),
    ("bad_escape", "x = \"a\\qb\"\n"),
    ("escapes", "x = \"\\0\\r\\t\\n\\\\\\\"\\{\\}\"\n"),
    ("code_escapes", "x = \"\\x41\\x4a\\u00e9\\U0001F98A\" + f\"\\x21{a}\"\n"),
    ("code_escape_short", "x = \"\\u12\"\n"),
    ("code_escape_not_hex", "x = \"\\xg1\"\n"),
    ("code_escape_surrogate", "x = \"\\ud800\"\n"),
    ("code_escape_too_big", "x = \"\\U00110000\"\n"),
    ("raw_strings", "x = r\"\\d+ \\n {}\" + r\"\" + rx + r\n"),
    ("raw_unterminated", "x = r\"abc\\\n"),
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
    ("delete_character", "# a \u{7f} in a comment\ns = \"\u{7f}\u{80}\u{7ff}\u{800}\"\nu = $\n"),
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
        "generics_and_typed_names",
        "def first[T, U: Shape](items: list[T]) -> T:\n    empty: list[int] = []\n    mut n: int = 0\n    x = loads[dict[str, int]]\n    y = grid[1, 2,]\n",
    ),
    ("generic_struct", "struct Box[T: Ordered & Hashable]:\n    item: T\n"),
    ("function_types", "def apply(f: (int, str) -> bool, g: () -> int?):\n    pass\n"),
    ("typed_attr", "def f():\n    a.b: int = 1\n"),
    ("typed_needs_value", "def f():\n    x: int\n"),
    ("fstring_leading_space", "def f():\n    x = f\"{ a} {\u{a0}b} { c:>3}\"\n"),
    ("lambda_trailing_comma", "def f():\n    g = (a, b,) => a\n    h = (a,) => a\n    i = () => 1\n"),
    ("lambda_lone_comma", "def f():\n    i = (,) => 1\n"),
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
        case (1, -2, -3.5, -1e999, -0.0, \"s\", True, False, None, _): 0
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

/// Small programs that reach the checker's errors about written types, and the places
/// types are written.
const CHECK_CASES: &[(&str, &str)] = &[
    (
        "unknown_types",
        "def f(a: lsit[int], b: String, c: f64, d: i32, e: boolean, g: List[int], h: Optional) -> Strng:\n    pass\n",
    ),
    (
        "bracket_counts",
        "def f(a: dict[int], b: int[str], c: Result[int], d: list, e: (int, str) -> bool):\n    pass\n",
    ),
    ("alias_loop", "type Loop = Loop\ntype Pair = (int, Loop)\n\ndef f(p: Pair):\n    pass\n"),
    (
        "bounds",
        "interface Shape:\n    def area(self) -> float\n\nstruct Enemy:\n    hp = 1\n\nstruct Bag[T: Enemy]:\n    items: list[T]\n\ndef f[T: Shape & Eq, U: Enemy](a: T, b: U, c: list[Shape]) -> T:\n    a\n",
    ),
    (
        "generic_scopes",
        "struct Box[T]:\n    item: T\n    other: U\n\n    def get(self) -> T:\n        self.item\n\ndef outer[T](x: T) -> T:\n    def inner(y: Nope) -> T:\n        y\n    names: list[T] = []\n    x\n",
    ),
    (
        "written_places",
        "from std import collections\nimport std.json\n\nerror Oops:\n    why: Reason\n\nstruct S:\n    q: collections.Deque[int]\n    r: collections.Nope\n\ndef main(stdout: Stdout):\n    a: Mystery = 1\n    match a:\n        case _:\n            b: Unknown2 = 2\n    if True:\n        c: Gone = 3\n\ntest \"t\" for n: Nada:\n    d: Nothing2 = 4\n",
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

/// Every `.fen` file under `dir`, in folders too, like the files of the kits in
/// `examples/projects`.
fn sources_under(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = sources_in(dir)?;
    let mut folders: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    for folder in folders {
        paths.extend(sources_under(&folder)?);
    }
    Ok(paths)
}

/// Every file the two front ends are compared on. Sources that don't live in files, like the
/// standard library's, are written into `work`.
fn corpus(work: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = sources_in(&repo().join("examples"))?;
    paths.extend(sources_in(&repo().join("examples/errors"))?);
    paths.extend(sources_under(&repo().join("examples/projects"))?);
    paths.extend(sources_under(&repo().join("selfhost/src"))?);
    let generated = fennec_syntax::prelude()
        .enumerate()
        .map(|(at, source)| (format!("prelude_{at}"), source))
        .chain(LEX_CASES.iter().map(|(name, source)| (format!("lex_{name}"), *source)))
        .chain(PARSE_CASES.iter().map(|(name, source)| (format!("parse_{name}"), *source)))
        .chain(CHECK_CASES.iter().map(|(name, source)| (format!("check_{name}"), *source)));
    for (name, source) in generated {
        let path = work.join(format!("{name}.fen"));
        fs::write(&path, source)?;
        paths.push(path);
    }
    Ok(paths)
}

/// Runs the self-hosted front end on every file in `corpus` with `mode`, returning what it
/// printed. It reads its work from standard input.
fn selfhost(work: &Path, mode: &str, corpus: &[PathBuf], engine: &[&str]) -> Result<String, String> {
    let mut inputs = String::new();
    if mode == "check" {
        let library = corpus.iter().filter(|path| {
            path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("prelude_"))
        });
        for path in library {
            let _ = writeln!(inputs, "std {}", path.display());
        }
    }
    for path in corpus {
        let _ = writeln!(inputs, "{mode} {}", path.display());
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .arg("run")
        .args(engine)
        .arg(repo().join("selfhost"))
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    // It reads every line before it writes anything, so writing all of them first can't stall.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(inputs.as_bytes()).map_err(|err| err.to_string())?;
    }
    let output = child.wait_with_output().map_err(|err| err.to_string())?;
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
/// dump against `rust_dump`. Returns a report of what differs.
fn compare(mode: &str, rust_dump: fn(&str) -> String) -> Result<(), String> {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("selfhost-{mode}"));
    fs::create_dir_all(&work).map_err(|err| err.to_string())?;
    let corpus = corpus(&work).map_err(|err| err.to_string())?;
    let interpreted = selfhost(&work, mode, &corpus, &[])?;
    let dumps = dumps(&interpreted);
    if dumps.len() != corpus.len() {
        return Err(format!("the self-hosted front end dumped {} of {} files", dumps.len(), corpus.len()));
    }
    let mut first = None;
    let mut others = Vec::new();
    for (path, (shown, dump)) in corpus.iter().zip(&dumps) {
        let source = fs::read_to_string(path).map_err(|err| err.to_string())?;
        let expected = rust_dump(&source);
        if *dump == expected {
            continue;
        }
        let shown = short(shown, &work);
        match first {
            None => first = Some(difference(&shown, &source, dump, &expected)),
            Some(_) => others.push(shown),
        }
    }
    if let Some(first) = first {
        let mut report = format!("the self-hosted {mode} differ from Rust's for {first}");
        if !others.is_empty() {
            let _ = write!(report, "\n\nThey also differ for:\n  {}", others.join("\n  "));
        }
        let _ = write!(report, "\n\n{}", mirror_hint(mode));
        return Err(report);
    }
    let native = selfhost(&work, mode, &corpus, &["--jit"])?;
    if native != interpreted {
        let (ours, theirs) = first_difference(&native, &interpreted);
        return Err(format!(
            "the JIT and the interpreter disagree on the self-hosted {mode}: the JIT printed\n  {ours}\nwhere the interpreter printed\n  {theirs}\nThat's a bug in an engine, not in selfhost/."
        ));
    }
    Ok(())
}

/// A corpus file's path as a report shows it: from the repository's root, or marked as one
/// of the small cases this test writes.
fn short(shown: &str, work: &Path) -> String {
    let root = format!("{}/", repo().display());
    let generated = format!("{}/", work.display());
    shown
        .strip_prefix(&root)
        .map(str::to_owned)
        .or_else(|| {
            shown.strip_prefix(&generated).map(|name| format!("{name} (a case in tests/selfhost.rs)"))
        })
        .unwrap_or_else(|| shown.to_owned())
}

/// A file's name, the first line where the two dumps differ, and the source line it's about.
fn difference(shown: &str, source: &str, ours: &str, theirs: &str) -> String {
    let (our_line, their_line) = first_difference(ours, theirs);
    let mut report = format!("{shown}\n  self-hosted: {our_line}\n  rust:        {their_line}");
    if let Some((line, text)) =
        span_start(their_line).or_else(|| span_start(our_line)).and_then(|at| line_at(source, at))
    {
        let _ = write!(report, "\n  source line {line}: {}", text.trim());
    }
    report
}

/// The first line where two dumps differ, from each. A dump that ended early shows
/// `(nothing more)`.
fn first_difference<'a>(ours: &'a str, theirs: &'a str) -> (&'a str, &'a str) {
    let mut ours = ours.lines();
    let mut theirs = theirs.lines();
    loop {
        match (ours.next(), theirs.next()) {
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return ("(nothing)", "(nothing)"),
            (a, b) => return (a.unwrap_or("(nothing more)"), b.unwrap_or("(nothing more)")),
        }
    }
}

/// Where the span a dump line mentions starts: the number before `..` in `12..16`,
/// `error 12..16 ...` or `name x @12..16`.
fn span_start(line: &str) -> Option<usize> {
    let (before, _) = line.split_once("..")?;
    let digits = before.rsplit(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

/// The line number and text of the line holding byte `at`.
fn line_at(source: &str, at: usize) -> Option<(usize, &str)> {
    let before = source.get(..at)?;
    let number = before.matches('\n').count() + 1;
    source.lines().nth(number - 1).map(|text| (number, text))
}

/// Where to make the self-hosted front end match a change to the Rust one.
fn mirror_hint(mode: &str) -> &'static str {
    match mode {
        "tokens" => {
            "The Rust lexer changed. Make the same change in selfhost/src/lexer.fen (and token.fen for a new token). See \"Changing the front end\" in selfhost/README.md."
        }
        "tree" => {
            "The Rust parser or tree dump changed. Make the same change in selfhost/src/parser.fen and ast.fen, and in the tree dump in selfhost/src/dump.fen if crates/syntax/src/dump/tree.rs changed. See \"Changing the front end\" in selfhost/README.md."
        }
        _ => {
            "The Rust checker changed. Make the same change in selfhost/src/check/, which mirrors crates/check/src file for file. Only the kinds of diagnostics in CHECK_KINDS are compared. See \"Changing the front end\" in selfhost/README.md."
        }
    }
}

/// The kinds of checker diagnostics the self-hosted checker reports so far, by a part of
/// their message. It grows until it covers everything the Rust checker says.
const CHECK_KINDS: &[&str] = &[
    "isn't a type",
    "can't be extended",
    "already has a method called",
    "type in brackets",
    "types in brackets",
    "refers to itself",
    "isn't an interface",
    "is an interface, not a type",
];

/// The Rust checker's diagnostics of the kinds in `CHECK_KINDS`, as `fennec dev check`
/// prints them, or the parse error that stopped it.
fn checked(source: &str) -> String {
    match fennec_syntax::parse(source) {
        Ok(module) => match fennec_check::check(&module) {
            Ok(_) => String::new(),
            Err(diagnostics) => diagnostics
                .iter()
                .filter(|diagnostic| CHECK_KINDS.iter().any(|kind| diagnostic.message.contains(kind)))
                .map(fennec_syntax::dump::error)
                .collect(),
        },
        Err(diagnostic) => fennec_syntax::dump::error(&diagnostic),
    }
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

#[test]
fn selfhost_checker_matches_the_rust_checker() {
    if let Err(difference) = compare("check", checked) {
        panic!("{difference}");
    }
}
