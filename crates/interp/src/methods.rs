//! Method calls: user-defined methods, named constructors, and methods on built-in values.
//! Methods that change a built-in value need it held in a `mut` place.

use std::cmp::Ordering;

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::access::might_be_none;
use crate::call::CallArg;
use crate::eval::unhandled_result;
use crate::interp::{Interp, TypeKind};
use crate::ops;
use crate::value::Value;

impl Interp<'_> {
    pub(crate) fn method_call(
        &mut self,
        object: &Expr,
        method: &str,
        optional: bool,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let receiver = self.eval(object)?;
        match &receiver {
            Value::None if optional => return Ok(Value::None),
            Value::None => return Err(might_be_none(object.span)),
            Value::Type(type_name) => return self.static_call(type_name, method, args, span),
            Value::Ok(_) | Value::Err(_) if method == "expect" => {
                let args = self.eval_args(args)?;
                return expect(receiver, &args, span);
            }
            Value::Ok(_) | Value::Err(_) => return Err(unhandled_result(object.span)),
            Value::Instance(instance) => {
                let function =
                    self.type_info(&instance.type_name).and_then(|info| info.methods.get(method).cloned());
                return match function {
                    Some(function) => {
                        drop(receiver);
                        self.call_method(object, &function, args, span)
                    }
                    None if let Some(call) = crate::stdlib::methods_of(&instance.type_name) => {
                        let args = self.eval_args(args)?;
                        call(self, instance, method, args, span)
                    }
                    None => Err(no_method(&receiver, method, span)),
                };
            }
            _ => {}
        }
        if changes_in_place(&receiver, method) {
            let what = format!(
                "`{method}` changes it, so call it on something held in a mut name, like `items.{method}(...)`"
            );
            let place = self.mut_place(object, &what)?;
            drop(receiver);
            let args = self.eval_args(args)?;
            let mut value = self.take(&place)?;
            let result = self.change_in_place(&mut value, method, args, span);
            self.put(&place, value)?;
            return result;
        }
        let args = self.eval_args(args)?;
        self.read_method(&receiver, method, args, span)
    }

    /// `Type.name(...)`: an enum case, or a named constructor.
    fn static_call(
        &mut self,
        type_name: &str,
        method: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let info = self.type_info(type_name);
        let args = self.eval_args(args)?;
        if info.as_ref().is_some_and(|info| {
            info.kind == TypeKind::Enum && info.def.cases.iter().any(|case| case.name == method)
        }) {
            return self.construct_variant(Some(type_name), method, args, span);
        }
        match info.and_then(|info| info.methods.get(method).cloned()) {
            Some(function) if !function.def.takes_self() => self.call_user(&function, args, span),
            Some(_) => Err(Diagnostic::new(format!("`{method}` needs a `{type_name}` to call it on"), span)),
            None => Err(Diagnostic::new(format!("`{type_name}` has nothing called `{method}`"), span)),
        }
    }

    /// Sorts in place, by `key` when given, stably. Types with their own `def <` sort by it.
    pub(crate) fn sort_values(
        &mut self,
        items: &mut Vec<Value>,
        key: Option<&Value>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let mut keyed = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            let key = match key {
                Some(key) => self.call_value(key, vec![CallArg::plain(item.clone(), span)], span)?,
                None => item.clone(),
            };
            keyed.push((key, item));
        }
        let sorted = self.merge_sort(keyed, span)?;
        items.extend(sorted.into_iter().map(|(_, item)| item));
        Ok(())
    }

    fn merge_sort(
        &mut self,
        mut items: Vec<(Value, Value)>,
        span: Span,
    ) -> Result<Vec<(Value, Value)>, Diagnostic> {
        if items.len() <= 1 {
            return Ok(items);
        }
        let right = items.split_off(items.len() / 2);
        let (left, right) = (self.merge_sort(items, span)?, self.merge_sort(right, span)?);
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let (mut left, mut right) = (left.into_iter().peekable(), right.into_iter().peekable());
        while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
            let take_right = self.order(&b.0, &a.0, span)? == Ordering::Less;
            merged.extend(if take_right { right.next() } else { left.next() });
        }
        merged.extend(left);
        merged.extend(right);
        Ok(merged)
    }

    /// Orders two values, using a type's own `def <` when it has one.
    pub(crate) fn order(&mut self, a: &Value, b: &Value, span: Span) -> Result<Ordering, Diagnostic> {
        if let Value::Instance(instance) = a
            && let Some(less) =
                self.type_info(&instance.type_name).and_then(|info| info.methods.get("<").cloned())
        {
            if self.call_with_values(&less, vec![a.clone(), b.clone()], span)? == Value::Bool(true) {
                return Ok(Ordering::Less);
            }
            let greater =
                self.call_with_values(&less, vec![b.clone(), a.clone()], span)? == Value::Bool(true);
            return Ok(if greater { Ordering::Greater } else { Ordering::Equal });
        }
        ops::compare(a, b, span)
    }
}

fn expect(result: Value, args: &[CallArg], span: Span) -> Result<Value, Diagnostic> {
    let reason = match args {
        [CallArg { value: Value::Str(reason), .. }] if !reason.trim().is_empty() => reason.to_string(),
        _ => {
            return Err(Diagnostic::new("`expect` needs a reason this can't fail", span)
                .with_help("write it as a string, like `.expect(\"the file was just written\")`"));
        }
    };
    match result {
        Value::Ok(value) => Ok(value.as_ref().clone()),
        Value::Err(error) => Err(Diagnostic::new(
            format!(
                "this was expected to succeed ({reason}), but it failed: {}",
                crate::format::repr(&error)
            ),
            span,
        )),
        other => Ok(other),
    }
}

/// Whether calling `method` on `value` changes it.
fn changes_in_place(value: &Value, method: &str) -> bool {
    let methods: &[&str] = match value {
        Value::List(_) => {
            &["append", "clear", "extend", "insert", "pop", "remove", "remove_where", "reverse", "sort"]
        }
        Value::Dict(_) => &["clear", "pop"],
        Value::Set(_) => &["add", "clear", "discard", "remove"],
        Value::IdMap(_) => &["clear", "insert", "modify", "remove", "remove_where"],
        _ => &[],
    };
    methods.contains(&method)
}

pub(crate) fn no_method(value: &Value, method: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("a {} has no method `{method}`", value.type_name()), span);
    match method {
        "len" | "length" | "size" | "count" => diagnostic.with_help("use `len(x)`"),
        "push" | "add" => diagnostic.with_help("use `append`"),
        "trim" => diagnostic.with_help("use `strip()`"),
        "toUpperCase" | "to_uppercase" => diagnostic.with_help("use `upper()`"),
        "toLowerCase" | "to_lowercase" => diagnostic.with_help("use `lower()`"),
        "log" | "println" => diagnostic.with_help("use `stdout.print(...)`"),
        "copy" | "clone" => diagnostic.with_help("values are copied automatically when assigned: `b = a`"),
        _ => diagnostic,
    }
}
