//! Python's builtins that Fennec keeps, plus `Ok`, `Err`, `replace` and `ignore`. There's
//! no `print`; output goes through `stdout`. Standard library functions are in `stdlib`.

use std::cmp::Ordering;
use std::rc::Rc;

use fennec_syntax::{BinOp, Diagnostic, Span};

use crate::call::CallArg;
use crate::collections::split_key;
use crate::interp::Interp;
use crate::value::Value;
use crate::{format, ops, stdlib};

/// The builtins. Standard library functions, like `json.loads`, are in `stdlib`.
const NAMES: [&str; 22] = [
    "Err",
    "Ok",
    "abs",
    "all",
    "any",
    "bytes",
    "enumerate",
    "float",
    "ignore",
    "int",
    "list",
    "max",
    "min",
    "parallel",
    "range",
    "replace",
    "reversed",
    "round",
    "set",
    "sorted",
    "str",
    "sum",
];

/// Built-in types that can be called or indexed, like `IdMap[Enemy]()`.
const TYPES: [&str; 3] = ["Id", "IdMap", "SoaList"];

/// The longest list `range` will build, until ranges become lazy.
const MAX_RANGE: usize = 10_000_000;

/// 2^63: a float converts to an int when it is at least -2^63 and below 2^63.
const INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// The builtin or standard library function of this name, like `len` or `json.loads`.
pub(crate) fn named(name: &str) -> Option<&'static str> {
    NAMES.iter().copied().chain(["zip"]).chain(stdlib::functions()).find(|builtin| *builtin == name)
}

/// The modules with a function of this name, like `json` and `toml` for `loads`.
pub(crate) fn modules_with(name: &str) -> Vec<&'static str> {
    stdlib::functions()
        .filter_map(|builtin| builtin.split_once('.').filter(|(_, function)| *function == name))
        .map(|(module, _)| module)
        .collect()
}

pub(crate) fn is_type(name: &str) -> bool {
    TYPES.contains(&name)
}

pub(crate) fn constant(name: &str) -> Option<Value> {
    stdlib::constant(name)
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
        if let Some(module) = stdlib::module_of(name) {
            return (module.call)(self, name, args, span);
        }
        let (values, key) = split_key(args, name)?;
        if key.is_some() && !matches!(name, "min" | "max" | "sorted") {
            return Err(Diagnostic::new(format!("`{name}` doesn't take `key`"), span));
        }
        match (name, values.as_slice()) {
            ("Ok", [value]) => Ok(Value::Ok(Rc::new(value.clone()))),
            ("bytes", []) => Ok(Value::Bytes(Rc::from([]))),
            ("bytes", [items]) => self.bytes(items.clone(), span),
            ("Ok", []) => Ok(Value::Ok(Rc::new(Value::Unit))),
            ("Err", [value]) => Ok(Value::Err(Rc::new(value.clone()))),
            ("ignore", [_, Value::Str(reason)]) if !reason.trim().is_empty() => Ok(Value::Unit),
            ("ignore", _) => Err(Diagnostic::new("`ignore` needs a reason the result doesn't matter", span)
                .with_help("like `ignore(save(fs, game), \"autosave is best effort\")`")),
            ("range", _) => range(&values, span),
            ("str", [value]) => Ok(Value::str(&format::display(value))),
            ("int" | "float", [Value::Str(text)]) => self.parse_number(name, text, span),
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
            _ => self.collection_builtin(name, values, key.as_ref(), span),
        }
    }

    /// `int(text)` and `float(text)`. Text that isn't a number comes from outside the
    /// program, so it fails with an `InvalidValue` rather than stopping it.
    fn parse_number(&mut self, name: &str, text: &str, span: Span) -> Result<Value, Diagnostic> {
        let parsed = if name == "int" {
            text.trim().parse().map(Value::Int).ok()
        } else {
            text.trim().parse().map(Value::Float).ok()
        };
        if let Some(number) = parsed {
            return Ok(Value::Ok(Rc::new(number)));
        }
        let what = if name == "int" { "an int" } else { "a float" };
        let message = format!("{text:?} isn't {what}");
        let error = self.make_value("InvalidValue", vec![("message", Value::str(&message))], span)?;
        Ok(Value::Err(Rc::new(error)))
    }

    /// `bytes(items)`: each item has to be an int from 0 to 255.
    fn bytes(&mut self, items: Value, span: Span) -> Result<Value, Diagnostic> {
        let mut data = Vec::new();
        for item in self.iterate(items, span)? {
            let byte = match item {
                Value::Int(n) => u8::try_from(n).ok(),
                _ => None,
            };
            let byte = byte.ok_or_else(|| {
                Diagnostic::new(format!("a byte is an int from 0 to 255, not {}", format::repr(&item)), span)
            })?;
            data.push(byte);
        }
        Ok(Value::Bytes(Rc::from(data)))
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

pub(crate) fn length(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    let len = match value {
        Value::List(items) | Value::Tuple(items) | Value::Set(items) => items.len(),
        Value::Dict(pairs) => pairs.len(),
        Value::Str(text) => text.chars().count(),
        Value::Bytes(data) => data.len(),
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

pub(crate) fn to_int(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    match value {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(x) if (-INT_LIMIT..INT_LIMIT).contains(x) => {
            #[expect(clippy::cast_possible_truncation, reason = "int() truncates toward zero, as in Python")]
            let n = x.trunc() as i64;
            Ok(Value::Int(n))
        }
        other => Err(Diagnostic::new(format!("can't turn a {} into an int", other.type_name()), span)),
    }
}

fn to_float(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    match value {
        Value::Int(n) => Ok(Value::Float(ops::to_float(*n))),
        Value::Float(x) => Ok(Value::Float(*x)),
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
