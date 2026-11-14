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

/// A character that changes the direction text is shown in, without showing itself, is refused
/// wherever it is: in a comment, a string of any kind, or the code. One can make code read
/// differently from what it does. A string that needs one writes its escape, and the error's own
/// excerpt shows it as U+FFFD, so the terminal doesn't turn the line around.
#[test]
fn hidden_text_direction_characters_are_refused_even_in_comments() {
    let hidden = [
        '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}',
        '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}',
    ];
    for c in hidden {
        let code = u32::from(c);
        let said = format!(
            "this line has a hidden character, U+{code:04X}, that changes the direction text is shown in"
        );
        for written in [
            format!("# is admin{c} ok\nx = 1\n"),
            format!("x = \"user{c}\"\n"),
            format!("x = r\"user{c}\"\n"),
            format!("x = f\"user{c} {{1}}\"\n"),
            format!("x = 1 {c}\n"),
        ] {
            let error = fennec_syntax::parse(&written).err();
            let at = written.find(c).and_then(|at| u32::try_from(at).ok()).unwrap();
            let width = u32::try_from(c.len_utf8()).unwrap();
            assert_eq!(
                error.as_ref().map(|diagnostic| (diagnostic.message.as_str(), diagnostic.span)),
                Some((said.as_str(), fennec_syntax::Span { start: at, end: at + width })),
                "{written:?}"
            );
            let shown = error.unwrap().render("hidden.fen", &written);
            assert!(!shown.contains(c), "{shown:?}");
            assert!(shown.contains('\u{fffd}'), "{shown:?}");
        }
        // Written as its escape, a string holds one.
        assert!(fennec_syntax::parse(&format!("x = \"user\\u{code:04x}\"\n")).is_ok(), "{code:04x}");
    }
}

/// The forms before 2026-11-11 that read as something else are refused where they're read, each
/// with the fix that writes the form of today: `else as why:` is `except why:`, and a `with`
/// source's `lend x else:` is `lend x except:`.
#[test]
fn the_old_failure_forms_are_refused_with_their_fixes() {
    let cases = [
        (
            "def f() -> int:\n    x = g() else as why:\n        fail why\n    return x\n",
            "a failure is handled with `except why:`: `else` is for a value that's missing",
            "def f() -> int:\n    x = g() except why:\n        fail why\n    return x\n",
        ),
        (
            "def f() -> with int:\n    lend 1 else:\n        pass\n",
            "a `with` source's cleanup is written `lend x except:`",
            "def f() -> with int:\n    lend 1 except:\n        pass\n",
        ),
    ];
    for (source, message, fixed) in cases {
        let Err(diagnostic) = fennec_syntax::parse(source) else { panic!("{source} parses") };
        assert_eq!(diagnostic.message, message, "{source}");
        let written = fennec_syntax::apply_fixes(source, 0, std::slice::from_ref(&diagnostic));
        assert_eq!(written.as_deref(), Some(fixed), "{source}");
        assert!(fennec_syntax::parse(fixed).is_ok(), "{fixed}");
    }
}
