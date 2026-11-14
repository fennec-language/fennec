//! JSON text read into a tree, held to JSON's rules as `std.json` holds text it couldn't load:
//! the first thing wrong, said as `std.json` says it, with its line and column.

use std::fmt::Write;

/// How deeply arrays and objects may nest.
const MOST_DEEP: usize = 500;

/// A JSON value, as the text writes it.
#[derive(Debug)]
pub(crate) enum Node {
    Null,
    Bool(bool),
    /// A number, as it's written.
    Number(String),
    Str(String),
    Array(Vec<Node>),
    Object(Vec<Entry>),
}

/// One of an object's entries.
#[derive(Debug)]
pub(crate) struct Entry {
    /// The key as the text writes it, from its opening quote up to the `:`, which is how a path
    /// into a dict names it.
    pub(crate) written: String,
    pub(crate) key: String,
    pub(crate) value: Node,
}

/// What's wrong with text that isn't JSON, and where.
#[derive(Clone, Debug)]
pub(crate) struct Bad {
    pub(crate) message: String,
    pub(crate) line: i64,
    pub(crate) column: i64,
}

/// Text that's JSON, but for ints too big for an int, which a float's place still reads.
pub(crate) struct Parsed {
    pub(crate) node: Node,
    /// The first int too big for an int, which is what's said wrong with the text if it doesn't
    /// load.
    pub(crate) too_big: Option<Bad>,
}

/// Reads `text` as JSON, or says the first thing wrong with it.
pub(crate) fn parse(text: &str) -> Result<Parsed, Bad> {
    let mut reader = Reader { text, at: 0, too_big: None };
    let read = reader.value(0).and_then(|node| {
        if reader.space() >= 0 {
            let found = reader.next_char();
            return Err(
                reader.bad(&format!("expected the end of the text after the value, but found {found}"))
            );
        }
        Ok(node)
    });
    match read {
        Ok(node) => Ok(Parsed { node, too_big: reader.too_big }),
        Err(bad) => Err(reader.too_big.unwrap_or(bad)),
    }
}

/// Whether a number is written as a float, with a point or an exponent.
pub(crate) fn is_float_text(number: &str) -> bool {
    number.contains(['.', 'e', 'E'])
}

/// Whether a value holds an int too big for an int.
pub(crate) fn holds_too_big(node: &Node) -> bool {
    match node {
        Node::Number(number) => !is_float_text(number) && number.parse::<i64>().is_err(),
        Node::Array(items) => items.iter().any(holds_too_big),
        Node::Object(entries) => entries.iter().any(|entry| holds_too_big(&entry.value)),
        Node::Null | Node::Bool(_) | Node::Str(_) => false,
    }
}

struct Reader<'t> {
    text: &'t str,
    at: usize,
    too_big: Option<Bad>,
}

impl Reader<'_> {
    /// The byte at `at`, or -1 past the end.
    fn byte_at(&self, at: usize) -> i32 {
        self.text.as_bytes().get(at).map_or(-1, |b| i32::from(*b))
    }

    fn byte(&self) -> i32 {
        self.byte_at(self.at)
    }

    /// Steps past spaces, tabs and line breaks, giving the byte after them.
    fn space(&mut self) -> i32 {
        while matches!(self.byte(), 32 | 10 | 13 | 9) {
            self.at += 1;
        }
        self.byte()
    }

    fn part(&self, start: usize, end: usize) -> String {
        self.text.get(start..end).unwrap_or_default().to_owned()
    }

    /// The character here, as a message names it.
    fn next_char(&self) -> String {
        let b = self.byte();
        if b < 0 {
            return "the end of the text".to_owned();
        }
        if b == 10 {
            return "the end of the line".to_owned();
        }
        let size = if b >= 240 {
            4
        } else if b >= 224 {
            3
        } else if b >= 192 {
            2
        } else {
            1
        };
        format!("`{}`", self.part(self.at, self.at + size))
    }

    fn bad(&self, message: &str) -> Bad {
        self.bad_at(message, self.at)
    }

    /// A failure at a place, by its line and its column, counted in characters.
    fn bad_at(&self, message: &str, place: usize) -> Bad {
        let mut line = 1;
        let mut column = 1;
        for b in self.text.as_bytes().iter().take(place) {
            if *b == 10 {
                line += 1;
                column = 1;
            } else if *b < 128 || *b >= 192 {
                column += 1;
            }
        }
        Bad { message: format!("{message}, at line {line}, column {column}"), line, column }
    }

    /// One value, inside `depth` arrays and objects.
    fn value(&mut self, depth: usize) -> Result<Node, Bad> {
        let b = self.space();
        if (b == 123 || b == 91) && depth >= MOST_DEEP {
            return Err(self.bad(&format!("arrays and objects nest more than {MOST_DEEP} deep here")));
        }
        match b {
            123 => self.object(depth),
            91 => self.array(depth),
            34 => self.string().map(Node::Str),
            45 | 48..=57 => self.number(),
            97..=122 => self.word(),
            _ => Err(self.bad(&format!("expected a value, but found {}", self.next_char()))),
        }
    }

    fn word(&mut self) -> Result<Node, Bad> {
        let start = self.at;
        let mut end = start;
        while self.text.as_bytes().get(end).is_some_and(u8::is_ascii_alphanumeric) {
            end += 1;
        }
        let word = self.part(start, end);
        let node = match word.as_str() {
            "true" => Node::Bool(true),
            "false" => Node::Bool(false),
            "null" => Node::Null,
            _ => {
                let quoted = quoted_input(&word);
                return Err(self.bad(&format!(
                    "{quoted} isn't a JSON value; the words JSON knows are true, false and null"
                )));
            }
        };
        self.at = end;
        Ok(node)
    }

    fn object(&mut self, depth: usize) -> Result<Node, Bad> {
        self.at += 1;
        let mut entries = Vec::new();
        if self.space() == 125 {
            self.at += 1;
            return Ok(Node::Object(entries));
        }
        loop {
            if self.space() != 34 {
                let found = self.next_char();
                return Err(self.bad(&format!("expected a key in double quotes, but found {found}")));
            }
            let start = self.at;
            let key = self.string()?;
            if self.space() != 58 {
                let found = self.next_char();
                return Err(self.bad(&format!("expected `:` after the key, but found {found}")));
            }
            let written = self.part(start, self.at);
            self.at += 1;
            let value = self.value(depth + 1)?;
            entries.push(Entry { written, key, value });
            match self.space() {
                125 => {
                    self.at += 1;
                    return Ok(Node::Object(entries));
                }
                44 => self.at += 1,
                _ => {
                    let found = self.next_char();
                    return Err(self.bad(&format!("expected `,` or `}}` after the value, but found {found}")));
                }
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node, Bad> {
        self.at += 1;
        let mut items = Vec::new();
        if self.space() == 93 {
            self.at += 1;
            return Ok(Node::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            match self.space() {
                93 => {
                    self.at += 1;
                    return Ok(Node::Array(items));
                }
                44 => self.at += 1,
                _ => {
                    let found = self.next_char();
                    return Err(self.bad(&format!("expected `,` or `]` after the value, but found {found}")));
                }
            }
        }
    }

    /// A string, from its opening quote, with its escapes read as what they stand for.
    fn string(&mut self) -> Result<String, Bad> {
        let start = self.at;
        self.at += 1;
        let mut out = String::new();
        let mut plain = self.at;
        loop {
            let b = self.byte();
            if b == 34 {
                out.push_str(&self.part(plain, self.at));
                self.at += 1;
                return Ok(out);
            }
            if b < 0 {
                return Err(self.bad_at("this string never ends", start));
            }
            if b < 32 {
                return Err(
                    self.bad("a string can't hold a line break or control character; write `\\n` or `\\t`")
                );
            }
            if b != 92 {
                self.at += 1;
                continue;
            }
            out.push_str(&self.part(plain, self.at));
            self.at += 1;
            out.push(self.escape()?);
            plain = self.at;
        }
    }

    /// The character an escape stands for, from just past its backslash.
    fn escape(&mut self) -> Result<char, Bad> {
        let at = self.at;
        let b = self.byte();
        if b < 0 {
            return Err(self.bad("this string never ends"));
        }
        self.at = at + 1;
        Ok(match b {
            34 => '"',
            92 => '\\',
            47 => '/',
            98 => '\u{8}',
            102 => '\u{c}',
            110 => '\n',
            114 => '\r',
            116 => '\t',
            117 => return self.unicode(),
            _ => {
                self.at = at;
                let found: Vec<char> = self.next_char().chars().collect();
                let inner: String =
                    found.get(1..found.len().saturating_sub(1)).unwrap_or_default().iter().collect();
                return Err(self.bad(&format!("`\\{inner}` isn't a JSON escape")));
            }
        })
    }

    /// Four hex digits at `at`, as a number, or `None` if they aren't.
    fn hex(&self, at: usize) -> Option<u32> {
        let digits = self.text.as_bytes().get(at..at + 4)?;
        let digits = std::str::from_utf8(digits).ok()?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok()
    }

    /// A `\u` escape, from just past its `u`: a character, or a pair of them that stand for one.
    fn unicode(&mut self) -> Result<char, Bad> {
        let start = self.at;
        let Some(high) = self.hex(start) else { return Err(self.bad("`\\u` needs 4 hex digits")) };
        self.at = start + 4;
        if !(0xD800..0xDC00).contains(&high) {
            if (0xDC00..0xE000).contains(&high) {
                return Err(self.bad("this `\\u` escape isn't a character"));
            }
            return char::from_u32(high).ok_or_else(|| self.bad("this `\\u` escape isn't a character"));
        }
        if self.byte() != 92 || self.byte_at(self.at + 1) != 117 {
            self.at = start;
            return Err(self.bad("this `\\u` escape is half of a pair, so another `\\u` must follow it"));
        }
        self.at += 2;
        let Some(low) = self.hex(self.at) else {
            return Err(self.bad("`\\u` needs 4 hex digits"));
        };
        if !(0xDC00..0xE000).contains(&low) {
            self.at = start;
            return Err(self.bad("these `\\u` escapes aren't a valid pair"));
        }
        self.at += 4;
        let code = 0x10000 + (high - 0xD800) * 1024 + (low - 0xDC00);
        char::from_u32(code).ok_or_else(|| self.bad("these `\\u` escapes aren't a valid pair"))
    }

    fn digits(&mut self) {
        while (48..=57).contains(&self.byte()) {
            self.at += 1;
        }
    }

    fn number(&mut self) -> Result<Node, Bad> {
        let start = self.at;
        if self.byte() == 45 {
            self.at += 1;
        }
        let whole = self.at;
        self.digits();
        if self.at == whole {
            return Err(self.bad(&format!("expected a digit, but found {}", self.next_char())));
        }
        if self.at - whole > 1 && self.byte_at(whole) == 48 {
            self.at = start;
            return Err(self.bad("a number can't start with 0 unless it is 0"));
        }
        let mut is_float = false;
        if self.byte() == 46 {
            is_float = true;
            self.at += 1;
            let place = self.at;
            self.digits();
            if self.at == place {
                return Err(self.bad(&format!("expected a digit after `.`, but found {}", self.next_char())));
            }
        }
        if matches!(self.byte(), 101 | 69) {
            is_float = true;
            self.at += 1;
            if matches!(self.byte(), 43 | 45) {
                self.at += 1;
            }
            let place = self.at;
            self.digits();
            if self.at == place {
                return Err(
                    self.bad(&format!("expected a digit in the exponent, but found {}", self.next_char()))
                );
            }
        }
        let number = self.part(start, self.at);
        if !is_float && number.parse::<i64>().is_err() && self.too_big.is_none() {
            self.too_big =
                Some(self.bad_at(&format!("{} is too big for an int", quoted_input(&number)), start));
        }
        Ok(Node::Number(number))
    }
}

/// Text from outside as a message quotes it, in backticks: whole, or past 40 characters, its first
/// 20 and how many there are, with its control characters written as escapes.
pub(crate) fn quoted_input(text: &str) -> String {
    let count = text.chars().count();
    if count <= 40 {
        return format!("`{}`", escaped(text));
    }
    let start: String = text.chars().take(20).collect();
    format!("`{}…` ({} characters)", escaped(&start), grouped(count))
}

/// Text from outside as a message names a key or a part of a path: whole, or past 40 characters,
/// its first 20 and `…`, with its control characters written as escapes.
pub(crate) fn cut_input(text: &str) -> String {
    if text.chars().count() <= 40 {
        return escaped(text);
    }
    let start: String = text.chars().take(20).collect();
    format!("{}…", escaped(&start))
}

/// Text with each control character written as an escape, as `\n` or `\u{1b}`.
pub(crate) fn escaped(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let code = u32::from(c);
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ if code < 32 || (127..160).contains(&code) => {
                let _ = write!(out, "\\u{{{code:02x}}}");
            }
            _ => out.push(c),
        }
    }
    out
}

/// A count with a comma between each group of three digits.
fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (at, c) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
