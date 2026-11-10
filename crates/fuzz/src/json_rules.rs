//! The `json-rules` target: std's JSON reader, built natively, held to JSON's own rules (RFC
//! 8259) and to what the language says it refuses. A strict reader here says what the text is,
//! and what each type it's read as should give: the value, a refusal, or nothing promised. The
//! native reader prints each value it reads as JSON again, which is read here and compared.
//!
//! Promised: text that isn't JSON is refused as every type; a value of the wrong type is refused;
//! an int too big for 64 bits and a number too big for a float are refused; what's read is what
//! the text says, to the last digit and character. Not promised, so passed over: a number with a
//! fraction or an exponent read as an int, a key given twice, and a byte order mark.

/// The program that reads the input as each of `TYPES`, one line each, in order.
pub(crate) const READER: &str = "\
from std import json

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    as_int: int? = json.loads[int](text) else None
    stdout.print(\"err\" if as_int is None else \"ok \" + json.dumps(as_int))
    as_float: float? = json.loads[float](text) else None
    stdout.print(\"err\" if as_float is None else \"ok \" + json.dumps(as_float))
    as_str: str? = json.loads[str](text) else None
    stdout.print(\"err\" if as_str is None else \"ok \" + json.dumps(as_str))
    as_bool: bool? = json.loads[bool](text) else None
    stdout.print(\"err\" if as_bool is None else \"ok \" + json.dumps(as_bool))
    as_ints: list[int]? = json.loads[list[int]](text) else None
    stdout.print(\"err\" if as_ints is None else \"ok \" + json.dumps(as_ints))
    as_words: list[str?]? = json.loads[list[str?]](text) else None
    stdout.print(\"err\" if as_words is None else \"ok \" + json.dumps(as_words))
    as_names: dict[str, str]? = json.loads[dict[str, str]](text) else None
    stdout.print(\"err\" if as_names is None else \"ok \" + json.dumps(as_names))
    as_floats: dict[str, list[float]]? = json.loads[dict[str, list[float]]](text) else None
    stdout.print(\"err\" if as_floats is None else \"ok \" + json.dumps(as_floats))
";

/// The types `READER` reads as, in its order.
fn types() -> [Ty; 8] {
    use Ty::{Bool, Dict, Float, Int, List, Optional, Str};
    [
        Int,
        Float,
        Str,
        Bool,
        List(Box::new(Int)),
        List(Box::new(Optional(Box::new(Str)))),
        Dict(Box::new(Str)),
        Dict(Box::new(List(Box::new(Float)))),
    ]
}

/// How deeply the language lets JSON nest.
const DEEPEST: usize = 500;

/// A JSON value, with numbers kept as they're written.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// What reading text found, when it isn't a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotRead {
    /// It isn't JSON.
    Invalid,
    /// It's JSON that the language doesn't promise anything about.
    Unpromised,
}

/// A type the reader reads as.
#[derive(Clone, Debug)]
enum Ty {
    Int,
    Float,
    Str,
    Bool,
    Optional(Box<Ty>),
    List(Box<Ty>),
    Dict(Box<Ty>),
}

/// What reading as a type should give.
enum Expected {
    Value,
    Refused,
    Unpromised,
}

/// Reads JSON text strictly.
pub(crate) fn parse(text: &str) -> Result<Json, NotRead> {
    if text.starts_with('\u{feff}') {
        return Err(NotRead::Unpromised);
    }
    let mut reader = Reader { bytes: text.as_bytes(), at: 0, unpromised: false };
    reader.space();
    let value = reader.value(0)?;
    reader.space();
    if reader.at != reader.bytes.len() {
        return Err(NotRead::Invalid);
    }
    if reader.unpromised {
        return Err(NotRead::Unpromised);
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Whether something read so far, like a key given twice, isn't promised anything.
    unpromised: bool,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn next(&mut self) -> Result<u8, NotRead> {
        let byte = self.peek().ok_or(NotRead::Invalid)?;
        self.at += 1;
        Ok(byte)
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn word(&mut self, word: &[u8], value: Json) -> Result<Json, NotRead> {
        if self.bytes.get(self.at..self.at + word.len()) == Some(word) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(NotRead::Invalid)
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, NotRead> {
        if depth > DEEPEST {
            return Err(NotRead::Invalid);
        }
        match self.peek().ok_or(NotRead::Invalid)? {
            b'n' => self.word(b"null", Json::Null),
            b't' => self.word(b"true", Json::Bool(true)),
            b'f' => self.word(b"false", Json::Bool(false)),
            b'"' => self.text().map(Json::Text),
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    self.space();
                    items.push(self.value(depth + 1)?);
                    self.space();
                    match self.next()? {
                        b',' => {}
                        b']' => return Ok(Json::Array(items)),
                        _ => return Err(NotRead::Invalid),
                    }
                }
            }
            b'{' => {
                self.at += 1;
                let mut pairs: Vec<(String, Json)> = Vec::new();
                self.space();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Json::Object(pairs));
                }
                loop {
                    self.space();
                    if self.peek() != Some(b'"') {
                        return Err(NotRead::Invalid);
                    }
                    let key = self.text()?;
                    self.space();
                    if self.next()? != b':' {
                        return Err(NotRead::Invalid);
                    }
                    self.space();
                    let value = self.value(depth + 1)?;
                    if pairs.iter().any(|(seen, _)| *seen == key) {
                        self.unpromised = true;
                    }
                    pairs.push((key, value));
                    self.space();
                    match self.next()? {
                        b',' => {}
                        b'}' => return Ok(Json::Object(pairs)),
                        _ => return Err(NotRead::Invalid),
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number().map(Json::Number),
            _ => Err(NotRead::Invalid),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<String, NotRead> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(NotRead::Invalid),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(NotRead::Invalid);
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(NotRead::Invalid);
            }
        }
        let written = self.bytes.get(start..self.at).ok_or(NotRead::Invalid)?;
        Ok(String::from_utf8_lossy(written).into_owned())
    }

    fn hex(&mut self) -> Result<u32, NotRead> {
        let mut code = 0;
        for _ in 0..4 {
            let digit = char::from(self.next()?).to_digit(16).ok_or(NotRead::Invalid)?;
            code = code * 16 + digit;
        }
        Ok(code)
    }

    fn text(&mut self) -> Result<String, NotRead> {
        self.at += 1;
        let mut out = Vec::new();
        loop {
            match self.next()? {
                b'"' => return String::from_utf8(out).map_err(|_| NotRead::Invalid),
                b'\\' => {
                    let escaped = match self.next()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.escaped_character()?,
                        _ => return Err(NotRead::Invalid),
                    };
                    let mut buffer = [0; 4];
                    out.extend_from_slice(escaped.encode_utf8(&mut buffer).as_bytes());
                }
                byte if byte < 0x20 => return Err(NotRead::Invalid),
                byte => out.push(byte),
            }
        }
    }

    /// What `\u` and four digits stand for, and a second `\u` with a high surrogate. A surrogate
    /// that isn't half of a pair is JSON, but no character, so nothing's promised about it.
    fn escaped_character(&mut self) -> Result<char, NotRead> {
        let first = self.hex()?;
        if (0xd800..0xdc00).contains(&first) && self.bytes.get(self.at..self.at + 2) == Some(b"\\u") {
            let back = self.at;
            self.at += 2;
            let second = self.hex()?;
            if (0xdc00..0xe000).contains(&second) {
                let code = 0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
                return char::from_u32(code).ok_or(NotRead::Invalid);
            }
            self.at = back;
        }
        char::from_u32(first).map_or_else(
            || {
                self.unpromised = true;
                Ok('\u{fffd}')
            },
            Ok,
        )
    }
}

/// Whether a number is written as a whole number, with no fraction or exponent.
fn whole(number: &str) -> bool {
    !number.contains(['.', 'e', 'E'])
}

/// What reading `value` as `ty` should give.
fn expected(value: &Json, ty: &Ty) -> Expected {
    match (ty, value) {
        (Ty::Int, Json::Number(number)) if whole(number) => {
            if number.parse::<i64>().is_ok() {
                Expected::Value
            } else {
                Expected::Refused
            }
        }
        (Ty::Int, Json::Number(_)) => Expected::Unpromised,
        (Ty::Float, Json::Number(number)) => {
            if number.parse::<f64>().is_ok_and(f64::is_finite) {
                Expected::Value
            } else {
                Expected::Refused
            }
        }
        (Ty::Str, Json::Text(_)) | (Ty::Bool, Json::Bool(_)) | (Ty::Optional(_), Json::Null) => {
            Expected::Value
        }
        (Ty::Optional(inner), value) => expected(value, inner),
        (Ty::List(item), Json::Array(items)) => all(items.iter().map(|value| expected(value, item))),
        (Ty::Dict(item), Json::Object(pairs)) => all(pairs.iter().map(|(_, value)| expected(value, item))),
        _ => Expected::Refused,
    }
}

/// What reading many values should give, together.
fn all(each: impl Iterator<Item = Expected>) -> Expected {
    let mut unpromised = false;
    for expected in each {
        match expected {
            Expected::Refused => return Expected::Refused,
            Expected::Unpromised => unpromised = true,
            Expected::Value => {}
        }
    }
    if unpromised { Expected::Unpromised } else { Expected::Value }
}

/// Whether `read`, which the native reader read `value` as and wrote out again, is the same
/// value as `ty`.
fn same(value: &Json, read: &Json, ty: &Ty) -> bool {
    match (ty, value, read) {
        (Ty::Int, Json::Number(a), Json::Number(b)) => a.parse::<i64>().ok() == b.parse::<i64>().ok(),
        (Ty::Float, Json::Number(a), Json::Number(b)) => {
            let (a, b) = (a.parse::<f64>(), b.parse::<f64>());
            a.is_ok() && a == b
        }
        (Ty::Optional(_), Json::Null, Json::Null) => true,
        (Ty::Optional(inner), value, read) => same(value, read, inner),
        (Ty::List(item), Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b, item))
        }
        (Ty::Dict(item), Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, a)| b.iter().any(|(other, b)| key == other && same(a, b, item)))
        }
        (_, a, b) => a == b,
    }
}

/// The target: the input read natively as each type, against what JSON's rules say.
pub(crate) fn json_rules(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("json-rules", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    let types = types();
    if lines.len() != types.len() {
        return Err(format!("the reader printed {} lines, not {}:\n{printed}", lines.len(), types.len()));
    }
    let parsed = parse(&text);
    for (ty, line) in types.iter().zip(&lines) {
        let wanted = match &parsed {
            Ok(value) => expected(value, ty),
            Err(NotRead::Invalid) => Expected::Refused,
            Err(NotRead::Unpromised) => Expected::Unpromised,
        };
        let read = line.strip_prefix("ok ");
        match (wanted, read, &parsed) {
            (Expected::Unpromised, ..)
            | (Expected::Refused, None, _)
            | (Expected::Value, Some(_), Err(_)) => {}
            (Expected::Refused, Some(read), _) => {
                return Err(format!("{text:?} read as {ty:?} should be refused, but gave {read}"));
            }
            (Expected::Value, None, _) => return Err(format!("{text:?} read as {ty:?} was refused")),
            (Expected::Value, Some(read), Ok(value)) => {
                let back =
                    parse(read).map_err(|_| format!("{text:?} as {ty:?} wrote back {read:?}, not JSON"))?;
                if !same(value, &back, ty) {
                    return Err(format!("{text:?} read as {ty:?} gave {read}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Json, NotRead, parse};

    #[test]
    fn the_strict_reader_holds_to_the_rules() {
        assert_eq!(
            parse(" [1, \"a\\u00e9\"] "),
            Ok(Json::Array(vec![Json::Number("1".to_owned()), Json::Text("a\u{e9}".to_owned())]))
        );
        for invalid in
            ["[1,]", "01", "1.", ".5", "{\"a\" 1}", "'a'", "[1] x", "\"\t\"", "nul", "-", "+1", "[1"]
        {
            assert_eq!(parse(invalid), Err(NotRead::Invalid), "{invalid}");
        }
        assert_eq!(parse("{\"a\": 1, \"a\": 2}"), Err(NotRead::Unpromised));
        assert_eq!(parse("\"\\ud83e\""), Err(NotRead::Unpromised));
        assert_eq!(parse("\"\\ud83e\\udd8a\""), Ok(Json::Text("\u{1f98a}".to_owned())));
        assert_eq!(parse(&"[".repeat(502)), Err(NotRead::Invalid));
    }
}
