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
            "a function reaches outside itself only through what's in its signature: write `stdout.print(...)`",
        ),
        (
            "console",
            "a function reaches outside itself only through what's in its signature: write `stdout.print(...)`",
        ),
        ("x = true", "write `True` or `False`"),
        ("x = false", "write `True` or `False`"),
        ("x = null", "write `None`"),
        ("x = nil", "write `None`"),
        ("x = none", "write `None`"),
        ("x = undefined", "write `None`"),
        ("lambda", "write an arrow function: `x => x * 2`"),
        ("raise(1)", "Fennec has no exceptions: write `fail SomeError(...)`"),
        ("nothing_here", "give it a value first, as `nothing_here = ...`, or check the spelling"),
    ];
    for (body, help) in hints {
        let program = format!("def main(stdout: Stdout):\n    {body}\n");
        assert_eq!(helps(&program), [help], "{body}");
    }
}

/// `print` is the first thing a newcomer writes, so what it says fits where it's written.
#[test]
fn print_says_what_to_do_where_it_is() {
    const WHY: &str = "a function reaches outside itself only through what's in its signature";
    for (program, help) in [
        (
            "def main():\n    print(1)\n",
            format!("{WHY}, so write `def main(stdout: Stdout):`, then `stdout.print(...)`"),
        ),
        (
            "def report(x: int):\n    print(x)\n\ndef main(stdout: Stdout):\n    report(1)\n",
            format!(
                "{WHY}, so give `report` a `stdout: Stdout` parameter and pass it from `main`, then `stdout.print(...)`"
            ),
        ),
        (
            "test \"it\":\n    print(1)\n",
            format!("{WHY}, so give this test a `(stdout: Stdout)` after its name, then `stdout.print(...)`"),
        ),
        (
            "def report(out: Stdout):\n    print(1)\n\ndef main(stdout: Stdout):\n    report(stdout)\n",
            format!("{WHY}: write `out.print(...)`"),
        ),
    ] {
        assert_eq!(helps(program), [help], "{program}");
    }
    assert_eq!(in_main("print(1)"), ["there's no global `print` @ print"]);
}

/// A capability read by its usual name in a function that wasn't given it says how to give it
/// one, the way a global `print` does, or what it's called when it has one under another name.
#[test]
fn a_capability_that_wasnt_given_says_how_to_give_it() {
    const WHY: &str = "a function reaches outside itself only through what's in its signature";
    for (program, help) in [
        ("def main():\n    stdout.print(1)\n", format!("{WHY}, so write `def main(stdout: Stdout):`")),
        (
            "def main(stdout: Stdout):\n    stdout.print(fs.read(\"x\"))\n",
            format!("{WHY}, so add `fs: Fs` to `main`'s parameters"),
        ),
        (
            "def greet():\n    stdout.print(1)\n\ndef main(stdout: Stdout):\n    greet()\n",
            format!("{WHY}, so give `greet` a `stdout: Stdout` parameter and pass it from `main`"),
        ),
        (
            "def report(out: Stdout):\n    stdout.print(1)\n\ndef main(stdout: Stdout):\n    report(stdout)\n",
            "this function's `Stdout` is called `out`".to_owned(),
        ),
    ] {
        assert_eq!(helps(program), [help], "{program}");
    }
}

/// A name that doesn't exist, spelled nearly like one in sight, is most likely that one, including
/// where `mut` says it's the thing to change.
#[test]
fn a_name_spelled_nearly_like_another_says_so() {
    for (body, help) in [
        ("mut units = [1]\n    grow(ref untis)", "did you mean `units`?"),
        ("mut units = [1]\n    for ref unit in unit:\n        unit += 1", "did you mean `units`?"),
        ("mut units = [1]\n    units.append(unts[0])", "did you mean `units`?"),
        ("stdout.print(growe)", "did you mean `grow`?"),
    ] {
        let program = format!(
            "def grow(ref items: list[int]):\n    items.append(1)\n\ndef main(stdout: Stdout):\n    {body}\n"
        );
        assert_eq!(helps(&program), [help], "{body}");
    }
}

/// A name like nothing in sight guesses at no other name: it says to give it a value.
#[test]
fn a_name_like_nothing_in_sight_guesses_at_none() {
    let none_like = |name: &str| format!("give it a value first, as `{name} = ...`, or check the spelling");
    for (body, name) in [
        ("stdout.print(health)", "health"),
        ("stdout.print(ab)", "ab"),
        ("unit = 1\n    stdout.print(unit, tin)", "tin"),
    ] {
        let program = format!("def main(stdout: Stdout):\n    {body}\n");
        assert_eq!(helps(&program), [none_like(name)], "{body}");
    }
    // What's out of sight isn't suggested, and neither is the standard library.
    let program = "def other():\n    secret = 1\n\ndef main(stdout: Stdout):\n    stdout.print(secrte)\n";
    assert_eq!(helps(program), [none_like("secrte")]);
}

/// `mut` on the wrong argument says which one the function does change.
#[test]
fn mut_on_the_wrong_argument_says_which_one() {
    let program = "def grow(amount: int, ref items: list[int]):\n    items.append(amount)\n\ndef main():\n    mut xs = [1]\n    mut n = 1\n    n += 1\n    grow(ref n, xs)\n";
    let helps = helps(program);
    assert!(
        helps.iter().any(|help| help == "did you mean to put `ref` on `items`, which `grow` does change?"),
        "{helps:?}"
    );
    let nothing_changes =
        "def grow(amount: int):\n    amount + 1\n\ndef main():\n    mut n = 1\n    n += 1\n    grow(ref n)\n";
    assert!(!crate::helps(nothing_changes).iter().any(|help| help.contains("did you mean")));
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
            assert items.len() >= 0
            assert missing.len() >= 0
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
                return Point(x=self.x + 1)

            def moved(self) -> Point:
                return self

        extend Point:
            def far(self) -> int:
                return self.moved().x + 'a'

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
                return self.item == item

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

const ACCOUNT: &str = "struct Account:
    owner: str
    _balance = 0

    def deposit(ref self, amount: int):
        self._balance += amount

    def _audit(self) -> bool:
        return self._balance >= 0

    def same(self, other: Account) -> bool:
        return other._balance == self._balance and self._audit()

    def opened(owner: str, start: int) -> Account:
        return Account(owner=owner, _balance=start)

";

#[test]
fn a_types_own_methods_reach_its_underscore_names() {
    let text = format!(
        "{ACCOUNT}def main(stdout: Stdout):\n    mut account = Account.opened(\"ash\", 5)\n    \
         account.deposit(3)\n    stdout.print(account.same(Account(\"kit\")), Account(owner=\"vex\").owner)\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
}

#[test]
fn nothing_outside_a_type_reaches_its_underscore_names() {
    let in_main = |body: &str| {
        problems(&format!("{ACCOUNT}def main(stdout: Stdout):\n    mut account = Account(\"ash\")\n{body}"))
    };
    let private = "`_balance` is private to `Account`";
    assert_eq!(in_main("    stdout.print(account._balance)\n"), [format!("{private} @ account._balance")]);
    assert_eq!(in_main("    account._balance = 1\n"), [format!("{private} @ account._balance")]);
    assert_eq!(
        in_main("    stdout.print(account._audit())\n"),
        ["`_audit` is private to `Account` @ account._audit()"]
    );
    assert_eq!(in_main("    forged = Account(owner=\"x\", _balance=100)\n"), [format!("{private} @ 100")]);
    // By position, values go to the public fields alone, so there's none left for `100`.
    assert_eq!(in_main("    forged = Account(\"x\", 100)\n"), ["`Account` takes 1 value(s) @ 100"]);
    assert_eq!(in_main("    forged = copy(account, _balance=9)\n"), [format!("{private} @ 9")]);
    assert_eq!(
        in_main("    match account:\n        case Account(owner, balance): stdout.print(balance)\n"),
        [format!("{private} @ case Account(owner, balance): stdout.print(balance)\n")]
    );
    assert_eq!(
        problems(&format!(
            "{ACCOUNT}extend Account:\n    def peek(self) -> int:\n        return self._balance\n"
        )),
        [format!("{private} @ self._balance")]
    );
}

/// From outside a type, its automatic constructor sets its public fields, and its `_` fields
/// take their defaults. One with no default is set by a named constructor, a method without
/// `self` that builds one, and a type with neither can't be built at all.
#[test]
fn a_private_field_with_no_default_needs_a_named_constructor() {
    let account = "struct Account:
    _id: int
    owner: str
    _audit = list[str]()

    def open(owner: str, id: int) -> Account:
        return Account(_id=id, owner=owner)

struct Counter:
    step: int
    _count = 0

";
    let in_main = |body: &str| problems(&format!("{account}def main(stdout: Stdout):\n{body}"));
    assert_eq!(
        in_main("    stdout.print(Account.open(\"ana\", 7).owner, Counter(2).step)\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        in_main("    stdout.print(Account(owner=\"bo\").owner)\n"),
        [
            "`Account` is built through its named constructors: only its own methods can set `_id` @ Account(owner=\"bo\")"
        ]
    );
    assert_eq!(
        helps(&format!(
            "{account}def main(stdout: Stdout):\n    stdout.print(Account(owner=\"bo\").owner)\n"
        )),
        ["use `Account.open(...)`"]
    );
    let sealed = "struct Sealed:\n    _key: str\n\n    def key(self) -> str:\n        return self._key\n";
    assert_eq!(
        problems(sealed),
        [
            "nothing can build `Sealed`: `_key` has no default, and `Sealed` has no named constructor @ struct Sealed"
        ]
    );
}

#[test]
fn a_type_another_std_module_imports_still_needs_its_own_import() {
    // `std.net` imports `Duration` for its own use, and that doesn't make it a program's.
    assert_eq!(
        problems("def wait(pause: Duration) -> int:\n    return 1\n"),
        ["`Duration` doesn't exist @ Duration"]
    );
    assert_eq!(
        helps("def wait(pause: Duration) -> int:\n    return 1\n"),
        ["import it from std.time, like `from std.time import Duration`"]
    );
    let imported =
        "from std.time import Duration\n\ndef wait(pause: Duration) -> int:\n    return pause.nanos\n";
    assert_eq!(problems(imported), Vec::<String>::new());
}

/// A value nobody uses is an error, since dropping it is usually a mistake, like `sorted(xs)`
/// for `xs.sort()`. `_ = ...` throws one away on purpose, and a function's last line is what it
/// gives back.
#[test]
fn a_value_nobody_uses_is_an_error() {
    let empty: [&str; 0] = [];
    assert_eq!(
        in_main("xs = [3, 1]\nsorted(xs)\nlen(xs)\nstdout.print(xs)\n"),
        [
            "this gives back a list[int], and nothing uses it @ sorted(xs)",
            "this gives back an int, and nothing uses it @ len(xs)",
        ]
    );
    assert_eq!(
        in_main("mut xs = [3, 1]\n_ = sorted(xs)\nxs.sort()\nxs.append(2)\nstdout.print(xs)\n"),
        empty
    );
    assert_eq!(problems("def last(xs: list[int]) -> int:\n    return len(xs)\n"), empty);
    assert_eq!(
        in_main("mut xs = [1]\nfor x in [1, 2]:\n    xs.pop()\nstdout.print(xs)\n"),
        ["this gives back an int?, and nothing uses it @ xs.pop()"]
    );
}

#[test]
fn each_underscore_throws_away_what_its_given_whatever_its_type() {
    let empty: [&str; 0] = [];
    let body = "mut numbers = [1]\nmut words = [\"a\"]\n_ = numbers.pop()\n_ = words.pop()\n_, one = (1, \"x\")\n_, two = (\"y\", 2)\nstdout.print(numbers, words, one, two)\n";
    assert_eq!(in_main(body), empty);
    let read = in_main("_ = 1\nstdout.print(_)\n");
    assert_eq!(read, ["`_` throws away what it's given, so it can't be read @ _"]);
}

/// A top-level name is private to its kit already, so `_` at its start is a mistake, for a
/// function, a type, an interface, an alias or a constant. A field's or method's `_` is fine.
#[test]
fn a_top_level_name_doesnt_start_with_an_underscore() {
    let source = "_LIMIT = 3\n\nstruct _Box:\n    _size = 1\n\n    def _grown(self) -> int:\n        return self._size + _LIMIT\n\ninterface _Sized:\n    def size(self) -> int\n\ntype _Count = int\n\ndef _helper() -> _Box:\n    return _Box()\n\ndef main(stdout: Stdout):\n    stdout.print(_helper()._size)\n";
    let found = problems(source);
    let named: Vec<&str> = found.iter().filter_map(|problem| problem.split('`').nth(1)).collect();
    assert_eq!(named, ["_LIMIT", "_Box", "_Sized", "_Count", "_helper", "_size"], "{found:?}");
    assert!(found[0].starts_with("`_LIMIT` is a top-level name, so it doesn't start with `_`"), "{found:?}");
    assert_eq!(
        helps("_LIMIT = 3\n\ndef main(stdout: Stdout):\n    stdout.print(_LIMIT)\n"),
        ["top-level names are already private to the kit; `_` marks a private field or method"]
    );
}

/// The standard library is one kit: its modules use each other's helpers, which a program can't
/// import, or reach through the module, since they aren't exported. Nothing in it starts with `_`
/// at the top level either, which every check would report.
#[test]
fn a_program_reaches_only_what_the_standard_library_exports() {
    let hidden = "`std.json` has nothing called `article`";
    let imported = problems(
        "from std.json import article\n\ndef main(stdout: Stdout):\n    stdout.print(article(\"x\"))\n",
    );
    assert!(imported.first().is_some_and(|problem| problem.starts_with(hidden)), "{imported:?}");
    let reached = problems(
        "from std import json\n\ndef main(stdout: Stdout):\n    stdout.print(json.article(\"x\"))\n",
    );
    assert!(reached.first().is_some_and(|problem| problem.starts_with(hidden)), "{reached:?}");
    let empty: [&str; 0] = [];
    assert_eq!(
        problems("from std.json import dumps\n\ndef main(stdout: Stdout):\n    stdout.print(dumps([1]))\n"),
        empty
    );
}

/// A type defines an operator once for each type on its right, and a use of it calls the one for
/// exactly that type. A second for the same type is refused, and a named method still has one
/// meaning.
#[test]
fn an_operator_is_defined_once_for_each_type_on_its_right() {
    let program = "
        struct Meters:
            value: float

            def /(self, other: float) -> Meters:
                return Meters(value=self.value / other)

            def /(self, other: Meters) -> float:
                return self.value / other.value

            def /(self, other: Meters) -> int:
                return 1

            def half(self) -> Meters:
                return self / 2.0

        def main(stdout: Stdout):
            ratio: float = Meters(value=6.0) / Meters(value=3.0)
            scaled: Meters = Meters(value=6.0) / 2.0
            stdout.print(ratio, scaled.value, Meters(value=1.0).half().value)
    ";
    assert_eq!(
        problems(program),
        ["`Meters` already has a `/` taking Meters on its right @ def /(self, other: Meters) -> int"]
    );
}
