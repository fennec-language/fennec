//! What each lint flags, and what `fennec fix` makes of it.

use fennec_lint::{Lint, lint};
use fennec_syntax::{Sources, apply_fixes, parse_at};

fn found(source: &str) -> Result<Vec<Lint>, String> {
    let mut sources = Sources::new();
    let base = sources.add("game.fen", source);
    let module = parse_at(source, base).map_err(|problem| problem.message)?;
    Ok(lint(&module, &sources))
}

/// The rules about names, which most tests' short names would set off: `name_rules` has them.
const NAME_RULES: [&str; 3] = ["short-name", "naming-case", "shadowed-builtin"];

/// The lints' names, which won't match anything a test expects if the source doesn't parse.
fn all_rules(source: &str) -> Vec<&'static str> {
    found(source)
        .map_or_else(|_| vec!["it doesn't parse"], |found| found.iter().map(|lint| lint.rule).collect())
}

fn rules(source: &str) -> Vec<&'static str> {
    all_rules(source).into_iter().filter(|rule| !NAME_RULES.contains(rule)).collect()
}

fn name_rules(source: &str) -> Vec<&'static str> {
    all_rules(source).into_iter().filter(|rule| NAME_RULES.contains(rule)).collect()
}

/// The source with every fix made, which has to parse.
fn fixed(source: &str) -> String {
    let lints = found(source).unwrap_or_default();
    let diagnostics: Vec<_> = lints.into_iter().map(|lint| lint.diagnostic).collect();
    let fixed = apply_fixes(source, 0, &diagnostics).unwrap_or_default();
    if fennec_syntax::parse(&fixed).is_err() { format!("this doesn't parse: {fixed}") } else { fixed }
}

/// No lints, as `rules` gives them.
const NONE: [&str; 0] = [];

fn in_function(body: &str) -> String {
    format!("def f(a: int, ready: bool, x: int?) -> bool:\n    {body}\n")
}

#[test]
fn comparing_with_a_bool_or_none_has_a_plain_form() {
    for (written, plain) in [
        ("ready == True", "ready"),
        ("ready == False", "not ready"),
        ("ready != True", "not ready"),
        ("ready != False", "ready"),
    ] {
        let source = in_function(written);
        assert_eq!(rules(&source), ["bool-compare"], "{written}");
        assert_eq!(fixed(&source), in_function(plain), "{written}");
    }
    for (written, plain) in [("x == None", "x is None"), ("x != None", "x is not None")] {
        assert_eq!(rules(&in_function(written)), ["none-compare"]);
        assert_eq!(fixed(&in_function(written)), in_function(plain));
    }
    assert_eq!(rules(&in_function("a == 1")), NONE);
}

#[test]
fn a_negated_comparison_says_the_opposite() {
    for (written, plain) in [
        ("not a == 1", "a != 1"),
        ("not a != 1", "a == 1"),
        ("not x is None", "x is not None"),
        ("not x is not None", "x is None"),
        ("not a in [1]", "a not in [1]"),
    ] {
        assert_eq!(rules(&in_function(written)), ["negated-compare"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function(plain), "{written}");
    }
    assert_eq!(rules(&in_function("not ready")), NONE);
    assert_eq!(rules(&in_function("not a < 1")), NONE, "NaN makes that not the same as `a >= 1`");
}

#[test]
fn an_operand_keeps_the_brackets_its_meaning_needs() {
    assert_eq!(rules(&in_function("(a > 1 or ready) == False")), NONE, "its span starts inside the bracket");
    assert_eq!(fixed(&in_function("not (a + 1) == 2")), in_function("(a + 1) != 2"));
}

#[test]
fn an_fstring_with_nothing_to_fill_in_is_a_string() {
    let source = "x = f\"ready\"\n";
    assert_eq!(rules(source), ["plain-fstring"]);
    assert_eq!(fixed(source), "x = \"ready\"\n");
    assert_eq!(rules("x = f\"{1}\"\n"), NONE);
    assert_eq!(rules("x = f\"{{\"\n"), NONE, "the braces would read differently");
}

#[test]
fn pass_is_only_for_an_empty_block() {
    assert_eq!(rules("def f():\n    pass\n"), NONE);
    let source = "def f():\n    pass\n    g()\n";
    assert_eq!(rules(source), ["needless-pass"]);
    assert_eq!(fixed(source), "def f():\n    \n    g()\n");
}

#[test]
fn a_last_line_needs_no_return() {
    let source = "def f() -> int:\n    return 1\n";
    assert_eq!(rules(source), ["needless-return"]);
    assert_eq!(fixed(source), "def f() -> int:\n    1\n");
    assert_eq!(rules("def f(a: bool) -> int:\n    if a: return 1\n    2\n"), NONE);
}

#[test]
fn sizes_have_limits() {
    let lines = "    g()\n".repeat(80);
    assert_eq!(rules(&format!("def f():\n{}", "    g()\n".repeat(79))), NONE, "80 lines is allowed");
    assert_eq!(rules(&format!("def f():\n{lines}")), ["long-function"]);

    assert_eq!(rules(&"x = 1\n".repeat(800)), NONE);
    assert_eq!(rules(&"x = 1\n".repeat(801)), ["long-file"]);

    let six = "a: int, b: str, c: int, d: str, e: int, f: str";
    assert_eq!(rules(&format!("def g({six}, out: Stdout):\n    pass\n")), NONE, "capabilities don't count");
    assert_eq!(rules(&format!("def g({six}, h: int):\n    pass\n")), ["too-many-params"]);

    let deep = |levels: usize| {
        let mut source = "def f(a: bool):\n".to_owned();
        for level in 1..=levels {
            source += &"    ".repeat(level);
            source += "if a:\n";
        }
        source += &"    ".repeat(levels + 1);
        source += "pass\n";
        source
    };
    assert_eq!(rules(&deep(4)), NONE);
    assert_eq!(rules(&deep(5)), ["deep-nesting"]);
}

const CALLEES: &str = "def spawn(name: str, hostile: bool):\n    pass\n\n\
    def between(low: int, high: int):\n    pass\n\n\
    def place(name: str, x: int, y: int):\n    pass\n\n\
    def one(a: int):\n    pass\n\n\
    def mixed(a: int, b: str):\n    pass\n\n\
    struct Point:\n    x: int\n    y: int\n\n    def moved(self, by: int, up: int):\n        pass\n\n";

fn calling(call: &str) -> String {
    format!("{CALLEES}def main():\n    {call}\n")
}

#[test]
fn an_argument_that_cant_be_read_from_the_call_needs_a_name() {
    for (call, named) in [
        ("spawn(\"fox\", True)", "spawn(\"fox\", hostile=True)"),
        ("between(1, 2)", "between(1, high=2)"),
        ("place(\"fox\", 1, 2)", "place(\"fox\", x=1, y=2)"),
        ("Point(1, 2)", "Point(1, y=2)"),
        ("p.moved(1, 2)", "p.moved(1, up=2)"),
    ] {
        assert_eq!(rules(&calling(call)), ["positional-arguments"], "{call}");
        assert_eq!(fixed(&calling(call)), calling(named), "{call}");
    }
}

#[test]
fn an_argument_that_can_be_read_stays_as_it_is() {
    for call in [
        "one(1)",
        "one(a=1)",
        "mixed(1, \"a\")",
        "between(low=1, high=2)",
        "between(1, high=2)",
        "len(1, 2, 3)",
    ] {
        assert_eq!(rules(&calling(call)), NONE, "{call}");
    }
}

#[test]
fn three_arguments_of_types_that_cant_be_swapped_stay_positional() {
    let types = "struct World:\n    size: int\n\nstruct Board:\n    wide: int\n\n\
        interface Named:\n    def name(self) -> str\n\ntype Meters = int\n\n";
    let program = |def: &str, call: &str| format!("{types}{def}\n    pass\n\ndef main():\n    {call}\n");
    let report = "def report(out: Stdout, world: World, board: Board):";
    assert_eq!(rules(&program(report, "report(stdout, world, board)")), NONE);
    for (def, call) in [
        ("def report(out: Stdout, world: World, again: World):", "report(stdout, world, other)"),
        ("def place[T](at: T, x: int, label: str):", "place(1, 2, \"a\")"),
        ("def place(at: Named, x: World, label: str):", "place(world, world, \"a\")"),
        ("def place(at: Meters, x: int, label: str):", "place(1, 2, \"a\")"),
        ("def place(at: int?, x: int, label: str):", "place(1, 2, \"a\")"),
    ] {
        assert_eq!(rules(&program(def, call)), ["positional-arguments"], "{def}");
    }
}

#[test]
fn a_method_with_two_shapes_cant_be_told_apart_so_it_isnt_flagged() {
    let source = "struct A:\n    def go(self, x: int, y: int):\n        pass\n\n\
        struct B:\n    def go(self, x: str, y: str):\n        pass\n\n\
        def main():\n    a.go(1, 2)\n";
    assert_eq!(rules(source), NONE);
}

#[test]
fn an_enum_case_names_its_arguments_too() {
    let enumerated = "enum Shape:\n    Rect(wide: int, high: int)\n    Dot\n\n";
    for (call, named) in [("Shape.Rect(1, 2)", "Shape.Rect(1, high=2)"), (".Rect(1, 2)", ".Rect(1, high=2)")]
    {
        let source = format!("{enumerated}def main():\n    x = {call}\n");
        assert_eq!(rules(&source), ["positional-arguments"], "{call}");
        assert_eq!(fixed(&source), format!("{enumerated}def main():\n    x = {named}\n"), "{call}");
    }
}

#[test]
fn the_standard_librarys_types_count_unless_the_program_replaces_them() {
    let library = "def main():\n    x = Vec3(1.0, 2.0, 3.0)\n";
    assert_eq!(rules(library), ["positional-arguments"], "std.math's Vec3");
    assert_eq!(fixed(library), "def main():\n    x = Vec3(1.0, y=2.0, z=3.0)\n");
    let replaced = "def Vec3(a: int, b: str):\n    pass\n\ndef main():\n    x = Vec3(1, \"a\")\n";
    assert_eq!(rules(replaced), NONE, "the program's own parameters are what matter");
}

#[test]
fn a_name_a_function_binds_itself_isnt_flagged() {
    let source = "def between(low: int, high: int):\n    pass\n\n\
                  def main(between: (int, int) -> int):\n    between(1, 2)\n";
    assert_eq!(rules(source), NONE, "a parameter");
    let source = "def between(low: int, high: int):\n    pass\n\n\
                  def main():\n    between = other\n    between(1, 2)\n";
    assert_eq!(rules(source), NONE, "a local");
}

#[test]
fn code_after_leaving_a_block_cant_run() {
    let source = "def f(a: int) -> int:\n    return a\n    a + 1\n";
    assert!(rules(source).contains(&"unreachable"));
    assert_eq!(rules("def f():\n    while True:\n        break\n        g()\n"), ["unreachable"]);
    assert_eq!(
        fixed("def f():\n    while True:\n        break\n        g()\n"),
        "def f():\n    while True:\n        break\n        \n"
    );
}

#[test]
fn an_if_that_only_gives_back_its_condition_is_its_condition() {
    for (written, plain) in [
        ("    if a > 0: return True\n    False\n", "    a > 0\n"),
        ("    if a > 0: return False\n    return True\n", "    not (a > 0)\n"),
        ("    if a > 0:\n        return True\n    else:\n        return False\n", "    a > 0\n"),
    ] {
        let source = format!("def f(a: int) -> bool:\n{written}");
        assert!(rules(&source).contains(&"return-bool"), "{written}");
        assert_eq!(
            fixed(&source).replace("\n    \n", "\n"),
            format!("def f(a: int) -> bool:\n{plain}"),
            "{written}"
        );
    }
    assert_eq!(rules("def f(a: int) -> bool:\n    if a > 0: return True\n    True\n"), NONE);
}

#[test]
fn a_condition_tested_twice_cant_run_the_second_time() {
    let source = "def f(a: int):\n    if a > 0:\n        g()\n    elif a < 0:\n        h()\n    elif a > 0:\n        i()\n";
    assert_eq!(rules(source), ["duplicate-condition"]);
    let calls = "def f():\n    if g():\n        h()\n    elif g():\n        i()\n";
    assert_eq!(rules(calls), NONE, "a call can give something else the second time");
}

#[test]
fn branches_that_do_the_same_are_flagged() {
    assert_eq!(
        rules("def f(a: bool):\n    if a:\n        g()\n    else:\n        g()\n"),
        ["identical-branches"]
    );
    assert_eq!(rules("def f(a: bool):\n    if a:\n        g()\n    else:\n        h()\n"), NONE);
}

#[test]
fn a_range_test_is_one_chain() {
    for (written, plain) in [("0 <= a and a <= 10", "0 <= a <= 10"), ("10 > a and a >= 0", "10 > a >= 0")] {
        assert_eq!(rules(&in_function(written)), ["chained-compare"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function(plain), "{written}");
    }
    assert_eq!(rules(&in_function("0 <= a and a >= 10")), NONE, "they point different ways");
    assert_eq!(rules(&in_function("0 <= a and x <= 10")), NONE, "they test different things");
    assert_eq!(rules(&in_function("0 <= a < 10")), NONE, "it's a chain already");
    assert_eq!(rules(&in_function("0 <= a < 10 and a > 2")), NONE, "and so is this, with more after");
}

#[test]
fn a_default_for_none_has_its_own_form() {
    for written in ["x if x is not None else 1", "1 if x is None else x"] {
        assert_eq!(rules(&in_function(written)), ["manual-default"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function("x else 1"), "{written}");
    }
    assert_eq!(rules(&in_function("a if x is not None else 1")), NONE);
}

#[test]
fn an_import_nothing_uses_goes() {
    let source = "from std.math import PI, sqrt\n\ndef f() -> float:\n    sqrt(2.0)\n";
    assert_eq!(rules(source), ["unused-import"]);
    assert_eq!(fixed(source), "from std.math import sqrt\n\ndef f() -> float:\n    sqrt(2.0)\n");
    let whole = "import std.json\nfrom std.math import PI\n\ndef f() -> float:\n    PI\n";
    assert_eq!(fixed(whole), "from std.math import PI\n\ndef f() -> float:\n    PI\n");
    let renamed = "from std.math import sqrt as root, PI\n\ndef f() -> float:\n    root(PI)\n";
    assert_eq!(rules(renamed), NONE);
}

#[test]
fn a_name_in_an_fstring_or_a_type_is_a_use() {
    assert_eq!(rules("from std.math import PI\n\ndef f() -> str:\n    f\"{PI}\"\n"), NONE);
    assert_eq!(rules("from std.math import Vec3\n\ndef f(v: Vec3) -> int:\n    1\n"), NONE);
    assert_eq!(rules("import std.json\n\ndef f() -> str:\n    std.json.dumps([1])\n"), NONE);
    assert_eq!(rules("from std import json\n\ndef f() -> str:\n    json.dumps([1])\n"), NONE);
}

#[test]
fn a_local_nothing_reads_keeps_what_its_value_does() {
    let source = "def f() -> int:\n    damage = attack()\n    1\n";
    assert_eq!(rules(source), ["unused-name"]);
    assert_eq!(fixed(source), "def f() -> int:\n    attack()\n    1\n");
    let read = [
        "def f() -> int:\n    damage = attack()\n    damage\n",
        "def f() -> int:\n    mut total = 0\n    total += 1\n    2\n",
        "def f() -> int:\n    xs = [1]\n    xs.append(2)\n    3\n",
        "def f() -> str:\n    name = \"a\"\n    f\"{name}\"\n",
        "def f() -> int:\n    n = 1\n    def g() -> int:\n        n\n    g()\n",
        "def f() -> int:\n    _skip = attack()\n    1\n",
    ];
    for source in read {
        assert_eq!(rules(source), NONE, "{source}");
    }
}

#[test]
fn mut_on_a_local_nothing_changes_goes() {
    let source = "def f() -> int:\n    mut speed = 4\n    speed + 1\n";
    assert_eq!(rules(source), ["needless-mut"]);
    assert_eq!(fixed(source), "def f() -> int:\n    speed = 4\n    speed + 1\n");
    let changed = [
        "def f() -> int:\n    mut n = 0\n    n += 1\n    n\n",
        "def f() -> int:\n    mut n = 0\n    n = 2\n    n\n",
        "def f() -> int:\n    mut xs = [1]\n    xs.append(2)\n    xs.len()\n",
        "def f() -> int:\n    mut xs = [1]\n    xs[0] = 2\n    xs[0]\n",
        "def f(p: P) -> int:\n    mut q = p\n    q.pos.x = 1\n    q.pos.x\n",
        "def f() -> int:\n    mut xs = [1]\n    g(mut xs)\n    xs[0]\n",
        "def f() -> int:\n    mut xs = [1]\n    for mut x in xs:\n        x += 1\n    xs[0]\n",
        "def f() -> int:\n    mut n = 0\n    def bump():\n        n += 1\n    bump()\n    n\n",
        "def f() -> int:\n    mut n = 0\n    m, n = pair()\n    m + n\n",
        "def f() -> int:\n    mut w = W(xs=[1])\n    mut part = w.xs\n    mut deeper = part\n    deeper[0] = 2\n    w.xs[0]\n",
        "def f(s: S) -> int:\n    mut at = s\n    match at:\n        case .A(inner):\n            at = inner\n        case _:\n            pass\n    1\n",
    ];
    for source in changed {
        assert_eq!(rules(source), NONE, "{source}");
    }
}

/// The source, with `fennec fix`'s blank lines left by removed lines taken out.
fn fixed_tidy(source: &str) -> String {
    let text = fixed(source);
    let kept: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    kept.join("\n") + "\n"
}

#[test]
fn an_unused_index_needs_no_enumerate() {
    let source = "def f(xs: list[int]):\n    for i, x in enumerate(xs):\n        g(x)\n";
    assert_eq!(rules(source), ["needless-enumerate"]);
    assert_eq!(fixed(source), "def f(xs: list[int]):\n    for x in xs:\n        g(x)\n");
    assert_eq!(rules("def f(xs: list[int]):\n    for i, x in enumerate(xs):\n        g(i, x)\n"), NONE);
}

#[test]
fn a_loop_that_only_appends_is_a_comprehension() {
    let source = "def f(ps: list[P]) -> list[str]:\n    mut names = []\n    for p in ps:\n        if p.alive:\n            names.append(p.name)\n    names\n";
    assert_eq!(rules(source), ["manual-comprehension"]);
    assert_eq!(
        fixed(source),
        "def f(ps: list[P]) -> list[str]:\n    mut names = [p.name for p in ps if p.alive]\n    names\n"
    );
    let more = "def f(ps: list[P]) -> list[str]:\n    mut names = []\n    for p in ps:\n        names.append(p.name)\n        g()\n    names\n";
    assert_eq!(rules(more), NONE, "the loop does something else too");
    let started = "def f(ps: list[P]) -> list[str]:\n    mut names = [\"a\"]\n    for p in ps:\n        names.append(p.name)\n    names\n";
    assert_eq!(rules(started), NONE, "it didn't start empty");
    let bracketed = "def f(ps: list[int]) -> list[int]:\n    mut out = []\n    for p in ps:\n        out.append(p + (p if p > 0 else 0))\n    out\n";
    assert_eq!(rules(bracketed), NONE, "the span leaves out a bracket it needs");
}

#[test]
fn a_loop_that_only_adds_is_a_sum() {
    let source = "def f(cart: list[I]) -> int:\n    mut total = 0\n    for item in cart:\n        total += item.price\n    total\n";
    assert_eq!(rules(source), ["manual-builtin"]);
    assert_eq!(
        fixed(source),
        "def f(cart: list[I]) -> int:\n    mut total = sum(item.price for item in cart)\n    total\n"
    );
    let uses_total = "def f(cart: list[I]) -> int:\n    mut total = 0\n    for item in cart:\n        total += total\n    total\n";
    assert_eq!(rules(uses_total), NONE);
}

#[test]
fn a_counted_while_is_a_for() {
    let source = "def f(n: int):\n    mut i = 0\n    while i < n:\n        g(i)\n        i += 1\n";
    assert_eq!(rules(source), ["counted-while"]);
    assert_eq!(fixed_tidy(source), "def f(n: int):\n    for i in range(n):\n        g(i)\n");
    for (why, source) in [
        (
            "it skips a step",
            "def f(n: int):\n    mut i = 0\n    while i < n:\n        if i == 2:\n            continue\n        i += 1\n",
        ),
        (
            "it's used after",
            "def f(n: int) -> int:\n    mut i = 0\n    while i < n:\n        i += 1\n    i\n",
        ),
        (
            "the limit moves",
            "def f(mut n: int):\n    mut i = 0\n    while i < n:\n        n -= 1\n        i += 1\n",
        ),
        (
            "it moves elsewhere too",
            "def f(n: int):\n    mut i = 0\n    while i < n:\n        i += 2\n        i += 1\n",
        ),
    ] {
        assert_eq!(rules(source), NONE, "{why}");
    }
}

#[test]
fn an_index_that_only_indexes_is_an_item() {
    let source = "def f(enemies: list[E]):\n    for i in range(enemies.len()):\n        draw(enemies[i])\n";
    assert_eq!(rules(source), ["range-len"]);
    assert_eq!(fixed(source), "def f(enemies: list[E]):\n    for enemy in enemies:\n        draw(enemy)\n");
    for (why, source) in [
        ("it's used for more", "def f(xs: list[int]):\n    for i in range(xs.len()):\n        g(i, xs[i])\n"),
        (
            "it writes the list",
            "def f(mut xs: list[int]):\n    for i in range(xs.len()):\n        xs[i] = xs[i] + 1\n",
        ),
        (
            "a dict's keys aren't its items",
            "def f(xs: dict[int, int]):\n    for i in range(xs.len()):\n        g(xs[i])\n",
        ),
        (
            "the name is taken",
            "def f(points: list[int]):\n    point = 1\n    for i in range(points.len()):\n        g(points[i], point)\n",
        ),
    ] {
        assert_eq!(rules(source), NONE, "{why}");
    }
}

#[test]
fn each_kind_of_name_is_written_its_own_way() {
    assert_eq!(name_rules("def DoThing():\n    pass\n"), ["naming-case"]);
    assert_eq!(name_rules("struct my_point:\n    x: int\n"), ["naming-case"]);
    assert_eq!(name_rules("limit = 3\n"), ["naming-case"]);
    assert_eq!(
        name_rules("LIMIT = 3\n\ndef count_it(start: int) -> int:\n    total_now = start\n    total_now\n"),
        NONE
    );
    assert_eq!(
        name_rules(
            "struct Point:\n    _secret: int\n\n    def +(self, other: Point) -> Point:\n        other\n"
        ),
        NONE,
        "a private field and an operator"
    );
}

#[test]
fn a_short_name_is_flagged_unless_it_says_enough() {
    assert_eq!(name_rules("def area(w: int) -> int:\n    w\n"), ["short-name"]);
    assert_eq!(
        name_rules("def f(hp: int) -> int:\n    hp\n").iter().filter(|rule| **rule == "short-name").count(),
        2,
        "and `f`"
    );
    for fine in [
        "def area(x: int, y: int, n: int) -> int:\n    x + y + n\n",
        "def step(dt: float, at: int) -> float:\n    dt\n",
        "def read(fs: Fs) -> int:\n    1\n",
        "def total(items: list[int]) -> int:\n    sum(item for item in items)\n",
    ] {
        assert_eq!(name_rules(fine), NONE, "{fine}");
    }
    assert_eq!(
        name_rules("from std.json import loads as j\n\ndef read() -> int:\n    j(\"1\")\n"),
        NONE,
        "kept apart, see below"
    );
}

#[test]
fn a_local_named_like_a_builtin_is_flagged() {
    assert_eq!(
        name_rules("def count_them(items: list[int]) -> int:\n    sum = 0\n    sum\n"),
        ["shadowed-builtin"]
    );
    assert_eq!(name_rules("def count_them(range: int) -> int:\n    range\n"), ["shadowed-builtin"]);
}

#[test]
fn an_alias_that_changes_nothing_goes() {
    let source = "from std import json as json\n\ndef read() -> str:\n    json.dumps([1])\n";
    assert_eq!(rules(source), ["needless-alias"]);
    assert_eq!(fixed(source), "from std import json\n\ndef read() -> str:\n    json.dumps([1])\n");
    let module = "import std.json as json\n\ndef read() -> str:\n    json.dumps([1])\n";
    assert_eq!(fixed(module), "from std import json\n\ndef read() -> str:\n    json.dumps([1])\n");
    let twice = "from std.math import sqrt, sqrt as root\n\ndef side() -> float:\n    sqrt(root(2.0))\n";
    assert_eq!(rules(twice), ["needless-alias"]);
}
