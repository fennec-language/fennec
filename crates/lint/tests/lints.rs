//! What each lint flags, and what `fennec fix` makes of it.

use fennec_lint::{Lint, lint};
use fennec_syntax::{Sources, apply_fixes, parse_at};

fn found(source: &str) -> Result<Vec<Lint>, String> {
    let mut sources = Sources::new();
    let base = sources.add("game.fen", source);
    let module = parse_at(source, base).map_err(|problem| problem.message)?;
    Ok(lint(&module, &sources))
}

/// The lints' names, which won't match anything a test expects if the source doesn't parse.
fn rules(source: &str) -> Vec<&'static str> {
    found(source)
        .map_or_else(|_| vec!["it doesn't parse"], |found| found.iter().map(|lint| lint.rule).collect())
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
    let lines = "    x = 1\n".repeat(80);
    assert_eq!(rules(&format!("def f():\n{}", "    x = 1\n".repeat(79))), NONE, "80 lines is allowed");
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
}

#[test]
fn a_default_for_none_has_its_own_form() {
    for written in ["x if x is not None else 1", "1 if x is None else x"] {
        assert_eq!(rules(&in_function(written)), ["manual-default"], "{written}");
        assert_eq!(fixed(&in_function(written)), in_function("x else 1"), "{written}");
    }
    assert_eq!(rules(&in_function("a if x is not None else 1")), NONE);
}
