//! Programs the checker can't fully type are mistakes, with a help line saying what to
//! write. Types still to be worked out, like the items of `[]`, are settled by how the
//! value is used later in the same function.

use fennec_check::Type;

use crate::{helps, in_main, in_main_after, list, optional, problems, returns};

#[test]
fn empty_collections_take_their_type_from_later_use() {
    let program = "
        mut names = []
        names.append('Ash')
        mut scores = {}
        scores['Ash'] = 3
        mut seen = set()
        seen.add(1)
        mut grid = []
        grid.append([1.5])
        stdout.print(names[0].upper(), scores['Ash'] + 1, seen.len(), grid[0][0] * 2.0)
    ";
    assert_eq!(in_main(program), Vec::<String>::new());
    let program = "def f(n: int) -> list[str]:\n    if n > 0:\n        return []\n    ['a']\n";
    assert_eq!(returns(program, "f"), Ok(list(Type::Str)));
    let program = "def take(items: list[int]):\n    pass\n\ndef main(stdout: Stdout):\n    take([])\n";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn empty_collections_nothing_settles_need_their_type_written() {
    assert_eq!(
        in_main("mut names = []\nstdout.print(names.len())\nscores = {}\nseen = set()"),
        [
            "can't tell what this empty list holds @ []",
            "can't tell what this empty dict holds @ {}",
            "can't tell what this empty set holds @ set()",
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    mut names = []\n    stdout.print(names)\n"),
        ["write the type with the name, like `names: list[int] = []`"]
    );
    assert_eq!(
        in_main("names: list[str] = []\nscores: dict[str, int] = {}\nstdout.print(names, scores)"),
        Vec::<String>::new()
    );
    let program = "def empty():\n    []\n";
    assert_eq!(problems(program), ["can't tell what this empty list holds @ []"]);
}

#[test]
fn a_name_first_given_none_takes_its_type_from_what_it_is_given_later() {
    let program = "
        mut best = None
        for score in [3, 1]:
            if best is None or score > best:
                best = score
        stdout.print(best else 0)
    ";
    assert_eq!(in_main(program), Vec::<String>::new());
    assert_eq!(
        in_main("target = None\nstdout.print(target is None)"),
        ["can't tell what this holds when it isn't None @ target"]
    );
    assert_eq!(in_main("target: str? = None\nstdout.print(target is None)"), Vec::<String>::new());
}

#[test]
fn a_name_keeps_one_type() {
    assert_eq!(
        in_main("mut health = 100\nhealth = 'full'"),
        ["`health` holds an int, so it can't be given a str @ health"]
    );
    assert_eq!(
        in_main("mut ratio = 1\nratio = 0.5"),
        ["`ratio` holds an int, so it can't be given a float @ ratio"]
    );
    assert_eq!(in_main("mut best = None\nbest = 1\nbest = None"), Vec::<String>::new());
    assert_eq!(
        in_main("mut items = [1]\nitems[0] = 'a'"),
        ["this should be an int, but this is a str @ \"a\""]
    );
}

#[test]
fn written_types_on_names_are_checked() {
    assert_eq!(in_main("count: int = 'many'"), ["`count` should be an int, but this is a str @ \"many\""]);
    assert_eq!(in_main("count = 1\ncount: int = 2"), ["`count` already exists @ count"]);
    assert_eq!(in_main("mut count: float = 1.0\ncount += 0.5\nstdout.print(count)"), Vec::<String>::new());
    assert_eq!(in_main("ratio: flot = 1.0"), ["`flot` isn't a type @ flot"]);
}

#[test]
fn values_from_several_places_have_to_fit_together() {
    assert_eq!(in_main("stdout.print([1, 'a'])"), ["this list mixes an int and a str @ [1, \"a\"]"]);
    assert_eq!(in_main("stdout.print({1, 2.5})"), ["this set mixes an int and a float @ {1, 2.5}"]);
    assert_eq!(
        in_main("stdout.print({'a': 1, 'b': 'c'})"),
        ["this dict's values mix an int and a str @ {\"a\": 1, \"b\": \"c\"}"]
    );
    assert_eq!(
        in_main("stdout.print(1 if True else 'a')"),
        ["the two sides of this `if` give an int and a str @ 1 if True else \"a\""]
    );
    assert_eq!(
        in_main("x = [1].index(1) else 'none'"),
        ["the two sides of this `else` give an int and a str @ [1].index(1) else \"none\""]
    );
    let program = "
        x = match 3:
            case 1: 'one'
            case _: 2
    ";
    assert_eq!(in_main(program).len(), 1);
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print([1, 2.5])\n"),
        [
            "ints and floats don't mix: write whole floats with a decimal point, like `10.0`, or convert with float(...)"
        ]
    );
}

#[test]
fn a_bare_return_drops_the_value_of_the_last_line() {
    let program = "
        def f(items: list[int]):
            if items.len() == 0:
                return
            items.index(1)
    ";
    assert_eq!(returns(program, "f"), Ok(Type::Unit));
    let program = "
        error Oops:
            why: str

        def risky() -> int:
            fail Oops('x')

        def f(n: int):
            if n > 0:
                return
            risky()
    ";
    assert_eq!(problems(program), ["this can fail, but nothing handles it @ risky()"]);
    let program = "def f(n: int):\n    if n > 0:\n        return 1\n    stdout = 2\n";
    assert_eq!(problems(program), ["`f` returns an int and nothing @ def f(n: int)"]);
}

#[test]
fn comparisons_need_values_that_could_be_equal() {
    assert_eq!(
        in_main("stdout.print(1 == 'a', 'a' != 2.5)"),
        [
            "`==` compares an int with a str, which are never equal @ 1 == \"a\"",
            "`!=` compares a str with a float, which are never equal @ \"a\" != 2.5",
        ]
    );
    assert_eq!(
        in_main("stdout.print([1].index(1) == 1, [1] == [], (1, 'a') == (2, 'b'))"),
        Vec::<String>::new()
    );
    assert_eq!(
        in_main("stdout.print('a' < 1, [1] < [2], 1 < 2.5)"),
        ["can't order a str and an int with `<` @ \"a\" < 1", "can't mix int and float in `<` @ 1 < 2.5"]
    );
    assert_eq!(
        in_main("stdout.print(1 in ['a'], 'a' in {'a': 1}, 2 in 3)"),
        [
            "can't look for an int in a list[str] @ 1 in [\"a\"]",
            "`in` looks inside a collection, but this is an int @ 2 in 3"
        ]
    );
}

#[test]
fn methods_of_built_in_values_check_their_arguments() {
    let program = "
        mut names = ['Ash']
        names.append(3)
        names.insert('first', 'Kit')
        names.extend(['a', 1])
        mut table = {'a': 1}
        table['b'] = 'two'
        stdout.print(table.get(1), 'a,b'.split(1), ','.join([1]))
        mut pool = IdMap[str]()
        id = pool.insert('x')
        stdout.print(pool[0], pool[id].upper(), table[2])
    ";
    assert_eq!(
        in_main_after("from std.game import IdMap", program),
        [
            "`append`'s argument should be a str, but this is an int @ 3",
            "`insert`'s argument should be an int, but this is a str @ \"first\"",
            "this list mixes a str and an int @ [\"a\", 1]",
            "this should be an int, but this is a str @ \"two\"",
            "`get`'s argument should be a str, but this is an int @ 1",
            "`split`'s argument should be a str, but this is an int @ 1",
            "`join`'s argument should be a list[str], but this is a list[int] @ [1]",
            "this is looked up by an Id[str], not an int @ 0",
            "this is looked up by a str, not an int @ 2",
        ]
    );
}

#[test]
fn builtins_check_what_they_are_given() {
    assert_eq!(
        in_main("stdout.print(sum(['a']), abs('a'), int([1]), range(1.5), round(1.5, 'a'))"),
        [
            "`sum` adds numbers, but these are strs @ sum([\"a\"])",
            "`abs` takes a number, but this is a str @ abs(\"a\")",
            "`int` converts a number or text, not a list[int] @ [1]",
            "`range` takes one to three ints @ range(1.5)",
            "`round` takes a number, but this is a float @ round(1.5, \"a\")",
        ]
    );
    assert_eq!(in_main("stdout.print(Err(1))"), ["can't tell what value this result could hold @ Err(1)"]);
    assert_eq!(
        in_main("stdout.print(replace(1, n=2))"),
        ["`replace` copies a struct, but this is an int @ replace(1, n=2)"]
    );
}

#[test]
fn arrow_functions_are_passed_straight_to_what_calls_them() {
    assert_eq!(
        in_main("double = n => n * 2\nstdout.print(double(2))"),
        ["can't tell the type of `n` @ n => n * 2"]
    );
    assert_eq!(in_main("stdout.print(sorted(['a'], key=name => name.len()))"), Vec::<String>::new());
}

#[test]
fn only_functions_can_be_called() {
    assert_eq!(in_main("n = 3\nstdout.print(n(1))"), ["an int can't be called @ n"]);
    assert_eq!(in_main("stdout.print(sum)"), ["`sum` can only be called @ sum"]);
}

#[test]
fn question_mark_passes_errors_up_not_none() {
    let program = "
        def find(n: int) -> str?:
            None

        def f(n: int):
            name = find(n)?
            name.upper()

        def g(n: int):
            n?
    ";
    assert_eq!(
        problems(program),
        ["`?` needs a result, but this is a str? @ find(n)", "`?` needs a result, but this is an int @ n"]
    );
}

#[test]
fn tuples_are_indexed_and_sliced_with_numbers_written_out() {
    let program = "
        pair = (1, 'a')
        same = (1, 2)
        at = 1
        stdout.print(pair[at], pair[2], same[at] + 1, pair[1:] == ('a',))
        for item in pair:
            pass
        stdout.print(pair[at:])
    ";
    assert_eq!(
        in_main(program),
        [
            "a tuple whose items have different types needs its index written as a number @ at",
            "this tuple has 2 items, so there's no item 2 @ 2",
            "can't loop over a tuple whose items have different types @ pair",
            "a tuple's slice needs its positions written as numbers @ pair[at:]",
        ]
    );
}

#[test]
fn several_errors_make_one_of_them() {
    let program = "
        error NotFound:
            name: str

        error TooWeak:
            level: int

        def challenge(level: int, name: str):
            if name == '':
                fail NotFound(name)
            if level < 3:
                fail TooWeak(level)
            name

        def main(stdout: Stdout):
            match challenge(1, 'Kit'):
                case Ok(name): stdout.print(name)
                case NotFound(name): stdout.print(name)
                case TooWeak(level): stdout.print(level + 1)
            stdout.print(challenge(1, '') == Err(TooWeak(level=1)))
    ";
    let both = Type::Errors(vec!["NotFound".to_owned(), "TooWeak".to_owned()]);
    assert_eq!(returns(program, "challenge"), Ok(crate::result(Type::Str, both)));
}

#[test]
fn fields_need_a_type_their_default_shows() {
    let program = "
        struct Player:
            weapon = None
            items = []
            score = 0

        def main(stdout: Stdout):
            stdout.print(Player().score)
    ";
    assert_eq!(
        problems(program),
        [
            "can't tell the type of `weapon` from its default @ None",
            "can't tell the type of `items` from its default @ []"
        ]
    );
    assert_eq!(
        helps(program),
        ["write its type, like `weapon: Enemy? = None`", "write its type, like `items: list[int] = []`"]
    );
}

#[test]
fn enum_fields_are_reached_through_match() {
    let declarations = "enum Shape:\n    Circle(radius: float)\n    Square(side: float)\n";
    assert_eq!(
        in_main_after(declarations, "shape = Shape.Circle(1.0)\nstdout.print(shape.radius)"),
        ["`Shape` is an enum, so its fields depend on its case @ shape.radius"]
    );
}

#[test]
fn types_are_not_values() {
    let declarations = "from std.game import IdMap\n\nstruct Enemy:\n    health = 100\n";
    assert_eq!(in_main_after(declarations, "kind = Enemy"), ["`Enemy` is a type, not a value @ Enemy"]);
    assert_eq!(
        in_main_after(declarations, "pool = IdMap[Enemy]\nempty = IdMap()"),
        [
            "`IdMap[...]` is a type, not a value @ IdMap[Enemy]",
            "`IdMap` was called with the wrong arguments @ IdMap()"
        ]
    );
    assert_eq!(in_main("optional = [1].index(1)\nstdout.print(optional)"), Vec::<String>::new());
    let _ = optional(Type::Int);
}

#[test]
fn functions_that_call_each_other_share_their_errors() {
    let program = "
        error TooLow:
            n: int

        error TooHigh:
            n: int

        def even(n: int) -> bool:
            if n < 0:
                fail TooLow(n)
            if n == 0:
                return True
            odd(n - 1)?

        def odd(n: int) -> bool:
            if n > 100:
                fail TooHigh(n)
            if n == 0:
                return False
            even(n - 1)?

        def check(n: int) -> bool:
            even(n)?
    ";
    let both = Type::Errors(vec!["TooHigh".to_owned(), "TooLow".to_owned()]);
    for function in ["even", "odd", "check"] {
        assert_eq!(returns(program, function), Ok(crate::result(Type::Bool, both.clone())), "{function}");
    }
}

#[test]
fn a_function_with_no_arrow_gives_back_nothing_even_when_it_calls_itself() {
    let program = "
        def countdown(stdout: Stdout, n: int):
            if n > 0:
                stdout.print(n)
                countdown(stdout, n - 1)
    ";
    assert_eq!(returns(program, "countdown"), Ok(Type::Unit));
}

#[test]
fn results_can_be_written_as_types() {
    let program = "
        def describe(result: Result[int, str]) -> str:
            match result:
                case Ok(n): f'{n}'
                case Err(why): why

        def main(stdout: Stdout):
            ok: Result[int, str] = Ok(3)
            stdout.print(describe(ok), describe(Err('no')), describe(Ok('three')))
    ";
    assert_eq!(
        problems(program),
        ["`result` should be a result of int, but this is a result of str @ Ok(\"three\")"]
    );
}

#[test]
fn tuple_slices_can_count_from_the_end() {
    let program = "def f():\n    (1, 'a', 2.5)[-2:]\n";
    assert_eq!(returns(program, "f"), Ok(Type::Tuple(vec![Type::Str, Type::Float])));
}
