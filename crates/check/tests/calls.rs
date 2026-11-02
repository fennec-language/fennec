//! Calls and construction: arguments matched to parameters and fields, methods and enum
//! cases that exist, and builtins called the right way.

use crate::{helps, in_main, problems};

const TYPES: &str = "
struct Enemy:
    name: str
    health = 100

    def at(name: str):
        Enemy(name=name)

    def zero():
        Enemy(name='zero')

    def heal(self, amount: int):
        self.health + amount

enum Shape:
    Circle(radius: float)
    Rect(width: float, height: float)
    Point

enum Team:
    Point
";

fn with_types(body: &str) -> Vec<String> {
    crate::in_main_after(TYPES, body)
}

#[test]
fn arguments_match_parameters() {
    let program = "
        def area(width: float, height: float):
            width * height

        def main(stdout: Stdout):
            area(1.0, 2.0)
            area(height=1.0, width=2.0)
            area(1.0, 2.0, 3.0)
            area(1.0)
            area(1.0, width=2.0)
            area(1.0, deep=2.0)
    ";
    assert_eq!(
        problems(program),
        [
            "`area` takes 2 argument(s) @ 3.0",
            "`area` is missing its `height` argument @ area(1.0)",
            "`area` is missing its `height` argument @ area(1.0, width=2.0)",
            "`width` is given twice @ 2.0",
            "`area` has no parameter called `deep` @ 2.0",
        ]
    );
    assert_eq!(helps("def f():\n    1\n\ndef g():\n    f(1)\n"), ["`f` takes no arguments"]);
    assert_eq!(helps("def f(a: int, b: int):\n    a\n\ndef g():\n    f(c=1)\n"), ["`f` takes `a`, `b`"]);
}

#[test]
fn arguments_have_the_parameters_types() {
    let program = "
        def scale(speed: float, items: list[int], name: str?):
            speed

        def main(stdout: Stdout):
            scale(1, [1], None)
            scale(1.0, ['a'], 'x')
            scale(1.0, [], 3)
            scale(speed=1.0, items=[1], name=None)
    ";
    assert_eq!(
        problems(program),
        [
            "`speed` should be a float, but this is an int @ 1",
            "`items` should be a list[int], but this is a list[str] @ ['a']".replace('\'', "\"").as_str(),
            "`name` should be a str?, but this is an int @ 3",
        ]
    );
    assert_eq!(
        helps("def f(n: int):\n    n\n\ndef g():\n    f(1.5)\n"),
        ["convert it with int(...), round(...) or math.floor(...)"]
    );
    assert_eq!(helps("def f(n: str):\n    n\n\ndef g():\n    f(1.5)\n"), [""]);
    assert_eq!(
        helps("def f(n: float):\n    n\n\ndef g():\n    f(1)\n"),
        ["write it with a decimal point, like `1.0`, or convert it with float(...)"]
    );
}

#[test]
fn structs_get_every_field_without_a_default() {
    let program = "
        Enemy(name='Ash')
        Enemy('Ash', 50)
        Enemy(health=5)
        Enemy(name='Ash', hp=5)
        Enemy('Ash', 50, 3)
        Enemy('Ash', name='Kit')
        Enemy(name=5)
    ";
    assert_eq!(
        with_types(program),
        [
            "`Enemy` needs a value for `name` @ Enemy(health=5)",
            "`Enemy` has no field called `hp` @ 5",
            "`Enemy` takes 2 value(s) @ 3",
            "this field is given twice @ 'Kit'".replace('\'', "\"").as_str(),
            "`name` should be a str, but this is an int @ 5",
        ]
    );
    assert_eq!(helps("struct A:\n    n: int\n\ndef f():\n    A()\n"), ["pass it, like `A(n=...)`"]);
    assert_eq!(helps("struct A:\n    n: int\n\ndef f():\n    A(m=1)\n"), ["`A` takes `n`"]);
}

#[test]
fn enum_cases_are_built_by_name() {
    let program = "
        shapes = [.Circle(radius=1.0), Shape.Rect(1.0, 2.0), Shape.Point, .Rect(width=1.0)]
        Shape()
        .Triangle
        .Point
        Shape.Hexagon(1)
        .Circle(radius=1)
    ";
    assert_eq!(
        with_types(program),
        [
            "`Shape.Rect` needs a value for `height` @ .Rect(width=1.0)",
            "`Shape` is an enum, so pick one of its cases @ Shape()",
            "no enum has a case called `Triangle` @ .Triangle",
            "more than one enum has a case called `Point` @ .Point",
            "`Shape` has nothing called `Hexagon` @ Shape.Hexagon(1)",
            "`radius` should be a float, but this is an int @ 1",
        ]
    );
    let help = helps(&format!("{TYPES}\ndef f():\n    Shape()\n    .Point\n"));
    assert_eq!(
        help,
        [
            "write one of Shape.Circle, Shape.Rect, Shape.Point",
            "say which one: `Shape.Point` or `Team.Point`"
        ]
    );
}

#[test]
fn named_constructors_and_methods_exist() {
    let program = "
        a = Enemy.at('Ash')
        b = Enemy.zero
        c = Enemy.zero()
        stdout.print(a.heal(1), b.heal(1), c.name)
        a.explode()
        Enemy.heal(1)
        Enemy.at
        a.heal()
        a.heal(1, 2)
    ";
    assert_eq!(
        with_types(program),
        [
            "`Enemy` has no method `explode` @ a.explode()",
            "`heal` needs a `Enemy` to call it on @ Enemy.heal(1)",
            "`Enemy` has nothing called `at` @ Enemy.at",
            "`heal` is missing its `amount` argument @ a.heal()",
            "`heal` takes 1 argument(s) @ 2",
        ]
    );
}

#[test]
fn fields_exist_on_structs_and_errors() {
    let program = "
        error Missing:
            path: str

        struct Box:
            size = 1

        def main(stdout: Stdout):
            stdout.print(Box().size, Box().weight, Missing('x').path, Missing('x').file)
            one = 1
            stdout.print(stdout.name, one.real, Box().size.real)
    ";
    assert_eq!(
        problems(program),
        [
            "`Box` has no field `weight` @ Box().weight",
            "`Missing` has no field `file` @ Missing('x').file".replace('\'', "\"").as_str(),
            "`Stdout` has no field `name` @ stdout.name",
            "an int has no field `real` @ one.real",
            "an int has no field `real` @ Box().size.real",
        ]
    );
}

#[test]
fn builtin_values_have_their_methods() {
    let program = "
        mut queue = ['a']
        queue.push('b')
        'text'.trim()
        'text'.to_uppercase()
        'text'.toLowerCase()
        stdout.log(1)
        queue.copy()
        queue.size()
        (1, 2).first()
    ";
    assert_eq!(
        in_main(program),
        [
            "a list has no method `push` @ queue.push('b')".replace('\'', "\"").as_str(),
            "a str has no method `trim` @ 'text'.trim()".replace('\'', "\"").as_str(),
            "a str has no method `to_uppercase` @ 'text'.to_uppercase()".replace('\'', "\"").as_str(),
            "a str has no method `toLowerCase` @ 'text'.toLowerCase()".replace('\'', "\"").as_str(),
            "`Stdout` has no method `log` @ stdout.log(1)",
            "a list has no method `copy` @ queue.copy()",
            "a list has no method `size` @ queue.size()",
            "a tuple has no method `first` @ (1, 2).first()",
        ]
    );
    let hints = helps(
        "def main(stdout: Stdout):\n    mut q = [1]\n    q.push(1)\n    'a'.trim()\n    'a'.to_uppercase()\n    'a'.toLowerCase()\n    stdout.log(1)\n    q.copy()\n    q.size()\n    q.first()\n",
    );
    assert_eq!(
        hints,
        [
            "use `append`",
            "use `strip()`",
            "use `upper()`",
            "use `lower()`",
            "use `stdout.print(...)`",
            "write `copy(value)`",
            "use `.len()`",
            "",
        ]
    );
}

#[test]
fn builtin_methods_take_the_right_arguments() {
    let program = "
        mut items = [3, 1]
        items.sort(key=n => -n)
        items.append(1, 2)
        items.insert(1)
        items.pop(key=1)
        items.count(by=1)
        'a b'.split(' ', 1)
        stdout.print(1, 2, 3)
    ";
    assert_eq!(
        in_main(program),
        [
            "`append` was called with the wrong arguments @ items.append(1, 2)",
            "`insert` was called with the wrong arguments @ items.insert(1)",
            "`pop` doesn't take `key` @ items.pop(key=1)",
            "`count` has no parameter called `by` @ 1",
            "`split` was called with the wrong arguments @ 'a b'.split(' ', 1)".replace('\'', "\"").as_str(),
        ]
    );
}

#[test]
fn builtins_take_the_right_arguments() {
    let program = "
        [1].len(2)
        sorted([1], key=n => n, reverse=True)
        sum([1], key=n => n)
        range(1, 2, 3, 4)
        range('a')
        5.len()
        zip([1])
        set()
        round(1.5, 2)
        any([1])
        ignore(1)
    ";
    assert_eq!(
        in_main(program),
        [
            "`len` was called with the wrong arguments @ [1].len(2)",
            "`sorted` has no parameter called `reverse` @ True",
            "`sum` doesn't take `key` @ sum([1], key=n => n)",
            "`range` was called with the wrong arguments @ range(1, 2, 3, 4)",
            "`range` takes one to three ints @ range('a')".replace('\'', "\"").as_str(),
            "an int has no method `len` @ 5.len()",
            "`zip` was called with the wrong arguments @ zip([1])",
            "`any` needs bools, but got an int @ any([1])",
            "`ignore` was called with the wrong arguments @ ignore(1)",
        ]
    );
}

#[test]
fn replace_changes_fields_that_exist() {
    let program = "
        enemy = Enemy(name='Ash')
        replace(enemy, health=5)
        replace(enemy, hp=5)
        replace(enemy, health='x')
        replace()
    ";
    assert_eq!(
        with_types(program),
        [
            "`Enemy` has no field called `hp` @ 5",
            "`health` should be an int, but this is a str @ 'x'".replace('\'', "\"").as_str(),
            "`replace` takes a struct value, then the fields to change @ replace()",
        ]
    );
}

#[test]
fn builtin_types_are_built_the_right_way() {
    let program = "
        a = IdMap[Enemy]()
        b = SoaList[Enemy]()
        c = Path('/tmp')
        d = IdMap(1)
        e = Path(1)
        f = Id()
    ";
    let imports = "from std.game import Id, IdMap\nfrom std.layout import SoaList\nfrom std.path import Path";
    assert_eq!(
        crate::in_main_after(&format!("{imports}\n{TYPES}"), program),
        [
            "`IdMap` was called with the wrong arguments @ IdMap(1)",
            "`text` should be a str, but this is an int @ 1",
            "`Id` was called with the wrong arguments @ Id()",
        ]
    );
}

#[test]
fn values_held_in_names_can_be_called() {
    let program = "
        def double(n: int):
            n * 2

        def main(stdout: Stdout):
            twice = double
            stdout.print(twice(1), twice(1, 2))
            add = (a, b) => a + b
            stdout.print(add(1, 2, 3))
    ";
    assert_eq!(
        problems(program),
        [
            "`double` takes 1 argument(s) @ 2",
            "can't tell the type of `a` @ (a, b) => a + b",
            "can't tell the type of `b` @ (a, b) => a + b",
            "`this function` takes 2 argument(s) @ 3",
        ]
    );
}

#[test]
fn result_expect_needs_a_reason() {
    let program = "
        error Oops:
            why: str

        def risky(n: int):
            if n > 1:
                fail Oops('big')
            n

        def main(stdout: Stdout):
            stdout.print(risky(1).expect('it is small') + 1)
            risky(1).expect()
            risky(1).expect(5)
    ";
    assert_eq!(
        problems(program),
        [
            "`expect` needs a reason this can't fail @ risky(1).expect()",
            "`expect` needs a reason this can't fail @ risky(1).expect(5)",
        ]
    );
}

#[test]
fn collections_and_tuples_match_their_types_item_by_item() {
    let program = "
        def f(table: dict[str, int], pair: (int, str), maybe: str?):
            pass

        def main(stdout: Stdout):
            f({'a': 1}, (1, 'a'), 'x')
            f({}, (1, 'b'), None)
            f({1: 1}, (1, 2), [1].index(1))
            f({'a': 'b'}, (1,), None)
    ";
    assert_eq!(
        problems(program),
        [
            "`table` should be a dict[str, int], but this is a dict[int, int] @ {1: 1}",
            "`pair` should be a (int, str), but this is a (int, int) @ (1, 2)",
            "`maybe` should be a str?, but this is an int? @ [1].index(1)",
            "`table` should be a dict[str, int], but this is a dict[str, str] @ {\"a\": \"b\"}",
            "`pair` should be a (int, str), but this is a (int) @ (1,)",
        ]
    );
}

#[test]
fn types_and_builtins_are_not_values() {
    let program = "
        struct Enemy:
            health = 100

        enum Shape:
            Point

        def main(stdout: Stdout):
            measure = sum
            kind = Enemy
            stdout.print(mystery.field, Shape.Hexagon)
    ";
    assert_eq!(
        problems(program),
        [
            "`sum` can only be called @ sum",
            "`Enemy` is a type, not a value @ Enemy",
            "`mystery` doesn't exist @ mystery",
            "`Shape` has nothing called `Hexagon` @ Shape.Hexagon",
        ]
    );
}

const MONEY: &str = "
struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        Money(cents=self.cents + other.cents)

struct Flag:
    on: bool

struct Worth:
    cents: int

    def +(self, other: Worth) -> float:
        1.5

struct Tip:
    cents: int

    def +(self, other: int) -> Tip:
        self
";

fn sum_problems(call: &str) -> Vec<String> {
    problems(&format!(
        "{MONEY}\ndef main(stdout: Stdout):\n    wallet = [Money(cents=1)]\n    stdout.print({call})\n"
    ))
}

#[test]
fn sum_needs_nothing_for_numbers_and_a_start_for_the_rest() {
    for fine in [
        "sum([1, 2])",
        "sum([1.5])",
        "sum([1], start=10)",
        "sum([1.5], start=0.5)",
        "sum(wallet, start=Money(cents=0))",
        "sum(m for m in wallet, start=Money(cents=0))",
        "sum([], start=0.0)",
        "sum(['a'], start='')",
        "sum([[1]], start=[])",
        "sum([], start=Money(cents=0))",
    ] {
        assert_eq!(sum_problems(fine), Vec::<String>::new(), "{fine}");
    }
    assert_eq!(
        sum_problems("sum(wallet)"),
        ["`sum` of `Money` needs a starting value: sum(wallet, start=Money(cents=0)) @ sum(wallet)"]
    );
    assert_eq!(
        sum_problems("sum(m for m in wallet)"),
        ["`sum` of `Money` needs a starting value: sum(..., start=Money(cents=0)) @ sum(m for m in wallet)"]
    );
    assert_eq!(
        sum_problems("sum([[1]])"),
        ["`sum` of `list[int]` needs a starting value: sum(..., start=[]) @ sum([[1]])"]
    );
}

#[test]
fn sum_finds_out_late_that_its_items_need_a_start() {
    let program = format!(
        "{MONEY}
def main(stdout: Stdout):
    mut wallet = []
    stdout.print(sum(wallet))
    wallet.append(Money(cents=1))
"
    );
    assert_eq!(
        problems(&program),
        ["`sum` of `Money` needs a starting value: sum(wallet, start=Money(cents=0)) @ sum(wallet)"]
    );
}

#[test]
fn sum_needs_a_plus_that_adds_two_items() {
    assert_eq!(
        sum_problems("sum([Flag(on=True)], start=Flag(on=False))"),
        ["`sum` adds with `+`, but `Flag` has no `def +` @ sum([Flag(on=True)], start=Flag(on=False))"]
    );
    assert_eq!(
        sum_problems("sum([Flag(on=True)])"),
        ["`sum` adds with `+`, but `Flag` has no `def +` @ sum([Flag(on=True)])"]
    );
    assert_eq!(sum_problems("sum([True])"), ["`sum` adds with `+`, but `bool` has no `+` @ sum([True])"]);
    assert_eq!(
        sum_problems("sum([Worth(cents=1)], start=Worth(cents=0))"),
        [
            "`sum` needs `+` to give back `Worth`, but `Worth`'s gives a float @ sum([Worth(cents=1)], start=Worth(cents=0))"
        ]
    );
    assert_eq!(
        sum_problems("sum([Tip(cents=1)], start=Tip(cents=0))"),
        ["`other` should be an int, but this is a Tip @ sum([Tip(cents=1)], start=Tip(cents=0))"]
    );
}

#[test]
fn sums_start_has_to_be_an_item() {
    assert_eq!(
        sum_problems("sum(wallet, start=0)"),
        ["`sum`'s start should be a Money, but this is an int @ 0"]
    );
    assert_eq!(
        sum_problems("sum([1.5], start=0)"),
        ["`sum`'s start should be a float, but this is an int @ 0"]
    );
    assert_eq!(
        sum_problems("sum([1], 2, 3)"),
        ["`sum` was called with the wrong arguments @ sum([1], 2, 3)"]
    );
}

#[test]
fn sum_of_a_type_parameter_needs_a_start_and_a_bound_with_plus() {
    let program = "
        interface Addable:
            def +(self, other: Self) -> Self

        interface Named:
            def name(self) -> str

        def total[T: Addable](items: list[T], start: T) -> T:
            sum(items, start=start)

        def bare[T: Addable](items: list[T]) -> T:
            sum(items)

        def named[T: Named](items: list[T], start: T) -> T:
            sum(items, start=start)
    ";
    assert_eq!(
        problems(program),
        [
            "`sum` of `T` needs a starting value: sum(items, start=...) @ sum(items)",
            "`sum` adds with `+`, but `T` has no `def +` @ sum(items, start=start)",
        ]
    );
}

#[test]
fn sum_hints_a_start_from_the_required_fields() {
    let program = "
        struct Bag:
            name: str
            weight: float
            flag: bool
            label = \"bag\"

            def +(self, other: Bag) -> Bag:
                self

        def main(stdout: Stdout):
            stdout.print(sum([Bag(name='a', weight=1.0, flag=True)]))
    ";
    let [problem] = problems(&program.replace('\'', "\"")).try_into().unwrap();
    assert!(
        problem.starts_with(
            "`sum` of `Bag` needs a starting value: sum(..., start=Bag(name=\"\", weight=0.0, flag=...)) @"
        ),
        "{problem}"
    );
}

#[test]
fn sums_error_says_what_to_add() {
    let program = format!(
        "{MONEY}\ndef main(stdout: Stdout):\n    stdout.print(sum([Flag(on=True)], start=Flag(on=False)))\n"
    );
    assert_eq!(helps(&program), ["add `def +(self, other: Flag) -> Flag` to `Flag`"]);
}

#[test]
fn only_sum_takes_a_start() {
    let text = "def main(stdout: Stdout):\n    stdout.print(max([1], start=1))\n";
    assert_eq!(problems(text), ["`max` has no parameter called `start` @ 1"]);
}

#[test]
fn a_literal_default_gives_its_type() {
    let text = "enum Mode:\n    Fast\n    Slow\n\ndef scan(limit=8, ratio=-0.5, name='x', on=True, mode=Mode.Fast) -> str:\n    \
        f'{limit + 1} {ratio * 2.0} {name.upper()} {not on} {mode}'\n\ndef main(stdout: Stdout):\n    \
        stdout.print(scan(limit=2, mode=Mode.Slow))\n";
    let empty: [&str; 0] = [];
    assert_eq!(problems(text), empty);
    let wrong = text.replace("scan(limit=2", "scan(limit='two'");
    assert!(problems(&wrong).iter().any(|problem| problem.contains("`limit`")), "{:?}", problems(&wrong));
}

#[test]
fn writing_the_type_a_literal_default_gives_is_an_error() {
    let text = "def scan(limit: int = 8, label: str? = None, items = list[int]()) -> int:\n    limit\n";
    assert_eq!(problems(text), ["`limit` takes its type from its default, so `: int` isn't needed @ int"]);
}

#[test]
fn a_default_that_isnt_a_literal_needs_its_type() {
    let text = "def scan(items=[], nothing=None) -> int:\n    1\n";
    assert_eq!(
        problems(text),
        [
            "can't tell the type of `items` from its default @ []",
            "can't tell the type of `nothing` from its default @ None",
        ]
    );
}
