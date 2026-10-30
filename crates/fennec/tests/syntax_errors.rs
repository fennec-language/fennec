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
        ("def [x](self):", "expected a function name, found `[`"),
        ("struct Point", "expected `:`, found the end of the line"),
        ("x = 1 1", "expected the end of the line, found a number"),
    ];
    for (source, message) in cases {
        let error = fennec_syntax::parse(&format!("{source}\n")).err().map(|diagnostic| diagnostic.message);
        assert_eq!(error.as_deref(), Some(message), "{source}");
    }
}

#[test]
fn lexer_errors_point_at_one_character() {
    let cases = [
        ("x =\t1", 3, "tabs aren't allowed"),
        // A tab is reported even when the spaces before it don't line up with any block.
        ("def f():\n    x = 1\n  \tx = 2", 21, "tabs aren't allowed"),
        ("x = \"open", 4, "this string never ends"),
        ("x = $", 4, "unexpected character `$`"),
        ("x = f\"{}\"", 7, "this f-string has an empty `{}`"),
    ];
    for (source, at, message) in cases {
        let error = fennec_syntax::parse(&format!("{source}\n"))
            .err()
            .map(|diagnostic| (diagnostic.message, diagnostic.span));
        assert_eq!(
            error,
            Some((message.to_owned(), fennec_syntax::Span { start: at, end: at + 1 })),
            "{source}"
        );
    }
}
