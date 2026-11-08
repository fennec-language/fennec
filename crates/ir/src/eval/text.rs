//! The text a constant's values give, as `str()`, an f-string and a failure's message show them.
//! A struct or an enum's value may write its own text with `def str`, so this doesn't show
//! one: what does is left for the program to work out when it starts.

use super::value::Value;

/// The text `str()` gives: a str as it is, and anything else as it shows inside a collection.
pub(super) fn shown(value: &Value) -> Option<String> {
    match value {
        Value::Str(text) => Some(text.to_string()),
        other => repr(other),
    }
}

/// The text a value shows inside a collection, with strs quoted.
pub(super) fn repr(value: &Value) -> Option<String> {
    let join = |items: &mut dyn Iterator<Item = &Value>| -> Option<String> {
        let shown: Option<Vec<String>> = items.map(repr).collect();
        Some(shown?.join(", "))
    };
    Some(match value {
        Value::None => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => fennec_syntax::dump::float(*x),
        Value::Str(text) => format!("{text:?}"),
        Value::Bytes(data) => {
            let numbers: Vec<String> = data.iter().map(u8::to_string).collect();
            format!("bytes([{}])", numbers.join(", "))
        }
        Value::List(items) => format!("[{}]", join(&mut items.iter())?),
        Value::Tuple(items) if items.len() == 1 => format!("({},)", join(&mut items.iter())?),
        Value::Tuple(items) => format!("({})", join(&mut items.iter())?),
        Value::Set(items) if items.is_empty() => "set()".to_owned(),
        Value::Set(items) => format!("{{{}}}", join(&mut items.keys())?),
        Value::Dict(pairs) => {
            let shown: Option<Vec<String>> = pairs
                .pairs
                .iter()
                .map(|(key, value)| Some(format!("{}: {}", repr(key)?, repr(value)?)))
                .collect();
            format!("{{{}}}", shown?.join(", "))
        }
        Value::Ok(inner) => format!("Ok({})", repr(inner)?),
        Value::Err(inner) => format!("Err({})", repr(inner)?),
        Value::Member(_, inner) => return repr(inner),
        Value::Unit | Value::Instance(_) | Value::Closure(..) => return None,
    })
}

/// A value in an f-string, with its format spec: an optional `,` for thousands separators, an
/// optional `.N` precision, and an optional `f` or `d`.
pub(super) fn formatted(value: &Value, spec: Option<&str>) -> Option<String> {
    let Some(spec) = spec.filter(|spec| !spec.is_empty()) else {
        return shown(value);
    };
    let (comma, rest) = spec.strip_prefix(',').map_or((false, spec), |rest| (true, rest));
    let (precision, kind) = match rest.strip_prefix('.') {
        Some(rest) => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (Some(digits.parse::<usize>().ok()?), rest.get(digits.len()..).unwrap_or(""))
        }
        None => (None, rest),
    };
    let text = match (value, kind, precision) {
        (Value::Int(n), "" | "d", None) => n.to_string(),
        (Value::Int(n), "" | "f", Some(places)) => format!("{:.places$}", super::builtins::to_float(*n)),
        (Value::Float(x), "" | "f", Some(places)) => format!("{x:.places$}"),
        (Value::Float(x), "" | "f", None) => fennec_syntax::dump::float(*x),
        _ => return None,
    };
    Some(if comma { thousands(&text) } else { text })
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
