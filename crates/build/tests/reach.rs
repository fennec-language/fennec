//! Which tests a change affects: each test's key changes exactly when something it can reach
//! changes.

use std::collections::HashMap;

use fennec_blake3::Hash;
use fennec_build::test_keys;

const PROGRAM: &str = "struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        Money(cents=self.cents + other.cents)

    def shown(self) -> str:
        f\"{self.cents}\"

enum Shape:
    Circle(radius: float)
    Square(side: float)

def double(x: int) -> int:
    x * 2

def area(shape: Shape) -> float:
    match shape:
        case .Circle(radius):
            3.0 * radius * radius
        case .Square(side):
            side * side

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
        "def double(x: int) -> int:\n    x * 2\n",
        "# Twice x.\ndef double(x: int) -> int:\n\n    x * 2\n",
    );
    assert_eq!(affected(PROGRAM, &moved), Vec::<String>::new());
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
