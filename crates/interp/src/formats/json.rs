//! Reading JSON. Objects become dicts, keeping their keys' order, arrays become lists, and
//! numbers become ints unless they have a fraction or an exponent.

use std::rc::Rc;

use super::{Cursor, Parsed};
use crate::value::Value;

/// How deeply arrays and objects may nest, so bad input can't overflow the stack.
const MAX_DEPTH: usize = 500;

pub(super) fn parse(text: &str) -> Parsed<Value> {
    let mut cursor = Cursor::new(text);
    skip_space(&mut cursor);
    let value = value(&mut cursor, 0)?;
    skip_space(&mut cursor);
    if cursor.peek().is_some() {
        return cursor
            .fail(format!("expected the end of the text after the value, but found {}", cursor.found()));
    }
    Ok(value)
}

fn skip_space(cursor: &mut Cursor<'_>) {
    cursor.take_while(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
}

fn value(cursor: &mut Cursor<'_>, depth: usize) -> Parsed<Value> {
    if depth > MAX_DEPTH {
        return cursor.fail(format!("arrays and objects nest more than {MAX_DEPTH} deep here"));
    }
    match cursor.peek() {
        Some('{') => object(cursor, depth),
        Some('[') => array(cursor, depth),
        Some('"') => string(cursor).map(|text| Value::str(&text)),
        Some('-' | '0'..='9') => number(cursor),
        Some('a'..='z') => {
            let start = cursor.at;
            let word = cursor.take_while(|c| c.is_ascii_alphanumeric());
            match word {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                "null" => Ok(Value::None),
                _ => {
                    cursor.at = start;
                    cursor.fail(format!(
                        "`{word}` isn't a JSON value; the words JSON knows are true, false and null"
                    ))
                }
            }
        }
        _ => cursor.fail(format!("expected a value, but found {}", cursor.found())),
    }
}

fn object(cursor: &mut Cursor<'_>, depth: usize) -> Parsed<Value> {
    cursor.bump();
    let mut pairs: Vec<(Value, Value)> = Vec::new();
    skip_space(cursor);
    if cursor.eat('}') {
        return Ok(Value::Dict(Rc::new(pairs.into())));
    }
    loop {
        skip_space(cursor);
        if cursor.peek() != Some('"') {
            return cursor.fail(format!("expected a key in double quotes, but found {}", cursor.found()));
        }
        let key = Value::str(&string(cursor)?);
        skip_space(cursor);
        if !cursor.eat(':') {
            return cursor.fail(format!("expected `:` after the key, but found {}", cursor.found()));
        }
        skip_space(cursor);
        let item = value(cursor, depth + 1)?;
        match pairs.iter_mut().find(|(existing, _)| *existing == key) {
            Some(pair) => pair.1 = item,
            None => pairs.push((key, item)),
        }
        skip_space(cursor);
        if cursor.eat('}') {
            return Ok(Value::Dict(Rc::new(pairs.into())));
        }
        if !cursor.eat(',') {
            return cursor
                .fail(format!("expected `,` or `}}` after the value, but found {}", cursor.found()));
        }
    }
}

fn array(cursor: &mut Cursor<'_>, depth: usize) -> Parsed<Value> {
    cursor.bump();
    let mut items = Vec::new();
    skip_space(cursor);
    if cursor.eat(']') {
        return Ok(Value::list(items));
    }
    loop {
        skip_space(cursor);
        items.push(value(cursor, depth + 1)?);
        skip_space(cursor);
        if cursor.eat(']') {
            return Ok(Value::list(items));
        }
        if !cursor.eat(',') {
            return cursor.fail(format!("expected `,` or `]` after the value, but found {}", cursor.found()));
        }
    }
}

fn string(cursor: &mut Cursor<'_>) -> Parsed<String> {
    let start = cursor.at;
    cursor.bump();
    let mut text = String::new();
    loop {
        match cursor.bump() {
            Some('"') => return Ok(text),
            Some('\\') => text.push(escape(cursor)?),
            Some(c) if u32::from(c) < 0x20 => {
                cursor.at -= c.len_utf8();
                return cursor
                    .fail("a string can't hold a line break or control character; write `\\n` or `\\t`");
            }
            Some(c) => text.push(c),
            None => {
                cursor.at = start;
                return cursor.fail("this string never ends");
            }
        }
    }
}

fn escape(cursor: &mut Cursor<'_>) -> Parsed<char> {
    let at = cursor.at;
    let c = match cursor.bump() {
        Some('"') => '"',
        Some('\\') => '\\',
        Some('/') => '/',
        Some('b') => '\u{8}',
        Some('f') => '\u{c}',
        Some('n') => '\n',
        Some('r') => '\r',
        Some('t') => '\t',
        Some('u') => return unicode(cursor),
        Some(other) => {
            cursor.at = at;
            return cursor.fail(format!("`\\{other}` isn't a JSON escape"));
        }
        None => return cursor.fail("this string never ends"),
    };
    Ok(c)
}

/// A `\u` escape, which may be the first half of a surrogate pair.
fn unicode(cursor: &mut Cursor<'_>) -> Parsed<char> {
    let start = cursor.at;
    let high = hex_units(cursor)?;
    if !(0xD800..0xDC00).contains(&high) {
        return char::from_u32(high).map_or_else(|| cursor.fail("this `\\u` escape isn't a character"), Ok);
    }
    if !cursor.eat_str("\\u") {
        cursor.at = start;
        return cursor.fail("this `\\u` escape is half of a pair, so another `\\u` must follow it");
    }
    let low = hex_units(cursor)?;
    let code = (0xDC00..0xE000).contains(&low).then(|| 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00));
    if let Some(c) = code.and_then(char::from_u32) {
        return Ok(c);
    }
    cursor.at = start;
    cursor.fail("these `\\u` escapes aren't a valid pair")
}

fn hex_units(cursor: &mut Cursor<'_>) -> Parsed<u32> {
    let start = cursor.at;
    let digits = cursor.take_while(|c| c.is_ascii_hexdigit());
    let Some(code) = digits.get(..4).and_then(|four| u32::from_str_radix(four, 16).ok()) else {
        cursor.at = start;
        return cursor.fail("`\\u` needs 4 hex digits");
    };
    cursor.at = start + 4;
    Ok(code)
}

fn number(cursor: &mut Cursor<'_>) -> Parsed<Value> {
    let start = cursor.at;
    cursor.eat('-');
    let whole = cursor.take_while(|c| c.is_ascii_digit());
    if whole.is_empty() {
        return cursor.fail(format!("expected a digit, but found {}", cursor.found()));
    }
    if whole.len() > 1 && whole.starts_with('0') {
        cursor.at = start;
        return cursor.fail("a number can't start with 0 unless it is 0");
    }
    let mut is_float = false;
    if cursor.eat('.') {
        is_float = true;
        if cursor.take_while(|c| c.is_ascii_digit()).is_empty() {
            return cursor.fail(format!("expected a digit after `.`, but found {}", cursor.found()));
        }
    }
    if cursor.eat('e') || cursor.eat('E') {
        is_float = true;
        if !cursor.eat('+') {
            cursor.eat('-');
        }
        if cursor.take_while(|c| c.is_ascii_digit()).is_empty() {
            return cursor.fail(format!("expected a digit in the exponent, but found {}", cursor.found()));
        }
    }
    let text = cursor.text.get(start..cursor.at).unwrap_or("");
    if is_float {
        return text
            .parse()
            .map(Value::Float)
            .map_err(|_| super::Failure { message: format!("`{text}` isn't a number"), at: start });
    }
    text.parse()
        .map(Value::Int)
        .map_err(|_| super::Failure { message: format!("`{text}` is too big for an int"), at: start })
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::format::repr;

    fn read(text: &str) -> String {
        match parse(text) {
            Ok(value) => repr(&value),
            Err(failure) => format!("{} @ {}", failure.message, failure.at),
        }
    }

    #[test]
    fn reads_every_kind_of_value() {
        assert_eq!(
            read(r#" {"a": [1, -2.5, 3e2, true, false, null], "b": {"c": "d\né🦊"}} "#),
            r#"{"a": [1, -2.5, 300.0, True, False, None], "b": {"c": "d\né🦊"}}"#
        );
        assert_eq!(read(r#"{"a": 1, "a": 2}"#), r#"{"a": 2}"#);
        assert_eq!(read("[]"), "[]");
    }

    #[test]
    fn says_where_bad_text_goes_wrong() {
        assert_eq!(read("[1 2]"), "expected `,` or `]` after the value, but found `2` @ 3");
        assert_eq!(read(r#"{"a" 1}"#), "expected `:` after the key, but found `1` @ 5");
        assert_eq!(read(r"{a: 1}"), "expected a key in double quotes, but found `a` @ 1");
        assert_eq!(read(r#""abc"#), "this string never ends @ 0");
        assert_eq!(read("01"), "a number can't start with 0 unless it is 0 @ 0");
        assert_eq!(read("99999999999999999999"), "`99999999999999999999` is too big for an int @ 0");
        assert_eq!(
            read("nul"),
            "`nul` isn't a JSON value; the words JSON knows are true, false and null @ 0"
        );
        assert_eq!(read("[1,]"), "expected a value, but found `]` @ 3");
        assert_eq!(read("1 2"), "expected the end of the text after the value, but found `2` @ 2");
        assert_eq!(read(&"[".repeat(600)), "arrays and objects nest more than 500 deep here @ 501");
    }
}
