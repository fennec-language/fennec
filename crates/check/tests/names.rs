//! Unknown names, with hints for habits from other languages, and where names exist.

use crate::{helps, in_main, problems};

#[test]
fn unknown_names_are_reported_where_they_are_used() {
    assert_eq!(in_main("stdout.print(score)"), ["`score` doesn't exist @ score"]);
    assert_eq!(in_main("total = count + 1"), ["`count` doesn't exist @ count"]);
}

#[test]
fn habits_from_other_languages_get_a_hint() {
    let hints = [
        (
            "print(1)",
            "there's no global output: write with the `stdout` that `main` receives, like `stdout.print(x)`",
        ),
        (
            "console",
            "there's no global output: write with the `stdout` that `main` receives, like `stdout.print(x)`",
        ),
        ("x = true", "write `True` or `False`"),
        ("x = false", "write `True` or `False`"),
        ("x = null", "write `None`"),
        ("x = nil", "write `None`"),
        ("x = none", "write `None`"),
        ("x = undefined", "write `None`"),
        ("lambda", "write an arrow function: `x => x * 2`"),
        ("raise(1)", "Fennec has no exceptions: write `fail SomeError(...)`"),
        ("nothing_here", ""),
    ];
    for (body, help) in hints {
        let program = format!("def main(stdout: Stdout):\n    {body}\n");
        assert_eq!(helps(&program), [help], "{body}");
    }
}

#[test]
fn functions_types_builtins_and_constants_exist_everywhere() {
    let program = "
        from std.math import PI, sqrt

        def double(n: int):
            n * 2

        struct Point:
            x = 0

        def main(stdout: Stdout):
            stdout.print(later(), double(2), Point(), len([1]), PI, sqrt(2.0), zip([1], [2]))
            stdout.print(Point.x if False else 0)

        def later():
            1
    ";
    assert_eq!(problems(program), ["`Point` has nothing called `x` @ Point.x"]);
}

#[test]
fn names_belong_to_the_block_they_are_made_in() {
    let program = "
        if True:
            inside = 1
        stdout.print(inside)
    ";
    assert_eq!(in_main(program), ["`inside` doesn't exist @ inside"]);
    let program = "
        for n in [1]:
            seen = n
        while False:
            looped = 1
        with 1 as held:
            pass
        stdout.print(n, seen, looped, held)
    ";
    let missing: Vec<String> = ["n", "seen", "looped", "held"]
        .iter()
        .map(|name| format!("`{name}` doesn't exist @ {name}"))
        .collect();
    assert_eq!(in_main(program), missing);
}

#[test]
fn a_name_is_only_known_after_it_is_made() {
    let program = "
        stdout.print(early)
        early = 1
    ";
    assert_eq!(in_main(program), ["`early` doesn't exist @ early"]);
}

#[test]
fn comprehension_and_match_names_stay_inside() {
    let program = "
        squares = [n * n for n in [1, 2] if n > 0]
        kind = match 3:
            case other: other
        stdout.print(squares, kind, n, other)
    ";
    assert_eq!(in_main(program), ["`n` doesn't exist @ n", "`other` doesn't exist @ other"]);
}

#[test]
fn nested_functions_see_the_names_before_them() {
    let program = "
        def outer(start: int):
            def add(n: int):
                n + start + later
            later = 1
            add(1)
    ";
    assert_eq!(problems(program), ["`later` doesn't exist @ later"]);
    let program = "
        def outer(start: int):
            def add(n: int):
                n + start
            [add(n) for n in [1, 2]]
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn arrow_function_parameters_exist_inside_them() {
    let program = "
        names = ['a']
        stdout.print(sorted(names, key=name => len(name)), max(names, key=(name) => size))
    ";
    assert_eq!(in_main(program), ["`size` doesn't exist @ size"]);
}

#[test]
fn test_inputs_are_names_in_the_test() {
    let program = "
        test \"reversing\" for items: list[int]:
            assert len(items) >= 0
            assert len(missing) >= 0
    ";
    assert_eq!(problems(program), ["`missing` doesn't exist @ missing"]);
}

#[test]
fn code_after_leaving_early_is_still_checked() {
    let program = "
        def f(n: int):
            return n
            unknown
    ";
    assert_eq!(problems(program), ["`unknown` doesn't exist @ unknown"]);
}

#[test]
fn f_strings_and_collections_are_checked_inside() {
    let program = "
        stdout.print(f\"{a}\", [b], (c, 1), {d}, {e: 1}, {1: g})
    ";
    let missing: Vec<String> = ["a", "b", "c", "d", "e", "g"]
        .iter()
        .map(|name| format!("`{name}` doesn't exist @ {name}"))
        .collect();
    assert_eq!(in_main(program), missing);
}

#[test]
fn extensions_add_methods_to_the_programs_own_types() {
    let program = "
        struct Point:
            x = 0

            def moved(self) -> Point:
                Point(x=self.x + 1)

            def moved(self) -> Point:
                self

        extend Point:
            def far(self) -> int:
                self.moved().x + 'a'

        interface Shape:
            def area(self) -> float

        extend Shape:
            pass

        extend int:
            pass

        extend Nowhere:
            pass

        def main(stdout: Stdout):
            stdout.print(Point().far())
    ";
    assert_eq!(
        problems(program),
        [
            "`Point` already has a method called `moved` @ def moved(self) -> Point",
            "can't use `+` on an int and a str @ self.moved().x + \"a\"",
            "`Shape` is an interface, so it can't be extended @ extend Shape",
            "`int` is built in, so it can't be extended @ extend int",
            "`Nowhere` isn't a type @ extend Nowhere",
        ]
    );
}

#[test]
fn a_generic_types_extension_names_its_parameters_and_bounds_make_it_conditional() {
    let program = "
        struct Box[T]:
            item: T

        struct Point:
            x = 0

        extend Box[U]:
            pass

        extend Point[T]:
            pass

        extend Box[T: Eq]:
            def holds(self, item: T) -> bool:
                self.item == item

        extend Box[T: Enemy]:
            pass

        def main(stdout: Stdout):
            stdout.print(Box(item=1).holds(1) + 1, Box(item=() => 1).holds(() => 2))
            stdout.print([1, 2].count(2) + 'a', [1].index(1) + 1)
    ";
    assert_eq!(
        problems(program),
        [
            "`Box` is generic, so its extension names its type parameters @ extend Box[U]",
            "`Point` isn't generic, so its extension takes no type parameters @ extend Point[T]",
            "`Enemy` isn't an interface @ Enemy",
            "can't use `+` on a bool and an int @ Box(item=1).holds(1) + 1",
            "`holds` needs `T` to fit `Eq`, but here `T` is a () -> int @ Box(item=() => 1).holds(() => 2)",
            "can't use `+` on an int and a str @ [1, 2].count(2) + \"a\"",
            "this might be None @ [1].index(1)",
        ]
    );
}
