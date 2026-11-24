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

/// The powers of ten a float holds exactly, ten to 0 up to ten to 22, written out. They aren't
/// worked out with `powi`: Rust leaves its precision to the platform, and with a scale one bit
/// off, `round(1.2345, 1)` is 1.2000000000000002.
const EXACT_TENS: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18,
    1e19, 1e20, 1e21, 1e22,
];

/// The most a power of ten can be and still be a float.
const MOST_TENS: usize = 308;

/// Ten to `power` as the float nearest it, the same bits wherever the program runs, or nothing
/// past the last one a float can be. Up to 22 it's exact, from the table. Past that it's read
/// from its digits, once: reading digits gives the nearest float on every platform, which
/// multiplying tens together doesn't.
fn ten_to(power: usize) -> Option<f64> {
    static PAST_EXACT: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    if let Some(ten) = EXACT_TENS.get(power) {
        return Some(*ten);
    }
    let past = PAST_EXACT.get_or_init(|| {
        (EXACT_TENS.len()..=MOST_TENS)
            .map(|power| format!("1e{power}").parse().unwrap_or(f64::INFINITY))
            .collect()
    });
    past.get(power - EXACT_TENS.len()).copied()
}

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
    if let Some(scale) = usize::try_from(places).ok().and_then(|places| EXACT_TENS.get(places)).copied() {
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
    let power = places.checked_neg().and_then(|power| usize::try_from(power).ok());
    match power.and_then(ten_to) {
        Some(scale) => (x / scale).round_ties_even() * scale,
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

// An `f32`'s, by `libm`'s `f32` functions. Native code holds an `f32` as the `f64` that's exactly
// its value, so each takes and gives that: narrowing it is exact, as widening the answer is.

pub extern "C" fn sinf(x: f64) -> f64 {
    f64::from(libm::sinf(narrow(x)))
}

pub extern "C" fn cosf(x: f64) -> f64 {
    f64::from(libm::cosf(narrow(x)))
}

pub extern "C" fn acosf(x: f64) -> f64 {
    f64::from(libm::acosf(narrow(x)))
}

/// The `f32` an `f64` holding one is.
#[expect(clippy::cast_possible_truncation, reason = "the f64 holds an f32 exactly")]
fn narrow(x: f64) -> f32 {
    x as f32
}

#[cfg(test)]
mod f32_tests {
    use super::{acosf, cosf, sinf};

    /// Each input's `sin`, `cos` and `acos` as `f32` bits, or `None` for NaN, whose sign bit
    /// platforms differ on. Pinned so a platform whose numbers differ, or a change away from
    /// `libm`'s own functions, fails here.
    const PINNED: [(f32, u32, u32, Option<u32>); 12] = [
        (0.0, 0x0000_0000, 0x3f80_0000, Some(0x3fc9_0fda)),
        (-0.0, 0x8000_0000, 0x3f80_0000, Some(0x3fc9_0fda)),
        (0.5, 0x3ef5_7744, 0x3f60_a940, Some(0x3f86_0a92)),
        (-0.5, 0xbef5_7744, 0x3f60_a940, Some(0x4006_0a92)),
        (1.0, 0x3f57_6aa4, 0x3f0a_5140, Some(0x0000_0000)),
        (-1.0, 0xbf57_6aa4, 0x3f0a_5140, Some(0x4049_0fda)),
        (0.1, 0x3dcc_7577, 0x3f7e_b898, Some(0x3fbc_3d91)),
        (2.0, 0x3f68_c7b7, 0xbed5_1133, None),
        (std::f32::consts::PI, 0xb3bb_bd2e, 0xbf80_0000, None),
        (100.0, 0xbf01_a12e, 0x3f5c_c0ee, None),
        (1.0e-30, 0x0da2_4260, 0x3f80_0000, Some(0x3fc9_0fda)),
        (12_345.678, 0xbf34_4b08, 0x3f35_be20, None),
    ];

    #[expect(clippy::cast_possible_truncation, reason = "each answer is an f32's exactly")]
    fn bits(x: f64) -> u32 {
        (x as f32).to_bits()
    }

    #[test]
    fn sin_cos_and_acos_of_an_f32_are_the_same_bits_everywhere() {
        for (x, sin, cos, acos) in PINNED {
            let wide = f64::from(x);
            assert_eq!(bits(sinf(wide)), sin, "sin {x}");
            assert_eq!(bits(cosf(wide)), cos, "cos {x}");
            match acos {
                Some(acos) => assert_eq!(bits(acosf(wide)), acos, "acos {x}"),
                None => assert!(acosf(wide).is_nan(), "acos {x}"),
            }
        }
    }

    /// What the reference evaluator prints for `sqrt(2)`, `sin(0.5)`, `cos(0.5)` and `acos(0.5)`
    /// on an `f32`, as an `f32` prints: its shortest text that reads back as it.
    #[test]
    fn an_f32_prints_what_the_reference_evaluator_does() {
        let shown = |x: f64| format!("{}", f32::from_bits(bits(x)));
        assert_eq!(format!("{}", 2.0_f32.sqrt()), "1.4142135");
        assert_eq!(shown(sinf(0.5)), "0.47942555");
        assert_eq!(shown(cosf(0.5)), "0.87758255");
        assert_eq!(shown(acosf(0.5)), "1.0471976");
    }
}

#[cfg(test)]
mod round_tests {
    use super::{EXACT_TENS, MOST_TENS, round_places, ten_to};

    /// Ten to `power` read from its digits, which gives the nearest float on every platform.
    fn read(power: usize) -> f64 {
        format!("1e{power}").parse().unwrap_or(f64::NAN)
    }

    #[test]
    fn the_table_holds_each_power_of_ten_exactly() {
        assert_eq!(EXACT_TENS.len(), 23);
        for (power, ten) in EXACT_TENS.iter().enumerate() {
            // Written out in full it's a one and `power` zeros: nothing was rounded away.
            assert_eq!(format!("{ten:.0}"), format!("1{}", "0".repeat(power)), "ten to {power}");
            assert_eq!(ten.to_bits(), read(power).to_bits(), "ten to {power}");
        }
        // The next one isn't a float: the table ends where exactness does.
        assert_ne!(format!("{:.0}", read(23)), format!("1{}", "0".repeat(23)));
    }

    #[test]
    fn every_power_of_ten_is_the_float_nearest_it() {
        for power in 0..=MOST_TENS {
            let ten = ten_to(power).unwrap_or(f64::NAN);
            assert_eq!(ten.to_bits(), read(power).to_bits(), "ten to {power}");
            assert!(ten.is_finite(), "ten to {power}");
        }
        assert_eq!(ten_to(MOST_TENS + 1), None);
        assert_eq!(ten_to(usize::MAX), None);
    }

    #[test]
    fn rounding_at_every_place_is_the_number_written_to_that_many_places() {
        let numbers = [
            1.2345,
            1.115,
            0.125,
            2.5,
            123_456.789_012_345,
            9.999_999_999,
            0.000_012_345_678_9,
            1.0 / 3.0,
            0.1 + 0.2,
            987_654_321.123_456_7,
            5e-324,
            4_503_599_627_370_497.5,
        ];
        for number in numbers {
            for sign in [1.0, -1.0] {
                let x = sign * number;
                for places in 0..=22_usize {
                    let written: f64 = format!("{x:.places$}").parse().unwrap_or(f64::NAN);
                    let rounded = round_places(x, i64::try_from(places).unwrap_or(0));
                    assert_eq!(rounded.to_bits(), written.to_bits(), "round({x}, {places})");
                }
            }
        }
    }

    #[test]
    fn rounding_to_every_power_of_ten() {
        for power in 1..=MOST_TENS {
            let ten = read(power);
            let places = -i64::try_from(power).unwrap_or(0);
            assert_eq!(round_places(ten, places).to_bits(), ten.to_bits(), "round(1e{power}, {places})");
            assert_eq!(round_places(0.6 * ten, places).to_bits(), ten.to_bits(), "0.6e{power}");
            assert_eq!(round_places(-0.6 * ten, places).to_bits(), (-ten).to_bits(), "-0.6e{power}");
            assert_eq!(round_places(0.4 * ten, places).to_bits(), 0.0_f64.to_bits(), "0.4e{power}");
            assert_eq!(round_places(-0.4 * ten, places).to_bits(), (-0.0_f64).to_bits(), "-0.4e{power}");
        }
    }

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
