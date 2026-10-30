//! `std.json` and `std.toml`: reading text into Fennec values and writing values back out.
//! Bad text fails with a `ParseError` that says the line and column of the problem.

mod dump;
mod json;
mod toml;
mod toml_value;

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::interp::Interp;
use crate::value::Value;

/// Why some text couldn't be read, and the byte it went wrong at.
pub(crate) struct Failure {
    message: String,
    at: usize,
}

type Parsed<T> = Result<T, Failure>;

/// A position in text being read.
pub(crate) struct Cursor<'t> {
    text: &'t str,
    at: usize,
}

impl<'t> Cursor<'t> {
    fn new(text: &'t str) -> Self {
        Cursor { text, at: 0 }
    }

    fn rest(&self) -> &'t str {
        self.text.get(self.at..).unwrap_or("")
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.at += c.len_utf8();
        }
        found
    }

    fn eat_str(&mut self, word: &str) -> bool {
        let found = self.rest().starts_with(word);
        if found {
            self.at += word.len();
        }
        found
    }

    /// Takes characters while `keep` holds, and returns them.
    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> &'t str {
        let start = self.at;
        while self.peek().is_some_and(&keep) {
            self.bump();
        }
        self.text.get(start..self.at).unwrap_or("")
    }

    fn fail<T>(&self, message: impl Into<String>) -> Parsed<T> {
        Err(self.failure(message))
    }

    fn failure(&self, message: impl Into<String>) -> Failure {
        Failure { message: message.into(), at: self.at }
    }

    /// What's next, for a message: "`x`", or "the end of the text".
    fn found(&self) -> String {
        match self.peek() {
            None => "the end of the text".to_owned(),
            Some('\n') => "the end of the line".to_owned(),
            Some(c) => format!("`{c}`"),
        }
    }
}

/// The line and column, both counting from 1, of a byte in `text`.
fn line_and_column(text: &str, at: usize) -> (usize, usize) {
    let before = text.get(..at).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |last| last.chars().count()) + 1;
    (line, column)
}

/// Reads a `\u` escape's hex digits into a character.
fn hex_char(cursor: &mut Cursor<'_>, digits: usize) -> Parsed<char> {
    let start = cursor.at;
    let mut code = 0;
    for _ in 0..digits {
        match cursor.peek().and_then(|c| c.to_digit(16)) {
            Some(digit) => {
                cursor.bump();
                code = code * 16 + digit;
            }
            None => return cursor.fail(format!("`\\u` needs {digits} hex digits")),
        }
    }
    char::from_u32(code).ok_or(Failure { message: format!("`{code:X}` isn't a character"), at: start })
}

impl Interp<'_> {
    /// The functions of `std.json` and `std.toml`.
    pub(crate) fn format_builtin(
        &mut self,
        name: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let mut indent = None;
        let mut values = Vec::new();
        for arg in args {
            match (arg.name.as_deref(), arg.value) {
                (None, value) => values.push(value),
                (Some("indent"), Value::Int(n)) if name == "json.dumps" => {
                    indent = Some(
                        usize::try_from(n)
                            .map_err(|_| Diagnostic::new("`indent` can't be negative", arg.span))?,
                    );
                }
                (Some(other), _) => {
                    return Err(Diagnostic::new(
                        format!("`{name}` has no parameter called `{other}`"),
                        arg.span,
                    ));
                }
            }
        }
        match (name, values.as_slice()) {
            ("json.loads", [Value::Str(text)]) => self.parsed(text, json::parse(text), span),
            ("toml.loads", [Value::Str(text)]) => self.parsed(text, toml::parse(text), span),
            ("json.dumps", [value]) => dump::json(value, indent)
                .map(|text| Value::str(&text))
                .map_err(|why| Diagnostic::new(why, span)),
            ("toml.dumps", [value]) => {
                dump::toml(value).map(|text| Value::str(&text)).map_err(|why| Diagnostic::new(why, span))
            }
            _ => Err(Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span)),
        }
    }

    /// `Ok` with the value read, or `Err` with a `ParseError` saying where reading stopped.
    fn parsed(&mut self, text: &str, parsed: Parsed<Value>, span: Span) -> Result<Value, Diagnostic> {
        let failure = match parsed {
            Ok(value) => return Ok(Value::Ok(Rc::new(value))),
            Err(failure) => failure,
        };
        let (line, column) = line_and_column(text, failure.at);
        let message = format!("{}, at line {line}, column {column}", failure.message);
        let fields = vec![
            ("message", Value::str(&message)),
            ("line", Value::Int(i64::try_from(line).unwrap_or(i64::MAX))),
            ("column", Value::Int(i64::try_from(column).unwrap_or(i64::MAX))),
        ];
        let error = self.make_value("ParseError", fields, span)?;
        Ok(Value::Err(Rc::new(error)))
    }
}
