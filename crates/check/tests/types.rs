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
            "`and` needs a bool, but this is a nothing @ nothing()",
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
            name = '' where len(name) >= 0
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
            "can't use `*` on a str and an int @ \"a\" * 2",
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
            stdout.print(items[0] + 1, 'abc'[0] + 'd', pair[0] + 1, pair[1] + 'b', pair[5], table['a'] + 1)
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
        ("int('1') + 1.0", "can't mix int and float in `+` @ int(\"1\") + 1.0"),
        ("floor(1.5) + 1.0", "can't mix int and float in `+` @ floor(1.5) + 1.0"),
        ("round(1.5) + 1.0", "can't mix int and float in `+` @ round(1.5) + 1.0"),
        ("round(1.5, 1) + 1", "can't mix int and float in `+` @ round(1.5, 1) + 1"),
        ("float(1) + 1", "can't mix int and float in `+` @ float(1) + 1"),
        ("sqrt(4.0) + 1", "can't mix int and float in `+` @ sqrt(4.0) + 1"),
        ("abs(-1) + 1.0", "can't mix int and float in `+` @ abs(-1) + 1.0"),
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
        ("xs.pop() + 1.0", "can't mix int and float in `+` @ xs.pop() + 1.0"),
        ("{'a': 1}.keys()[0] + 1", "can't use `+` on a str and an int @ {\"a\": 1}.keys()[0] + 1"),
        ("{'a': 1}.values()[0] + 1.0", "can't mix int and float in `+` @ {\"a\": 1}.values()[0] + 1.0"),
        ("{'a': 1}.items()[0][1] + 1.0", "can't mix int and float in `+` @ {\"a\": 1}.items()[0][1] + 1.0"),
        ("'a'.split()[0] + 1", "can't use `+` on a str and an int @ \"a\".split()[0] + 1"),
        ("'a'.isdigit() + 1", "can't use `+` on a bool and an int @ \"a\".isdigit() + 1"),
        ("IdMap[str]().values()[0] + 1", "can't use `+` on a str and an int @ IdMap[str]().values()[0] + 1"),
    ];
    for (expression, expected) in cases {
        let program =
            format!("def main(stdout: Stdout):\n    mut xs = [1]\n    stdout.print({expression})\n");
        assert_eq!(problems(&program), [expected.to_owned()], "{expression}");
    }
}

#[test]
fn optional_results_of_builtins_might_be_none() {
    let cases =
        ["[1].index(1) + 1", "'abc'.find('b') + 1", "{'a': 1}.get('a') + 1", "pool.get(pool.insert(1)) + 1"];
    for expression in cases {
        let program = format!(
            "def main(stdout: Stdout):\n    mut pool = IdMap[int]()\n    stdout.print({expression})\n"
        );
        let expression = expression.replace('\'', "\"");
        let operand = expression.rsplit_once(" + ").map_or("", |(left, _)| left);
        assert_eq!(problems(&program), [format!("this might be None @ {operand}")], "{expression}");
    }
    assert_eq!(in_main("stdout.print({'a': 1}.get('a', 0) + 1)"), Vec::<String>::new());
}

#[test]
fn sum_and_comprehensions_keep_item_types() {
    let program = "
        totals = {n: n * 2.0 for n in [1.0]}
        stdout.print(sum(n * 2 for n in [1]) + 1.0, {n for n in [1]}, totals)
    ";
    assert_eq!(in_main(program), ["can't mix int and float in `+` @ sum(n * 2 for n in [1]) + 1.0"]);
}
