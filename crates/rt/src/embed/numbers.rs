//! `int(text)` and `float(text)` for native code. Text that isn't a number comes from outside the
//! program, so it fails with an `InvalidValue` rather than stopping it, which native code makes
//! from the message here.

use super::heap::{block_text, text_address};
use super::{State, with};

fn text_of(state: &State, text: i64) -> String {
    state.heap.block(text.cast_unsigned()).map(block_text).unwrap_or_default()
}

/// `int(text)`, with the whitespace around the number ignored: whether it was one, with the
/// number written to `bits`. A value of two words isn't given back whole, since platforms differ
/// on how one comes back.
pub extern "C" fn parse_int(text: i64, bits: &mut i64) -> i64 {
    with(|state| match text_of(state, text).trim().parse::<i64>() {
        Ok(number) => {
            *bits = number;
            1
        }
        Err(_) => 0,
    })
}

/// `u64.parse(text)`, as [`parse_int`] reads an int: 0 to 2^64 − 1, its bits written to `bits`.
pub extern "C" fn parse_u64(text: i64, bits: &mut i64) -> i64 {
    with(|state| match text_of(state, text).trim().parse::<u64>() {
        Ok(number) => {
            *bits = number.cast_signed();
            1
        }
        Err(_) => 0,
    })
}

/// `float(text)`, as [`parse_int`] reads an int, with the float's bits written to `bits`.
pub extern "C" fn parse_float(text: i64, bits: &mut i64) -> i64 {
    with(|state| match text_of(state, text).trim().parse::<f64>() {
        Ok(number) => {
            *bits = number.to_bits().cast_signed();
            1
        }
        Err(_) => 0,
    })
}

/// A new string saying text isn't a number: an int if `int` isn't 0, or else a float. The text
/// comes from outside, so past 40 characters only its first 20 are quoted, then how long it was.
pub extern "C" fn not_a_number(text: i64, int: i64) -> i64 {
    with(|state| {
        let what = if int == 0 { "a float" } else { "an int" };
        let message = format!("{} isn't {what}", quoted_input(&text_of(state, text)));
        text_address(state, &message)
    })
}

/// Text from outside as a message quotes it: whole, or past 40 characters, its first 20 and `…`,
/// then how long it was, as `"12345678901234567890…" (5,000 characters)`.
fn quoted_input(text: &str) -> String {
    let length = text.chars().count();
    if length <= 40 {
        return format!("{text:?}");
    }
    let start: String = text.chars().take(20).collect();
    format!("\"{}…\" ({} characters)", start.escape_debug(), with_commas(length))
}

/// `n` with a comma between each group of three digits, as `5,000`.
pub(crate) fn with_commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
