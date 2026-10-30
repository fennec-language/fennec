//! Python's builtins that Fennec keeps, plus `Ok`, `Err`, `replace`, `ignore` and the math
//! functions. There's no `print`; output goes through `stdout`.

use std::cmp::Ordering;
use std::rc::Rc;

use fennec_syntax::{BinOp, Diagnostic, Span};

use crate::call::CallArg;
use crate::collections::split_key;
use crate::interp::Interp;
use crate::value::Value;
use crate::{format, ops};

const NAMES: [&str; 27] = [
    "Err",
    "Ok",
    "abs",
    "all",
    "any",
    "ceil",
    "cos",
    "enumerate",
    "float",
    "floor",
    "ignore",
    "int",
    "len",
    "list",
    "max",
    "min",
    "parallel",
    "range",
    "replace",
    "reversed",
    "round",
    "set",
    "sin",
    "sorted",
    "sqrt",
    "str",
    "sum",
];

/// Built-in types that can be called or indexed, like `IdMap[Enemy]()`.
const TYPES: [&str; 4] = ["Id", "IdMap", "Path", "SoaList"];

/// The longest list `range` will build, until ranges become lazy.
const MAX_RANGE: usize = 10_000_000;

/// 2^63: a float converts to an int when it is at least -2^63 and below 2^63.
const INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

pub(crate) fn named(name: &str) -> Option<&'static str> {
    NAMES.iter().copied().find(|builtin| *builtin == name).or_else(|| (name == "zip").then_some("zip"))
}

pub(crate) fn is_type(name: &str) -> bool {
    TYPES.contains(&name)
}

pub(crate) fn constant(name: &str) -> Option<Value> {
    match name {
        "PI" => Some(Value::Float(std::f64::consts::PI)),
        _ => None,
    }
}

impl Interp<'_> {
    pub(crate) fn builtin(
        &mut self,
        name: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if args.iter().any(|arg| arg.mut_place.is_some()) {
            for arg in args {
                if let Some(place) = &arg.mut_place {
                    self.put(place, arg.value)?;
                }
            }
            return Err(Diagnostic::new(
                format!("`{name}` doesn't change its arguments, so don't pass `mut`"),
                span,
            ));
        }
        if name == "replace" {
            return Interp::replace(args, span);
        }
        let (values, key) = split_key(args, name)?;
        if key.is_some() && !matches!(name, "min" | "max" | "sorted") {
            return Err(Diagnostic::new(format!("`{name}` doesn't take `key`"), span));
        }
        match (name, values.as_slice()) {
            ("Ok", [value]) => Ok(Value::Ok(Rc::new(value.clone()))),
            ("Ok", []) => Ok(Value::Ok(Rc::new(Value::Unit))),
            ("Err", [value]) => Ok(Value::Err(Rc::new(value.clone()))),
            ("ignore", [_, Value::Str(reason)]) if !reason.trim().is_empty() => Ok(Value::Unit),
            ("ignore", _) => Err(Diagnostic::new("`ignore` needs a reason the result doesn't matter", span)
                .with_help("like `ignore(save(fs, game), \"autosave is best effort\")`")),
            ("len", [value]) => length(value, span),
            ("range", _) => range(&values, span),
            ("str", [value]) => Ok(Value::str(&format::display(value))),
            ("int", [value]) => to_int(value, span),
            ("float", [value]) => to_float(value, span),
            ("abs", [Value::Int(n)]) => {
                n.checked_abs().map(Value::Int).ok_or_else(|| Diagnostic::new("this overflows an int", span))
            }
            ("abs", [Value::Float(x)]) => Ok(Value::Float(x.abs())),
            ("round", [Value::Float(x)]) => to_int(&Value::Float(x.round_ties_even()), span),
            ("round", [Value::Float(x), Value::Int(places)]) => {
                let scale = 10_f64.powi(i32::try_from(*places).unwrap_or(0));
                Ok(Value::Float((x * scale).round_ties_even() / scale))
            }
            ("sqrt" | "floor" | "ceil" | "sin" | "cos", [value]) => math(name, value, span),
            _ => self.collection_builtin(name, values, key.as_ref(), span),
        }
    }

    /// Builtins that walk through a collection.
    fn collection_builtin(
        &mut self,
        name: &str,
        values: Vec<Value>,
        key: Option<&Value>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let mut values = values.into_iter();
        let (first, second, rest) = (values.next(), values.next(), values.next());
        match (name, first, second, rest) {
            ("min" | "max", Some(first), Some(second), rest) => {
                let items = [first, second].into_iter().chain(rest).chain(values).collect();
                self.extreme(name, items, key, span)
            }
            ("min" | "max", Some(items), None, None) => {
                let items = self.iterate(items, span)?;
                self.extreme(name, items, key, span)
            }
            ("sum", Some(items), None, None) => {
                let items = self.iterate(items, span)?;
                let mut total = if matches!(items.first(), Some(Value::Float(_))) {
                    Value::Float(0.0)
                } else {
                    Value::Int(0)
                };
                for item in items {
                    total = self.binary(BinOp::Add, total, item, span)?;
                }
                Ok(total)
            }
            ("sorted", Some(items), None, None) => {
                let mut items = self.iterate(items, span)?;
                self.sort_values(&mut items, key, span)?;
                Ok(Value::list(items))
            }
            ("reversed", Some(Value::Str(text)), None, None) => {
                Ok(Value::str(&text.chars().rev().collect::<String>()))
            }
            ("reversed", Some(items), None, None) => {
                Ok(Value::list(self.iterate(items, span)?.into_iter().rev().collect()))
            }
            ("list" | "parallel", Some(items), None, None) => Ok(Value::list(self.iterate(items, span)?)),
            ("set", None, None, None) => Ok(Value::Set(Rc::default())),
            ("set", Some(items), None, None) => {
                let mut unique: Vec<Value> = Vec::new();
                for item in self.iterate(items, span)? {
                    if !unique.contains(&item) {
                        unique.push(item);
                    }
                }
                Ok(Value::Set(Rc::new(unique)))
            }
            ("enumerate", Some(items), None, None) => Ok(Value::list(
                self.iterate(items, span)?
                    .into_iter()
                    .zip(0_i64..)
                    .map(|(item, at)| Value::tuple(vec![Value::Int(at), item]))
                    .collect(),
            )),
            ("zip", Some(a), Some(b), None) => {
                let (a, b) = (self.iterate(a, span)?, self.iterate(b, span)?);
                Ok(Value::list(a.into_iter().zip(b).map(|(a, b)| Value::tuple(vec![a, b])).collect()))
            }
            ("any" | "all", Some(items), None, None) => {
                let items = self.iterate(items, span)?;
                all_or_any(name, &items, span)
            }
            _ => Err(Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span)),
        }
    }

    fn extreme(
        &mut self,
        name: &str,
        items: Vec<Value>,
        key: Option<&Value>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let wanted = if name == "max" { Ordering::Greater } else { Ordering::Less };
        let mut best: Option<(Value, Value)> = None;
        for item in items {
            let score = match key {
                Some(key) => self.call_value(key, vec![CallArg::plain(item.clone(), span)], span)?,
                None => item.clone(),
            };
            let better = match &best {
                Some((best_score, _)) => self.order(&score, best_score, span)? == wanted,
                None => true,
            };
            if better {
                best = Some((score, item));
            }
        }
        best.map(|(_, item)| item).ok_or_else(|| Diagnostic::new(format!("`{name}` of an empty list"), span))
    }
}

fn length(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    let len = match value {
        Value::List(items) | Value::Tuple(items) | Value::Set(items) => items.len(),
        Value::Dict(pairs) => pairs.len(),
        Value::Str(text) => text.chars().count(),
        Value::IdMap(map) => map.entries().count(),
        other => return Err(Diagnostic::new(format!("a {} has no length", other.type_name()), span)),
    };
    i64::try_from(len).map(Value::Int).map_err(|_| Diagnostic::new("this is too long to measure", span))
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
    while if stride.is_positive() { at < end } else { at > end } {
        if items.len() >= MAX_RANGE {
            return Err(Diagnostic::new(format!("ranges are limited to {MAX_RANGE} items for now"), span));
        }
        items.push(Value::Int(at));
        at = at.saturating_add(stride);
    }
    Ok(Value::list(items))
}

fn math(name: &str, value: &Value, span: Span) -> Result<Value, Diagnostic> {
    let x = match value {
        Value::Float(x) => *x,
        Value::Int(n) => ops::to_float(*n),
        other => {
            return Err(Diagnostic::new(
                format!("`{name}` needs a number, not a {}", other.type_name()),
                span,
            ));
        }
    };
    match name {
        "floor" => to_int(&Value::Float(x.floor()), span),
        "ceil" => to_int(&Value::Float(x.ceil()), span),
        "sin" => Ok(Value::Float(x.sin())),
        "cos" => Ok(Value::Float(x.cos())),
        _ if x < 0.0 => Err(Diagnostic::new("can't take the square root of a negative number", span)),
        _ => Ok(Value::Float(x.sqrt())),
    }
}

fn to_int(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    match value {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(x) if (-INT_LIMIT..INT_LIMIT).contains(x) => {
            #[expect(clippy::cast_possible_truncation, reason = "int() truncates toward zero, as in Python")]
            let n = x.trunc() as i64;
            Ok(Value::Int(n))
        }
        Value::Str(text) => text
            .trim()
            .parse()
            .map(Value::Int)
            .map_err(|_| Diagnostic::new(format!("can't turn {text:?} into an int"), span)),
        other => Err(Diagnostic::new(format!("can't turn a {} into an int", other.type_name()), span)),
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
        other => Err(Diagnostic::new(format!("can't turn a {} into a float", other.type_name()), span)),
    }
}

fn all_or_any(name: &str, items: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut bools = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::Bool(b) => bools.push(*b),
            other => {
                return Err(Diagnostic::new(
                    format!("`{name}` needs bools, but got a {}", other.type_name()),
                    span,
                )
                .with_help("Fennec has no truthiness: compare explicitly, like `any(x > 0 for x in xs)`"));
            }
        }
    }
    Ok(Value::Bool(if name == "all" { bools.iter().all(|&b| b) } else { bools.iter().any(|&b| b) }))
}
