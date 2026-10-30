//! Habits from other languages get a hint toward the Fennec way. Each case is the body of a
//! `main`, the error it stops with, and the hint.

fn first_error(body: &str) -> Option<(String, Option<String>)> {
    let source = format!("def main(stdout: Stdout):\n    {body}\n");
    let module = fennec_syntax::parse(&source).ok()?;
    let diagnostic = fennec_interp::run(&module, &mut Vec::new()).err()?;
    Some((diagnostic.message, diagnostic.help))
}

#[test]
fn methods_from_other_languages_point_to_fennec_ones() {
    let cases = [
        ("[1].length()", "a list has no method `length`", "use `.len()`"),
        ("[1].push(2)", "a list has no method `push`", "use `append`"),
        ("\" x \".trim()", "a str has no method `trim`", "use `strip()`"),
        ("\"x\".toUpperCase()", "a str has no method `toUpperCase`", "use `upper()`"),
        ("\"X\".to_lowercase()", "a str has no method `to_lowercase`", "use `lower()`"),
        ("stdout.println(1)", "a Stdout has no method `println`", "use `stdout.print(...)`"),
        (
            "[1].copy()",
            "a list has no method `copy`",
            "values are copied automatically when assigned: `b = a`",
        ),
    ];
    for (body, message, help) in cases {
        assert_eq!(first_error(body), Some((message.to_owned(), Some(help.to_owned()))), "{body}");
    }
    assert_eq!(first_error("[1].shuffle()"), Some(("a list has no method `shuffle`".to_owned(), None)));
}

#[test]
fn names_from_other_languages_point_to_fennec_ones() {
    let cases = [
        ("stdout.print(true)", "`true` doesn't exist", "write `True` or `False`"),
        ("stdout.print(null)", "`null` doesn't exist", "write `None`"),
        ("stdout.print(lambda)", "`lambda` doesn't exist", "write an arrow function: `x => x * 2`"),
        (
            "stdout.print(raise)",
            "`raise` doesn't exist",
            "Fennec has no exceptions: write `fail SomeError(...)`",
        ),
    ];
    for (body, message, help) in cases {
        assert_eq!(first_error(body), Some((message.to_owned(), Some(help.to_owned()))), "{body}");
    }
    assert_eq!(first_error("stdout.print(nothing)"), Some(("`nothing` doesn't exist".to_owned(), None)));
}

#[test]
fn conditions_need_bools() {
    let cases = [
        (
            "if None:\n        pass",
            "`if` needs a bool, but this is a None",
            "write `x is None` or `x is not None`",
        ),
        ("if [1]:\n        pass", "`if` needs a bool, but this is a list", "write `x.len() > 0`"),
        (
            "if 1:\n        pass",
            "`if` needs a bool, but this is a int",
            "compare explicitly, like `count > 0`",
        ),
    ];
    for (body, message, help) in cases {
        let (actual, actual_help) = first_error(body).unwrap();
        assert_eq!(actual, message, "{body}");
        assert!(actual_help.is_some_and(|actual| actual.ends_with(help)), "{body}");
    }
}
