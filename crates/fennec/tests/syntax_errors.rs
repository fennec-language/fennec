//! A syntax error names what the parser expected and what it found instead.

#[test]
fn syntax_errors_say_what_was_expected_and_found() {
    let cases = [
        ("x = ]", "expected a value, found `]`"),
        ("x = }", "expected a value, found `}`"),
        ("x = ,", "expected a value, found `,`"),
        ("x = :", "expected a value, found `:`"),
        ("x = ..", "expected an enum case after `.`, found `.`"),
        ("x = =", "expected a value, found `=`"),
        ("x = >", "expected a value, found `>`"),
        ("x = /", "expected a value, found `/`"),
        ("x = %", "expected a value, found `%`"),
        ("import (", "expected a module name, found `(`"),
        ("import [", "expected a module name, found `[`"),
        ("import {", "expected a module name, found `{`"),
        ("x = (1 ]", "expected `)`, found `]`"),
        ("x = [1 )", "expected `]`, found `)`"),
        ("x = {1: 2 )", "expected `}`, found `)`"),
        ("def f x", "expected `(`, found `x`"),
        ("struct Point", "expected `:`, found the end of the line"),
        ("x = 1 1", "expected the end of the line, found a number"),
    ];
    for (source, message) in cases {
        let error = fennec_syntax::parse(&format!("{source}\n")).err().map(|diagnostic| diagnostic.message);
        assert_eq!(error.as_deref(), Some(message), "{source}");
    }
}
