//! The numbers behind `Random`: `SplitMix64`, drawn by position. The `n`th number of a seed's
//! sequence depends only on the seed and `n`, so it's the same on every machine and can be
//! drawn without keeping any state.

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants};
use crate::call::CallArg;
use crate::collections::plain;
use crate::interp::Interp;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &["random.random_choice", "random.random_float", "random.random_int"],
    call,
    constant: no_constants,
    methods: &[],
};

fn call(_: &mut Interp<'_>, name: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
    draw(name, &plain(args, name, span)?, span)
}

/// The `n`th 64-bit number of `seed`'s sequence.
pub(crate) fn splitmix(seed: i64, n: i64) -> u64 {
    const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;
    let step = n.cast_unsigned().wrapping_add(1).wrapping_mul(GOLDEN);
    let mut z = seed.cast_unsigned().wrapping_add(step);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A number from 0 up to `count`, with no bias, and how many numbers were drawn after it.
/// `count` of 0 means the whole 64-bit range.
fn below(seed: i64, mut drawn: i64, count: u64) -> (u64, i64) {
    // Lemire's method: multiply into 128 bits, and redraw the rare values that would make
    // some results more likely than others.
    let threshold = count.wrapping_neg().checked_rem(count).unwrap_or(0);
    loop {
        let bits = splitmix(seed, drawn);
        drawn = drawn.saturating_add(1);
        if count == 0 {
            return (bits, drawn);
        }
        let wide = u128::from(bits) * u128::from(count);
        if u64::try_from(wide & u128::from(u64::MAX)).unwrap_or(0) >= threshold {
            return (u64::try_from(wide >> 64).unwrap_or(0), drawn);
        }
    }
}

/// `random_int(seed, drawn, low, high)`, `random_float(seed, drawn)` and
/// `random_choice(seed, drawn, items)`: a number or item, and how many numbers were drawn.
fn draw(name: &str, values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let pair = |value: Value, drawn: i64| Value::tuple(vec![value, Value::Int(drawn)]);
    match (name, values) {
        ("random.random_int", [Value::Int(seed), Value::Int(drawn), Value::Int(low), Value::Int(high)]) => {
            if low > high {
                return Err(Diagnostic::new(
                    format!("can't pick a number from {low} to {high}: `low` is bigger than `high`"),
                    span,
                ));
            }
            // The count of numbers from low to high, wrapping to 0 for the whole range.
            let count = high.wrapping_sub(*low).cast_unsigned().wrapping_add(1);
            let (offset, drawn) = below(*seed, *drawn, count);
            Ok(pair(Value::Int(low.wrapping_add(offset.cast_signed())), drawn))
        }
        ("random.random_float", [Value::Int(seed), Value::Int(drawn)]) => {
            let bits = splitmix(*seed, *drawn) >> 11;
            #[expect(clippy::cast_precision_loss, reason = "53 bits fit a float exactly")]
            let fraction = bits as f64 / (1_u64 << 53) as f64;
            Ok(pair(Value::Float(fraction), drawn.saturating_add(1)))
        }
        ("random.random_choice", [Value::Int(seed), Value::Int(drawn), Value::List(items)]) => {
            let count = u64::try_from(items.len()).unwrap_or(0);
            if count == 0 {
                return Err(Diagnostic::new("can't choose from an empty list", span));
            }
            let (at, drawn) = below(*seed, *drawn, count);
            let item = usize::try_from(at).ok().and_then(|at| items.get(at)).cloned().unwrap_or(Value::None);
            Ok(pair(item, drawn))
        }
        _ => Err(Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span)),
    }
}

#[cfg(test)]
mod tests {
    /// The reference `SplitMix64`'s first outputs for seed 0, so every build draws the same.
    #[test]
    fn splitmix_matches_the_reference() {
        assert_eq!(super::splitmix(0, 0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(super::splitmix(0, 1), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(super::splitmix(0, 2), 0x06C4_5D18_8009_454F);
    }
}
