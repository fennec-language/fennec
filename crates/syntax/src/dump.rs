//! Plain-text dumps of the token stream and the syntax tree. The self-hosted front end in
//! `selfhost/` prints the same text, so the two can be compared line by line.

use std::fmt::Write as _;

use crate::diagnostic::Diagnostic;
use crate::lexer::lex;
use crate::token::{FPart, Tok};

/// Every token in `source`, one per line, or the error that stopped the lexer.
pub fn tokens(source: &str) -> String {
    match lex(source, 0) {
        Ok(tokens) => {
            let mut text = String::new();
            for token in tokens {
                let _ = writeln!(text, "{}..{} {}", token.span.start, token.span.end, tok(&token.tok));
            }
            text
        }
        Err(diagnostic) => error(&diagnostic),
    }
}

/// A diagnostic as the dumps show it: its span and message, then its help on a second line.
pub(crate) fn error(diagnostic: &Diagnostic) -> String {
    let mut text =
        format!("error {}..{} {}\n", diagnostic.span.start, diagnostic.span.end, diagnostic.message);
    if let Some(help) = &diagnostic.help {
        let _ = writeln!(text, "help {help}");
    }
    text
}

fn tok(tok: &Tok) -> String {
    match tok {
        Tok::Name(name) => format!("name {name}"),
        Tok::Int(value) => format!("int {value}"),
        Tok::Float(value) => format!("float {}", float(*value)),
        Tok::Str(text) => format!("str {}", quoted(text)),
        Tok::FStr(parts) => {
            let parts: Vec<String> = parts.iter().map(fpart).collect();
            format!("fstr {}", parts.join(" | "))
        }
        Tok::Kw(kw) => format!("keyword {}", kw.text()),
        Tok::P(punct) => format!("punct {}", punct.text()),
        Tok::Newline => "newline".to_owned(),
        Tok::Indent => "indent".to_owned(),
        Tok::Dedent => "dedent".to_owned(),
        Tok::Eof => "eof".to_owned(),
    }
}

fn fpart(part: &FPart) -> String {
    match part {
        FPart::Text(text) => format!("text {}", quoted(text)),
        FPart::Expr { source, offset, spec: None } => format!("expr {offset} {}", quoted(source)),
        FPart::Expr { source, offset, spec: Some(spec) } => {
            format!("expr {offset} {} spec {}", quoted(source), quoted(spec))
        }
    }
}

/// A string in double quotes, with backslashes, quotes and line breaks escaped.
pub(crate) fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A float the way Fennec prints one: with a decimal point when it's whole.
#[expect(clippy::float_cmp, reason = "checking for a whole number is an exact comparison")]
pub(crate) fn float(x: f64) -> String {
    if x.is_infinite() {
        if x > 0.0 { "inf".to_owned() } else { "-inf".to_owned() }
    } else if x == x.trunc() && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}
