//! Errors as values: `fail` needs an error, and the result of something that can fail has to
//! be dealt with by `?`, `else` or `match` before it's used.

use crate::{helps, problems};

/// A program with the fixes of its problems made, the way `fennec fix` makes them.
fn fixed(source: &str) -> String {
    let Ok(module) = fennec_syntax::parse(source) else { return source.to_owned() };
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    fennec_syntax::apply_fixes(source, 0, &diagnostics).unwrap_or_else(|| source.to_owned())
}

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
fn question_mark_and_else_get_the_value_out() {
    let program = "
        def total(n: int):
            a = risky(n)? + 1
            b = (risky(n) else 0) + 1
            c = (player('x')?).name.upper()
            d = risky(n) else as oops:
                oops.why.len()
            e = risky(n).expect('small') + 1
            a + b + d + e
    ";
    assert_eq!(problems(&format!("{RISKY}\n{}", crate::dedent(program))), Vec::<String>::new());
}

/// A `match` over the failure `else as` names handles each of an error's cases, or has `case _`,
/// as any `match` over an enum does.
#[test]
fn a_match_over_a_failure_handles_each_case() {
    let one = "error Bad:\n    Empty\n    Long(size: int)\n\n\
        def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    n\n\n\
        def f(n: int) -> int:\n    load(n) else as why:\n        match why:\n            case .Empty: 1\n";
    assert_eq!(problems(one), ["this `match` doesn't handle `Bad.Long` @ why"]);
    let all = format!("{one}            case .Long(size): size\n");
    assert_eq!(problems(&all), Vec::<String>::new());
    let caught = format!("{one}            case _: 0\n");
    assert_eq!(problems(&caught), Vec::<String>::new());
}

/// `except` handles a failure as `else as` does, in its three forms: any failure named, `case`
/// arms under the name, and one case on the line. One case covers only a call that fails in no
/// other way, so one that leaves a case out is refused, as a `match` that does is.
#[test]
fn except_handles_a_failure_in_three_forms() {
    let declared = "error Bad:\n    Empty\n    Long(size: int)\n\n\
        def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    n\n\n\
        def only(n: int) -> int fails InvalidValue:\n    if n == 1: fail InvalidValue(message='one')\n    n\n\n\
        def act(n: int) fails InvalidValue:\n    if n == 1: fail InvalidValue(message='one')\n\n";
    let fine = "def named(n: int) -> int:\n    x = load(n) except why:\n        return why.notes().len()\n    x\n\n\
        def arms(n: int) -> int fails Bad:\n    x = load(n) except why:\n        case .Long(size):\n            size\n        case _:\n            fail why\n    x\n\n\
        def one(n: int) -> int:\n    x = only(n) except InvalidValue(): 8080\n    y = only(n) except InvalidValue(): return 0\n    x + y\n\n\
        def bare(n: int) -> str:\n    act(n) except why:\n        return why.message\n    return \"\"\n";
    assert_eq!(problems(&format!("{declared}{fine}")), Vec::<String>::new());
    let one_of_two = "def f(n: int) -> int:\n    x = load(n) except .Empty: 0\n    x\n";
    assert_eq!(
        problems(&format!("{declared}{one_of_two}")),
        ["this `except` doesn't handle `Bad.Long` @ except"]
    );
}

/// What `except` names goes somewhere on every way out: read into an argument, an f-string, a
/// field read or `fail why`. Looking at it with `match` or `is`, or throwing it away with `_ =`,
/// drops it. A `case _:` under it uses it too, and nothing else may drop every failure.
#[test]
fn except_drops_no_failure_without_a_word() {
    let declared = "error Bad:\n    Empty\n    Long(size: int)\n\n\
        error Other:\n    why: str\n\n\
        def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    return n\n\n\
        def plain(n: int) -> int:\n    return n\n\n\
        def maybe(n: int) -> int?:\n    return None\n\n";
    let body = |handled: &str| -> Vec<String> {
        let program = format!("{declared}def f(n: int) -> int fails Other:\n    {handled}\n    return 0\n");
        problems(&program)
            .into_iter()
            .map(|problem| problem.split(" @ ").next().unwrap_or_default().to_owned())
            .collect()
    };
    let dropped = "`why` isn't used on this way out, so it drops the failure";
    assert_eq!(body("x = load(n) except why:\n        return 0"), [dropped]);
    assert_eq!(body("x = load(n) except why:\n        _ = why\n        return 0"), [dropped]);
    assert_eq!(
        body(
            "x = load(n) except why:\n        if n > 1:\n            return 0\n        return why.notes().len()"
        ),
        [dropped]
    );
    // A `match` over it is the arms under `except`, written another way.
    let matched =
        body("x = load(n) except why:\n        match why:\n            case _:\n                return 0");
    assert!(matched.contains(&"`match why:` goes away under `except`".to_owned()), "{matched:?}");
    for used in [
        "x = load(n) except why:\n        return why.notes().len()",
        "x = load(n) except why:\n        fail Other(why=f\"{why}\")",
        "x = load(n) except why:\n        fail Other(why=\"lost\")",
        "x = load(n) except why:\n        case .Empty:\n            0\n        case .Long(size):\n            size",
        "x = load(n) except why:\n        case .Empty:\n            0\n        case _:\n            fail Other(why=f\"{why}\")",
    ] {
        assert_eq!(body(used), Vec::<String>::new(), "{used}");
    }
    assert_eq!(
        body(
            "x = load(n) except why:\n        case .Empty:\n            0\n        case _:\n            return 1"
        ),
        ["`case _:` takes every other failure, so it uses `why`"]
    );
    assert_eq!(
        body(
            "x = load(n) except why:\n        case .Empty:\n            0\n        case other:\n            return 1"
        ),
        ["`case other:` names the failure again, which `why` already names"]
    );
    assert_eq!(body("x = load(n) except _: 0"), ["`except _` drops every failure"]);
    assert_eq!(
        body("x = load(n) except Bad(): 0"),
        ["`Bad()` takes every case of `Bad`, which drops them all"]
    );
    assert_eq!(
        body("x = plain(n) except why:\n        return why.notes().len()"),
        ["this can't fail, so `except` has nothing to handle"]
    );
    let optional = helps(&format!(
        "{declared}def f(n: int) -> int:\n    x = maybe(n) except why:\n        return 1\n    return 0\n"
    ));
    assert!(optional.iter().any(|help| help.contains("takes `else`")), "{optional:?}");
    assert!(
        body("x = n except why:\n        return why.notes().len()")
            .contains(&"`except` goes right after the call it handles".to_owned())
    );
}

/// A function that can fail says so in its signature, with `fails`, and `fennec fix` writes
/// it: with its return type, with nothing given back, and with several errors. `main` doesn't,
/// since nothing calls it.
#[test]
fn a_function_that_can_fail_says_fails() {
    let program = "error TooBig:\n    limit: int\n\nerror Empty:\n    pass\n\n\
        def word(n: int) -> str:\n    if n > 2: fail TooBig(limit=2)\n    \"w\"\n\n\
        def check(n: int):\n    if n > 2: fail TooBig(limit=2)\n\n\
        def both(n: int) -> int:\n    if n == 0: fail Empty()\n    _ = word(n)?\n    n\n\n\
        def main(stdout: Stdout):\n    stdout.print(word(1)?)\n";
    assert_eq!(
        crate::problems_as_written(program),
        [
            "`word` can fail with TooBig, so its signature says `fails TooBig` @ def word(n: int) -> str",
            "`check` can fail with TooBig, so its signature says `fails TooBig` @ def check(n: int)",
            "`both` can fail with Empty | TooBig, so its signature says `fails Empty | TooBig` @ def both(n: int) -> int",
        ]
    );
    let module = fennec_syntax::parse(program).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    let fixed = fennec_syntax::apply_fixes(program, 0, &diagnostics).unwrap();
    for written in ["-> str fails TooBig:", "def check(n: int) fails TooBig:", "-> int fails Empty | TooBig:"]
    {
        assert!(fixed.contains(written), "{fixed}");
    }
    assert_eq!(crate::problems_as_written(&fixed), Vec::<String>::new());
}

/// Every `error` type carries notes, as Python's exceptions do: `notes()` reads them and
/// `add_note(text)` adds one, which changes the error, so it needs a mut name. Other types have
/// neither.
#[test]
fn an_error_carries_notes() {
    let found = |body: &str| crate::in_main_after(RISKY, body);
    let empty: [&str; 0] = [];
    assert_eq!(
        found(
            "_ = risky(5) else as failure:\n    mut noted = failure\n    noted.add_note('while loading')\n    texts: list[str] = noted.notes()\n    _ = texts\n    0\n"
        ),
        empty
    );
    assert_eq!(
        found("_ = risky(5) else as failure:\n    failure.add_note('x')\n    0\n"),
        ["`failure` isn't mut @ failure"]
    );
    assert_eq!(
        found("_ = risky(5) else as failure:\n    mut noted = failure\n    noted.add_note(3)\n    0\n"),
        ["`add_note` takes the note's text, a str, but this is an int @ 3"]
    );
    assert_eq!(found("p = player('a')?\n_ = p.notes()\n").len(), 1);
}

#[test]
fn ignored_results_are_mistakes() {
    let program = "
        risky(3)
        if True:
            risky(3)
        for n in [1]:
            risky(n)
        _ = risky(3)
        _ = risky(3)?
        stdout.print('done')
    ";
    assert_eq!(
        with_risky(program),
        [
            "this can fail, but nothing handles it @ risky(3)",
            "this can fail, but nothing handles it @ risky(3)",
            "this can fail, but nothing handles it @ risky(n)",
            "`_ =` would drop this failure @ risky(3)",
        ]
    );
    let help = helps(&format!("{RISKY}\ndef f():\n    risky(3)\n    1\n"));
    assert_eq!(help, ["pass the error up with `?`, give a default with `else`, or handle it with `match`"]);
}

#[test]
fn a_function_passes_a_failure_up_only_with_a_question_mark() {
    let program = "
        def passthrough(n: int) -> int fails Oops:
            risky(n)

        def early(n: int) -> int fails Oops:
            if n > 0:
                return risky(n)
            risky(n)?
    ";
    let program = format!("{}\n{}", crate::dedent(RISKY), crate::dedent(program));
    assert_eq!(
        problems(&program),
        [
            "this can fail, and a failure passes up only with `?` @ risky(n)",
            "this can fail, and a failure passes up only with `?` @ risky(n)"
        ]
    );
    let module = fennec_syntax::parse(&program).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    let fixed = fennec_syntax::apply_fixes(&program, 0, &diagnostics).unwrap();
    assert_eq!(fixed.matches("risky(n)?").count(), 3, "{fixed}");
}

#[test]
fn passing_a_result_where_a_value_is_wanted_is_a_mistake() {
    let program = "
        def double(n: int):
            n * 2

        def main(stdout: Stdout):
            _ = double(risky(1))
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
            if fs.exists(path)?:
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
        "{NET_ERROR}def connect(host: str, port: int) -> int fails DialError:\n    if host == \"\":\n        \
         fail .NotFound(host)\n    if port == 0:\n        fail .Closed\n    port\n\n\
         def described(port: int) -> str:\n    got = connect(\"db\", port) else as why:\n        match why:\n            \
         case .Refused(host, port): return f\"refused {{host}}:{{port}}\"\n            \
         case .NotFound(host): return f\"no {{host}}\"\n            case _: return f\"failed: {{why}}\"\n    \
         f\"port {{got}}\"\n"
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
            "    problem = DialError.Closed\n    match problem:\n        case .Closed: stdout.print(\"closed\")\n        \
             case .NotFound(host): stdout.print(host)\n"
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
         fail .NotFound(path)\n    if path == \"bad\":\n        fail .BadRow(3)\n    1.5\n\n\
         def total(paths: list[str]) -> float fails LoadError:\n    mut sum = 0.0\n    for path in paths:\n        \
         sum += load(path)?\n    sum\n\ndef check(path: str) fails LoadError:\n    if load(path)? < 0.0:\n        \
         fail .BadRow(0)\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
}

#[test]
fn fails_can_name_several_errors_and_question_mark_passes_any_up() {
    let text = format!(
        "{LOAD_ERROR}error ParseError:\n    Bad(text: str)\n\n\
         def parse(text: str) -> int fails ParseError:\n    if text == \"\":\n        fail .Bad(text)\n    1\n\n\
         def load(path: str) -> int fails LoadError | ParseError:\n    if path == \"\":\n        \
         fail .NotFound(path)\n    parse(path)?\n\n\
         def apply(read: (str) -> int fails LoadError | ParseError, path: str) -> int fails LoadError | ParseError:\n    \
         read(path)?\n\n\
         def main(stdout: Stdout):\n    n = load(\"a\") else as why:\n        match why:\n            \
         case .NotFound(path): stdout.print(path)\n            case .Bad(text): stdout.print(text)\n            \
         case _: pass\n        -1\n    stdout.print(n)\n    stdout.print(apply(path => load(path)?, \"b\") else 0)\n    \
         reader: (str) -> int fails LoadError | ParseError = path => load(path)?\n    \
         stdout.print(said(() => reader(\"c\")?))\n\n\
         def said(run: () -> int fails LoadError | ParseError) -> int:\n    run() else -1\n"
    );
    assert_eq!(problems(&text), Vec::<String>::new());
    assert_eq!(
        problems(&format!(
            "{LOAD_ERROR}def load(path: str) -> int fails LoadError | int:\n    1\n\n\
             def read(path: str) -> int fails LoadError | Other:\n    if path == \"\":\n        fail .Gone(path)\n    1\n\n\
             def other() -> int fails Other:\n    fail Other(why=\"no\")\n\n\
             def check(path: str) -> int fails LoadError:\n    other()?\n"
        )),
        [
            "`|` joins errors, but `int` is an int @ int",
            "no enum has a case called `Gone` @ .Gone(path)",
            "`check` says it fails with `LoadError`, but it can fail with `Other` too @ LoadError",
        ]
    );
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
        problems("struct Point:\n    x = 0\n\ndef load() -> int fails Point:\n    1\n"),
        ["`fails` names an error, but `Point` is a Point @ Point"]
    );
}

#[test]
fn a_result_passed_on_adds_its_errors_to_the_functions() {
    let program = "error Bad:\n    why: str\n\nerror Worse:\n    why: str\n\ndef inner(n: int) -> int:\n    \
                   if n < 0:\n        fail Bad(why=\"neg\")\n    n\n\ndef outer(n: int) -> int:\n    if n > 9:\n        \
                   fail Worse(why=\"big\")\n    inner(n)?\n";
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

/// A failure passes up from an arrow function only with `?`, as from a `def`, and `fennec fix`
/// adds it. What it passes up is the arrow's own failure, not one more way the function around it
/// fails. An arrow that can fail is refused where a builtin takes a function, as `sorted`'s
/// `key=`: nothing there is told to expect a failure. Where this got through, the JIT ended the
/// program there with no error and an exit code of 0.
#[test]
fn an_arrow_function_passes_a_failure_up_only_with_question_mark() {
    let unmarked = "error Bad:\n    why: str\n\ndef half(n: int) -> int fails Bad:\n    if n < 0:\n        fail Bad(why=\"neg\")\n    n // 2\n\ntest \"half\":\n    assert failure_of(() => half(-1)) is not None\n";
    assert_eq!(problems(unmarked), ["this can fail, and a failure passes up only with `?` @ half(-1)"]);
    assert!(fixed(unmarked).contains("failure_of(() => half(-1)?)"), "{}", fixed(unmarked));
    assert_eq!(problems(&fixed(unmarked)), Vec::<String>::new());
    // The arrow's `?`s, with `from` too, pass up its own failures, and a `def` takes it as one
    // that fails.
    let marked = "error Bad:\n    why: str\n\ndef twice(run: () -> int fails Bad | InvalidValue) -> int fails Bad | InvalidValue:\n    run()? + run()?\n\ndef main(stdout: Stdout):\n    stdout.print(twice(() => int(\"1\")? + (int(\"2\")? from Bad(why=\"no\"))) else 0)\n";
    assert_eq!(problems(marked), Vec::<String>::new());
    let said = "this arrow function can fail, but where it's passed takes one that can't";
    for body in [
        "kept = sorted(['1'], key=text => int(text)?)\nstdout.print(kept)",
        "kept = sorted(['1'], key=text => [int(part)? for part in [text]][0])\nstdout.print(kept)",
    ] {
        let source = format!(
            "error Bad:\n    why: str\n\ndef main(stdout: Stdout):\n    {}\n",
            body.replace('\n', "\n    ")
        );
        let found = problems(&source);
        assert_eq!(found.iter().filter(|one| one.starts_with(said)).count(), 1, "{body}: {found:?}");
        assert_eq!(found.len(), 1, "{body}: {found:?}");
    }
    // The failure is the arrow's, not one more way the function around it fails.
    let around = "def around() -> list[str]:\n    sorted(['1'], key=text => int(text)?)\n";
    let found = problems(around);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found.iter().all(|one| one.starts_with(said)), "{found:?}");
    // After an arrow, `?` is as it was.
    let after = "def after(text: str) -> int fails InvalidValue:\n    kept = sorted(['1'], key=one => one)\n    int(text)? + kept.len()\n\ndef main(stdout: Stdout):\n    stdout.print(after(\"1\") else 0)\n";
    assert_eq!(problems(after), Vec::<String>::new());
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

#[test]
fn else_as_is_for_what_can_fail_and_its_block_leaves() {
    let found = problems(
        "def main(stdout: Stdout):\n    held: int? = None\n    value = held else as failure:\n        stdout.print(failure)\n        return\n    stdout.print(value)\n",
    );
    assert_eq!(found, ["this can't fail, so `else as` has no failure to name @ held"]);
    let ends_on_nothing = problems(
        "error Oops:\n    pass\n\ndef risky() -> int fails Oops:\n    fail Oops()\n\ndef main(stdout: Stdout):\n    value = risky() else as failure:\n        stdout.print(failure)\n    stdout.print(value)\n",
    );
    assert_eq!(
        ends_on_nothing,
        [
            "this `else` block must give an `int` or leave; its last line `stdout.print(...)` gives nothing @ stdout.print(failure)"
        ]
    );
}

/// An `else:` block, or `else as why:`, leaves or gives the value in place of what failed or was
/// `None`, as its last line, of that one's type. One that ends on nothing says what it must give
/// and what its last line is.
#[test]
fn an_else_block_gives_a_value_or_leaves() {
    let declared = "error Oops:\n    why: str\n\nstruct World:\n    size: int\n\ndef load(n: int) -> World fails Oops:\n    if n > 1:\n        fail Oops('big')\n    World(size=n)\n\ndef find(n: int) -> int?:\n    if n > 1:\n        return None\n    n\n";
    let fine = format!(
        "{declared}\ndef main(stdout: Stdout):\n    world = load(3) else as why:\n        stdout.print(why.why)\n        World(size=0)\n    kept = load(1) else:\n        World(size=-1)\n    found = find(5) else:\n        stdout.print('none')\n        0\n    for at in range(2):\n        held = load(at) else:\n            continue\n        stdout.print(held.size)\n    stdout.print(world.size, kept.size, found)\n"
    );
    assert_eq!(problems(&fine), Vec::<String>::new());
    let set = format!(
        "{declared}\ndef main(stdout: Stdout):\n    mut total = 0\n    world = load(3) else as why:\n        total = 1\n    stdout.print(world.size, total)\n"
    );
    assert_eq!(
        problems(&set),
        [
            "this `else` block must give a `World` or leave; its last line `total = ...` gives nothing @ total = 1"
        ]
    );
    let wrong = format!(
        "{declared}\ndef main(stdout: Stdout):\n    found = find(5) else:\n        'none'\n    stdout.print(found)\n"
    );
    assert_eq!(problems(&wrong).len(), 1, "{:?}", problems(&wrong));
}

/// A one-line `if` and a fallback `else` in one expression read two ways, so they need
/// parentheses, and the fix writes them where they're read now. In parentheses they're fine,
/// and a side of a one-line `if` that can fail says that's the problem.
#[test]
fn a_one_line_if_and_a_fallback_else_need_parentheses_to_mix() {
    let said = "a one-line `if` and a fallback `else` need parentheses to share an expression";
    for (body, fixed) in [
        ("int(text) else 80 if given else 8080", "int(text) else (80 if given else 8080)"),
        ("8080 if not given else int(text) else 80", "8080 if not given else (int(text) else 80)"),
    ] {
        let source = format!("def port(text: str, given: bool) -> int:\n    {body}\n");
        let problem = fennec_syntax::parse(&source).err();
        assert_eq!(problem.as_ref().map(|problem| problem.message.as_str()), Some(said), "{body}");
        let written = fennec_syntax::apply_fixes(&source, 0, problem.as_slice());
        assert_eq!(written, Some(format!("def port(text: str, given: bool) -> int:\n    {fixed}\n")));
    }
    let parenthesized =
        "def port(text: str, given: bool) -> int:\n    (int(text) else 80) if given else 8080\n";
    assert!(fennec_syntax::parse(parenthesized).is_ok());
    let chained = "def size(n: int) -> int:\n    1 if n < 1 else 2 if n < 2 else 3\n";
    assert!(fennec_syntax::parse(chained).is_ok());
    let unhandled = "def port(text: str, given: bool) -> int:\n    int(text) if given else 8080\n";
    assert_eq!(problems(unhandled), ["this can fail, but nothing handles it @ int(text)"]);
}

/// On `T? fails E`, a plain `else` would handle the failure and the `None` alike, so it's refused,
/// on one line or in a chain, with `?` first as the fix to make by hand. `?` then `else`, and
/// `else as why:`, which handles only the failure, check.
#[test]
fn a_plain_else_cant_handle_a_failure_and_a_none_alike() {
    let declared = "error Bad:\n    why: str\n\ndef cached(n: int) -> int? fails Bad:\n    if n > 5:\n        fail Bad('broken')\n    if n > 2:\n        return None\n    n\n";
    let said = "this can fail as well as give `None`, and a plain `else` would handle both";
    for line in [
        "found = cached(7) else 0",
        "found = cached(1) else None else 0",
        "found = cached(1) else:\n        0",
    ] {
        let program = format!("{declared}\ndef main(stdout: Stdout):\n    {line}\n    stdout.print(found)\n");
        assert_eq!(
            problems(&program),
            [format!("{said} @ cached({})", if line.contains('7') { 7 } else { 1 })],
            "{line}"
        );
    }
    let fine = format!(
        "{declared}\ndef first(n: int) -> int fails Bad:\n    cached(n)? else 0\n\ndef main(stdout: Stdout):\n    held = cached(7) else as why:\n        None\n    stdout.print(held, first(1) else 2)\n"
    );
    assert_eq!(problems(&fine), Vec::<String>::new());
}

/// A block gives a value to a name only as `x = match`, `x = lock ... as`, or `x = f() else as
/// why:`. An `if` over several lines doesn't, and says what to write instead.
#[test]
fn an_if_over_several_lines_gives_no_value() {
    let parsed = fennec_syntax::parse(
        "def f(n: int) -> int:\n    x = if n > 2:\n        1\n    else:\n        2\n    x\n",
    );
    let problem = parsed.err();
    assert_eq!(
        problem.as_ref().map(|problem| problem.message.as_str()),
        Some("`if` doesn't give a value over several lines")
    );
    assert!(
        problem
            .and_then(|problem| problem.help)
            .is_some_and(|help| help.contains("x = a if condition else b"))
    );
    let locked = "def f(scores: shared list[int]) -> int:\n    total = lock scores as current:\n        sum(current)\n    total\n";
    assert_eq!(problems(locked), Vec::<String>::new());
}

/// `assert value fails Pattern` is for what can fail, with a pattern of what it fails with, and
/// `fails` checks a failure only there.
#[test]
fn an_assert_that_something_fails_takes_what_can_fail() {
    let declared = "error Oops:\n    line: int\n\ndef load(n: int) -> int fails Oops:\n    if n > 1:\n        fail Oops(line=n)\n    n\n\ndef plain(n: int) -> int:\n    n\n";
    let fine = format!(
        "{declared}\ntest \"load\":\n    assert load(3) fails Oops(line=3)\n    assert load(3) fails Oops()\n"
    );
    assert_eq!(problems(&fine), Vec::<String>::new());
    let plain = format!("{declared}\ntest \"plain\":\n    assert plain(3) fails Oops()\n");
    assert_eq!(problems(&plain), ["this can't fail, so `assert ... fails` has nothing to check @ plain(3)"]);
    let parsed = fennec_syntax::parse(&format!(
        "{declared}\ndef main():\n    if load(3) fails Oops():\n        pass\n"
    ));
    assert_eq!(
        parsed.err().map(|problem| problem.message),
        Some("`fails` checks a failure only in `assert`".to_owned())
    );
}

/// What `else as why:` names when a call can fail several ways is matched as any union is: each
/// error has a case or `_` catches the rest, a later case holds what the earlier ones left, and
/// what can fail can't be compared as it is.
#[test]
fn a_failure_of_several_kinds_is_matched_case_by_case() {
    let declared = "error Gone:\n    path: str\n\nerror Bad:\n    line: int\n\n    def said(self) -> str:\n        f\"line {self.line}\"\n\ndef load(n: int) -> int fails Gone | Bad:\n    if n == 1:\n        fail Gone(path='x')\n    if n == 2:\n        fail Bad(line=2)\n    n\n";
    let missing = format!(
        "{declared}\ndef main(stdout: Stdout):\n    got = load(1) else as why:\n        match why:\n            case Gone(path):\n                stdout.print(path)\n        0\n    stdout.print(got)\n"
    );
    assert_eq!(problems(&missing), ["this `match` doesn't handle `Bad` @ why"]);
    let narrowed = format!(
        "{declared}\ndef main(stdout: Stdout):\n    got = load(2) else as why:\n        match why:\n            case Gone(path):\n                stdout.print(path)\n            case rest:\n                stdout.print(rest.said())\n        0\n    stdout.print(got)\n"
    );
    assert_eq!(problems(&narrowed), Vec::<String>::new());
    let compared = format!("{declared}\ndef main(stdout: Stdout):\n    stdout.print(load(1) == load(2))\n");
    assert_eq!(problems(&compared), ["this is the result of something that can fail @ load(1) == load(2)"]);
}

/// `Ok`, `Err` and `Result` are gone. Where a program writes one, it's refused with what to write
/// instead, and the fix writes `return Err(e)` as `fail e` and `Ok(x)` as `x`. What lowering
/// writes of them, for `?` and `else`, still checks.
#[test]
fn ok_err_and_result_are_refused_with_what_to_write() {
    let program = "error Bad:\n    why: str\n\ndef half(n: int) -> int fails Bad:\n    if n < 0:\n        return Err(Bad(\"small\"))\n    return Ok(n - 1)\n\ndef show(n: int) -> str:\n    match half(n):\n        case Ok(value): str(value)\n        case Err(problem): problem.why\n\ndef held(found: Result[int, Bad]) -> int:\n    0\n";
    assert_eq!(
        crate::problems_as_written(program),
        [
            "`Err` is gone: a function fails with `fail error` @ Err(Bad(\"small\"))",
            "`Ok` is gone: a value is given as it is @ Ok(n - 1)",
            "`Ok` and `Err` are gone: a failure is handled with `else as why:` @ case Ok(value): str(value)\n",
            "`Result` is gone: what can fail says so with `fails` @ Result[int, Bad]",
        ]
    );
    let module = fennec_syntax::parse(program).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    let fixed = fennec_syntax::apply_fixes(program, 0, &diagnostics).unwrap();
    for written in ["        fail Bad(\"small\")\n", "    return n - 1\n"] {
        assert!(fixed.contains(written), "{fixed}");
    }
    let lowered = "error Bad:\n    why: str\n\ndef half(n: int) -> int fails Bad:\n    if n < 0:\n        fail Bad(\"small\")\n    n - 1\n\ndef twice(n: int) -> int fails Bad:\n    half(n)? + (half(n) else 0)\n";
    assert_eq!(crate::problems_as_written(lowered), Vec::<String>::new());
}

/// `failure_of` is only for tests. Anywhere else, in `main` or in a helper a test calls, it's
/// refused, and the help points at `else as why:`, which names a failure where it happens.
#[test]
fn failure_of_is_only_for_tests() {
    let declared = "error Bad:\n    why: str\n\ndef half(n: int) -> int fails Bad:\n    if n < 0:\n        fail Bad(\"small\")\n    n - 1\n";
    let said = "`failure_of` is only for tests @ failure_of(() => half(-1)?)";
    let help = "outside a test, name a failure where it happens with `x = f() else as why:`; a test checks one with `assert f() fails Oops()`";
    let in_main =
        format!("{declared}\ndef main(stdout: Stdout):\n    stdout.print(failure_of(() => half(-1)?))\n");
    assert_eq!(problems(&in_main), [said]);
    assert_eq!(helps(&in_main), [help]);
    let helper = format!("{declared}\ndef problem() -> Bad?:\n    failure_of(() => half(-1)?)\n");
    assert_eq!(problems(&helper), [said]);
    let in_test = format!("{declared}\ntest \"half\":\n    assert failure_of(() => half(-1)?) is not None\n");
    assert_eq!(problems(&in_test), Vec::<String>::new());
}

/// What can fail is read only through `?`, `else`, `.expect` or `assert ... fails`: held, printed,
/// compared, put in a collection or handed on as a plain value, it's refused. An arrow function
/// may give one back, for `failure_of`.
#[test]
fn what_can_fail_is_never_a_plain_value() {
    let declared = "error Bad:\n    why: str\n\ndef half(n: int) -> int fails Bad:\n    if n < 0:\n        fail Bad(\"small\")\n    n - 1\n\ndef keep[T](x: T) -> T:\n    x\n";
    let said = "this can fail, but nothing handles it";
    for line in [
        "stdout.print(half(3))",
        "held = half(3)",
        "items = [half(3)]",
        "stdout.print(f\"{half(3)}\")",
        "stdout.print(keep(half(3)))",
        "pair = (half(3), 1)",
        "counts = {\"a\": half(3)}",
        "stdout.print(str(half(3)))",
        "stdout.print(half(3) is None)",
        "stdout.print([half(n) for n in [1, 2]])",
    ] {
        let program = format!("{declared}\ndef main(stdout: Stdout):\n    {line}\n");
        let call = if line.contains("half(n)") { "half(n)" } else { "half(3)" };
        assert_eq!(crate::problems_as_written(&program), [format!("{said} @ {call}")], "{line}");
    }
    let fine = format!(
        "{declared}\ntest \"half\":\n    assert half(-1) fails Bad()\n    assert half(3)? == 2\n\ndef main(stdout: Stdout):\n    stdout.print(half(3) else 0, half(3).expect(\"3 is big enough\"))\n    held = half(3) else as why:\n        stdout.print(why.why)\n        0\n    stdout.print(held)\n\ntest \"its failure\":\n    problem = failure_of(() => half(-1)?)\n    assert problem is not None\n"
    );
    assert_eq!(crate::problems_as_written(&fine), Vec::<String>::new());
    // In a function that fails with what the call does, the fix passes the failure up.
    let passing = format!("{declared}\ndef shown(n: int) -> str fails Bad:\n    str(half(n))\n");
    let module = fennec_syntax::parse(&passing).unwrap();
    let diagnostics = fennec_check::check(&module).err().unwrap_or_default();
    let fixed = fennec_syntax::apply_fixes(&passing, 0, &diagnostics).unwrap();
    assert!(fixed.contains("    str(half(n)?)\n"), "{fixed}");
    assert_eq!(crate::problems_as_written(&fixed), Vec::<String>::new());
}
