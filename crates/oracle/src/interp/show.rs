//! How values print, compare and order, as the compiled program does: a type's own `def str`,
//! `def ==` and `def <` are called where it has them.

use std::cmp::Ordering;

use super::calls::{ArgVal, Receiver};
use super::{Binding, Interp, Place, Run, unsupported};
use crate::text;
use crate::value::{Func, Instance, Value};

impl Interp {
    /// What `stdout.print` and `str()` show: a string as it is, and anything else as it shows
    /// inside a collection.
    pub(crate) fn display(&mut self, value: &Value) -> Run<String> {
        match value {
            Value::Str(text) => Ok(text.clone()),
            other => self.repr(other),
        }
    }

    /// A value as it shows inside a collection, strings in quotes.
    pub(crate) fn repr(&mut self, value: &Value) -> Run<String> {
        Ok(match value {
            Value::Unit => String::new(),
            Value::None => "None".to_owned(),
            Value::Bool(true) => "True".to_owned(),
            Value::Bool(false) => "False".to_owned(),
            Value::Int(n) => n.to_string(),
            Value::Sized(_, n) => n.to_string(),
            Value::Float(x) => text::float(*x),
            Value::Str(text) => text::quoted(text),
            Value::List(items) => format!("[{}]", self.joined(items)?),
            Value::Tuple(items) if items.len() == 1 => format!("({},)", self.joined(items)?),
            Value::Tuple(items) => format!("({})", self.joined(items)?),
            Value::Set(items) if items.is_empty() => "set()".to_owned(),
            Value::Set(items) => format!("{{{}}}", self.joined(items)?),
            Value::Dict(pairs) => {
                let mut shown = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    shown.push(format!("{}: {}", self.repr(key)?, self.repr(value)?));
                }
                format!("{{{}}}", shown.join(", "))
            }
            Value::Instance(instance) => self.instance_repr(instance)?,
            Value::Stream(stream) => match &stream.shown {
                Some(shown) => format!("<{shown}>"),
                None => return unsupported("printing a generator whose type isn't known"),
            },
            Value::Table(table) => {
                let items: Vec<Value> = table.live().map(|(_, item)| item.clone()).collect();
                format!("Table([{}])", self.joined(&items)?)
            }
            Value::Id(id) => format!("Id({})", id.slot),
            Value::Func(_) => return unsupported("printing a function"),
            Value::Cap(_) => return unsupported("printing a capability"),
            Value::Unknown(why) => return unsupported(why.clone()),
        })
    }

    fn joined(&mut self, items: &[Value]) -> Run<String> {
        let mut shown = Vec::with_capacity(items.len());
        for item in items {
            shown.push(self.repr(item)?);
        }
        Ok(shown.join(", "))
    }

    fn instance_repr(&mut self, instance: &Instance) -> Run<String> {
        if let Some(def) = self.program.method(&instance.ty, "str") {
            return self.own_str(def, instance);
        }
        // A generic type's value prints with no type arguments, as it's written, `Holder(item=3)`,
        // whatever `T` is: decided 2026-11-14, after the compiled program was found printing
        // `Holder[int](item=3)` but `Holder(item="x")`.
        let printed_by_std =
            is_core_error(&instance.ty) || matches!(instance.ty.as_str(), "Path" | "Metadata");
        if !self.program.types.contains_key(instance.ty.as_str()) && !printed_by_std {
            return unsupported(format!("printing a {}", instance.ty));
        }
        let name = match &instance.case {
            Some(case) => format!("{}.{case}", instance.ty),
            None => instance.ty.clone(),
        };
        if instance.fields.is_empty() && instance.case.is_some() {
            return Ok(name);
        }
        let mut fields = Vec::with_capacity(instance.fields.len());
        for (field, value) in &instance.fields {
            fields.push(format!("{field}={}", self.repr(value)?));
        }
        Ok(format!("{name}({})", fields.join(", ")))
    }

    /// A type's own `def str(self, ref out: StrWriter)`, called with a writer of its own.
    fn own_str(&mut self, def: &'static fennec_syntax::FnDef, instance: &Instance) -> Run<String> {
        let writer = Value::instance("StrWriter", None, vec![("_parts".to_owned(), Value::List(Vec::new()))]);
        let previous = self.push_frame(vec![("out".to_owned(), Binding::Value(writer))], None)?;
        let place = Place { frame: self.cur, name: "out".to_owned(), path: Vec::new() };
        let receiver = Receiver::Value(Value::Instance(Box::new(instance.clone())));
        let called =
            self.call_def(def, None, Some(receiver), vec![ArgVal::Ref(None, place.clone())], def.span);
        let written = called.and_then(|_| self.read_place(&place, def.span));
        self.pop_frame(previous);
        match written? {
            Value::Instance(writer) => match writer.field("_parts") {
                Some(Value::List(parts)) => {
                    let mut text = String::new();
                    for part in parts {
                        if let Value::Str(part) = part {
                            text.push_str(part);
                        }
                    }
                    Ok(text)
                }
                _ => unsupported("a StrWriter without its parts"),
            },
            _ => unsupported("a StrWriter that isn't one"),
        }
    }

    /// `left == right`, with a type's own `def ==` where it has one, however deep it's held.
    pub(crate) fn equal(&mut self, left: &Value, right: &Value) -> Run<bool> {
        match (left, right) {
            (Value::Instance(a), Value::Instance(b)) => {
                if let Some(def) = self.program.method(&a.ty, "==") {
                    let receiver = Receiver::Value(left.clone());
                    let given = vec![ArgVal::Value(None, right.clone())];
                    return match self.call_def(def, None, Some(receiver), given, def.span)? {
                        Value::Bool(same) => Ok(same),
                        _ => unsupported("a `def ==` that doesn't give a bool"),
                    };
                }
                if a.ty != b.ty || a.case != b.case || a.fields.len() != b.fields.len() {
                    return Ok(false);
                }
                for ((_, x), (_, y)) in a.fields.iter().zip(&b.fields) {
                    if !self.equal(x, y)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for (x, y) in a.iter().zip(b) {
                    if !self.equal(x, y)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Set(a), Value::Set(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for x in a {
                    if !self.contains(right, x)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Dict(a), Value::Dict(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for (key, x) in a {
                    let Some(at) = super::find_key(b, key)? else { return Ok(false) };
                    let Some((_, y)) = b.get(at) else { return Ok(false) };
                    if !self.equal(x, y)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Table(a), Value::Table(b)) => {
                if a.slots.len() != b.slots.len() {
                    return Ok(false);
                }
                for (x, y) in a.slots.iter().zip(&b.slots) {
                    let same = match (&x.item, &y.item) {
                        (Some(x), Some(y)) => self.equal(x, y)?,
                        (None, None) => true,
                        _ => false,
                    };
                    if !same || x.generation != y.generation {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => plain_equal(left, right),
        }
    }

    /// The order of two values, with a type's own `def <` where it has one.
    pub(crate) fn compare(&mut self, left: &Value, right: &Value) -> Run<Ordering> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
            (Value::Sized(_, a), Value::Sized(_, b)) => Ok(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => Ok(float_order(*a, *b)),
            (Value::Str(a), Value::Str(b)) => Ok(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
                for (x, y) in a.iter().zip(b) {
                    let order = self.compare(x, y)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                Ok(a.len().cmp(&b.len()))
            }
            (Value::Instance(a), Value::Instance(_)) => match self.program.method(&a.ty, "<") {
                Some(def) => {
                    if self.less(def, left, right)? {
                        Ok(Ordering::Less)
                    } else if self.less(def, right, left)? {
                        Ok(Ordering::Greater)
                    } else {
                        Ok(Ordering::Equal)
                    }
                }
                None => unsupported(format!("ordering a {}", a.ty)),
            },
            _ => unsupported(format!("ordering a {} and a {}", left.kind(), right.kind())),
        }
    }

    fn less(&mut self, def: &'static fennec_syntax::FnDef, left: &Value, right: &Value) -> Run<bool> {
        let receiver = Receiver::Value(left.clone());
        match self.call_def(def, None, Some(receiver), vec![ArgVal::Value(None, right.clone())], def.span)? {
            Value::Bool(less) => Ok(less),
            _ => unsupported("a `def <` that doesn't give a bool"),
        }
    }
}

/// The errors every program has without an import.
fn is_core_error(name: &str) -> bool {
    matches!(name, "InvalidValue" | "ParseError" | "IoError")
}

/// Floats in Fennec's one order: NaN equals itself and comes after everything else.
pub(crate) fn float_order(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
    }
}

/// Equality with no type's own `def ==`: what a dict's keys and a set's items are told apart by.
pub(crate) fn plain_equal(left: &Value, right: &Value) -> Run<bool> {
    Ok(match (left, right) {
        (Value::Unit, Value::Unit) | (Value::None, Value::None) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Sized(x, a), Value::Sized(y, b)) => x == y && a == b,
        (Value::Float(a), Value::Float(b)) => float_order(*a, *b) == Ordering::Equal,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::List(a), Value::List(b))
        | (Value::Tuple(a), Value::Tuple(b))
        | (Value::Set(a), Value::Set(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (x, y) in a.iter().zip(b) {
                if !plain_equal(x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Dict(a), Value::Dict(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for ((k, x), (l, y)) in a.iter().zip(b) {
                if !plain_equal(k, l)? || !plain_equal(x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Instance(a), Value::Instance(b)) => {
            if a.ty != b.ty || a.case != b.case || a.fields.len() != b.fields.len() {
                return Ok(false);
            }
            for ((_, x), (_, y)) in a.fields.iter().zip(&b.fields) {
                if !plain_equal(x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Id(a), Value::Id(b)) => a == b,
        (Value::Table(a), Value::Table(b)) => {
            if a.slots.len() != b.slots.len() {
                return Ok(false);
            }
            for (x, y) in a.slots.iter().zip(&b.slots) {
                let same = match (&x.item, &y.item) {
                    (Some(x), Some(y)) => plain_equal(x, y)?,
                    (None, None) => true,
                    _ => false,
                };
                if !same || x.generation != y.generation {
                    return Ok(false);
                }
            }
            true
        }
        // A function the program declares is equal only to itself.
        (Value::Func(a), Value::Func(b)) if matches!((&**a, &**b), (Func::Named(_), Func::Named(_))) => {
            matches!((&**a, &**b), (Func::Named(x), Func::Named(y)) if x == y)
        }
        (Value::Func(_) | Value::Stream(_) | Value::Cap(_), _)
        | (_, Value::Func(_) | Value::Stream(_) | Value::Cap(_)) => {
            return unsupported("comparing functions, generators or capabilities");
        }
        (Value::Unknown(why), _) | (_, Value::Unknown(why)) => return unsupported(why.clone()),
        _ => false,
    })
}
