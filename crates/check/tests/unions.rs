//! `A | B`: a value that's exactly one of those types, narrowed by `is` and `match`.

use crate::{helps, in_main_after, problems};

const SHAPES: &str = "struct Circle:
    radius: float

    def area(self) -> float:
        return self.radius

struct Square:
    side: float

    def area(self) -> float:
        return self.side

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
        problems(&format!("{SHAPES}def radius(s: Shape) -> float:\n    return s.radius\n")),
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
    assert_eq!(problems(&format!("{SHAPES}def area(s: Shape) -> float:\n    return s.area()\n")), empty);
    assert_eq!(
        problems(&format!("{SHAPES}def area(s: Circle | Dot) -> float:\n    return s.area()\n")),
        ["a Dot has no method `area`, so a Circle | Dot doesn't either @ s.area()"]
    );
}

/// A `match` case naming a member narrows the subject to it inside the case, as `is` does: a
/// method only that member has, or a field of its own, reads there.
/// A union with a function member, as a server's `Router | (Request) -> Response`: a named
/// function or an arrow goes where it's wanted, an arrow takes its parameters' types from the one
/// function member, and `case _:` after the other members' cases, or `is` ruling them out, leaves
/// the function to call. With another member left, it can't be called, and a function that
/// doesn't fit the member doesn't go there.
#[test]
fn a_union_with_a_function_member_takes_a_function_and_calls_it_once_narrowed() {
    let fine = "struct Table:\n    rows: int\n\ntype Answerer = Table | (int) -> str\n\ndef twice(number: int) -> str:\n    return str(number * 2)\n\ndef use(answer: Answerer) -> str:\n    match answer:\n        case Table(rows):\n            return str(rows)\n        case _:\n            return answer(3)\n\ndef also(answer: Answerer) -> str:\n    if answer is Table:\n        return f\"table {answer.rows}\"\n    return answer(5)\n\ndef main(stdout: Stdout):\n    stdout.print(use(Table(rows=2)))\n    stdout.print(use((number) => str(number * 2)))\n    stdout.print(use(twice))\n    held: Answerer = (number) => f\"held {number}\"\n    stdout.print(also(held), also(Table(rows=7)))\n    return\n";
    assert_eq!(problems(fine), Vec::<String>::new());
    let left = "struct Table:\n    rows: int\n\nstruct Chart:\n    bars: int\n\n\
        def use(answer: Table | Chart | (int) -> str) -> str:\n    match answer:\n        case Table(rows):\n            \
        return str(rows)\n        case _:\n            return answer(3)\n";
    let wrong = "struct Table:\n    rows: int\n\ndef shown(text: str) -> str:\n    return text\n\n\
        def use(answer: Table | (int) -> str) -> int:\n    return 1\n\n\
        def main(stdout: Stdout):\n    stdout.print(use(shown))\n    return\n";
    assert_eq!(problems(left), ["a Chart | (int) -> str can't be called @ answer"]);
    assert_eq!(
        problems(wrong),
        ["`answer` should be a Table | (int) -> str, but this is a function @ shown"]
    );
}

/// A union goes where a union is wanted when its function member is the wanted one's once a
/// type parameter is worked out, as a handler's `Router | (Request) -> Response` passed on to a
/// generic `serve`: one that can't fail, `E` nothing, and one that `fails Oops`, `E` that. One
/// that can't fail doesn't go where the function member is one that `fails Oops`: a function in
/// a union is passed on as it is.
#[test]
fn a_union_goes_where_one_with_its_function_member_is_wanted_once_worked_out() {
    let start = "error Oops:\n    why: str\n\n    def message(self) -> str:\n        return self.why\n\n\
        struct Table:\n    rows: int\n\n\
        def run[E: Worded](step: Table | (int) -> int fails E) -> int:\n    if step is Table:\n        return step.rows\n    \
        got = step(2) except why:\n        return len(why.message())\n    return got\n\n\
        def strict(step: Table | (int) -> int fails Oops) -> int:\n    return run(step)\n\n";
    let fine = format!("{start}def plain(step: Table | (int) -> int) -> int:\n    return run(step)\n");
    assert_eq!(problems(&fine), Vec::<String>::new());
    let narrower = format!("{start}def loose(step: Table | (int) -> int) -> int:\n    return strict(step)\n");
    assert_eq!(
        problems(&narrower),
        ["`step` should be a Table | (int) -> int fails Oops, but this is a Table | (int) -> int @ step"]
    );
}

/// What a function type gives back runs on to the end of it, so `(int) -> str | (str) -> str` is
/// one function giving back `str | (str) -> str`, and a union's function member is shown last, as
/// it reads back. A union of two function types, made of two names for them, has no written form:
/// an arrow where one goes could be either, so it's refused naming both, given to a parameter or a
/// name, with nothing else said about it.
#[test]
fn an_arrow_where_a_union_of_two_function_types_goes_names_both() {
    let one = "type Two = (int) -> str | (str) -> str\n\ndef main(stdout: Stdout):\n    held: Two = 5\n    stdout.print(held(1))\n";
    assert_eq!(problems(one), ["`held` should be a (int) -> str | (str) -> str, but this is an int @ 5"]);
    let names = "type Shown = (int) -> str\ntype Measured = (str) -> str\ntype Two = Shown | Measured\n\n\
        def keep(answer: Two) -> Two:\n    return answer\n\n";
    let said = "this arrow could be either `(int) -> str` or `(str) -> str`, so what its parameters are can't be told";
    let passed = format!("{names}def main(stdout: Stdout):\n    _ = keep((number) => f\"{{number}}\")\n");
    assert_eq!(problems(&passed), [format!("{said} @ (number) => f\"{{number}}\"")]);
    assert_eq!(helps(&passed), ["write a nested `def` with its parameters' types, and pass it by its name"]);
    let held = format!(
        "{names}def main(stdout: Stdout):\n    held: Two = (number) => f\"{{number}}\"\n    _ = keep(held)\n"
    );
    assert_eq!(problems(&held), [format!("{said} @ (number) => f\"{{number}}\"")]);
}

#[test]
fn a_match_case_narrows_its_subject_to_the_member_it_names() {
    let empty: [&str; 0] = [];
    let narrowed = format!(
        "{SHAPES}def size(s: Shape) -> float:\n    match s:\n        case Circle(radius=_):\n            return s.radius\n        case Square(side=_):\n            return s.side\n"
    );
    assert_eq!(problems(&narrowed), empty);
    let outside = format!(
        "{SHAPES}def size(s: Shape) -> float:\n    match s:\n        case Circle(radius=_):\n            pass\n        case Square(side=_):\n            pass\n    return s.radius\n"
    );
    assert_eq!(problems(&outside).len(), 1, "{:?}", problems(&outside));
}

/// A `with` source each of a union's types has, called on a name holding the union, runs the one
/// of the type the value holds, as it does narrowed with `is` or a `match` case. Called on a
/// field, which `is` doesn't narrow, it's refused, with a fix that names it first.
#[test]
fn a_with_source_on_a_union_runs_the_member_it_holds() {
    let store = "error Nope:\n    why: str\n\nstruct Transaction:\n    log: str\n\nstruct Lite:\n    name: str\n\n    def transaction(self) -> with Transaction fails Nope:\n        lend Transaction(log=\"lite\") except:\n            pass\n\nstruct Postgres:\n    host: str\n\n    def transaction(self) -> with Transaction fails Nope:\n        lend Transaction(log=\"pg\") except:\n            pass\n\ntype Store = Lite | Postgres\n\nstruct Holder:\n    db: Store\n\n";
    let direct = format!(
        "{store}def used(store: Store) -> str fails Nope:\n    with store.transaction()? as tx:\n        return tx.log\n    return \"\"\n"
    );
    assert_eq!(problems(&direct), Vec::<String>::new());
    let matched = format!(
        "{store}def used(store: Store) -> str fails Nope:\n    match store:\n        case Lite(name=_):\n            with store.transaction()? as tx:\n                return tx.log\n        case Postgres(host=_):\n            return \"pg\"\n    return \"\"\n"
    );
    assert_eq!(problems(&matched), Vec::<String>::new());
    let field = format!(
        "{store}def used(holder: Holder) -> str fails Nope:\n    with holder.db.transaction()? as tx:\n        return tx.log\n    return \"\"\n"
    );
    let said = "`transaction` is a `with` source each type in a Lite | Postgres has, called on what `is` can't narrow @ holder.db.transaction()";
    assert_eq!(problems(&field), [said]);
    assert!(helps(&field).iter().any(|help| help.contains("`db = holder.db`")), "{:?}", helps(&field));
}

#[test]
fn a_match_over_a_union_handles_each_member() {
    let empty: [&str; 0] = [];
    let both = "def name(s: Shape) -> str:\n    match s:\n        case Circle(radius): return \"c\"\n        case Square(side): return \"s\"\n";
    assert_eq!(problems(&format!("{SHAPES}{both}")), empty);
    let one = "def name(s: Shape) -> str:\n    match s:\n        case Circle(radius): return \"c\"\n";
    assert_eq!(problems(&format!("{SHAPES}{one}")), ["this `match` doesn't handle `Square` @ s"]);
    let rest = "def name(s: Shape) -> str:\n    match s:\n        case Circle(radius): return \"c\"\n        case _: return \"other\"\n";
    assert_eq!(problems(&format!("{SHAPES}{rest}")), empty);
}

#[test]
fn a_union_has_no_none_no_member_twice_and_no_type_parameter() {
    assert_eq!(
        helps("def f(x: str | None) -> int:\n    return 1\n"),
        ["write `str?`: an optional says it can be missing"]
    );
    assert_eq!(
        helps(
            "struct A:\n    n: int\n\nstruct B:\n    n: int\n\ndef f(x: A | B | None) -> int:\n    return 1\n"
        ),
        ["write `(A | B)?`: an optional says it can be missing"]
    );
    assert_eq!(
        problems("def f(x: int | int) -> int:\n    return 1\n"),
        ["`int` is in this union twice @ int"]
    );
    assert_eq!(
        problems("def f[T](x: T | int) -> int:\n    return 1\n"),
        ["`T` is a type parameter, which can't be part of a union @ T"]
    );
}

#[test]
fn a_union_is_a_key_when_each_type_in_it_is() {
    let empty: [&str; 0] = [];
    let keyed = "counts: dict[int | str, int] = {1: 2, \"a\": 3}\nseen: set[Dot | str] = {Dot(at=1), \"x\"}\nstdout.print(counts, seen)\n";
    assert_eq!(with_shapes(keyed), empty);
    // A float field is fine: it hashes as `==` has it.
    assert_eq!(with_shapes("shapes: dict[Shape, int] = {}\nstdout.print(shapes)\n"), empty);
}

#[test]
fn a_union_fits_an_interface_every_type_in_it_fits() {
    let empty: [&str; 0] = [];
    let interface = "interface HasArea:\n    def area(self) -> float\n\ndef total[T: HasArea](items: list[T]) -> float:\n    return sum(item.area() for item in items)\n\n";
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
