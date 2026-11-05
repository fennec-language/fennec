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
                case Oops(why): why.len()
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
        with_risky("stdout.print(names().len(), risky(1).abs())"),
        [
            "this is the result of something that can fail @ names()",
            "this is the result of something that can fail @ risky(1)",
        ]
    );
}

const NET_ERROR: &str =
    "error DialError:\n    NotFound(host: str)\n    Refused(host: str, port: int)\n    Closed\n\n";

#[test]
fn an_error_with_cases_fails_and_is_matched_like_an_enum() {
    let text = format!(
        "{NET_ERROR}def connect(host: str, port: int) -> int:\n    if host == \"\":\n        \
         fail DialError.NotFound(host)\n    if port == 0:\n        fail DialError.Closed\n    port\n\n\
         def described(port: int) -> str:\n    match connect(\"db\", port):\n        \
         case Err(Refused(host, port)): f\"refused {{host}}:{{port}}\"\n        \
         case Err(NotFound(host)): f\"no {{host}}\"\n        case Err(error): f\"failed: {{error}}\"\n        \
         case Ok(port): f\"port {{port}}\"\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
}

#[test]
fn an_error_with_cases_is_made_from_one_and_matched_whole() {
    let in_main = |body: &str| problems(&format!("{NET_ERROR}def main(stdout: Stdout):\n{body}"));
    assert_eq!(
        in_main("    problem = DialError(host=\"x\")\n    stdout.print(problem)\n"),
        ["`DialError` has cases, so pick one of its cases @ DialError(host=\"x\")"]
    );
    assert_eq!(
        in_main("    problem = DialError.Refused(\"x\", 1)\n    stdout.print(problem.host)\n"),
        ["`DialError` has cases, so its fields depend on its case @ problem.host"]
    );
    assert_eq!(
        in_main(
            "    problem = DialError.Closed\n    match problem:\n        case Closed: stdout.print(\"closed\")\n        \
             case NotFound(host): stdout.print(host)\n"
        ),
        ["this `match` doesn't handle `DialError.Refused` @ problem"]
    );
    assert_eq!(
        problems("error LoadError:\n    path: str\n    NotFound(path: str)\n"),
        ["parse: `LoadError` has both fields and cases"]
    );
}

const LOAD_ERROR: &str =
    "error LoadError:\n    NotFound(path: str)\n    BadRow(line: int)\n\nerror Other:\n    why: str\n\n";

#[test]
fn fails_names_the_one_error_a_function_can_fail_with() {
    let text = format!(
        "{LOAD_ERROR}def load(path: str) -> float fails LoadError:\n    if path == \"\":\n        \
         fail LoadError.NotFound(path)\n    if path == \"bad\":\n        return Err(LoadError.BadRow(3))\n    1.5\n\n\
         def total(paths: list[str]) -> float fails LoadError:\n    mut sum = 0.0\n    for path in paths:\n        \
         sum += load(path)?\n    sum\n\ndef check(path: str) fails LoadError:\n    if load(path)? < 0.0:\n        \
         fail LoadError.BadRow(0)\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
}

#[test]
fn only_the_error_fails_names_can_escape() {
    assert_eq!(
        problems(&format!(
            "{LOAD_ERROR}def load(path: str) -> int fails LoadError:\n    if path == \"\":\n        \
             fail Other(why=\"empty\")\n    1\n"
        )),
        ["`load` says it fails with `LoadError`, but it can fail with `Other` too @ LoadError"]
    );
    assert_eq!(
        problems(&format!(
            "{LOAD_ERROR}def load(path: str) -> int fails LoadError:\n    if path == \"\":\n        \
             return Err(Other(why=\"empty\"))\n    1\n"
        )),
        ["`load` says it fails with a LoadError, but this fails with an Other @ Err(Other(why=\"empty\"))"]
    );
    assert_eq!(
        problems("struct Point:\n    x = 0\n\ndef load() -> int fails Point:\n    1\n"),
        ["`fails` names an error, but `Point` is a Point @ Point"]
    );
}

#[test]
fn a_result_passed_on_adds_its_errors_to_the_functions() {
    let program = "error Bad:\n    why: str\n\nerror Worse:\n    why: str\n\ndef inner(n: int) -> int:\n    \
                   if n < 0:\n        fail Bad(why=\"neg\")\n    n\n\ndef outer(n: int) -> int:\n    if n > 9:\n        \
                   fail Worse(why=\"big\")\n    inner(n)\n";
    let errors = fennec_check::Type::Errors(vec!["Bad".to_owned(), "Worse".to_owned()]);
    assert_eq!(
        crate::returns(program, "outer"),
        Ok(fennec_check::Type::Result(Box::new(fennec_check::Type::Int), Box::new(errors)))
    );
}

const LOOKUPS: &str = "
error Missing:
    name: str

def add_up(names: list[str], ages: dict[str, int]) -> int:
    mut total = 0
    for name in names:
        age: int = ages.get(name) else continue
        total += age
    total

def age_of(ages: dict[str, int], name: str) -> int:
    age: int = ages.get(name) else return -1
    age

def show(ages: dict[str, int], name: str, stdout: Stdout):
    age: int = ages.get(name) else return
    stdout.print(age)

def must(ages: dict[str, int], name: str) -> int fails Missing:
    age: int = ages.get(name) else fail Missing(name=name)
    age

def parse(text: str) -> int:
    int(text) else return 0
";

#[test]
fn else_can_leave_and_what_it_gives_is_whats_inside() {
    assert_eq!(problems(LOOKUPS), Vec::<String>::new());
}

#[test]
fn leaving_with_else_is_checked_like_the_statement() {
    let wrong_value =
        "def age_of(ages: dict[str, int]) -> int:\n    age = ages.get('a') else return 'old'\n    age\n";
    assert_eq!(
        problems(wrong_value).len(),
        1,
        "the value `else return` gives has to fit the function: {:?}",
        problems(wrong_value)
    );
    let cant_fail = "def age_of(ages: dict[str, int]) -> int:\n    ages.get('a') else fail 'nope'\n";
    assert_eq!(problems(cant_fail).len(), 1, "{:?}", problems(cant_fail));
}

/// Nothing is told to expect a failure from a function used as a value. Where this got through,
/// the JIT ended the program there with no error and an exit code of 0.
#[test]
fn an_arrow_function_cant_pass_a_failure_up() {
    let said = "an arrow function can't pass a failure up with `?`";
    for body in [
        "kept = sorted(['1'], key=text => int(text)?)\nstdout.print(kept)",
        "kept = sorted(['1'], key=text => [int(part)? for part in [text]][0])\nstdout.print(kept)",
        "kept = sorted(['1'], key=text => int(text)? from Bad(why=text))\nstdout.print(kept)",
    ] {
        let source = format!(
            "error Bad:\n    why: str\n\ndef main(stdout: Stdout):\n    {}\n",
            body.replace('\n', "\n    ")
        );
        let found = problems(&source);
        assert_eq!(found.iter().filter(|one| one.starts_with(said)).count(), 1, "{body}: {found:?}");
        assert_eq!(found.len(), 1, "{body}: {found:?}");
    }
    // `?` still leaves from a comprehension, and an arrow can still give a value instead.
    let fine = "def parsed(texts: list[str]) -> list[int] fails InvalidValue:\n    [int(text)? for text in texts]\n\ndef main(stdout: Stdout):\n    stdout.print(sorted(['1'], key=text => int(text) else 0))\n";
    assert_eq!(problems(fine), Vec::<String>::new());
}

#[test]
fn an_arrow_function_or_comprehension_cant_leave_with_else() {
    let said = "an arrow function or comprehension can't leave with `else`";
    for body in [
        "ages = {'a': 1}\nkept = [ages.get(name) else continue for name in ['a', 'b']]\nstdout.print(kept)",
        "ages = {'a': 1}\nkept = [ages.get(name) else return 0 for name in ['a', 'b']]\nstdout.print(kept)",
        "ages = {'a': 1}\nkept = sorted(['a'], key=name => ages.get(name) else return 0)\nstdout.print(kept)",
    ] {
        let found = crate::in_main(body);
        assert!(found.iter().any(|one| one.starts_with(said)), "{body}: {found:?}");
    }
}
