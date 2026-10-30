//! TOML's values: strings (basic, literal and multi-line), ints, floats, bools, arrays and
//! inline tables. Dates and times are kept as the strings they're written as.

use super::toml::{Table, key_value, skip_blank_lines};
use super::{Cursor, Failure, Parsed, hex_char};
use crate::value::Value;

/// How deeply arrays and inline tables may nest, so bad input can't overflow the stack.
const MAX_DEPTH: usize = 500;

pub(super) fn value(cursor: &mut Cursor<'_>, depth: usize) -> Parsed<Value> {
    if depth > MAX_DEPTH {
        return cursor.fail(format!("arrays and tables nest more than {MAX_DEPTH} deep here"));
    }
    let rest = cursor.rest();
    match cursor.peek() {
        Some('"') if rest.starts_with("\"\"\"") => multi_line(cursor, '"').map(|text| Value::str(&text)),
        Some('\'') if rest.starts_with("'''") => multi_line(cursor, '\'').map(|text| Value::str(&text)),
        Some('"') => basic_string(cursor).map(|text| Value::str(&text)),
        Some('\'') => literal_string(cursor).map(|text| Value::str(&text)),
        Some('[') => array(cursor, depth),
        Some('{') => inline_table(cursor, depth),
        Some('0'..='9') if is_date_or_time(rest) => Ok(Value::str(date_or_time(cursor)?)),
        Some('0'..='9' | '+' | '-' | 'i' | 'n') => number(cursor),
        Some('t' | 'f') => {
            let at = cursor.at;
            match cursor.take_while(|c| c.is_ascii_alphanumeric()) {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                word => Err(Failure { message: format!("`{word}` isn't a TOML value"), at }),
            }
        }
        _ => cursor.fail(format!("expected a value, but found {}", cursor.found())),
    }
}

/// `"text"`, with backslash escapes, on one line.
pub(super) fn basic_string(cursor: &mut Cursor<'_>) -> Parsed<String> {
    let start = cursor.at;
    cursor.bump();
    let mut text = String::new();
    loop {
        match cursor.peek() {
            Some('"') => {
                cursor.bump();
                return Ok(text);
            }
            Some('\\') => text.push(escape(cursor)?),
            Some('\n' | '\r') | None => {
                return Err(Failure {
                    message: "this string never ends on its line; use `\"\"\"` for text over several lines"
                        .to_owned(),
                    at: start,
                });
            }
            Some(c) => text.push(plain_char(cursor, c)?),
        }
    }
}

/// `'text'`, taken exactly as written, on one line.
pub(super) fn literal_string(cursor: &mut Cursor<'_>) -> Parsed<String> {
    let start = cursor.at;
    cursor.bump();
    let mut text = String::new();
    loop {
        match cursor.peek() {
            Some('\'') => {
                cursor.bump();
                return Ok(text);
            }
            Some('\n' | '\r') | None => {
                return Err(Failure {
                    message: "this string never ends on its line; use `'''` for text over several lines"
                        .to_owned(),
                    at: start,
                });
            }
            Some(c) => text.push(plain_char(cursor, c)?),
        }
    }
}

/// `"""text"""` or `'''text'''`. A line break right after the opening quotes is dropped.
fn multi_line(cursor: &mut Cursor<'_>, quote: char) -> Parsed<String> {
    let start = cursor.at;
    cursor.at += 3;
    if !cursor.eat_str("\r\n") {
        cursor.eat('\n');
    }
    let mut text = String::new();
    loop {
        match cursor.peek() {
            Some(c) if c == quote => {
                let quotes = cursor.take_while(|next| next == quote).len();
                if quotes >= 3 {
                    if quotes > 5 {
                        return cursor.fail("too many quotes end this string");
                    }
                    text.extend(std::iter::repeat_n(quote, quotes - 3));
                    return Ok(text);
                }
                text.extend(std::iter::repeat_n(quote, quotes));
            }
            Some('\\') if quote == '"' => {
                let after = cursor.rest().get(1..).unwrap_or("");
                if after.trim_start_matches([' ', '\t']).starts_with(['\n', '\r']) {
                    cursor.bump();
                    cursor.take_while(char::is_whitespace);
                } else {
                    text.push(escape(cursor)?);
                }
            }
            Some(c @ ('\n' | '\r')) => {
                cursor.bump();
                text.push(c);
            }
            Some(c) => text.push(plain_char(cursor, c)?),
            None => return Err(Failure { message: "this string never ends".to_owned(), at: start }),
        }
    }
}

/// A character inside a string, which can't be a control character other than a tab.
fn plain_char(cursor: &mut Cursor<'_>, c: char) -> Parsed<char> {
    if c.is_control() && c != '\t' {
        return cursor.fail("a string can't hold a control character; write an escape like `\\u0007`");
    }
    cursor.bump();
    Ok(c)
}

fn escape(cursor: &mut Cursor<'_>) -> Parsed<char> {
    let at = cursor.at;
    cursor.bump();
    let c = match cursor.bump() {
        Some('b') => '\u{8}',
        Some('t') => '\t',
        Some('n') => '\n',
        Some('f') => '\u{c}',
        Some('r') => '\r',
        Some('e') => '\u{1b}',
        Some('"') => '"',
        Some('\\') => '\\',
        Some('u') => return hex_char(cursor, 4),
        Some('U') => return hex_char(cursor, 8),
        Some(other) => return Err(Failure { message: format!("`\\{other}` isn't a TOML escape"), at }),
        None => return Err(Failure { message: "this string never ends".to_owned(), at }),
    };
    Ok(c)
}

fn array(cursor: &mut Cursor<'_>, depth: usize) -> Parsed<Value> {
    cursor.bump();
    let mut items = Vec::new();
    loop {
        skip_blank_lines(cursor);
        if cursor.eat(']') {
            return Ok(Value::list(items));
        }
        items.push(value(cursor, depth + 1)?);
        skip_blank_lines(cursor);
        if cursor.eat(']') {
            return Ok(Value::list(items));
        }
        if !cursor.eat(',') {
            return cursor.fail(format!("expected `,` or `]` after the value, but found {}", cursor.found()));
        }
    }
}

/// `{ name = "fennec", version = "0.1" }`.
fn inline_table(cursor: &mut Cursor<'_>, depth: usize) -> Parsed<Value> {
    cursor.bump();
    let mut table = Table::default();
    skip_blank_lines(cursor);
    if cursor.eat('}') {
        return Ok(table.into_value());
    }
    loop {
        skip_blank_lines(cursor);
        let at = cursor.at;
        if depth >= MAX_DEPTH {
            return cursor.fail(format!("arrays and tables nest more than {MAX_DEPTH} deep here"));
        }
        let (keys, item) = key_value(cursor)?;
        table.insert(&keys, item, at)?;
        skip_blank_lines(cursor);
        if cursor.eat('}') {
            return Ok(table.into_value());
        }
        if !cursor.eat(',') {
            return cursor
                .fail(format!("expected `,` or `}}` after the value, but found {}", cursor.found()));
        }
    }
}

/// Whether a value starts like a date (`1979-05-27`) or a time (`07:32`).
fn is_date_or_time(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    let digit = |at: usize| bytes.get(at).is_some_and(u8::is_ascii_digit);
    (digit(0) && digit(1) && digit(2) && digit(3) && bytes.get(4) == Some(&b'-'))
        || (digit(0) && digit(1) && bytes.get(2) == Some(&b':'))
}

/// A date, a time, or both, with an optional offset. Kept as written.
fn date_or_time<'t>(cursor: &mut Cursor<'t>) -> Parsed<&'t str> {
    let start = cursor.at;
    let fail = |message: &str| Err(Failure { message: format!("this isn't a valid {message}"), at: start });
    if cursor.rest().as_bytes().get(4) == Some(&b'-') {
        if !(field(cursor, 4, 0, 9999) && cursor.eat('-') && field(cursor, 2, 1, 12)) {
            return fail("date");
        }
        if !(cursor.eat('-') && field(cursor, 2, 1, 31)) {
            return fail("date");
        }
        let rest = cursor.rest().as_bytes();
        let time_follows = matches!(rest.first(), Some(b'T' | b't'))
            || (rest.first() == Some(&b' ') && rest.get(1).is_some_and(u8::is_ascii_digit));
        if !time_follows {
            return Ok(cursor.text.get(start..cursor.at).unwrap_or(""));
        }
        cursor.bump();
    }
    if !(field(cursor, 2, 0, 23) && cursor.eat(':') && field(cursor, 2, 0, 59)) {
        return fail("time");
    }
    if cursor.eat(':') && !field(cursor, 2, 0, 60) {
        return fail("time");
    }
    if cursor.eat('.') && cursor.take_while(|c| c.is_ascii_digit()).is_empty() {
        return fail("time");
    }
    if !(cursor.eat('Z') || cursor.eat('z'))
        && matches!(cursor.peek(), Some('+' | '-'))
        && !(cursor.bump().is_some() && field(cursor, 2, 0, 23) && cursor.eat(':') && field(cursor, 2, 0, 59))
    {
        return fail("time offset");
    }
    Ok(cursor.text.get(start..cursor.at).unwrap_or(""))
}

/// Reads exactly `width` digits making a number from `low` to `high`.
fn field(cursor: &mut Cursor<'_>, width: usize, low: u32, high: u32) -> bool {
    let digits = cursor.rest().get(..width).unwrap_or("");
    if digits.len() != width || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    cursor.at += width;
    digits.parse::<u32>().is_ok_and(|n| (low..=high).contains(&n))
}

fn number(cursor: &mut Cursor<'_>) -> Parsed<Value> {
    let at = cursor.at;
    let token = cursor.take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.'));
    let fail = || Err(Failure { message: format!("`{token}` isn't a number TOML knows"), at });
    let (sign, unsigned) = match token.strip_prefix(['+', '-']) {
        Some(rest) => (token.get(..1).unwrap_or(""), rest),
        None => ("", token),
    };
    let negative = sign == "-";
    match unsigned {
        "inf" => return Ok(Value::Float(if negative { f64::NEG_INFINITY } else { f64::INFINITY })),
        "nan" => return Ok(Value::Float(f64::NAN)),
        _ => {}
    }
    let radix = match unsigned.get(..2) {
        Some("0x") => 16,
        Some("0o") => 8,
        Some("0b") => 2,
        _ => 10,
    };
    if radix != 10 {
        let digits = unsigned.get(2..).unwrap_or("");
        if !sign.is_empty() || !digits_ok(digits, radix) {
            return fail();
        }
        return i64::from_str_radix(&digits.replace('_', ""), radix)
            .map(Value::Int)
            .map_err(|_| Failure { message: format!("`{token}` is too big for an int"), at });
    }
    if unsigned.contains(['.', 'e', 'E']) {
        return if float_ok(unsigned) {
            token.replace('_', "").parse().map(Value::Float).or_else(|_| fail())
        } else {
            fail()
        };
    }
    if !whole_ok(unsigned) {
        return fail();
    }
    token
        .replace('_', "")
        .parse()
        .map(Value::Int)
        .map_err(|_| Failure { message: format!("`{token}` is too big for an int"), at })
}

/// Digits in a radix, with each underscore between two digits.
fn digits_ok(digits: &str, radix: u32) -> bool {
    !digits.is_empty()
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits.chars().all(|c| c == '_' || c.is_digit(radix))
}

/// The whole part of a decimal number: no leading zeros.
fn whole_ok(digits: &str) -> bool {
    digits_ok(digits, 10) && (digits == "0" || !digits.starts_with('0'))
}

/// `3.14`, `5e+22` or `6.626e-34`, without its sign.
fn float_ok(text: &str) -> bool {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (text, None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (mantissa, None),
    };
    whole_ok(whole)
        && fraction.is_none_or(|fraction| digits_ok(fraction, 10))
        && exponent.is_none_or(|exponent| digits_ok(exponent.trim_start_matches(['+', '-']), 10))
}

#[cfg(test)]
mod tests {
    use super::super::toml::parse;
    use crate::format::repr;

    fn read(text: &str) -> String {
        match parse(text) {
            Ok(value) => repr(&value),
            Err(failure) => format!("{} @ {}", failure.message, failure.at),
        }
    }

    #[test]
    fn reads_numbers() {
        assert_eq!(
            read("a = [+1, -2, 1_000, 0xff, 0o17, 0b101, 3.5, -1e3, 6.25e-2, 0.0]"),
            r#"{"a": [1, -2, 1000, 255, 15, 5, 3.5, -1000.0, 0.0625, 0.0]}"#
        );
        assert_eq!(read("a = [inf, -inf, true, false]"), r#"{"a": [inf, -inf, True, False]}"#);
        assert_eq!(read("a = 01"), "`01` isn't a number TOML knows @ 4");
        assert_eq!(read("a = 1__0"), "`1__0` isn't a number TOML knows @ 4");
        assert_eq!(read("a = 1."), "`1.` isn't a number TOML knows @ 4");
        assert_eq!(read("a = 9223372036854775808"), "`9223372036854775808` is too big for an int @ 4");
    }

    #[test]
    fn reads_strings() {
        assert_eq!(read(r#"a = "tab\there \u00e9 \"q\"""#), r#"{"a": "tab\there é \"q\""}"#);
        assert_eq!(read(r"a = 'C:\path'"), r#"{"a": "C:\\path"}"#);
        assert_eq!(read("a = \"\"\"\none\ntwo \\\n   three\"\"\"\""), r#"{"a": "one\ntwo three\""}"#);
        assert_eq!(read("a = '''\nraw \\n'''"), r#"{"a": "raw \\n"}"#);
        assert_eq!(
            read("a = \"open\nb = 1"),
            "this string never ends on its line; use `\"\"\"` for text over several lines @ 4"
        );
        assert_eq!(read(r#"a = "\q""#), "`\\q` isn't a TOML escape @ 5");
    }

    #[test]
    fn keeps_dates_and_times_as_written() {
        assert_eq!(
            read("a = [1979-05-27, 1979-05-27T07:32:00Z, 1979-05-27 00:32:00.999-07:00, 07:32:00]"),
            r#"{"a": ["1979-05-27", "1979-05-27T07:32:00Z", "1979-05-27 00:32:00.999-07:00", "07:32:00"]}"#
        );
        assert_eq!(read("a = 1979-13-01"), "this isn't a valid date @ 4");
    }

    #[test]
    fn reads_arrays_and_inline_tables() {
        assert_eq!(
            read("a = [\n  1, # one\n  2,\n]\nb = { x = 1, y.z = 'w' }\nc = []\nd = {}"),
            r#"{"a": [1, 2], "b": {"x": 1, "y": {"z": "w"}}, "c": [], "d": {}}"#
        );
        assert_eq!(read("a = { x = 1, x = 2 }"), "`x` is defined twice @ 13");
        assert_eq!(read("a = [1 2]"), "expected `,` or `]` after the value, but found `2` @ 7");
    }
}
