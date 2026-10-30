//! What builtins and the methods of built-in values give back, and what an arrow function
//! passed to one receives.

use crate::{helps, in_main, in_main_after, problems};

/// Checks `expression + 1` reports `expected` about the sum, which shows the expression's type.
fn plus_one(setup: &str, expression: &str, expected: &str) {
    let program = format!("{setup}\nstdout.print({expression} + 1)");
    let sum = format!("{expression} + 1").replace('\'', "\"");
    assert_eq!(in_main(&program), [format!("{expected} @ {sum}")], "{expression}");
}

#[test]
fn methods_of_built_in_values_give_back_their_types() {
    let setup = "mut pool = IdMap[int]()\nid = pool.insert(1)";
    plus_one(setup, "pool.ids()[0]", "can't use `+` on an Id and an int");
    plus_one(setup, "pool.items()[0][0]", "can't use `+` on an Id and an int");
    plus_one(setup, "pool.items()[0][1] / 2", "can't mix int and float in `+`");
    plus_one(setup, "'a'.lines()[0]", "can't use `+` on a str and an int");
    plus_one(setup, "'a'.replace('a', 'b')", "can't use `+` on a str and an int");
    plus_one(setup, "','.join(['a'])", "can't use `+` on a str and an int");
    plus_one(setup, "'a'.strip()", "can't use `+` on a str and an int");
    plus_one(setup, "'a'.startswith('a')", "can't use `+` on a bool and an int");
    plus_one(setup, "[1].count(1) / 2", "can't mix int and float in `+`");
    plus_one(setup, "pool.modify(id, n => n)", "can't use `+` on a bool and an int");
    plus_one(setup, "stdout.write('a')", "can't use `+` on a nothing and an int");
}

#[test]
fn removing_from_an_id_map_might_find_nothing() {
    let program = "
        mut pool = IdMap[int]()
        id = pool.insert(1)
        stdout.print(pool.remove(id) + 1, pool[id] + 1)
    ";
    assert_eq!(in_main(program), ["this might be None @ pool.remove(id)"]);
}

#[test]
fn builtins_give_back_their_types() {
    plus_one("", "ignore([1].index(1), 'fine')", "can't use `+` on a nothing and an int");
    plus_one("", "Ok(1)", "this is the result of something that can fail");
    plus_one("", "Err(1)", "this is the result of something that can fail");
    plus_one("", "list(set([1.5]))[0]", "can't mix int and float in `+`");
    assert_eq!(in_main("stdout.print(list(set())[0] + 1)"), Vec::<String>::new());
}

#[test]
fn arrow_functions_receive_the_items() {
    let program = "
        mut items = [1]
        mut pool = IdMap[str]()
        items.remove_where(n => n + 'a')
        items.sort(key=n => n + 'a')
        pool.remove_where(s => s + 1)
        sorted(['a'], key=s => s + 1)
        max((1, 2), key=n => n + 'a')
        min('ab', key=c => c + 1)
        sorted({1: 2}, key=k => k + 'a')
        sorted(5, key=k => k + 'a')
    ";
    assert_eq!(
        in_main(program),
        [
            "can't use `+` on an int and a str @ n + \"a\"",
            "can't use `+` on an int and a str @ n + \"a\"",
            "can't use `+` on a str and an int @ s + 1",
            "can't use `+` on a str and an int @ s + 1",
            "can't use `+` on an int and a str @ n + \"a\"",
            "can't use `+` on a str and an int @ c + 1",
            "can't use `+` on an int and a str @ k + \"a\"",
            "can't loop over an int @ 5",
        ]
    );
}

#[test]
fn optional_chaining_calls_methods_only_when_there_is_a_value() {
    let program = "
        found = {'a': 'b'}.get('a')
        stdout.print(found?.upper() + 'c')
        nothing = None
        stdout.print(nothing?.upper(), nothing.upper())
        stdout.print(found.upper())
    ";
    assert_eq!(
        in_main(program),
        ["this might be None @ found?.upper()", "this might be None @ nothing", "this might be None @ found",]
    );
}

#[test]
fn local_names_hide_functions_with_the_same_name() {
    let program = "
        def size(a: int, b: int):
            a + b

        def main(stdout: Stdout):
            def size(n: int):
                n
            stdout.print(size(1))
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn arguments_to_anything_are_checked() {
    let program = "
        value = 5
        value(missing)
        f = n => n
        f(also_missing)
    ";
    assert_eq!(
        in_main(program),
        ["`missing` doesn't exist @ missing", "`also_missing` doesn't exist @ also_missing"]
    );
}

/// The imports the standard library tests below use.
const IMPORTS: &str = "from std.collections import Deque
from std.random import Random, random_int
from std.re import Regex, is_regex";

/// Like `plus_one`, for a program that imports from the standard library.
fn plus_one_imported(setup: &str, expression: &str, expected: &str) {
    let program = format!("{setup}\nstdout.print({expression} + 1)");
    let sum = format!("{expression} + 1").replace('\'', "\"");
    assert_eq!(in_main_after(IMPORTS, &program), [format!("{expected} @ {sum}")], "{expression}");
}

#[test]
fn standard_library_methods_give_back_their_types() {
    let setup = "mut rng = Random(seed=1)\nword = Regex(r'\\w+')\nmut queue = Deque()";
    plus_one_imported(setup, "rng.randint(1, 6) / 2", "can't mix int and float in `+`");
    plus_one_imported(setup, "rng.random()", "can't mix int and float in `+`");
    plus_one_imported(setup, "random_int(1, 2, 3, 4)", "can't use `+` on a tuple and an int");
    plus_one_imported(setup, "word.replace('a', 'b')", "can't use `+` on a str and an int");
    plus_one_imported(setup, "word.split('a')[0]", "can't use `+` on a str and an int");
    plus_one_imported(setup, "word.find_all('a')[0].start / 2", "can't mix int and float in `+`");
    plus_one_imported(setup, "len(queue) / 2", "can't mix int and float in `+`");
    plus_one_imported(setup, "is_regex('a')", "can't use `+` on a bool and an int");
}

#[test]
fn the_newer_modules_names_have_to_be_imported() {
    let program = "
        mut rng = Random(seed=1)
        mut queue = Deque()
        stdout.print(random_int(1, 2, 3, 4), Random(seed=2), Duration.millis(5))
    ";
    assert_eq!(
        in_main(program),
        [
            "`Random` doesn't exist @ Random",
            "`Deque` doesn't exist @ Deque",
            "`random_int` doesn't exist @ random_int",
            "`Duration` doesn't exist @ Duration",
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(Duration())"),
        ["import it from std.time, like `from std.time import Duration`"]
    );
    // A module imported whole gives its names through it; `Clock` is a capability, which
    // `main` names without importing, like `Stdout`.
    let program = "
        import std.random

        def main(stdout: Stdout, clock: Clock):
            mut rng = random.Random(seed=1)
            stdout.print(rng.randint(1, 6), clock.now())
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn a_regex_search_might_find_nothing() {
    let program = "stdout.print(Regex(r'a').search('b').text)";
    assert_eq!(in_main_after(IMPORTS, program), ["this might be None @ Regex(r\"a\").search(\"b\").text"]);
}

#[test]
fn patterns_written_in_the_program_are_checked() {
    assert_eq!(
        in_main_after(IMPORTS, "stdout.print(Regex(r'(a'))"),
        ["this pattern isn't valid: this `(` is never closed @ r\"(a\""]
    );
    // A pattern made while the program runs might not be valid, so making it can fail.
    assert_eq!(
        in_main_after(IMPORTS, "stdout.print(Regex('(' + 'a').search('a'))"),
        ["this is the result of something that can fail @ Regex(\"(\" + \"a\")"]
    );
}

#[test]
fn a_clock_gives_instants() {
    let program = "
        def main(stdout: Stdout, clock: Clock):
            elapsed = clock.now() - clock.now()
            stdout.print(elapsed.nanos + 'a')
    ";
    assert_eq!(problems(program), ["can't use `+` on an int and a str @ elapsed.nanos + \"a\""]);
}
