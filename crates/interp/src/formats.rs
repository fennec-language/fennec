//! `std.json` and `std.toml`: reading text into Fennec values and writing values back out.
//! Bad text fails with a `ParseError` that says the line and column of the problem.

mod base64;
mod decode;
mod dump;
mod ids;
mod json;
mod shapes;
mod toml;
mod toml_value;

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span, TypeExpr};

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

