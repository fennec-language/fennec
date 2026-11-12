//! Types in expressions: conditions are bools, ints and floats don't mix, operators work on
//! the types they're defined for, and indexing, slicing and looping fit what's indexed.

use crate::{helps, in_main, problems};

#[test]
fn conditions_need_bools() {
    let program = "
        def nothing():
            pass

        def main(stdout: Stdout):
            items = [1]
            if items:
                pass
            while 'text':
                pass
            assert {1}
            stdout.print(1 if {} else 2, (1, 2) and True, True or 2.5, not None)
            stdout.print([n for n in items if n], nothing() and True)
            if True and 1 > 0 or not False:
                pass
    ";
    assert_eq!(
        problems(program),
        [
            "`if` needs a bool, but this is a list @ items",
            "`while` needs a bool, but this is a str @ 'text'".replace('\'', "\"").as_str(),
            "`assert` needs a bool, but this is a set @ {1}",
            "`if` needs a bool, but this is a dict @ {}",
            "`and` needs a bool, but this is a tuple @ (1, 2)",
            "`or` needs a bool, but this is a float @ 2.5",
            "`not` needs a bool, but this is a None @ not None",
            "`if` needs a bool, but this is an int @ n",
            "`and` needs a bool, but this is nothing @ nothing()",
        ]
    );
}

#[test]
fn truthiness_hints_say_what_to_write() {
    let program = "
        def main(stdout: Stdout):
            maybe = [1].index(1)
            if [1]:
                pass
            if maybe:
                pass
            if 3:
                pass
            stdout.print(not 3)
    ";
    assert_eq!(
        helps(program),
        [
            "Fennec has no truthiness: write `len(x) > 0`",
            "Fennec has no truthiness: write `x is None` or `x is not None`",
            "Fennec has no truthiness: compare explicitly, like `count > 0`",
            "Fennec has no truthiness: compare explicitly, like `len(items) == 0`",
        ]
    );
}

#[test]
fn where_rules_are_conditions() {
    let program = "
        struct Player:
            level: int where level
            name = '' where name.len() >= 0
    ";
    assert_eq!(problems(program), ["`where` needs a bool, but this is an int @ level"]);
}

#[test]
fn ints_and_floats_do_not_mix() {
    let program = "
        count = 3
        stdout.print(count + 2.5, 2.5 * count, count / 2, 7 // 2 + 1, 2.0 ** 2.0, -count, -2.5)
        mut total = 0
        total += 1.5
    ";
    assert_eq!(
        in_main(program),
        [
            "can't mix int and float in `+` @ count + 2.5",
            "can't mix int and float in `*` @ 2.5 * count",
            "can't mix int and float in `+` @ total",
        ]
    );
    let program = "
        half = 7 / 2
        stdout.print(half + 1.0, half + 1)
    ";
    assert_eq!(in_main(program), ["can't mix int and float in `+` @ half + 1"]);
}

/// `u8(x)` makes an integer of another size this one, and `int(x)` and `float(x)` take any integer
/// size; text goes through `.parse`, so `u8(text)` says so.
#[test]
fn sized_integers_convert_when_written_out() {
    let program = "
        def sizes(stdout: Stdout, small: i32, byte: u8, text: str):
            stdout.print(u8(small), i16(byte), int(byte) + 1, float(small) * 0.5, u8(255), i8(-128))
            stdout.print(u8(text), u8(1.5), u8(300), i8(-129))
    ";
    assert_eq!(
        problems(program),
        [
            "`u8(...)` converts an integer, not a str @ text",
            "`u8(...)` converts an integer, not a float @ 1.5",
            "300 doesn't fit in u8 @ 300",
            "-129 doesn't fit in i8 @ -129",
        ]
    );
    let helps = helps("def f(stdout: Stdout, text: str):\n    stdout.print(u8(text), u8(1.5))\n");
    assert_eq!(
        helps,
        [
            "read it as an int first: `u8(int(text)?)`",
            "make it a whole number first: `u8(round(x))` or `u8(int(x))`"
        ]
    );
}

/// A program using an `f32` or a `u64` checks all the way to where it's built.
#[test]
fn f32_and_u64_check_to_run() {
    let source = "def main(stdout: Stdout):\n    mut speed: f32 = 1.5\n    speed += 1.0\n    big: u64 = 3\n    stdout.print(speed, big)\n";
    let mut module = fennec_syntax::parse(source).expect("it parses");
    assert!(fennec_check::check_to_run(&mut module).is_ok());
}

/// A number literal takes the size it goes into, checked to fit: a name's declared type, a
/// parameter, a return, the other side of an operator, and what `+=` changes. With nothing to go
/// by, it's an `int` or a `float` as before.
#[test]
fn literals_take_the_size_they_go_into() {
    let program = "
        def scaled(by: f32) -> f32:
            return by * 2.0

        def first(stdout: Stdout) -> u8:
            mut pixel: u8 = 255
            pixel -= 1
            low: i8 = -128
            speed: f32 = 1.5
            stdout.print(scaled(0.5), 3 * low, low < 0, 0 == pixel, speed + 1.0, pixel >> 1)
            count = 3
            stdout.print(count + 1, 1.5 * 2.0)
            return 7

        def too_big(stdout: Stdout) -> u16:
            over: u8 = 256
            under: u8 = -1
            low: i8 = -129
            huge: f32 = 1e39
            whole: f32 = 1
            stdout.print(over, under, low, huge, whole)
            return 70000
    ";
    assert_eq!(
        problems(program),
        [
            "256 doesn't fit in u8 @ 256",
            "-1 doesn't fit in u8 @ -1",
            "-129 doesn't fit in i8 @ -129",
            "1000000000000000000000000000000000000000 is too big for an f32 @ 1e39",
            "`whole` should be an f32, but this is an int @ 1",
            "70000 doesn't fit in u16 @ 70000",
        ]
    );
}

/// A set, dict or tuple literal written where one holding sized numbers goes takes their sizes,
/// each number checked to fit, as a list literal's items are.
#[test]
fn sized_numbers_fit_in_sets_dicts_and_tuples_too() {
    let program = "
        def main(stdout: Stdout):
            pixels: set[u8] = {1, 255}
            names: dict[str, u8] = {'a': 200}
            keyed: dict[u8, i16] = {7: -3}
            pair: (u8, int) = (250, 1)
            pairs: list[(u8, f32)] = [(9, 1.5)]
            over: set[u8] = {1, 300}
            wide: dict[str, u8] = {'b': 256}
            low: (i8, int) = (-129, 1)
            stdout.print(pixels, names, keyed, pair, pairs, over, wide, low)
    ";
    assert_eq!(
        problems(program),
        ["300 doesn't fit in u8 @ 300", "256 doesn't fit in u8 @ 256", "-129 doesn't fit in i8 @ -129"]
    );
}

/// Each sized number type is a type of its own: `i64` is `int` and `f64` is `float`, and sizes
/// never mix, with each other or with `int` and `float`, until one side is converted.
#[test]
fn sized_numbers_mix_only_with_their_own_size() {
    let program = "
        def same(stdout: Stdout, small: i32, other: i32, single: f32, wide: i64, count: int, double: f64, real: float) -> i32:
            whole = small + other * small // other % small - small ** other
            bits = small & other | small << other
            half = single / single - single
            both = wide + count
            reals = double * real
            stdout.print(bits, half, both, reals, small < other, -small, -single)
            return whole

        def mixed(stdout: Stdout, small: i32, wide: i64, byte: u8, single: f32, double: float, count: int):
            stdout.print(small + wide, byte - small, single * double, count + small)
            stdout.print(small < wide, small & wide, small / small + 0.5, -byte, small == count)
            stdout.print(byte << count, small >> byte, 1 == double, byte << single)
    ";
    assert_eq!(
        problems(program),
        [
            "can't mix i32 and int in `+` @ small + wide",
            "can't mix u8 and i32 in `-` @ byte - small",
            "can't mix f32 and float in `*` @ single * double",
            "can't mix int and i32 in `+` @ count + small",
            "can't mix i32 and int in `<` @ small < wide",
            "can't mix i32 and int in `&` @ small & wide",
            "can't negate a u8, which is never below 0 @ -byte",
            "`==` compares an i32 with an int, which are never equal @ small == count",
            "`==` compares an int with a float, which are never equal @ 1 == double",
            "can't mix u8 and f32 in `<<` @ byte << single",
        ]
    );
}

#[test]
fn operators_work_on_the_types_they_are_defined_for() {
    let program = "
        stdout.print('a' + 'b', [1] + [2], 'a' + 1, [1] - [2], True + True, 'a' * 2, -'a', -True, 'a' - 'b')
    ";
    assert_eq!(
        in_main(program),
        [
            "can't use `+` on a str and an int @ \"a\" + 1",
            "can't use `-` on a list and a list @ [1] - [2]",
            "can't use `+` on a bool and a bool @ True + True",
            "can't negate a str @ -\"a\"",
            "can't negate a bool @ -True",
            "can't use `-` on a str and a str @ \"a\" - \"b\"",
        ]
    );
}

#[test]
fn types_define_their_own_operators() {
    let program = "
        struct Money:
            cents: int

            def +(self, other: Money):
                Money(cents=self.cents + other.cents)

            def <(self, other: Money):
                self.cents < other.cents

            def [](self, at: int):
                self.cents

        def main(stdout: Stdout):
            total = Money(cents=1) + Money(cents=2)
            stdout.print(total.cents + 1, total > Money(cents=1), total[0] + 1)
            stdout.print(total + 5, total - total, total['x'])
    ";
    assert_eq!(
        problems(program),
        [
            "`other` should be a Money, but this is an int @ 5",
            "can't use `-` on a Money and a Money @ total - total",
            "`at` should be an int, but this is a str @ 'x'".replace('\'', "\"").as_str(),
        ]
    );
}

#[test]
fn indexing_fits_what_is_indexed() {
    let program = "
        struct Plain:
            n = 1

        def main(stdout: Stdout):
            items = [1]
            pair = (1, 'a')
            table = {'a': 1}
            stdout.print(items[0] + 1, 'abc'[0] + 'd', pair[0] + 1, pair[1] + 'b', table['a'] + 1)
            stdout.print(items['a'], 'abc'[1.5], Plain()[0], 5[0], {1}[0])
    ";
    assert_eq!(
        problems(&program.replace('\'', "\"")),
        [
            "an index must be an int, not a str @ \"a\"",
            "an index must be an int, not a float @ 1.5",
            "`Plain` can't be indexed @ Plain()[0]",
            "an int can't be indexed @ 5[0]",
            "a set can't be indexed @ {1}[0]",
        ]
    );
    assert_eq!(
        helps("struct P:\n    n = 1\n\ndef f():\n    P()[0]\n"),
        ["define `def [](self, index: ...)` to index it"]
    );
}

#[test]
fn slicing_fits_what_is_sliced() {
    let program = "
        stdout.print([1, 2][1:], 'abc'[:2] + 'd', (1, 2)[1:], [1]['a':], 5[1:])
    ";
    assert_eq!(
        in_main(&program.replace('\'', "\"")),
        ["a slice position must be an int, not a str @ \"a\"", "an int can't be sliced @ 5[1:]",]
    );
}

#[test]
fn a_fixed_size_array_holds_exactly_its_size() {
    let program = "
        struct Item:
            name = ''

        struct Bag:
            slots = array[Item?, 3]()
            m: array[float, 4] = [1.0, 0.0, 0.0, 1.0] where all(x >= 0.0 for x in m)

        def bump(ref xs: array[int, 3]):
            for ref x in xs:
                x += 1

        def main(stdout: Stdout):
            mut bag = Bag()
            bag.slots[0] = Item(name='rope')
            bag.slots[-1] = None
            mut xs = array[int, 3]()
            bump(ref xs)
            grid = array[array[int, 2], 2]()
            mut maybe = array[str?, 3]()
            maybe[1] = 'x'
            lists: list[array[int, 3]] = [array[int, 3](), [1, 2, 3]]
            by_name: dict[str, array[float, 2]] = {'a': [1.0, 2.0]}
            picked: array[int, 2]? = [4, 5]
            stdout.print(maybe, lists, by_name, picked)
            stdout.print(xs[2] + 1, len(xs), 2 in xs, xs == array[int, 3](), list(xs), grid[1][0], bag.m[3] * 2.0)
    ";
    assert_eq!(problems(&program.replace('\'', "\"")), Vec::<String>::new());
}

#[test]
fn a_fixed_size_array_refuses_a_wrong_size_or_position() {
    let program = "
        struct Enemy:
            name: str

        def takes(xs: list[int]) -> int:
            return len(xs)

        def main(stdout: Stdout):
            a: array[int, 3] = [1, 2]
            b: array[int, 0] = [1]
            c: array[str, 2] = ['a', 1]
            d = array[Enemy, 2]()
            e = array[int, 3](5)
            mut f = array[int, 3]()
            f[3] = 1
            stdout.print(f[-4], f[-3], takes(f), f[0:2])
            lists: list[array[int, 3]] = [[1, 2]]
    ";
    assert_eq!(
        problems(&program.replace('\'', "\"")),
        [
            "this array holds 3 items, but this list has 2 items @ [1, 2]",
            "an array's size is a whole number of at least 1, written out @ array[int, 0]",
            "this array holds a str, not an int @ 1",
            "an Enemy has no zero to fill an array with @ array[Enemy, 2]()",
            "`array[int, 3]()` fills it with zeros, so it takes nothing @ array[int, 3](5)",
            "this array has 3 items, so there's no item 3 @ 3",
            "this array has 3 items, so there's no item -4 @ -4",
            "`xs` should be a list[int], but this is an array[int, 3] @ f",
            "an array can't be sliced @ f[0:2]",
            "this array holds 3 items, but this list has 2 items @ [1, 2]",
        ]
    );
}

#[test]
fn loops_go_over_collections() {
    let program = "
        struct Bag:
            items = [1]

            def iter(self):
                for item in self.items:
                    yield item

        struct Plain:
            n = 1

        def main(stdout: Stdout):
            for n in Bag():
                stdout.print(n + 'x')
            for key in {'a': 1}:
                stdout.print(key + 'b')
            for letter in 'abc':
                stdout.print(letter + 'd')
            for n in (1, 2):
                stdout.print(n + 1)
            for n in Plain():
                pass
            for n in 5:
                pass
    ";
    assert_eq!(
        problems(&program.replace('\'', "\"")),
        [
            "can't use `+` on an int and a str @ n + \"x\"",
            "can't loop over a `Plain` @ Plain()",
            "can't loop over an int @ 5",
        ]
    );
    let help = helps("struct P:\n    n = 1\n\ndef f():\n    for x in P():\n        pass\n");
    assert_eq!(help, ["define `def iter(self)` that yields its items"]);
}

#[test]
fn unpacking_needs_the_right_number_of_values() {
    let program = "
        a, b = (1, 2, 3)
        c, d = 5
        for x, y in [(1, 2, 3)]:
            pass
        e, f = (1, 'a')
        stdout.print(e + 1, f + 'b')
    ";
    assert_eq!(
        in_main(&program.replace('\'', "\"")),
        [
            "can't unpack 3 values into 2 names @ (1, 2, 3)",
            "can't unpack an int into 2 names @ 5",
            "can't unpack 3 values into 2 names @ [(1, 2, 3)]",
        ]
    );
}

#[test]
fn builtins_give_back_the_right_types() {
    let cases = [
        ("len([1]) + 1.0", "can't mix int and float in `+` @ len([1]) + 1.0"),
        ("str(1) + 1", "can't use `+` on a str and an int @ str(1) + 1"),
        ("int(1.5) + 1.0", "can't mix int and float in `+` @ int(1.5) + 1.0"),
        ("int('1') + 1", "this is the result of something that can fail @ int(\"1\") + 1"),
        ("floor(1.5) + 1.0", "can't mix int and float in `+` @ floor(1.5) + 1.0"),
        ("round(1.5) + 1.0", "can't mix int and float in `+` @ round(1.5) + 1.0"),
        ("round(1.5, 1) + 1", "can't mix int and float in `+` @ round(1.5, 1) + 1"),
        ("float(1) + 1", "can't mix int and float in `+` @ float(1) + 1"),
        ("sqrt(4.0) + 1", "can't mix int and float in `+` @ sqrt(4.0) + 1"),
        ("abs(-1) + 1.0", "can't mix int and float in `+` @ -1).abs() + 1.0"),
        ("sum([1.0]) + 1", "can't mix int and float in `+` @ sum([1.0]) + 1"),
        ("max([1]) + 1.0", "can't mix int and float in `+` @ max([1]) + 1.0"),
        ("min(1, 2) + 1.0", "can't mix int and float in `+` @ min(1, 2) + 1.0"),
        ("sorted([1])[0] + 1.0", "can't mix int and float in `+` @ sorted([1])[0] + 1.0"),
        ("reversed('ab') + 1", "can't use `+` on a str and an int @ reversed(\"ab\") + 1"),
        ("reversed([1])[0] + 1.0", "can't mix int and float in `+` @ reversed([1])[0] + 1.0"),
        ("list({1})[0] + 1.0", "can't mix int and float in `+` @ list({1})[0] + 1.0"),
        ("enumerate(['a'])[0][0] + 1.0", "can't mix int and float in `+` @ enumerate([\"a\"])[0][0] + 1.0"),
        ("enumerate(['a'])[0][1] + 1", "can't use `+` on a str and an int @ enumerate([\"a\"])[0][1] + 1"),
        ("zip([1], ['a'])[0][1] + 1", "can't use `+` on a str and an int @ zip([1], [\"a\"])[0][1] + 1"),
        ("range(3)[0] + 1.0", "can't mix int and float in `+` @ range(3)[0] + 1.0"),
        ("any([True]) + 1", "can't use `+` on a bool and an int @ any([True]) + 1"),
        ("set([1]) + 1", "can't use `+` on a set and an int @ set([1]) + 1"),
        ("PI + 1", "can't mix int and float in `+` @ PI + 1"),
        ("xs.pop(0) + 1.0", "can't mix int and float in `+` @ xs.pop(0) + 1.0"),
        ("xs.pop() + 1", "this might be None @ xs.pop()"),
        ("{'a': 1}.keys()[0] + 1", "can't use `+` on a str and an int @ {\"a\": 1}.keys()[0] + 1"),
        ("{'a': 1}.values()[0] + 1.0", "can't mix int and float in `+` @ {\"a\": 1}.values()[0] + 1.0"),
        ("{'a': 1}.items()[0][1] + 1.0", "can't mix int and float in `+` @ {\"a\": 1}.items()[0][1] + 1.0"),
        ("'a'.split()[0] + 1", "can't use `+` on a str and an int @ \"a\".split()[0] + 1"),
        ("'a'.isdigit() + 1", "can't use `+` on a bool and an int @ \"a\".isdigit() + 1"),
        ("Table[str]().values()[0] + 1", "can't use `+` on a str and an int @ Table[str]().values()[0] + 1"),
    ];
    for (expression, expected) in cases {
        let program = format!(
            "from std.math import PI, sqrt, floor\nfrom std.collections import Table\n\ndef main(stdout: Stdout):\n    mut xs = [1]\n    stdout.print({expression})\n"
        );
        assert_eq!(problems(&program), [expected.to_owned()], "{expression}");
    }
}

#[test]
fn optional_results_of_builtins_might_be_none() {
    let cases =
        ["[1].index(1) + 1", "'abc'.find('b') + 1", "{'a': 1}.get('a') + 1", "pool.get(pool.add(1)) + 1"];
    for expression in cases {
        let program = format!(
            "from std.collections import Table\n\ndef main(stdout: Stdout):\n    mut pool = Table[int]()\n    stdout.print({expression})\n"
        );
        let expression = expression.replace('\'', "\"");
        let operand = expression.rsplit_once(" + ").map_or("", |(left, _)| left);
        assert_eq!(problems(&program), [format!("this might be None @ {operand}")], "{expression}");
    }
    assert_eq!(in_main("stdout.print(({'a': 1}.get('a') else 0) + 1)"), Vec::<String>::new());
}

#[test]
fn sum_and_comprehensions_keep_item_types() {
    let program = "
        totals = {n: n * 2.0 for n in [1.0]}
        stdout.print(sum(n * 2 for n in [1]) + 1.0, {n for n in [1]}, totals)
    ";
    assert_eq!(in_main(program), ["can't mix int and float in `+` @ sum(n * 2 for n in [1]) + 1.0"]);
}

#[test]
fn text_repeats_with_its_own_operator() {
    let program = "
        def main(stdout: Stdout):
            repeated = 'ab' * 3
            stdout.print(repeated + 1, 'ab' * 'c', 3 * 'ab')
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on a str and an int @ repeated + 1",
            "`times` should be an int, but this is a str @ \"c\"",
            "can't use `*` on an int and a str @ 3 * \"ab\"",
        ]
    );
}

/// A `LiteralStr` is text written in quotes in the source, or such text joined with `+`. It goes
/// anywhere a `str` does, but a `str` never goes where one is asked for: an f-string, a `str`
/// made while running, or a name holding a literal that isn't typed `LiteralStr`.
#[test]
fn only_text_written_in_the_source_is_a_literal_str() {
    let program = "
        struct Query:
            sql: LiteralStr
            parts: list[LiteralStr]

        def query(sql: LiteralStr) -> int:
            return len(sql)

        def named() -> LiteralStr:
            return 'select ' + 'name'

        def main(stdout: Stdout):
            name = 'ash'
            kept: LiteralStr = 'select 3'
            mut grown: LiteralStr = 'a'
            grown += ' b'
            stdout.print(query('select 1'), query('select ' + '2'), query(kept), query(grown))
            stdout.print(query(kept + ' where 1'), query(named()), kept.upper(), f'{kept}!')
            stdout.print(Query(sql=named(), parts=['x', 'y' + 'z']).parts[0] + name)
            copied = kept
            stdout.print(query(f'select {name}'), query(name), query(copied), query(kept + name))
            stdout.print(Query(sql='a', parts=['b', name]))
            grown = name
    ";
    let wanted = "`sql` should be a LiteralStr, but this is a str";
    assert_eq!(
        problems(program),
        [
            format!("{wanted} @ f\"select {{name}}\""),
            format!("{wanted} @ name"),
            format!("{wanted} @ copied"),
            format!("{wanted} @ kept + name"),
            "this holds a LiteralStr, not a str @ name".to_owned(),
            "`grown` holds a LiteralStr, so it can't be given a str @ grown".to_owned(),
        ]
    );
}

/// `t"..."` builds a `Template`, its values checked against what the template's `T` is where it
/// goes, and its text `LiteralStr`s: an f-string is never a `Template`, and one built by hand
/// can't take text made while running.
#[test]
fn a_template_string_checks_its_values_where_it_goes() {
    let program = "
        type SqlValue = int | str

        def count(query: Template[SqlValue]) -> int:
            return len(query.values)

        def main(stdout: Stdout):
            name = 'ash'
            price = 1.5
            kept: Template[SqlValue] = t'select {name} where {price > 1.0}'
            stdout.print(count(t'select {name}, {1}'), count(t'no values'), count(kept))
            stdout.print(t'{name}!'.strings, t'{name:>5}'.format_specs[0])
            stdout.print(count(t'select {price}'), count(f'select {name}'))
            stdout.print(count(Template(strings=[f'{name}'], values=[], expressions=[], format_specs=[])))
    ";
    assert_eq!(
        problems(program),
        [
            "this holds an int | str, not a bool @ price > 1.0",
            "this holds an int | str, not a float @ price",
            "`query` should be a Template[int | str], but this is a str @ f\"select {name}\"",
            "this holds a LiteralStr, not a str @ f\"{name}\"",
        ]
    );
}

#[test]
fn a_types_own_equality_has_to_give_back_a_bool() {
    let text = "struct Pair:\n    a: int\n\n    def ==(self, other: Pair) -> int:\n        return 1\n\ndef main(stdout: Stdout):\n    stdout.print(Pair(a=1) == Pair(a=2))\n";
    assert_eq!(
        problems(text),
        ["`Pair`'s `def ==` gives back an int, so `==` can't use it @ Pair(a=1) == Pair(a=2)"]
    );
}

#[test]
fn a_types_own_equality_takes_its_own_type() {
    let text = "struct Pair:\n    a: int\n\n    def ==(self, other: Pair) -> bool:\n        return True\n\ndef main(stdout: Stdout):\n    stdout.print(Pair(a=1) != 2)\n";
    assert!(problems(text).iter().any(|problem| problem.ends_with("@ 2")), "{:?}", problems(text));
}

#[test]
fn annotations_on_fields_are_checked() {
    let text = "struct User:\n    @serialize(name=\"uid\")\n    id: int\n    @skip\n    total: int\n    \
                @serialize(\"uid\")\n    name: str\n    @json\n    age: int\n    @skip\n    cache = \"\"\n";
    assert_eq!(
        problems(text),
        [
            "`@skip` needs `total` to have a default @ @skip",
            "`@serialize` takes the field's saved name @ @serialize(\"uid\")",
            "there's no annotation called `@json` @ @json",
        ]
    );
}

#[test]
fn a_field_can_be_named_by_a_keyword() {
    let text = "struct User:\n    type: str\n    from = 0\n\ndef main(stdout: Stdout):\n    \
                user = User(type=\"admin\", from=2)\n    stdout.print(user.type, user.from)\n";
    let empty: [&str; 0] = [];
    assert_eq!(problems(text), empty);
}

#[test]
fn every_case_field_is_checked_whether_anything_uses_the_case_or_not() {
    let program = "
        enum Unused:
            A(x: Nope)
            B(y: list[Gone])

        enum Holder[T]:
            Full(item: T)
            Odd(other: U)

        error Failed:
            Missing(what: Absent)
    ";
    assert_eq!(
        problems(&crate::dedent(program)),
        [
            "`Nope` isn't a type @ Nope",
            "`Gone` isn't a type @ Gone",
            "`U` isn't a type @ U",
            "`Absent` isn't a type @ Absent",
        ]
    );
}

#[test]
fn a_case_field_default_has_to_fit_its_type() {
    let program = "
        enum Unused:
            A(x: int = \"text\")
            B(y: int = 1)
    ";
    let found = problems(&crate::dedent(program));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found.iter().all(|problem| problem.contains("`x`")), "{found:?}");
}

#[test]
fn a_float_compared_as_not_itself_isnt_a_nan_test() {
    let empty: [&str; 0] = [];
    let held = "struct Unit:\n    speed = 1.5\n";
    assert_eq!(
        crate::in_main_after(held, "x = 1.5\nstdout.print(x != x)"),
        ["`x != x` is always False, since NaN equals NaN here @ x != x"]
    );
    assert_eq!(
        crate::in_main_after(held, "unit = Unit()\nstdout.print(unit.speed != unit.speed)"),
        ["`unit.speed != unit.speed` is always False, since NaN equals NaN here @ unit.speed != unit.speed"]
    );
    for body in [
        "x = 1.5\ny = 2.5\nstdout.print(x != y, x.is_nan())",
        "unit = Unit()\nstdout.print(unit.speed.is_nan(), unit.speed != 1.0)",
        // An int can't be NaN, and the lint has what it means to compare one with itself.
        "x = 1\nstdout.print(x != x)",
    ] {
        assert_eq!(crate::in_main_after(held, body), empty, "{body}");
    }
    let found = crate::in_main_after(held, "x = 1\nstdout.print(x.is_nan())");
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_dict_or_set_cant_hold_what_might_be_missing() {
    let dict = "a dict can't hold optional values";
    for body in [
        "mut ages: dict[str, int?] = {}\nstdout.print(ages)",
        "ages = dict[str, int?]()\nstdout.print(ages)",
        "maybe: int? = None\nages = {\"a\": maybe}\nstdout.print(ages)",
        "ages = {\"a\": 1, \"b\": None}\nstdout.print(ages)",
    ] {
        let found = in_main(body);
        assert!(found.iter().any(|one| one.starts_with(dict)), "{body}: {found:?}");
    }
    let found = in_main("names: dict[str?, int] = {}\nstdout.print(names)");
    assert!(found.iter().any(|one| one.starts_with("a dict's keys can't be optional")), "{found:?}");
    let found = in_main("seen: set[int?] = set()\nstdout.print(seen)");
    assert!(found.iter().any(|one| one.starts_with("a set can't hold optional items")), "{found:?}");
    let fields = problems("struct Book:\n    pages: dict[str, int?]\n");
    assert!(fields.iter().any(|one| one.starts_with(dict)), "{fields:?}");
    // A list of optionals stays, and so does looking up what may be missing.
    let empty: [&str; 0] = [];
    assert_eq!(
        in_main(
            "mut numbers: list[int?] = [1]\nnumbers.append(None)\nages = {\"a\": 1}\nstdout.print(numbers, ages.get(\"b\"))"
        ),
        empty
    );
}
