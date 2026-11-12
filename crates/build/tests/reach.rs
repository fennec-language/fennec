//! Which tests a change affects: each test's key changes exactly when something it can reach
//! changes.

use std::collections::HashMap;

use fennec_blake3::Hash;
use fennec_build::test_keys;

const PROGRAM: &str = "struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        return Money(cents=self.cents + other.cents)

    def shown(self) -> str:
        return f\"{self.cents}\"

enum Shape:
    Circle(radius: float)
    Square(side: float)

def double(x: int) -> int:
    return x * 2

def area(shape: Shape) -> float:
    match shape:
        case .Circle(radius):
            return 3.0 * radius * radius
        case .Square(side):
            return side * side

test \"doubling\":
    assert double(2) == 4

test \"adding money\":
    assert (Money(cents=1) + Money(cents=2)).cents == 3

test \"showing money\":
    assert Money(cents=5).shown() == \"5\"

test \"areas\":
    assert area(.Square(side=2.0)) == 4.0
";

/// Each test's key, or nothing if `source` doesn't parse, which fails the comparisons below.
fn keys(source: &str) -> HashMap<String, Hash> {
    fennec_syntax::parse(source)
        .map_or_else(|_| HashMap::new(), |module| test_keys(&module).into_iter().collect())
}

/// The tests whose keys differ between two versions of a program.
fn affected(before: &str, after: &str) -> Vec<String> {
    let (before, after) = (keys(before), keys(after));
    let mut changed: Vec<String> = after
        .iter()
        .filter(|(name, key)| before.get(*name) != Some(*key))
        .map(|(name, _)| name.clone())
        .collect();
    changed.sort();
    changed
}

#[test]
fn every_test_has_a_key() {
    assert_eq!(keys(PROGRAM).len(), 4);
}

#[test]
fn layout_and_comments_change_no_key() {
    let moved = PROGRAM.replace(
        "def double(x: int) -> int:\n    return x * 2\n",
        "# Twice x.\ndef double(x: int) -> int:\n\n    return x * 2\n",
    );
    assert_eq!(affected(PROGRAM, &moved), Vec::<String>::new());
}

/// A program whose function and test have names of their own to rename.
const LOCALS: &str = "def total(items: list[int]) -> int:
    mut sum = 0
    for item in items:
        sum += item
    return sum

def shown(pairs: list[(str, int)]) -> list[str]:
    return [f\"{name}={count}\" for name, count in pairs if count > 0]

test \"adds\":
    values = [1, 2, -4]
    picked = [value for value in values if value > 0]
    assert total(picked) == 3

test \"shows\":
    pair = (1, shown([(\"a\", 1)])[0])
    match pair:
        case (times, first):
            assert times == 1 and first == \"a=1\"
";

/// `source` with every whole word `from` written as `to`.
fn with_word_renamed(source: &str, from: &str, to: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::new();
    let mut rest = source;
    while let Some(at) = rest.find(from) {
        let before = rest.get(..at).unwrap_or_default();
        let after = rest.get(at + from.len()..).unwrap_or_default();
        let whole = !before.chars().next_back().is_some_and(word) && !after.chars().next().is_some_and(word);
        out.push_str(before);
        out.push_str(if whole { to } else { from });
        rest = after;
    }
    out + rest
}

/// Renaming a name a function or test binds itself changes nothing it does, so no test runs
/// again for it: a local, a loop's, a comprehension's, an arrow function's or a case's.
#[test]
fn renaming_a_local_affects_no_test() {
    assert_eq!(keys(LOCALS).len(), 2, "it has to parse, or every key is the same");
    for (from, to) in [
        ("sum", "acc"),
        ("item", "each"),
        ("values", "numbers"),
        ("value", "number"),
        ("name", "label"),
        ("first", "only"),
        ("times", "count_seen"),
    ] {
        let renamed = with_word_renamed(LOCALS, from, to);
        assert_ne!(renamed, LOCALS);
        assert_eq!(affected(LOCALS, &renamed), Vec::<String>::new(), "renaming {from}");
    }
}

/// Names that keep their spelling: a parameter, which a call can name, and two locals swapping
/// places, which isn't a rename.
#[test]
fn renaming_a_parameter_or_swapping_locals_still_counts() {
    assert_eq!(affected(LOCALS, &LOCALS.replace("items", "numbers")), ["adds"]);
    let source = "def f() -> int:\n    a = 5\n    b = 2\n    return a - b\n\ntest \"t\":\n    assert f() == 3\n";
    assert_eq!(affected(source, &source.replace("a - b", "b - a")), ["t"]);
}

/// A name that's called keeps its spelling, since out of a block's reach it can mean a function
/// the program or the standard library declares.
#[test]
fn renaming_a_name_that_is_called_still_counts() {
    let source = "test \"t\":\n    twice = x => x * 2\n    assert twice(2) == 4\n";
    assert_eq!(affected(source, &source.replace("twice", "double")), ["t"]);
}

#[test]
fn changing_a_function_affects_the_tests_that_call_it() {
    assert_eq!(affected(PROGRAM, &PROGRAM.replace("x * 2", "x + x")), ["doubling"]);
}

#[test]
fn changing_an_operator_affects_the_tests_that_use_the_type() {
    let changed = PROGRAM.replace("self.cents + other.cents", "other.cents + self.cents");
    assert_eq!(affected(PROGRAM, &changed), ["adding money", "showing money"]);
}

#[test]
fn changing_a_method_affects_the_tests_that_call_it_by_name() {
    assert_eq!(
        affected(PROGRAM, &PROGRAM.replace("f\"{self.cents}\"", "str(self.cents)")),
        ["showing money"]
    );
}

#[test]
fn changing_an_enum_affects_the_tests_that_reach_its_cases() {
    let changed = PROGRAM.replace("Square(side: float)", "Square(side: float)\n    Dot");
    assert_eq!(affected(PROGRAM, &changed), ["areas"]);
}

#[test]
fn changing_a_test_affects_only_that_test() {
    assert_eq!(affected(PROGRAM, &PROGRAM.replace("double(2) == 4", "double(3) == 6")), ["doubling"]);
}

#[test]
fn changing_where_a_name_comes_from_affects_the_tests_that_use_it() {
    let program = |module: &str| {
        format!(
            "from std.{module} import pick\nfrom std.math import sqrt\n\ntest \"picking\":\n    assert pick(1) == 1\n\n\
            test \"roots\":\n    assert sqrt(4.0) == 2.0\n"
        )
    };
    assert_eq!(affected(&program("one"), &program("other")), ["picking"]);
}

#[test]
fn changing_where_an_aliased_name_comes_from_affects_the_tests_that_use_it() {
    let program = |module: &str| {
        format!(
            "from std.{module} import pick as choose\nimport std.{module} as chosen\n\ntest \"by name\":\n    \
            assert choose(1) == 1\n\ntest \"by module\":\n    assert chosen.pick(1) == 1\n\n\
            test \"neither\":\n    assert True\n"
        )
    };
    assert_eq!(affected(&program("one"), &program("other")), ["by module", "by name"]);
}
