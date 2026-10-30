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
    assert_eq!(problems(program), [
        "`area` takes 2 argument(s) @ 3.0",
        "`area` is missing its `height` argument @ area(1.0)",
        "`area` is missing its `height` argument @ area(1.0, width=2.0)",
        "`width` is given twice @ 2.0",
        "`area` has no parameter called `deep` @ 2.0",
    ]);
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
    assert_eq!(problems(program), [
        "`speed` should be a float, but this is an int @ 1",
        "`items` should be a list[int], but this is a list[str] @ ['a']".replace('\'', "\"").as_str(),
        "`name` should be a str?, but this is an int @ 3",
    ]);
    assert_eq!(helps("def f(n: int):\n    n\n\ndef g():\n    f(1.5)\n"), [
        "convert it with int(...), round(...) or floor(...)"
    ]);
    assert_eq!(helps("def f(n: str):\n    n\n\ndef g():\n    f(1.5)\n"), [""]);
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
    assert_eq!(with_types(program), [
        "`Enemy` needs a value for `name` @ Enemy(health=5)",
        "`Enemy` has no field called `hp` @ 5",
        "`Enemy` takes 2 value(s) @ 3",
        "this field is given twice @ 'Kit'".replace('\'', "\"").as_str(),
        "`name` should be a str, but this is an int @ 5",
    ]);
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
    assert_eq!(with_types(program), [
        "`Shape.Rect` needs a value for `height` @ .Rect(width=1.0)",
        "`Shape` is an enum, so pick one of its cases @ Shape()",
        "no enum has a case called `Triangle` @ .Triangle",
        "more than one enum has a case called `Point` @ .Point",
        "`Shape` has nothing called `Hexagon` @ Shape.Hexagon(1)",
        "`radius` should be a float, but this is an int @ 1",
    ]);
    let help = helps(&format!("{TYPES}\ndef f():\n    Shape()\n    .Point\n"));
    assert_eq!(help, ["write one of Shape.Circle, Shape.Rect, Shape.Point", "say which one: `Shape.Point` or `Team.Point`"]);
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
    assert_eq!(with_types(program), [
        "`Enemy` has no method `explode` @ a.explode()",
        "`heal` needs a `Enemy` to call it on @ Enemy.heal(1)",
        "`Enemy` has nothing called `at` @ Enemy.at",
        "`heal` is missing its `amount` argument @ a.heal()",
        "`heal` takes 1 argument(s) @ 2",
    ]);
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
    assert_eq!(problems(program), [
        "`Box` has no field `weight` @ Box().weight",
        "`Missing` has no field `file` @ Missing('x').file".replace('\'', "\"").as_str(),
        "`Stdout` has no field `name` @ stdout.name",
        "an int has no field `real` @ one.real",
        "an int has no field `real` @ Box().size.real",
    ]);
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
    assert_eq!(in_main(program), [
        "a list has no method `push` @ queue.push('b')".replace('\'', "\"").as_str(),
        "a str has no method `trim` @ 'text'.trim()".replace('\'', "\"").as_str(),
        "a str has no method `to_uppercase` @ 'text'.to_uppercase()".replace('\'', "\"").as_str(),
        "a str has no method `toLowerCase` @ 'text'.toLowerCase()".replace('\'', "\"").as_str(),
        "`Stdout` has no method `log` @ stdout.log(1)",
        "a list has no method `copy` @ queue.copy()",
        "a list has no method `size` @ queue.size()",
        "a tuple has no method `first` @ (1, 2).first()",
    ]);
    let hints = helps("def main(stdout: Stdout):\n    mut q = [1]\n    q.push(1)\n    'a'.trim()\n    'a'.to_uppercase()\n    'a'.toLowerCase()\n    stdout.log(1)\n    q.copy()\n    q.size()\n    q.first()\n");
    assert_eq!(hints, [
        "use `append`",
        "use `strip()`",
        "use `upper()`",
        "use `lower()`",
        "use `stdout.print(...)`",
        "values are copied automatically when assigned: `b = a`",
        "use `len(x)`",
        "",
    ]);
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
    assert_eq!(in_main(program), [
        "`append` was called with the wrong arguments @ items.append(1, 2)",
        "`insert` was called with the wrong arguments @ items.insert(1)",
        "`pop` doesn't take `key` @ items.pop(key=1)",
        "`count` was called with the wrong arguments @ items.count(by=1)",
        "`count` has no parameter called `by` @ 1",
        "`split` was called with the wrong arguments @ 'a b'.split(' ', 1)".replace('\'', "\"").as_str(),
    ]);
}

#[test]
fn builtins_take_the_right_arguments() {
    let program = "
        len([1], [2])
        sorted([1], key=n => n, reverse=True)
        sum([1], key=n => n)
        range(1, 2, 3, 4)
        range('a')
        len(5)
        zip([1])
        set()
        round(1.5, 2)
        any([1])
        ignore(1)
    ";
    assert_eq!(in_main(program), [
        "`len` was called with the wrong arguments @ len([1], [2])",
        "`sorted` has no parameter called `reverse` @ True",
        "`sum` doesn't take `key` @ sum([1], key=n => n)",
        "`range` was called with the wrong arguments @ range(1, 2, 3, 4)",
        "`range` takes one to three ints @ range('a')".replace('\'', "\"").as_str(),
        "an int has no length @ len(5)",
        "`zip` was called with the wrong arguments @ zip([1])",
        "`any` needs bools, but got an int @ any([1])",
        "`ignore` was called with the wrong arguments @ ignore(1)",
    ]);
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
    assert_eq!(with_types(program), [
        "`Enemy` has no field called `hp` @ 5",
        "`health` should be an int, but this is a str @ 'x'".replace('\'', "\"").as_str(),
        "`replace` takes a struct value, then the fields to change @ replace()",
    ]);
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
    assert_eq!(with_types(program), [
        "`IdMap` was called with the wrong arguments @ IdMap(1)",
        "`Path` was called with the wrong arguments @ Path(1)",
        "`Id` was called with the wrong arguments @ Id()",
    ]);
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
    assert_eq!(problems(program), ["`double` takes 1 argument(s) @ 2"]);
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
    assert_eq!(problems(program), [
        "`expect` needs a reason this can't fail @ risky(1).expect()",
        "`expect` needs a reason this can't fail @ risky(1).expect(5)",
    ]);
}
