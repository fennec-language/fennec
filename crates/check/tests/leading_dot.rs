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
            stdout.print(tok == .Name('x'), .Int(1) != tok, tok == .Eof)
    ";
    assert_eq!(with_enums(program), Vec::<String>::new());
}

#[test]
fn with_nothing_expected_a_shared_case_is_ambiguous() {
    let program = "
        def main(stdout: Stdout):
            stdout.print(.Int(1))
            n = 2
            n == .Name('x')
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
