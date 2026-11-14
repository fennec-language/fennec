//! How numbers and strings are written as text, as the compiled program writes them: floats as
//! Python writes them, strings in quotes inside a collection, and f-string format specs.

/// A float as Fennec prints it: the fewest digits that read back as the same float, always with a
/// point or an exponent, and with an exponent below 1e-4 and from 1e16 up, written as Python does,
/// as in `1e-05` and `1.5e+17`.
pub(crate) fn float(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".to_owned() } else { "-inf".to_owned() };
    }
    if x != 0.0 && !(1e-4..1e16).contains(&x.abs()) {
        return with_python_exponent(&format!("{x:e}"));
    }
    if x.fract() == 0.0 { format!("{x}.0") } else { format!("{x}") }
}

/// Rust's `1.5e17` written as Python writes it, `1.5e+17`, with at least two digits after the sign.
fn with_python_exponent(text: &str) -> String {
    match text.split_once('e') {
        Some((mantissa, exponent)) => {
            let (sign, digits) = exponent.strip_prefix('-').map_or(("+", exponent), |digits| ("-", digits));
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => text.to_owned(),
    }
}

/// A string as it shows inside a collection: in double quotes, with Rust's escapes, as the
/// runtime writes it.
pub(crate) fn quoted(text: &str) -> String {
    format!("{text:?}")
}

/// What a number looks like under an f-string's format spec, or `None` for a spec the oracle
/// doesn't model. A spec is an optional `,` for thousands, an optional `.N` for places, and an
/// optional `f` or `d`.
pub(crate) enum Number {
    Int(i64),
    Float(f64),
}

pub(crate) fn with_spec(number: &Number, spec: &str) -> Option<String> {
    let (comma, rest) = spec.strip_prefix(',').map_or((false, spec), |rest| (true, rest));
    let (places, kind) = match rest.strip_prefix('.') {
        Some(rest) => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let places = digits.parse::<usize>().ok()?;
            (Some(places), rest.get(digits.len()..).unwrap_or(""))
        }
        None => (None, rest),
    };
    let text = match (number, kind, places) {
        (Number::Int(n), "" | "d", None) => n.to_string(),
        (Number::Int(n), "" | "f", Some(places)) => format!("{:.places$}", int_as_float(*n)),
        (Number::Float(x), "" | "f", Some(places)) => format!("{x:.places$}"),
        (Number::Float(x), "" | "f", None) => float(*x),
        _ => return None,
    };
    Some(if comma { thousands(&text) } else { text })
}

/// An int as the nearest float, as `float(n)` gives it.
#[expect(clippy::cast_precision_loss, reason = "an int becomes the nearest float, as the language says")]
pub(crate) fn int_as_float(n: i64) -> f64 {
    n as f64
}

/// A number's whole part with a comma between each group of three digits.
fn thousands(number: &str) -> String {
    let (sign, digits) = number.strip_prefix('-').map_or(("", number), |rest| ("-", rest));
    let (whole, fraction) =
        digits.split_once('.').map_or((digits, None), |(whole, fraction)| (whole, Some(fraction)));
    let mut grouped = String::new();
    let count = whole.chars().count();
    for (at, c) in whole.chars().enumerate() {
        if at > 0 && (count - at) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Number, float, with_spec};

    /// What `stdout.print` writes for each of these floats, built natively.
    #[test]
    fn floats_print_as_fennec_prints_them() {
        let cases = [
            (1.0 / 3.0, "0.3333333333333333"),
            (100.0, "100.0"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-1.5e-10, "-1.5e-10"),
            (0.1 + 0.2, "0.30000000000000004"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (-0.0, "-0.0"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
        ];
        for (x, written) in cases {
            assert_eq!(float(x), written, "{x:e}");
        }
    }

    #[test]
    fn specs_write_what_the_formatting_example_shows() {
        assert_eq!(with_spec(&Number::Int(1_234_567), ",").as_deref(), Some("1,234,567"));
        assert_eq!(with_spec(&Number::Int(-1_234_567), ",").as_deref(), Some("-1,234,567"));
        assert_eq!(with_spec(&Number::Float(1234.5), ",.2f").as_deref(), Some("1,234.50"));
        assert_eq!(with_spec(&Number::Float(-0.5), ",").as_deref(), Some("-0.5"));
        assert_eq!(with_spec(&Number::Int(7), ".2f").as_deref(), Some("7.00"));
        assert_eq!(with_spec(&Number::Float(2.5), "f").as_deref(), Some("2.5"));
        assert_eq!(with_spec(&Number::Float(87.456), ".1f").as_deref(), Some("87.5"));
        assert_eq!(with_spec(&Number::Int(7), "x"), None);
    }
}
