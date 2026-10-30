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

/// The text a value shows inside a collection, or when the REPL echoes it: strings are quoted.
pub(crate) fn repr(value: &Value) -> String {
    match value {
        Value::Unit => String::new(),
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
        Value::List(items) => format!("[{}]", join(items.iter())),
        Value::Packed(packed) => repr(&packed.unpack()),
        Value::Tuple(items) if items.len() == 1 => format!("({},)", join(items.iter())),
        Value::Tuple(items) => format!("({})", join(items.iter())),
        Value::Set(items) if items.is_empty() => "set()".to_owned(),
        Value::Set(items) => format!("{{{}}}", join(items.iter())),
        Value::Dict(pairs) => {
            let pairs: Vec<String> =
                pairs.iter().map(|(key, value)| format!("{}: {}", repr(key), repr(value))).collect();
            format!("{{{}}}", pairs.join(", "))
        }
        Value::Instance(instance) => instance_repr(instance),
        Value::Ok(inner) => format!("Ok({})", repr(inner)),
        Value::Err(inner) => format!("Err({})", repr(inner)),
        Value::IdMap(map) => format!("IdMap([{}])", join(map.entries().map(|(_, value)| value))),
        Value::Id(id) => format!("Id({})", id.slot),
        Value::Type(name) => format!("<type {name}>"),
        Value::Function(function) => format!("<function {}>", function.def.name),
        Value::Builtin(name) => format!("<builtin {name}>"),
        Value::Lambda(_) => "<function>".to_owned(),
        Value::Capability(_) => format!("<{}>", value.type_name()),
    }
}

fn join<'a>(items: impl Iterator<Item = &'a Value>) -> String {
    items.map(repr).collect::<Vec<_>>().join(", ")
}

fn instance_repr(instance: &Instance) -> String {
    let name = match &instance.case {
        Some(case) => format!("{}.{case}", instance.type_name),
        None => instance.type_name.to_string(),
    };
    if instance.fields.is_empty() && instance.case.is_some() {
        return name;
    }
    let fields: Vec<String> =
        instance.fields.iter().map(|(field, value)| format!("{field}={}", repr(value))).collect();
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
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// `1.5e+17` and `1e-05`, as Python writes them.
fn scientific(x: f64) -> String {
    let text = format!("{x:e}");
    match text.split_once('e') {
        Some((mantissa, exponent)) => {
            let (sign, digits) = exponent.strip_prefix('-').map_or(("+", exponent), |digits| ("-", digits));
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => text,
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
