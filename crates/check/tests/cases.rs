//! `case` with several patterns separated by `|`, and with an `if` guard. Each alternative
//! binds the same names with the same types, a guard is a bool that changes nothing, and
//! a guarded case never counts toward handling every case.

use crate::problems;

const SHAPES: &str = "
    enum Shape:
        Circle(radius: float)
        Square(side: float)
        Line(length: int)
        Point
";

fn with_shapes(code: &str) -> Vec<String> {
    problems(&format!("{}\n{}", crate::dedent(SHAPES), crate::dedent(code)))
}

#[test]
fn alternatives_and_guards_check() {
    let program = "
        def size(shape: Shape) -> float:
            match shape:
                case .Circle(r) | .Square(r) if r > 0.0:
                    return r
                case .Circle(_) | .Square(_):
                    return 0.0
                case .Line(n) if n > 0:
                    return float(n)
                case .Line(_) | .Point:
                    return 0.0

        def name(code: int?) -> str:
            match code:
                case 1 | 2:
                    return 'low'
                case n if n is not None:
                    return str(n + 1)
                case _:
                    return 'none'
    ";
    assert_eq!(with_shapes(program), Vec::<String>::new());
}

#[test]
fn every_alternative_binds_the_same_names() {
    let program = "
        def size(shape: Shape) -> int:
            match shape:
                case .Circle(r) | .Point:
                    return 1
                case .Square(side) | .Line(side):
                    return 2
                case _:
                    return 3
    ";
    assert_eq!(
        with_shapes(program),
        [
            "`r` is bound by only some of the alternatives of this `case` @ case .Circle(r) | .Point:\n            1\n        ",
            "`side` is a float in one alternative and an int in another @ case .Square(side) | .Line(side):\n            2\n        ",
        ]
    );
}

#[test]
fn a_guard_is_a_bool_that_changes_nothing() {
    let program = "
        def grow(ref sizes: list[int]):
            sizes.append(1)

        def check(n: int, ref sizes: list[int]) -> int:
            match n:
                case 0 if n:
                    return 0
                case 1 if grow(ref sizes) is None:
                    return 1
                case _:
                    return 2
    ";
    assert_eq!(
        with_shapes(program),
        [
            "`if` needs a bool, but this is an int @ n",
            "a guard can't change anything, so it can't pass `ref` @ sizes",
        ]
    );
}

#[test]
fn a_guarded_case_handles_nothing() {
    let program = "
        def size(shape: Shape) -> int:
            match shape:
                case .Circle(r) if r > 1.0:
                    return 1
                case .Square(_) | .Line(_) | .Point:
                    return 2
    ";
    assert_eq!(with_shapes(program), ["this `match` doesn't handle `Shape.Circle` @ shape"]);
}
