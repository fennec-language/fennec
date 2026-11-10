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

/// A `match` over what something that can fail gives back handles its value and its failure, or
/// has `case _`, as any `match` handles every case: a failure would otherwise stop the program.
#[test]
fn a_match_over_a_result_handles_the_value_and_the_failure() {
    let found =
        |arms: &str| problems(&format!("{RISKY}\ndef f(n: int) -> int:\n    match risky(n):\n{arms}"));
    let empty: [&str; 0] = [];
    assert_eq!(
        found("        case Ok(value): value\n"),
        ["this `match` doesn't handle the failure @ risky(n)"]
    );
    assert_eq!(
        found("        case Err(error): 0\n"),
        ["this `match` doesn't handle the value it gives when it works @ risky(n)"]
    );
    // A guarded case might not be taken, so it covers nothing.
    assert_eq!(
        found("        case Ok(value): value\n        case Err(error) if n > 0: 0\n"),
        ["this `match` doesn't handle the failure @ risky(n)"]
    );
    assert_eq!(found("        case Ok(value): value\n        case Oops(why): why.len()\n"), empty);
    assert_eq!(found("        case Ok(value): value\n        case _: 0\n"), empty);
    assert_eq!(found("        case Ok(value): value\n        case Err(_): 0\n"), empty);
    // A name is bound to the value, so it doesn't handle the failure, as `_` does.
    assert_eq!(found("        case value: value\n"), ["this `match` doesn't handle the failure @ risky(n)"]);
    assert_eq!(found("        case _: 0\n"), empty);
    // Several errors are handled one by one, an enum's case by case.
    let several = "error Gone:\n    path: str\n\nerror Bad:\n    Empty\n    Long(size: int)\n\n\
        def load(n: int) -> int fails Gone | Bad:\n    if n == 0: fail Gone(path=\"a\")\n    if n == 1: fail .Empty\n    n\n\n\
        def f(n: int) -> int:\n    match load(n):\n        case Ok(value): value\n        case Err(Gone(path)): 0\n        case Err(.Empty): 1\n";
    assert_eq!(problems(several), ["this `match` doesn't handle the failure @ load(n)"]);
    let all = format!("{several}        case Err(.Long(size)): size\n");
    assert_eq!(problems(&all), empty);
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
            "match risky(5):\n    case Ok(n): pass\n    case Err(failure):\n        mut noted = failure\n        noted.add_note('while loading')\n        texts: list[str] = noted.notes()\n        _ = texts\n"
        ),
        empty
    );
    assert_eq!(
        found(
            "match risky(5):\n    case Ok(n): pass\n    case Err(failure):\n        failure.add_note('x')\n"
        ),
        ["`failure` isn't mut @ failure"]
    );
    assert_eq!(
        found(
            "match risky(5):\n    case Ok(n): pass\n    case Err(failure):\n        mut noted = failure\n        noted.add_note(3)\n"
        ),
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
         fail .NotFound(path)\n    if path == \"bad\":\n        return Err(LoadError.BadRow(3))\n    1.5\n\n\
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
         def main(stdout: Stdout):\n    match load(\"a\"):\n        case Ok(n): stdout.print(n)\n        \
         case Err(.NotFound(path)): stdout.print(path)\n        case Err(.Bad(text)): stdout.print(text)\n        \
         case Err(_): pass\n    stdout.print(apply(path => load(path), \"b\") else 0)\n    \
         reader: (str) -> int fails LoadError | ParseError = path => load(path)\n    \
         stdout.print(said(reader(\"c\")))\n\n\
         def said(found: Result[int, LoadError | ParseError]) -> int:\n    found else -1\n"
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
    // The failure is the arrow's mistake, not one more way the function around it fails.
    let around = "error Bad:\n    why: str\n\ndef around() -> list[str] fails Bad:\n    fail Bad(why=\"no\")\n    _ = sorted(['1'], key=text => int(text)?)\n";
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
