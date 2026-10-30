//! JSON, which the protocol is written in: reading a message's text into a value, and
//! writing a value back out.

use std::fmt::{self, Write as _};

/// A JSON value. An object keeps its keys in the order they're written.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// An object from its keys and values.
    pub fn object<const N: usize>(fields: [(&str, Json); N]) -> Json {
        Json::Object(fields.into_iter().map(|(key, value)| (key.to_owned(), value)).collect())
    }

    pub fn string(text: impl Into<String>) -> Json {
        Json::String(text.into())
    }

    /// A count or a position, which the protocol keeps well inside the numbers JSON can hold
    /// exactly.
    #[expect(clippy::cast_precision_loss, reason = "positions and counts are far below 2^53")]
    pub fn number(value: usize) -> Json {
        Json::Number(value as f64)
    }

    /// The value at `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    /// The value at a path of keys: `message.at(&["params", "textDocument", "uri"])`.
    pub fn at(&self, path: &[&str]) -> Option<&Json> {
        path.iter().try_fold(self, |value, key| value.get(key))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// A whole number that fits a `usize`, like a line or a column.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to be whole and in range"
    )]
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            Json::Number(value) if value.fract() == 0.0 && *value >= 0.0 && *value < 9.0e15 => {
                Some(*value as usize)
            }
            _ => None,
        }
    }

    /// Reads JSON text. None if it isn't valid JSON.
    pub fn parse(text: &str) -> Option<Json> {
        let mut reader = Reader { bytes: text.as_bytes(), at: 0, depth: 0 };
        let value = reader.value()?;
        reader.space();
        (reader.at == reader.bytes.len()).then_some(value)
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Bool(value) => write!(f, "{value}"),
            Json::Number(value) if value.is_finite() => write!(f, "{value}"),
            // JSON has no infinity or NaN.
            Json::Null | Json::Number(_) => f.write_str("null"),
            Json::String(text) => write_string(f, text),
            Json::Array(items) => {
                f.write_char('[')?;
                for (at, item) in items.iter().enumerate() {
                    if at > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_char(']')
            }
            Json::Object(fields) => {
                f.write_char('{')?;
                for (at, (key, value)) in fields.iter().enumerate() {
                    if at > 0 {
                        f.write_char(',')?;
                    }
                    write_string(f, key)?;
                    write!(f, ":{value}")?;
                }
                f.write_char('}')
            }
        }
    }
}

fn write_string(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_char('"')?;
    for ch in text.chars() {
        match ch {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            ch if u32::from(ch) < 0x20 => write!(f, "\\u{:04x}", u32::from(ch))?,
            ch => f.write_char(ch)?,
        }
    }
    f.write_char('"')
}

/// How deeply arrays and objects may nest, so a hostile message can't overflow the stack.
const MAX_DEPTH: usize = 256;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Option<Json> {
        let end = self.at.checked_add(word.len())?;
        (self.bytes.get(self.at..end)? == word.as_bytes()).then(|| {
            self.at = end;
            value
        })
    }

    fn value(&mut self) -> Option<Json> {
        self.space();
        match self.peek()? {
            b'n' => self.word("null", Json::Null),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'"' => self.string().map(Json::String),
            b'[' => self.nested(Self::array),
            b'{' => self.nested(Self::object),
            _ => self.number(),
        }
    }

    fn nested(&mut self, read: fn(&mut Self) -> Option<Json>) -> Option<Json> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        self.at += 1;
        let value = read(self);
        self.depth -= 1;
        value
    }

    fn array(&mut self) -> Option<Json> {
        let mut items = Vec::new();
        self.space();
        if self.peek()? == b']' {
            self.at += 1;
            return Some(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.space();
            match self.peek()? {
                b',' => self.at += 1,
                b']' => {
                    self.at += 1;
                    return Some(Json::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn object(&mut self) -> Option<Json> {
        let mut fields = Vec::new();
        self.space();
        if self.peek()? == b'}' {
            self.at += 1;
            return Some(Json::Object(fields));
        }
        loop {
            self.space();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.space();
            if self.peek()? != b':' {
                return None;
            }
            self.at += 1;
            fields.push((key, self.value()?));
            self.space();
            match self.peek()? {
                b',' => self.at += 1,
                b'}' => {
                    self.at += 1;
                    return Some(Json::Object(fields));
                }
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.at += 1;
        }
        let text = std::str::from_utf8(self.bytes.get(start..self.at)?).ok()?;
        text.parse().ok().filter(|value: &f64| value.is_finite()).map(Json::Number)
    }

    /// A string, from its opening quote.
    fn string(&mut self) -> Option<String> {
        self.at += 1;
        let mut text = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek()?, b'"' | b'\\') {
                self.at += 1;
            }
            text.push_str(std::str::from_utf8(self.bytes.get(start..self.at)?).ok()?);
            let quote = self.peek()? == b'"';
            self.at += 1;
            if quote {
                return Some(text);
            }
            let escape = self.peek()?;
            self.at += 1;
            text.push(match escape {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => self.unicode()?,
                _ => return None,
            });
        }
    }

    /// The character a `\u` escape stands for, with the second half of a surrogate pair.
    fn unicode(&mut self) -> Option<char> {
        let first = self.hex4()?;
        if !(0xD800..0xDC00).contains(&first) {
            return char::from_u32(first);
        }
        self.word("\\u", Json::Null)?;
        let second = self.hex4()?;
        if !(0xDC00..0xE000).contains(&second) {
            return None;
        }
        char::from_u32(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00))
    }

    fn hex4(&mut self) -> Option<u32> {
        let end = self.at.checked_add(4)?;
        let digits = std::str::from_utf8(self.bytes.get(self.at..end)?).ok()?;
        self.at = end;
        u32::from_str_radix(digits, 16).ok()
    }
}
