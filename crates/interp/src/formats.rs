//! `std.json` and `std.toml`: reading text into Fennec values and writing values back out.
//! Bad text fails with a `ParseError` that says the line and column of the problem.

mod decode;
mod dump;
mod ids;
mod json;
mod shapes;
mod toml;
mod toml_value;

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span, TypeExpr};

use crate::call::CallArg;
use crate::interp::Interp;
use crate::value::Value;

/// How field names are spelled in the text, from `names=`. The program spells them in snake
/// case, so `Snake` writes them as they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Names {
    #[default]
    Snake,
    Camel,
    Pascal,
    Kebab,
}

impl Names {
    fn of(case: &str) -> Option<Names> {
        Some(match case {
            "snake_case" => Names::Snake,
            "camel_case" => Names::Camel,
            "pascal_case" => Names::Pascal,
            "kebab_case" => Names::Kebab,
            _ => return None,
        })
    }

    /// A field's name as the text spells it. Leading underscores stay as they are.
    pub(crate) fn spell(self, field: &str) -> String {
        if self == Names::Snake {
            return field.to_owned();
        }
        let rest = field.trim_start_matches('_');
        let lead = field.get(..field.len() - rest.len()).unwrap_or_default();
        let words = rest.split('_').filter(|word| !word.is_empty());
        let capital = |word: &str| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        };
        let joined = match self {
            Names::Kebab => words.collect::<Vec<_>>().join("-"),
            Names::Pascal => words.map(capital).collect(),
            _ => words
                .enumerate()
                .map(|(at, word)| if at == 0 { word.to_owned() } else { capital(word) })
                .collect(),
        };
        format!("{lead}{joined}")
    }
}

/// What reading does with a key no field has, from `extra=`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Extra {
    #[default]
    Error,
    Ignore,
}

/// How a `json.loads` call reads, at every level of what it reads.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Reading {
    pub(crate) names: Names,
    pub(crate) extra: Extra,
}

/// The case an option was given as, like `camel_case` for `.camel_case`.
fn case_of(value: &Value) -> Option<&str> {
    match value {
        Value::Instance(instance) => instance.case.as_deref(),
        _ => None,
    }
}

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
        let mut reading = Reading::default();
        let mut values = Vec::new();
        for arg in args {
            match (arg.name.as_deref(), arg.value) {
                (None, value) => values.push(value),
                (Some("names"), value) if name.starts_with("json.") => {
                    reading.names = case_of(&value).and_then(Names::of).ok_or_else(|| {
                        Diagnostic::new(
                            "`names` is `.snake_case`, `.camel_case`, `.pascal_case` or `.kebab_case`",
                            arg.span,
                        )
                    })?;
                }
                (Some("extra"), value) if name == "json.loads" => {
                    reading.extra = match case_of(&value) {
                        Some("ignore") => Extra::Ignore,
                        Some("error") => Extra::Error,
                        _ => return Err(Diagnostic::new("`extra` is `.error` or `.ignore`", arg.span)),
                    };
                }
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
            ("json.loads", [Value::Str(text)]) => {
                self.reading = reading;
                self.parsed(text, json::parse(text), span)
            }
            ("toml.loads", [Value::Str(text)]) => self.parsed(text, toml::parse(text), span),
            ("json.dumps", [value]) => {
                let value = self.natural(value, reading.names, span)?;
                dump::json(&value, indent)
                    .map(|text| Value::str(&text))
                    .map_err(|why| Diagnostic::new(why, span))
            }
            ("toml.dumps", [value]) => {
                let value = self.natural(value, Names::Snake, span)?;
                dump::toml(&value).map(|text| Value::str(&text)).map_err(|why| Diagnostic::new(why, span))
            }
            _ => Err(Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span)),
        }
    }

    /// `json.loads[T](text)`: the text read as a `T`. Text that reads fine but doesn't fit
    /// the type fails with a `ParseError` at line 0, column 0.
    pub(crate) fn typed_loads(
        &mut self,
        name: &str,
        ty: &TypeExpr,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let read = self.format_builtin(name, args, span)?;
        let Value::Ok(value) = read else { return Ok(read) };
        let read = self.decode(value.as_ref().clone(), ty, "", span);
        // How this call reads is over once it has.
        self.reading = Reading::default();
        match read.and_then(|value| self.check_ids(&value, ty).map(|()| value)) {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(message) => {
                let fields = vec![
                    ("message", Value::str(&message)),
                    ("line", Value::Int(0)),
                    ("column", Value::Int(0)),
                ];
                let error = self.make_value("ParseError", fields, span)?;
                Ok(Value::Err(Rc::new(error)))
            }
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
