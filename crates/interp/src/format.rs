//! Turning values into text: how `stdout.print` shows them, and f-string format specs like
//! `{coins:,}` and `{health:.1f}`.



use crate::value::{Instance, Value};

/// The text `stdout.print` and `str()` produce.
pub(crate) fn display(value: &Value) -> String {
    match value {
        Value::Str(text) => text.to_string(),
        other => repr(other),
    }
}

/// A failure as it's shown when nothing handled it: the error, then each of its notes on a
/// line of its own under it, as `note: ...`, a note's later lines indented under its first.
pub(crate) fn failure(error: &Value) -> String {
    let mut text = repr(error);
    if let Value::Instance(instance) = error
        && let Some(notes) = &instance.notes
    {
        for note in notes.iter() {
            text.push_str("\nnote: ");
            text.push_str(&note.replace('\n', "\n  "));
        }
    }
    text
}

/// The text a value shows inside a collection: strings are quoted.
pub(crate) fn repr(value: &Value) -> String {
    repr_with(value, &mut |_| None)
}

/// Like `repr`, with `own` as in `display_with`.
pub(crate) fn repr_with(value: &Value, own: &mut dyn FnMut(&Instance) -> Option<String>) -> String {
    let mut join = |items: &mut dyn Iterator<Item = &Value>| {
        items.map(|item| repr_with(item, own)).collect::<Vec<_>>().join(", ")
    };
    match value {
        Value::Unit | Value::Lent(_) => String::new(),
        Value::None => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => float(*x),
        Value::Str(text) => format!("{text:?}"),
        Value::Bytes(data) => {
            let numbers: Vec<String> = data.iter().map(u8::to_string).collect();
            format!("bytes([{}])", numbers.join(", "))
        }
        Value::List(items) => format!("[{}]", join(&mut items.iter())),
        Value::Packed(packed) => repr_with(&packed.unpack(), own),
        Value::Tuple(items) if items.len() == 1 => format!("({},)", join(&mut items.iter())),
        Value::Tuple(items) => format!("({})", join(&mut items.iter())),
        Value::Set(items) if items.is_empty() => "set()".to_owned(),
        Value::Set(items) => format!("{{{}}}", join(&mut items.iter())),
        Value::Dict(pairs) => {
            let pairs: Vec<String> = pairs
                .iter()
                .map(|(key, value)| format!("{}: {}", repr_with(key, own), repr_with(value, own)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
        Value::Instance(instance) => match own(instance) {
            Some(text) => text,
            None => instance_repr(instance, own),
        },
        Value::Ok(inner) => format!("Ok({})", repr_with(inner, own)),
        Value::Err(inner) => format!("Err({})", repr_with(inner, own)),
        Value::Table(map) => format!("Table([{}])", join(&mut map.entries().map(|(_, value)| value))),
        Value::Id(id) => format!("Id({})", id.slot),
        Value::Capability(_) | Value::Limited(_) => format!("<{}>", value.type_name()),
    }
}

fn instance_repr(instance: &Instance, own: &mut dyn FnMut(&Instance) -> Option<String>) -> String {
    let type_name = fennec_syntax::rename::shown_type_name(&instance.type_name);
    let name = match &instance.case {
        Some(case) => format!("{type_name}.{case}"),
        None => type_name.to_owned(),
    };
    if instance.fields.is_empty() && instance.case.is_some() {
        return name;
    }
    let fields: Vec<String> =
        instance.fields.iter().map(|(field, value)| format!("{field}={}", repr_with(value, own))).collect();
    format!("{name}({})", fields.join(", "))
}

/// Floats print like Python's: as short as round-trips, always with a decimal point or an
/// exponent, and with an exponent when they're very large or very small.
#[expect(clippy::float_cmp, reason = "checking for a whole number is an exact comparison")]
pub fn float(x: f64) -> String {
    if x.is_nan() {
        "nan".to_owned()
    } else if x.is_infinite() {
        if x.is_sign_positive() { "inf".to_owned() } else { "-inf".to_owned() }
    } else if x != 0.0 && !(1e-4..1e16).contains(&x.abs()) {
        scientific(x)
    } else if x == x.trunc() {
        format!("{x}.0")
    } else {
        format!("{x}")
    }
}

/// An `f32`, held as the `f64` that's exactly it, written as `float` writes a float, from the
/// shortest digits that read back as that `f32`: `0.1`, not `0.10000000149011612`.
#[expect(clippy::cast_possible_truncation, reason = "the value is exactly an f32")]
#[expect(clippy::float_cmp, reason = "checking for a whole number is an exact comparison")]
pub(crate) fn float32(wide: f64) -> String {
    let x = wide as f32;
    if x.is_nan() {
        "nan".to_owned()
    } else if x.is_infinite() {
        if x.is_sign_positive() { "inf".to_owned() } else { "-inf".to_owned() }
    } else if x != 0.0 && !(1e-4..1e16).contains(&x.abs()) {
        exponent_as_python(&format!("{x:e}"))
    } else if x == x.trunc() {
        format!("{x}.0")
    } else {
        format!("{x}")
    }
}

/// `1.5e+17` and `1e-05`, as Python writes them.
fn scientific(x: f64) -> String {
    exponent_as_python(&format!("{x:e}"))
}

/// Rust's exponent, as in `1.5e17`, written as Python writes it: `1.5e+17`.
fn exponent_as_python(text: &str) -> String {
    match text.split_once('e') {
        Some((mantissa, exponent)) => {
            let (sign, digits) = exponent.strip_prefix('-').map_or(("+", exponent), |digits| ("-", digits));
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => text.to_owned(),
    }
}

/// Applies an f-string format spec: an optional `,` for thousands separators, an optional
/// `.N` precision, and an optional `f` or `d`.
pub(crate) fn with_spec(value: &Value, spec: &str) -> Result<String, String> {
    let (comma, rest) = spec.strip_prefix(',').map_or((false, spec), |rest| (true, rest));
    let (precision, kind) = match rest.strip_prefix('.') {
        Some(rest) => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let precision =
                digits.parse::<usize>().map_err(|_| format!("`{spec}` needs a number after `.`"))?;
            (Some(precision), rest.get(digits.len()..).unwrap_or(""))
        }
        None => (None, rest),
    };
    let text = match (value, kind, precision) {
        (Value::Int(n), "" | "d", None) => n.to_string(),
        (Value::Int(n), "" | "f", Some(places)) => format!("{:.places$}", crate::ops::to_float(*n)),
        (Value::Float(x), "" | "f", Some(places)) => format!("{x:.places$}"),
        (Value::Float(x), "" | "f", None) => float(*x),
        (other, _, _) if spec.is_empty() => display(other),
        (other, _, _) => {
            return Err(format!("the format `{spec}` doesn't apply to a {}", other.type_name()));
        }
    };
    Ok(if comma { thousands(&text) } else { text })
}

/// `with_spec` for a `u64`: one that fits an `int` formats as that int does, and a bigger one
/// takes only the plain and `,` formats.
pub(crate) fn with_spec_u64(n: u64, spec: &str) -> Result<String, String> {
    if let Ok(fits) = i64::try_from(n) {
        return with_spec(&Value::Int(fits), spec);
    }
    match spec {
        "" | "d" => Ok(n.to_string()),
        "," | ",d" => Ok(thousands(&n.to_string())),
        _ => Err(format!("the format `{spec}` isn't built yet for a `u64` of 2^63 or more")),
    }
}

/// `with_spec` for an `f32`, held as the `f64` that's exactly it: with a precision it's that
/// float's, and without one it's the shortest text that reads back as the `f32`.
pub(crate) fn with_spec_f32(wide: f64, spec: &str) -> Result<String, String> {
    match spec {
        "" | "f" => Ok(float32(wide)),
        "," | ",f" => Ok(thousands(&float32(wide))),
        _ => with_spec(&Value::Float(wide), spec),
    }
}

fn thousands(number: &str) -> String {
    let (sign, digits) = number.strip_prefix('-').map_or(("", number), |rest| ("-", rest));
    let (whole, fraction) =
        digits.split_once('.').map_or((digits, None), |(whole, fraction)| (whole, Some(fraction)));
    let mut grouped = String::new();
    for (at, c) in whole.chars().enumerate() {
        if at > 0 && (whole.len() - at) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

