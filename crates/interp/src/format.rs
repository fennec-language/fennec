//! Turning values into text: how `stdout.print` shows them, and f-string format specs like
//! `{coins:,}` and `{health:.1f}`.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::interp::Interp;
use crate::place::Place;
use crate::value::{Instance, Value};

/// The text `stdout.print` and `str()` produce.
pub(crate) fn display(value: &Value) -> String {
    match value {
        Value::Str(text) => text.to_string(),
        other => repr(other),
    }
}

/// The text a value shows inside a collection: strings are quoted.
pub(crate) fn repr(value: &Value) -> String {
    repr_with(value, &mut |_| None)
}

/// Like `display`, with `own` giving the text of a value whose type writes its own with
/// `def str`, wherever it sits.
pub(crate) fn display_with(value: &Value, own: &mut dyn FnMut(&Instance) -> Option<String>) -> String {
    match value {
        Value::Str(text) => text.to_string(),
        other => repr_with(other, own),
    }
}

/// Like `repr`, with `own` as in `display_with`.
pub(crate) fn repr_with(value: &Value, own: &mut dyn FnMut(&Instance) -> Option<String>) -> String {
    let mut join = |items: &mut dyn Iterator<Item = &Value>| {
        items.map(|item| repr_with(item, own)).collect::<Vec<_>>().join(", ")
    };
    match value {
        Value::Unit | Value::Lent(_) => String::new(),
        Value::Shared(cell) => repr_with(&cell.borrow(), own),
        Value::Generator(generator) => format!("<{}>", generator.label()),
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
        Value::Buffer(buffer) => format!("Buffer([{}])", join(&mut buffer.items.iter())),
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
        Value::Type(name) => format!("<type {name}>"),
        Value::Function(function) => format!("<function {}>", function.def.name),
        Value::Builtin(name) => format!("<builtin {name}>"),
        Value::Lambda(_) => "<function>".to_owned(),
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

impl Interp<'_> {
    /// The text printing gives a value, through the `def str` of any type that has one,
    /// wherever it sits: the value itself, or inside a list, a dict or another value.
    pub(crate) fn shown(&mut self, value: &Value, span: Span) -> Result<String, Diagnostic> {
        let mut failure = None;
        let text = display_with(value, &mut |instance| match self.own_str(instance, span) {
            Ok(text) => text,
            Err(problem) => {
                failure.get_or_insert(problem);
                None
            }
        });
        failure.map_or(Ok(text), Err)
    }

    /// Like `shown`, in an f-string's format spec. Only numbers take a spec.
    pub(crate) fn shown_spec(&mut self, value: &Value, spec: &str, span: Span) -> Result<String, Diagnostic> {
        if spec.is_empty() {
            return self.shown(value, span);
        }
        with_spec(value, spec).map_err(|message| Diagnostic::new(message, span))
    }

    /// What a value's own `def str` writes, if its type has one.
    fn own_str(&mut self, instance: &Instance, span: Span) -> Result<Option<String>, Diagnostic> {
        const OUT: &str = "\0out";
        let Some(method) =
            self.type_info(&instance.type_name).and_then(|info| info.methods.get("str").cloned())
        else {
            return Ok(None);
        };
        let writer = self.make_value("StrWriter", Vec::new(), span)?;
        self.frame.push();
        self.frame.declare(OUT, Value::Unit, true);
        let place = Place { root: OUT.to_owned(), steps: Vec::new(), span };
        let written = CallArg { name: None, value: writer, mut_place: Some(place), literal: false, span };
        let args = vec![CallArg::plain(Value::Instance(Rc::new(instance.clone())), span), written];
        let ran = self.call_user(&method, args, span);
        let writer =
            self.frame.get_mut(OUT).map(|binding| std::mem::replace(&mut binding.value, Value::Unit));
        self.frame.pop();
        ran?;
        let parts = match &writer {
            Some(Value::Instance(writer)) => writer.field("_parts").cloned(),
            _ => None,
        };
        Ok(Some(match parts {
            Some(Value::List(parts)) => parts.iter().map(display).collect(),
            _ => String::new(),
        }))
    }
}
