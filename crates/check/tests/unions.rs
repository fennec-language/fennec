//! `A | B`: a value that's exactly one of those types, narrowed by `is` and `match`.

use crate::{helps, in_main_after, problems};

const SHAPES: &str = "struct Circle:
    radius: float

    def area(self) -> float:
        self.radius

struct Square:
    side: float

    def area(self) -> float:
        self.side

struct Dot:
    at: int

type Shape = Circle | Square
";

fn with_shapes(body: &str) -> Vec<String> {
    in_main_after(SHAPES, body)
}

#[test]
fn a_member_goes_where_its_union_does_and_order_doesnt_matter() {
    let empty: [&str; 0] = [];
    assert_eq!(with_shapes("s: Shape = Circle(radius=1.0)\nstdout.print(s)\n"), empty);
    assert_eq!(
        with_shapes(
            "shapes: list[Square | Circle] = [Square(side=1.0), Circle(radius=2.0)]\nmore: list[Shape] = shapes\nstdout.print(more)\n"
        ),
        empty
    );
    assert_eq!(with_shapes("n: int | str = 3\nstdout.print(n)\n"), empty);
    assert_eq!(
        with_shapes("s: Shape = Dot(at=1)\nstdout.print(s)\n"),
        ["`s` should be a Circle | Square, but this is a Dot @ Dot(at=1)"]
    );
}

#[test]
fn is_narrows_a_union_to_one_type_and_the_rest() {
    let empty: [&str; 0] = [];
    let body = "s: Shape = Circle(radius=1.0)\nif s is Circle:\n    stdout.print(s.radius)\nelse:\n    stdout.print(s.side)\n";
    assert_eq!(with_shapes(body), empty);
    let body = "s: Shape = Circle(radius=1.0)\nif s is not Square:\n    stdout.print(s.radius)\n";
    assert_eq!(with_shapes(body), empty);
    let body = "n: int | str = 3\nif n is int:\n    stdout.print(n + 1)\n";
    assert_eq!(with_shapes(body), empty);
    assert_eq!(
        problems(&format!("{SHAPES}def radius(s: Shape) -> float:\n    s.radius\n")),
        ["a Circle | Square has no field `radius` @ s.radius"]
    );
    assert_eq!(
        with_shapes("s: Shape = Circle(radius=1.0)\nstdout.print(s is Dot)\n"),
        ["`Dot` isn't one of the types in a Circle | Square @ Dot"]
    );
}

#[test]
fn a_union_has_a_method_every_member_has() {
    let empty: [&str; 0] = [];
    assert_eq!(with_shapes("s: Shape = Circle(radius=1.0)\nstdout.print(s.area())\n"), empty);
    assert_eq!(problems(&format!("{SHAPES}def area(s: Shape) -> float:\n    s.area()\n")), empty);
    assert_eq!(
        problems(&format!("{SHAPES}def area(s: Circle | Dot) -> float:\n    s.area()\n")),
        ["a Dot has no method `area`, so a Circle | Dot doesn't either @ s.area()"]
    );
}

#[test]
fn a_match_over_a_union_handles_each_member() {
    let empty: [&str; 0] = [];
    let both = "def name(s: Shape) -> str:\n    match s:\n        case Circle(radius): \"c\"\n        case Square(side): \"s\"\n";
    assert_eq!(problems(&format!("{SHAPES}{both}")), empty);
    let one = "def name(s: Shape) -> str:\n    match s:\n        case Circle(radius): \"c\"\n";
    assert_eq!(problems(&format!("{SHAPES}{one}")), ["this `match` doesn't handle `Square` @ s"]);
    let rest = "def name(s: Shape) -> str:\n    match s:\n        case Circle(radius): \"c\"\n        case _: \"other\"\n";
    assert_eq!(problems(&format!("{SHAPES}{rest}")), empty);
}

#[test]
fn a_union_has_no_none_no_member_twice_and_no_type_parameter() {
    assert_eq!(
        helps("def f(x: str | None) -> int:\n    1\n"),
        ["write `str?`: an optional says it can be missing"]
    );
    assert_eq!(
        helps("struct A:\n    n: int\n\nstruct B:\n    n: int\n\ndef f(x: A | B | None) -> int:\n    1\n"),
        ["write `(A | B)?`: an optional says it can be missing"]
    );
    assert_eq!(problems("def f(x: int | int) -> int:\n    1\n"), ["`int` is in this union twice @ int"]);
    assert_eq!(
        problems("def f[T](x: T | int) -> int:\n    1\n"),
        ["`T` is a type parameter, which can't be part of a union @ T"]
    );
}
