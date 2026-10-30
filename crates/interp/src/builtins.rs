//! Python's builtins that Fennec keeps: `len`, `range`, `sum`, `min`, `max`, `sorted` and the
//! rest. There's no `print`; output goes through `stdout`.

use std::cmp::Ordering;

use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::interp::Interp;
use crate::methods::split_key;
use crate::value::Value;
use crate::{format, ops};

const NAMES: [&str; 14] = [
    "abs", "all", "any", "float", "int", "len", "max", "min", "range", "reversed", "round", "sorted", "str",
    "sum",
];

/// The longest list `range` will build, until ranges become lazy.
const MAX_RANGE: usize = 10_000_000;

/// The builtin with this name, if there is one.
pub(crate) fn named(name: &str) -> Option<&'static str> {
    NAMES.iter().copied().find(|builtin| *builtin == name)
}

impl Interp<'_> {
    pub(crate) fn builtin(
        &mut self,
        name: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let (values, key) = split_key(args, name)?;
        if key.is_some() && !matches!(name, "min" | "max" | "sorted") {
            return Err(Diagnostic::new(format!("`{name}` doesn't take `key`"), span));
        }
        match (name, values.as_slice()) {
            ("len", [Value::List(items)]) => int_len(items.len(), span),
            ("len", [Value::Str(text)]) => int_len(text.chars().count(), span),
            ("range", _) => range(&values, span),
            ("sum", [Value::List(items)]) => sum(items, span),
            ("min" | "max", _) => self.extreme(name, values, key.as_ref(), span),
            ("sorted", [Value::List(items)]) => {
                let mut items = items.as_ref().clone();
                self.sort_values(&mut items, key.as_ref(), span)?;
                Ok(Value::list(items))
            }
            ("reversed", [Value::List(items)]) => Ok(Value::list(items.iter().rev().cloned().collect())),
            ("reversed", [Value::Str(text)]) => Ok(Value::str(&text.chars().rev().collect::<String>())),
            ("abs", [Value::Int(n)]) => n
                .checked_abs()
                .map(Value::Int)
                .ok_or_else(|| Diagnostic::new("this overflows an int", span)),
            ("abs", [Value::Float(x)]) => Ok(Value::Float(x.abs())),
            ("str", [value]) => Ok(Value::str(&format::display(value))),
            ("int", [value]) => to_int(value, span),
            ("float", [value]) => to_float(value, span),
            ("round", [Value::Float(x)]) => to_int(&Value::Float(x.round_ties_even()), span),
            ("round", [Value::Float(x), Value::Int(places)]) => {
                let scale = 10_f64.powi(i32::try_from(*places).unwrap_or(0));
                Ok(Value::Float((x * scale).round_ties_even() / scale))
            }
            ("any" | "all", [Value::List(items)]) => all_or_any(name, items, span),
            _ => Err(Diagnostic::new(
                format!("`{name}` was called with the wrong arguments"),
                span,
            )),
        }
    }

    fn extreme(
        &mut self,
        name: &str,
        values: Vec<Value>,
        key: Option<&Value>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let items = match values.as_slice() {
            [Value::List(items)] => items.as_ref().clone(),
            [_, _, ..] => values,
            _ => {
                return Err(Diagnostic::new(
                    format!("`{name}` needs a list, or two or more values"),
                    span,
                ));
            }
        };
        let wanted = if name == "max" {
            Ordering::Greater
        } else {
            Ordering::Less
        };
        let mut best: Option<(Value, Value)> = None;
        for item in items {
            let score = match key {
                Some(key) => self.call_value(key, vec![CallArg::plain(item.clone(), span)], span)?,
                None => item.clone(),
            };
            let better = match &best {
                Some((best_score, _)) => ops::compare(&score, best_score, span)? == wanted,
                None => true,
            };
            if better {
                best = Some((score, item));
            }
        }
        best.map(|(_, item)| item)
            .ok_or_else(|| Diagnostic::new(format!("`{name}` of an empty list"), span))
    }
}

fn int_len(len: usize, span: Span) -> Result<Value, Diagnostic> {
    i64::try_from(len)
        .map(Value::Int)
        .map_err(|_| Diagnostic::new("this is too long to measure", span))
}

fn range(values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let (first, end, stride) = match values {
        [Value::Int(end)] => (0, *end, 1),
        [Value::Int(first), Value::Int(end)] => (*first, *end, 1),
        [Value::Int(first), Value::Int(end), Value::Int(stride)] => (*first, *end, *stride),
        _ => return Err(Diagnostic::new("`range` takes one to three ints", span)),
    };
    if stride == 0 {
        return Err(Diagnostic::new("`range` can't step by 0", span));
    }
    let mut items = Vec::new();
    let mut at = first;
    while (stride > 0 && at < end) || (stride < 0 && at > end) {
        if items.len() >= MAX_RANGE {
            return Err(Diagnostic::new(
                format!("ranges are limited to {MAX_RANGE} items for now"),
                span,
            ));
        }
        items.push(Value::Int(at));
        at = at.saturating_add(stride);
    }
    Ok(Value::list(items))
}

fn sum(items: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut total = match items.first() {
        Some(Value::Float(_)) => Value::Float(0.0),
        _ => Value::Int(0),
    };
    for item in items {
        total = ops::binary(fennec_syntax::BinOp::Add, total, item.clone(), span)?;
    }
    Ok(total)
}

fn to_int(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    match value {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(x) if x.is_finite() && x.abs() < 9.2e18 => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "int() truncates toward zero, as in Python"
            )]
            let n = x.trunc() as i64;
            Ok(Value::Int(n))
        }
        Value::Str(text) => text
            .trim()
            .parse()
            .map(Value::Int)
            .map_err(|_| Diagnostic::new(format!("can't turn {text:?} into an int"), span)),
        other => Err(Diagnostic::new(
            format!("can't turn a {} into an int", other.type_name()),
            span,
        )),
    }
}

fn to_float(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    match value {
        Value::Int(n) => Ok(Value::Float(ops::to_float(*n))),
        Value::Float(x) => Ok(Value::Float(*x)),
        Value::Str(text) => text
            .trim()
            .parse()
            .map(Value::Float)
            .map_err(|_| Diagnostic::new(format!("can't turn {text:?} into a float"), span)),
        other => Err(Diagnostic::new(
            format!("can't turn a {} into a float", other.type_name()),
            span,
        )),
    }
}

fn all_or_any(name: &str, items: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut bools = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::Bool(b) => bools.push(*b),
            other => {
                return Err(Diagnostic::new(
                    format!(
                        "`{name}` needs a list of bools, but it has a {}",
                        other.type_name()
                    ),
                    span,
                )
                .with_help("Fennec has no truthiness: compare explicitly, like `any(x > 0 for x in xs)`"));
            }
        }
    }
    Ok(Value::Bool(if name == "all" {
        bools.iter().all(|&b| b)
    } else {
        bools.iter().any(|&b| b)
    }))
}
