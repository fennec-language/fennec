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

/// A `with` source each of a union's types has runs the one the value holds only once it's
/// narrowed with `is`. Called on the union, even inside a `match` case, which one runs would be
/// chosen where the program's built, so it's refused there.
#[test]
fn a_with_source_on_a_union_is_refused_until_it_is_narrowed() {
    let store = "error Nope:\n    why: str\n\nstruct Transaction:\n    log: str\n\nstruct Lite:\n    name: str\n\n    def transaction(self) -> with Transaction fails Nope:\n        lend Transaction(log=\"lite\") else:\n            pass\n\nstruct Postgres:\n    host: str\n\n    def transaction(self) -> with Transaction fails Nope:\n        lend Transaction(log=\"pg\") else:\n            pass\n\ntype Store = Lite | Postgres\n\n";
    let said = "`transaction` is a `with` source each type in a Lite | Postgres has, and which one runs can't be chosen as the program runs yet @ store.transaction()";
    let direct = format!(
        "{store}def used(store: Store) -> str fails Nope:\n    with store.transaction()? as tx:\n        return tx.log\n    \"\"\n"
    );
    assert_eq!(problems(&direct), [said]);
    let matched = format!(
        "{store}def used(store: Store) -> str fails Nope:\n    match store:\n        case Lite(name=_):\n            with store.transaction()? as tx:\n                return tx.log\n        case Postgres(host=_):\n            return \"pg\"\n    \"\"\n"
    );
    assert_eq!(problems(&matched), [said]);
    let narrowed = format!(
        "{store}def used(store: Store) -> str fails Nope:\n    if store is Lite:\n        with store.transaction()? as tx:\n            return tx.log\n    \"\"\n"
    );
    assert_eq!(problems(&narrowed), Vec::<String>::new());
    assert!(helps(&direct).iter().any(|help| help.contains("if store is Lite:")), "{:?}", helps(&direct));
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

#[test]
fn a_union_is_a_key_when_each_type_in_it_is() {
    let empty: [&str; 0] = [];
    let keyed = "counts: dict[int | str, int] = {1: 2, \"a\": 3}\nseen: set[Dot | str] = {Dot(at=1), \"x\"}\nstdout.print(counts, seen)\n";
    assert_eq!(with_shapes(keyed), empty);
    assert_eq!(
        with_shapes("shapes: dict[Shape, int] = {}\nstdout.print(shapes)\n"),
        [
            "`Circle | Square` can't be a dict key or set item, because `Circle` is one of its types, and its field `radius` is a float @ shapes"
        ]
    );
}

#[test]
fn a_union_fits_an_interface_every_type_in_it_fits() {
    let empty: [&str; 0] = [];
    let interface = "interface HasArea:\n    def area(self) -> float\n\ndef total[T: HasArea](items: list[T]) -> float:\n    sum(item.area() for item in items)\n\n";
    let fits = format!(
        "{SHAPES}{interface}def main(stdout: Stdout):\n    shapes: list[Shape] = [Circle(radius=1.0)]\n    stdout.print(total(shapes))\n"
    );
    assert_eq!(problems(&fits), empty);
    let not = format!(
        "{SHAPES}{interface}def main(stdout: Stdout):\n    dots: list[Circle | Dot] = [Dot(at=1)]\n    stdout.print(total(dots))\n"
    );
    assert_eq!(
        helps(&not),
        ["give `Dot` the method `def area(self) -> float`"],
        "the member that doesn't fit is named"
    );
}

#[test]
fn one_of_several_errors_can_be_optional() {
    let declared = "error Alpha:\n    at: int\n\nerror Beta:\n    at: int\n\ndef pick(which: int) -> (Alpha | Beta)?:\n    if which == 0:\n        return None\n    return Alpha(at=which)\n";
    assert_eq!(in_main_after(declared, "stdout.print(pick(0) is None)\n"), Vec::<String>::new());
}
