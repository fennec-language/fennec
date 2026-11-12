//! `int(text)` and `float(text)` for native code. Text that isn't a number comes from outside the
//! program, so it fails with an `InvalidValue` rather than stopping it, which native code makes
//! from the message here.

use super::heap::{block_text, text_address};
use super::{State, with};

fn text_of(state: &State, text: i64) -> String {
    state.heap.block(text.cast_unsigned()).map(block_text).unwrap_or_default()
}

/// The most bytes a number's text is read from where it is, whitespace around it and all.
const SHORT: usize = 40;

/// Reads `text` as a number with `parse`, the whitespace around it ignored as `str::trim` ignores
/// it. Short ASCII text, as a number's is, is read from its block's words into bytes on the stack,
/// with no `String` made: for ASCII, `trim`'s whitespace is tab to carriage return and space.
/// Anything else goes through `trim` itself.
fn parsed<T: std::str::FromStr>(state: &State, text: i64) -> Option<T> {
    let block = state.heap.block(text.cast_unsigned())?;
    let length = block.get(2).and_then(|length| usize::try_from(*length).ok()).unwrap_or(0);
    if length <= SHORT {
        let mut bytes = [0_u8; SHORT + 8];
        let words = block.get(super::heap::TEXT_HEADER_WORDS..).unwrap_or_default();
        for (to, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(words.iter().take(length.div_ceil(8))) {
            *to = word.to_le_bytes();
        }
        let bytes = bytes.get(..length).unwrap_or_default();
        if bytes.is_ascii() {
            let space = |byte: &u8| matches!(byte, b'\t'..=b'\r' | b' ');
            let start = bytes.iter().position(|byte| !space(byte)).unwrap_or(bytes.len());
            let end = bytes.iter().rposition(|byte| !space(byte)).map_or(start, |at| at + 1);
            let number = std::str::from_utf8(bytes.get(start..end).unwrap_or_default()).ok()?;
            return number.parse().ok();
        }
    }
    text_of(state, text).trim().parse().ok()
}

/// `int(text)`, with the whitespace around the number ignored: whether it was one, with the
/// number written to `bits`. A value of two words isn't given back whole, since platforms differ
/// on how one comes back.
pub extern "C" fn parse_int(text: i64, bits: &mut i64) -> i64 {
    with(|state| match parsed::<i64>(state, text) {
        Some(number) => {
            *bits = number;
            1
        }
        None => 0,
    })
}

/// `u64.parse(text)`, as [`parse_int`] reads an int: 0 to 2^64 − 1, its bits written to `bits`.
pub extern "C" fn parse_u64(text: i64, bits: &mut i64) -> i64 {
    with(|state| match parsed::<u64>(state, text) {
        Some(number) => {
            *bits = number.cast_signed();
            1
        }
        None => 0,
    })
}

/// `float(text)`, as [`parse_int`] reads an int, with the float's bits written to `bits`.
pub extern "C" fn parse_float(text: i64, bits: &mut i64) -> i64 {
    with(|state| match parsed::<f64>(state, text) {
        Some(number) => {
            *bits = number.to_bits().cast_signed();
            1
        }
        None => 0,
    })
}

/// A new string saying text isn't a number: an int if `int` isn't 0, or else a float. The text
/// comes from outside, so past 40 characters only its first 20 are quoted, then how long it was.
pub extern "C" fn not_a_number(text: i64, int: i64) -> i64 {
    super::made_with("fennec_not_a_number", |state| {
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
