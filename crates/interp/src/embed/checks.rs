//! Failures native code's own checks find, like an overflow, by number, and the math
//! functions it calls.

use fennec_syntax::{Diagnostic, Span};

pub const OVERFLOW: i64 = 1;
pub const DIVISION_BY_ZERO: i64 = 2;
pub const ZERO_STEP: i64 = 3;
pub const TOO_DEEP: i64 = 4;
pub const NEGATIVE_SQRT: i64 = 5;
pub const NOT_AN_INT: i64 = 6;
pub const BAD_SHIFT: i64 = 7;

/// Called by native code when one of its own checks fails.
pub extern "C" fn fail(code: i64, start: i64, end: i64) {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    super::fail_with(match code {
        OVERFLOW => Diagnostic::new("this calculation overflows an int", span)
            .with_help("ints are 64-bit; check the values involved, or use floats for very large numbers"),
        DIVISION_BY_ZERO => Diagnostic::new("division by zero", span),
        NEGATIVE_SQRT => Diagnostic::new("can't take the square root of a negative number", span),
        NOT_AN_INT => Diagnostic::new("can't turn a float into an int", span),
        BAD_SHIFT => crate::ops::bad_shift(span),
        TOO_DEEP => {
            Diagnostic::new("calls nested too deeply", span).with_help("check for recursion that never stops")
        }
        _ => Diagnostic::new("`range` can't step by 0", span),
    });
}

pub extern "C" fn sin(x: f64) -> f64 {
    x.sin()
}

pub extern "C" fn cos(x: f64) -> f64 {
    x.cos()
}
