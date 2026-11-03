//! `int(text)` and `float(text)` for native code. Text that isn't a number comes from outside the
//! program, so it fails with an `InvalidValue` rather than stopping it, which native code makes
//! from the message here.

use super::heap::{block_text, text_address};
use super::{State, with};

/// What reading a number gives: whether it was one, and an int, or a float's bits.
#[repr(C)]
#[derive(Default)]
pub struct Parsed {
    pub read: i64,
    pub bits: i64,
}

fn text_of(state: &State, text: i64) -> String {
    state.heap.block(text.cast_unsigned()).map(block_text).unwrap_or_default()
}

/// `int(text)`, with what's around the number ignored, as the interpreter reads it.
pub extern "C" fn parse_int(text: i64) -> Parsed {
    with(|state| match text_of(state, text).trim().parse::<i64>() {
        Ok(number) => Parsed { read: 1, bits: number },
        Err(_) => Parsed { read: 0, bits: 0 },
    })
}

/// `float(text)`, as [`parse_int`] reads an int.
pub extern "C" fn parse_float(text: i64) -> Parsed {
    with(|state| match text_of(state, text).trim().parse::<f64>() {
        Ok(number) => Parsed { read: 1, bits: number.to_bits().cast_signed() },
        Err(_) => Parsed { read: 0, bits: 0 },
    })
}

/// A new string saying text isn't a number: an int if `int` isn't 0, or else a float.
pub extern "C" fn not_a_number(text: i64, int: i64) -> i64 {
    with(|state| {
        let what = if int == 0 { "a float" } else { "an int" };
        let message = format!("{:?} isn't {what}", text_of(state, text));
        text_address(state, &message)
    })
}
