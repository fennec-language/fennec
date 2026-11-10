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

/// Fails with a string's text at a span: an `assert`, a `where` rule, or a generator used
/// twice, which end the program.
pub extern "C" fn stop(message: i64, start: i64, end: i64) {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    let text = super::with(|state| {
        u64::try_from(message)
            .ok()
            .and_then(|at| state.heap.block(at))
            .map_or_else(String::new, super::heap::block_text)
    });
    // A help to show under it comes after a mark.
    match text.split_once(fennec_syntax::HELP_MARK) {
        Some((message, help)) => super::fail_with(Diagnostic::new(message, span).with_help(help)),
        None => super::fail_with(Diagnostic::new(text, span)),
    }
}

/// `fennec_proof_broken(message, start, end)`: in a build that checks proofs, something the
/// compiler proved, and left a check out for, didn't hold. That's a bug in the compiler, not the
/// program, so nothing can pass it on: the whole program ends at once, whatever's running, saying
/// which proof and where.
pub extern "C" fn proof_broken(message: i64, start: i64, end: i64) {
    stop(message, start, end);
    super::aot::stop_for_failure();
    // Somewhere with nothing to say where, like a thread the program's sources never reached.
    let failure = super::with(|state| state.failure.take()).map(|failure| failure.message);
    let _ = std::io::Write::write_all(
        &mut std::io::stderr(),
        format!("error: {}\n", failure.unwrap_or_else(|| "proof broken".to_owned())).as_bytes(),
    );
    super::aot::stop_now(1);
}

/// Fails an `assert ... fails`, saying what really happened, given as a string: what the call
/// gave, or what it failed with instead.
pub extern "C" fn assert_said(happened: i64, start: i64, end: i64) {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    let help = super::with(|state| {
        u64::try_from(happened)
            .ok()
            .and_then(|at| state.heap.block(at))
            .map_or_else(String::new, super::heap::block_text)
    });
    super::fail_with(Diagnostic::new("this assertion failed", span).with_help(help));
}

/// Fails a comparison in an `assert`, saying what each side was, given as strings.
pub extern "C" fn assert_sides(left: i64, right: i64, start: i64, end: i64) {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    let text_of = |at: i64| {
        super::with(|state| {
            u64::try_from(at)
                .ok()
                .and_then(|at| state.heap.block(at))
                .map_or_else(String::new, super::heap::block_text)
        })
    };
    let help = format!("the left side is {} and the right side is {}", text_of(left), text_of(right));
    super::fail_with(Diagnostic::new("this assertion failed", span).with_help(help));
}

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

/// The most places a float can have digits at.
const MOST_PLACES: i64 = 1100;

/// `round(x, places)`, by the number `x` really is: 1.115 is a little under it, so it rounds down.
/// Past every digit `x` has, it's `x`.
pub extern "C" fn round_places(x: f64, places: i64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if places < 0 {
        return round_to_tens(x, places);
    }
    if places > MOST_PLACES {
        return x;
    }
    // Nowhere near a tie, multiplying can't change which way it rounds, and both the power of ten
    // and the rounded number are exact, so dividing gives the nearest float to the answer.
    if places <= 22 {
        let scale = 10_f64.powi(i32::try_from(places).unwrap_or(0));
        let scaled = x * scale;
        if scaled.abs() < 4_503_599_627_370_496.0 && (scaled.fract().abs() - 0.5).abs() > 1e-6 {
            return scaled.round_ties_even() / scale;
        }
    }
    let digits = usize::try_from(places).unwrap_or(0);
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// `round(x, places)` for a negative `places`: to tens, hundreds and so on.
fn round_to_tens(x: f64, places: i64) -> f64 {
    let power = places.checked_neg().and_then(|power| i32::try_from(power).ok());
    match power.filter(|power| *power <= 308) {
        Some(power) => {
            let scale = 10_f64.powi(power);
            (x / scale).round_ties_even() * scale
        }
        None => 0.0_f64.copysign(x),
    }
}

// The float functions are computed with `libm`'s, the same on every platform, so a
// program's floats are the same bits wherever it runs. Past -1 or 1, `acos` has no angle, so it's
// NaN, as `log` is of a number below 0.

pub extern "C" fn sin(x: f64) -> f64 {
    libm::sin(x)
}

pub extern "C" fn cos(x: f64) -> f64 {
    libm::cos(x)
}

pub extern "C" fn tan(x: f64) -> f64 {
    libm::tan(x)
}

pub extern "C" fn acos(x: f64) -> f64 {
    libm::acos(x)
}

pub extern "C" fn exp(x: f64) -> f64 {
    libm::exp(x)
}

pub extern "C" fn log(x: f64) -> f64 {
    libm::log(x)
}

#[cfg(test)]
mod round_tests {
    use super::round_places;

    #[test]
    fn rounding_goes_by_the_number_a_float_really_is() {
        // 1.115 is really a little below it.
        assert_eq!(round_places(1.115, 2), 1.11);
        assert_eq!(round_places(1.2345, 1), 1.2);
        assert_eq!(round_places(2.5, 0), 2.0);
        assert_eq!(round_places(0.125, 2), 0.12);
    }

    #[test]
    fn rounding_past_what_a_float_holds_changes_nothing() {
        assert_eq!(round_places(1e307, 2), 1e307);
        assert_eq!(round_places(1.5, 400), 1.5);
        assert_eq!(round_places(1.5, 5_000_000_000), 1.5);
        assert!(round_places(f64::NAN, 2).is_nan());
        assert_eq!(round_places(f64::INFINITY, 2), f64::INFINITY);
    }

    #[test]
    fn rounding_to_tens_and_past_every_digit() {
        assert_eq!(round_places(1234.5, -2), 1200.0);
        assert_eq!(round_places(1234.5, -400), 0.0);
        assert_eq!(round_places(1234.5, -5_000_000_000), 0.0);
    }
}
