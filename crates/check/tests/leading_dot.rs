//! A leading-dot case like `.Int(value)` takes its enum from the type expected where it's
//! written: a parameter's, a field's, the return type, a name's declared type, or the other
//! side of `==`. Only with nothing expected is a case more than one enum has ambiguous.

use crate::problems;

const ENUMS: &str = "
    enum Tok:
        Int(value: int)
        Name(text: str)
        Eof

    enum ExprKind:
        Int(value: int)
        Name(text: str)

    struct Expr:
        kind: ExprKind
";

fn with_enums(code: &str) -> Vec<String> {
    problems(&format!("{}\n{}", crate::dedent(ENUMS), crate::dedent(code)))
}

#[test]
fn the_return_type_chooses_the_enum() {
    let program = "
        def literal(tok: Tok) -> ExprKind?:
            match tok:
                case .Int(value): .Int(value)
                case .Name(text): return .Name(text)
                case .Eof: None
    ";
    assert_eq!(with_enums(program), Vec::<String>::new());
}

#[test]
fn a_parameter_or_field_chooses_the_enum() {
    let program = "
        def show(kind: ExprKind) -> str:
            match kind:
                case .Int(value): str(value)
                case .Name(text): text

        def main(stdout: Stdout):
            stdout.print(show(.Int(1)), show(kind=.Name('x')))
            expr = Expr(.Int(2))
            stdout.print(Expr(kind=.Name('y')).kind == expr.kind)
    ";
    assert_eq!(with_enums(program), Vec::<String>::new());
}

#[test]
fn a_name_or_the_other_side_of_equals_chooses_the_enum() {
    let program = "
        def main(stdout: Stdout):
            mut tok: Tok = .Int(1)
            tok = .Name('x')
            mut expr = Expr(.Int(1))
            expr.kind = .Name('y')
            stdout.print(tok == .Name('x'), .Int(1) != tok, tok == .Eof, expr)
    ";
    assert_eq!(with_enums(program), Vec::<String>::new());
}

#[test]
fn with_nothing_expected_a_shared_case_is_ambiguous() {
    let program = "
        def main(stdout: Stdout):
            stdout.print(.Int(1))
            n = 2
            _ = n == .Name('x')
            stdout.print(.Eof)
    ";
    assert_eq!(
        with_enums(program),
        [
            "more than one enum has a case called `Int` @ .Int(1)",
            "more than one enum has a case called `Name` @ .Name(\"x\")",
        ]
    );
}

#[test]
fn the_expected_enum_has_to_have_the_case() {
    let program = "
        def eof() -> ExprKind:
            .Eof
    ";
    assert_eq!(with_enums(program), ["`eof` returns an ExprKind, but this is a Tok @ .Eof"]);
}

#[test]
fn running_the_module_names_the_chosen_enum() {
    let source = format!(
        "{}\ndef literal(tok: Tok) -> ExprKind:\n    match tok:\n        case .Int(value): .Int(value)\n        case _: .Name(\"?\")\n",
        crate::dedent(ENUMS)
    );
    let mut module = fennec_syntax::parse(&source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let tree = format!("{module:?}");
    assert!(!tree.contains("Variant(\"Int\")") && !tree.contains("Variant(\"Name\")"), "{tree}");
    assert_eq!(tree.matches("Name(\"ExprKind\")").count(), 2, "{tree}");
}

const FAILURES: &str = "
    error DialError:
        NotFound(host: str)
        Closed

    error SaveError:
        Closed
        Full(left: int)
";

fn with_failures(code: &str) -> Vec<String> {
    problems(&format!("{}\n{}", crate::dedent(FAILURES), crate::dedent(code)))
}

/// `fail .Case` is a case of the error the function says it fails with, even if another error has
/// a case by that name too.
#[test]
fn a_failure_takes_its_error_from_what_the_function_says() {
    assert_eq!(
        with_failures(
            "
            def dial(host: str) -> int fails DialError:
                if host == \"\":
                    fail .Closed
                fail .NotFound(host)

            def save() fails SaveError:
                fail .Closed

            def held() -> DialError:
                .Closed
            "
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_failure_needs_to_be_able_to_tell_which_error() {
    let problems = with_failures(
        "
        def unsaid():
            fail .Closed
        ",
    );
    assert!(problems.iter().any(|problem| problem.contains("Closed")), "{problems:?}");

    let problems = with_failures(
        "
        def other() fails DialError:
            fail .Full(1)
        ",
    );
    assert!(problems.iter().any(|problem| problem.contains("Full")), "{problems:?}");
}

/// `case Kind.One:` where the type is known is written `case .One:`, as a value is. On a failure
/// that's one of a union, `case Shut.Stuck(name)` says which error's case it is.
#[test]
fn a_case_names_its_type_only_where_the_type_isnt_one() {
    let known = "enum Kind:\n    One\n    Two\n\ndef main(stdout: Stdout):\n    match Kind.One:\n        case Kind.One:\n            stdout.print(1)\n        case _:\n            stdout.print(2)\n";
    let said: Vec<String> =
        problems(known).iter().filter_map(|said| said.split(" @ ").next().map(str::to_owned)).collect();
    assert_eq!(said, ["the type here is known, so write this case as `.One`"]);
    let union = "error Shut:\n    Stuck(name: str)\n    Jammed\n\n    def said(self) -> str:\n        \"shut\"\n\n\
        error Late:\n    waited: int\n\ndef run(n: int) fails Shut | Late:\n    if n == 1: fail Late(waited=1)\n\n\
        def main(stdout: Stdout):\n    run(0) else as why:\n        match why:\n            \
        case Late(waited=_):\n                stdout.print(1)\n            case Shut.Stuck(name=name):\n                \
        stdout.print(name)\n            case Shut.Jammed:\n                stdout.print(why)\n        return\n    \
        stdout.print(0)\n";
    let empty: [&str; 0] = [];
    assert_eq!(problems(union), empty);
}
