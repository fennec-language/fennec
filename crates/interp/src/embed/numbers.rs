//! `int(text)` and `float(text)` for native code. Text that isn't a number comes from outside the
//! program, so it fails with an `InvalidValue` rather than stopping it, which native code makes
//! from the message here.

use super::heap::{block_text, text_address};
use super::{State, with};

fn text_of(state: &State, text: i64) -> String {
    state.heap.block(text.cast_unsigned()).map(block_text).unwrap_or_default()
}

/// `int(text)`, with what's around the number ignored, as the interpreter reads it: whether it
/// was one, with the number written to `bits`. A value of two words isn't given back whole, since
/// platforms differ on how one comes back.
pub extern "C" fn parse_int(text: i64, bits: &mut i64) -> i64 {
    with(|state| match text_of(state, text).trim().parse::<i64>() {
        Ok(number) => {
            *bits = number;
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

/// A new string saying text isn't a number: an int if `int` isn't 0, or else a float.
pub extern "C" fn not_a_number(text: i64, int: i64) -> i64 {
    with(|state| {
        let what = if int == 0 { "a float" } else { "an int" };
        let message = format!("{:?} isn't {what}", text_of(state, text));
        text_address(state, &message)
    })
}
