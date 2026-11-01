//! What builtins and the methods of built-in values give back, and what an arrow function
//! passed to one receives.

use crate::{helps, in_main, in_main_after, problems};

const ID_MAP: &str = "from std.game import IdMap";

/// Checks `expression + 1` reports `expected` about the sum, which shows the expression's type.
fn plus_one(setup: &str, expression: &str, expected: &str) {
    let program = format!("{setup}\nstdout.print({expression} + 1)");
    let sum = format!("{expression} + 1").replace('\'', "\"");
    assert_eq!(in_main_after(ID_MAP, &program), [format!("{expected} @ {sum}")], "{expression}");
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
    plus_one(setup, "stdout.write('a')", "can't use `+` on nothing and an int");
}

#[test]
fn removing_from_an_id_map_might_find_nothing() {
    let program = "
        mut pool = IdMap[int]()
        id = pool.insert(1)
        stdout.print(pool.remove(id) + 1, pool[id] + 1)
    ";
    assert_eq!(in_main_after(ID_MAP, program), ["this might be None @ pool.remove(id)"]);
}

#[test]
fn builtins_give_back_their_types() {
    plus_one("", "ignore([1].index(1), 'fine')", "can't use `+` on nothing and an int");
    plus_one("", "Ok(1)", "this is the result of something that can fail");
    plus_one("", "Err(IoError('x'))", "this is the result of something that can fail");
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
        in_main_after(ID_MAP, program),
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
        [
            "an int can't be called @ value",
            "`missing` doesn't exist @ missing",
            "can't tell the type of `n` @ n => n",
            "`also_missing` doesn't exist @ also_missing",
        ]
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
    plus_one_imported(setup, "queue.len() / 2", "can't mix int and float in `+`");
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
        from std import random

        def main(stdout: Stdout, clock: Clock):
            mut rng = random.Random(seed=1)
            stdout.print(rng.randint(1, 6), clock.now())
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn every_modules_names_have_to_be_imported() {
    let program = "
        position = Vec3(x=1.0)
        mut pool = IdMap[int]()
        stdout.print(sqrt(2.0), PI, floor(1.5), Path('a'), SoaList[int]())
    ";
    assert_eq!(
        in_main(program),
        [
            "`Vec3` doesn't exist @ Vec3",
            "`IdMap` doesn't exist @ IdMap",
            "`sqrt` doesn't exist @ sqrt",
            "`PI` doesn't exist @ PI",
            "`floor` doesn't exist @ floor",
            "`Path` doesn't exist @ Path",
            "`SoaList` doesn't exist @ SoaList",
        ]
    );
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(cos(0.0))"),
        ["import it from std.math, like `from std.math import cos`"]
    );
    // Through a module imported whole, its types can be built and used as types.
    let program = "
        from std import math, game

        def main(stdout: Stdout):
            mut pool = game.IdMap[int]()
            id = pool.insert(1)
            stdout.print(math.sqrt(2.0) + 'a', math.PI + 'a', math.Vec3.zero().x + 'a', pool[id] + 'a')
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on a float and a str @ math.sqrt(2.0) + \"a\"",
            "can't use `+` on a float and a str @ math.PI + \"a\"",
            "can't use `+` on a float and a str @ math.Vec3.zero().x + \"a\"",
            "can't use `+` on an int and a str @ pool[id] + \"a\"",
        ]
    );
}

#[test]
fn capabilities_and_errors_need_no_import() {
    let program = "
        def read(fs: Fs, path: str):
            match fs.read_text(path):
                case Ok(text): text
                case IoError(reason): reason

        def main(stdout: Stdout, fs: Fs, clock: Clock):
            stdout.print(read(fs, 'a.txt'), InvalidValue('no'), ParseError('no', 1, 1), clock.now())
    ";
    assert_eq!(problems(program), Vec::<String>::new());
}

#[test]
fn file_system_methods_give_back_their_types() {
    let program = "
        def main(stdout: Stdout, fs: Fs):
            found = fs.metadata('a')?
            paths = fs.list_dir('.')?
            stdout.print(found.size + 'a', found.is_dir + 1, paths[0].name() + 1, fs.remove('a') + 1)
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on an int and a str @ found.size + \"a\"",
            "can't use `+` on a bool and an int @ found.is_dir + 1",
            "can't use `+` on a str and an int @ paths[0].name() + 1",
            "this is the result of something that can fail @ fs.remove(\"a\") + 1",
        ]
    );
    let program = "def main(stdout: Stdout, fs: Fs):\n    stdout.print(fs.list_dir())";
    assert_eq!(problems(program), ["`list_dir` was called with the wrong arguments @ fs.list_dir()"]);
}

#[test]
fn bytes_hold_ints_and_decode_to_text_that_might_fail() {
    let program = "
        def first(data: bytes) -> int:
            data[0]

        def main(stdout: Stdout, fs: Fs):
            data = 'fox'.encode()
            saved = fs.read_bytes('save.bin')?
            stdout.print(first(data) + 'a', data[1:] + 1, [byte + 'a' for byte in saved])
            stdout.print(data.decode() + 'a', bytes(['a']), bytes(), fs.write_bytes('a', 'text'))
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on an int and a str @ first(data) + \"a\"",
            "can't use `+` on bytes and an int @ data[1:] + 1",
            "can't use `+` on an int and a str @ byte + \"a\"",
            "this is the result of something that can fail @ data.decode() + \"a\"",
            "`bytes`'s argument should be a list[int], but this is a list[str] @ [\"a\"]",
            "`write_bytes`'s argument should be bytes, but this is a str @ \"text\"",
        ]
    );
}

#[test]
fn reading_numbers_from_text_can_fail_and_characters_have_codes() {
    let program = "
        stdout.print(int('4') + 1, float('2.5') + 1.0, int(2.5) + 'a', float(2) + 'a')
        stdout.print('A'.code() + 'a', 'Fox'.codes()[0] + 'a', str.from_code(65) + 'a', str.from_code('A'))
    ";
    assert_eq!(
        in_main(program),
        [
            "this is the result of something that can fail @ int(\"4\") + 1",
            "this is the result of something that can fail @ float(\"2.5\") + 1.0",
            "can't use `+` on an int and a str @ int(2.5) + \"a\"",
            "can't use `+` on a float and a str @ float(2) + \"a\"",
            "can't use `+` on an int and a str @ \"A\".code() + \"a\"",
            "can't use `+` on an int and a str @ \"Fox\".codes()[0] + \"a\"",
            "this is the result of something that can fail @ str.from_code(65) + \"a\"",
            "the code should be an int, but this is a str @ \"A\"",
        ]
    );
    // A type's own `from_code` is its own; only `str.from_code` makes a character.
    let program = "
        struct Glyph:
            code: int

            def from_code(code: int) -> Glyph:
                Glyph(code=code)

        def main(stdout: Stdout):
            stdout.print(Glyph.from_code(65).code + 1)
    ";
    assert_eq!(problems(program), Vec::<String>::new());
    assert_eq!(
        helps("def main(stdout: Stdout):\n    stdout.print(ord('A'), chr(65))"),
        [
            "a character's code is a method: write `c.code()`",
            "make a character from its code with `str.from_code(n)`",
        ]
    );
}

#[test]
fn arguments_and_input_are_text() {
    let program = "
        def main(stdout: Stdout, args: Args, stdin: Stdin):
            stdout.print([arg + 1 for arg in args], args[0] + 1, args.len() + 'a', args.program + 1)
            stdout.print(stdin.lines()[0] + 1, args['a'])
    ";
    assert_eq!(
        problems(program),
        [
            "can't use `+` on a str and an int @ arg + 1",
            "can't use `+` on a str and an int @ args[0] + 1",
            "can't use `+` on an int and a str @ args.len() + \"a\"",
            "can't use `+` on a str and an int @ args.program + 1",
            "can't use `+` on a str and an int @ stdin.lines()[0] + 1",
            "an index must be an int, not a str @ \"a\"",
        ]
    );
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

#[test]
fn a_std_function_written_in_fennec_has_to_be_imported() {
    let problems = problems("def main(stdout: Stdout):\n    stdout.print(lerp(0.0, 1.0, 0.5))\n");
    assert_eq!(problems, ["`lerp` doesn't exist @ lerp"]);
}

#[test]
fn a_std_function_written_in_fennec_is_checked_like_any_other() {
    let text = "from std.math import lerp\n\ndef main(stdout: Stdout):\n    stdout.print(lerp(0.0, 1.0))\n";
    assert!(problems(text).iter().any(|problem| problem.contains("amount")), "{:?}", problems(text));
}
