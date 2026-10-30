//! Errors as values: `fail` needs an error, and the result of something that can fail has to
//! be dealt with by `?`, `else` or `match` before it's used.

use crate::{helps, problems};

const RISKY: &str = "
error Oops:
    why: str

struct Player:
    name: str

def risky(n: int):
    if n > 1:
        fail Oops('big')
    n

def player(name: str):
    if name == '':
        fail Oops('empty')
    Player(name=name)

def names():
    if False:
        fail Oops('never')
    ['a']
";

fn with_risky(body: &str) -> Vec<String> {
    crate::in_main_after(RISKY, body)
}

#[test]
fn fail_needs_an_error() {
    let program = "
        error Oops:
            why: str

        struct NotAnError:
            why: str

        def f(n: int):
            if n == 1:
                fail Oops('x')
            if n == 2:
                fail NotAnError('x')
            if n == 3:
                fail 'text'
            fail mystery
    ";
    assert_eq!(
        problems(program),
        [
            "`fail` needs an error, but this is a NotAnError @ NotAnError('x')".replace('\'', "\"").as_str(),
            "`fail` needs an error, but this is a str @ 'text'".replace('\'', "\"").as_str(),
            "`mystery` doesn't exist @ mystery",
        ]
    );
    assert_eq!(helps("def f():\n    fail 1\n"), ["declare error types with `error`, like `error NotFound:`"]);
}

#[test]
fn results_are_dealt_with_before_use() {
    let program = "
        stdout.print(risky(1) + 1, 1 + risky(1), risky(1) < 2, -risky(1))
        stdout.print(player('a').name, player('a').name.upper(), names()[0])
        for name in names():
            pass
        stdout.print(risky(1) == risky(2), risky(1) != Err(Oops('x')))
    ";
    assert_eq!(
        with_risky(program),
        [
            "this is the result of something that can fail @ risky(1) + 1",
            "this is the result of something that can fail @ 1 + risky(1)",
            "this is the result of something that can fail @ risky(1) < 2",
            "this is the result of something that can fail @ risky(1)",
            "this is the result of something that can fail @ player('a').name".replace('\'', "\"").as_str(),
            "this is the result of something that can fail @ player('a').name".replace('\'', "\"").as_str(),
            "this is the result of something that can fail @ names()[0]",
            "this is the result of something that can fail @ names()",
        ]
    );
}

#[test]
fn question_mark_else_and_match_get_the_value_out() {
    let program = "
        def total(n: int):
            a = risky(n)? + 1
            b = (risky(n) else 0) + 1
            c = (player('x')?).name.upper()
            d = match risky(n):
                case Ok(value): value + 1
                case Oops(why): len(why)
                case Err(error): 0
            e = risky(n).expect('small') + 1
            a + b + d + e
    ";
    assert_eq!(problems(&format!("{RISKY}\n{}", crate::dedent(program))), Vec::<String>::new());
}

#[test]
fn ignored_results_are_mistakes() {
    let program = "
        risky(3)
        if True:
            risky(3)
        for n in [1]:
            risky(n)
        ignore(risky(3), 'best effort')
        stdout.print('done')
    ";
    assert_eq!(
        with_risky(program),
        [
            "this can fail, but nothing handles it @ risky(3)",
            "this can fail, but nothing handles it @ risky(3)",
            "this can fail, but nothing handles it @ risky(n)",
        ]
    );
    let help = helps(&format!("{RISKY}\ndef f():\n    risky(3)\n    1\n"));
    assert_eq!(help, ["pass the error up with `?`, give a default with `else`, or handle it with `match`"]);
}

#[test]
fn a_function_gives_back_its_last_result_unhandled() {
    let program = "
        def passthrough(n: int):
            risky(n)
    ";
    assert_eq!(problems(&format!("{RISKY}\n{}", crate::dedent(program))), Vec::<String>::new());
}

#[test]
fn passing_a_result_where_a_value_is_wanted_is_a_mistake() {
    let program = "
        def double(n: int):
            n * 2

        def main(stdout: Stdout):
            double(risky(1))
            stdout.print(risky(1))
    ";
    assert_eq!(
        problems(&format!("{RISKY}\n{}", crate::dedent(program))),
        ["this is the result of something that can fail @ risky(1)"]
    );
}

#[test]
fn where_rules_make_construction_fail_unless_every_value_is_written_out() {
    let program = "
        struct Score:
            value: int where value >= 0

        def main(stdout: Stdout):
            n = 5
            stdout.print(Score(value=5).value, Score(value=n).value, (Score(n)?).value)
    ";
    assert_eq!(problems(program), ["this is the result of something that can fail @ Score(value=n).value"]);
}

#[test]
fn files_can_fail_to_read() {
    let program = "
        error Unreadable:
            path: str

        def read(fs: Fs, path: str):
            text = fs.read_text(path)? from Unreadable(path)
            if fs.exists(path):
                fs.write_text(path, text)?
            text.strip()

        def main(stdout: Stdout, fs: Fs):
            stdout.print(fs.read_text('x').upper(), read(fs, 'x') else '')
    ";
    assert_eq!(
        problems(program),
        ["this is the result of something that can fail @ fs.read_text('x')".replace('\'', "\"")]
    );
}

#[test]
fn builtins_need_values_not_results() {
    assert_eq!(
        with_risky("stdout.print(len(names()), abs(risky(1)))"),
        [
            "this is the result of something that can fail @ names()",
            "this is the result of something that can fail @ risky(1)",
        ]
    );
}
