//! Method calls: user-defined methods, named constructors, and methods on built-in values.
//! Methods that change a built-in value need it held in a `mut` place.

use std::cmp::Ordering;
use std::rc::Rc;

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::access::might_be_none;
use crate::call::CallArg;
use crate::eval::unhandled_result;
use crate::interp::Interp;
use crate::ops;
use crate::value::{Callable, Value};

impl Interp<'_> {
    /// A capability declared in std, like `Fs` in `fs.fen`, has methods written in Fennec as well
    /// as its builtin ones, as `Connection` does. A narrowed one has them too, and its builtin
    /// ones are still narrowed when they run. Nothing imports the module a capability given to
    /// `main` is declared in, so it's loaded here.
    fn capability_fennec_method(
        &mut self,
        receiver: &Value,
        method: &str,
    ) -> Result<Option<Rc<Callable>>, Diagnostic> {
        let name = receiver.type_name();
        self.load_std_providing(&name)?;
        Ok(self.type_info(&name).and_then(|info| info.methods.get(method).cloned()))
    }

    /// `response.json[T]()`: the body read as the `T` the call names, when it's that call.
    fn response_json(
        &mut self,
        wanted: Option<&fennec_syntax::TypeExpr>,
        method: &str,
        receiver: &Value,
        args: &[Arg],
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        let (Some(ty), "json", Value::Instance(response)) = (wanted, method, receiver) else { return None };
        if response.type_name.as_ref() != "Response" || !args.is_empty() {
            return None;
        }
        let body = match response.field("body") {
            Some(Value::Bytes(body)) => String::from_utf8_lossy(body).into_owned(),
            _ => String::new(),
        };
        Some(self.typed_loads("json.loads", ty, vec![CallArg::plain(Value::str(&body), span)], span))
    }

    pub(crate) fn method_call(
        &mut self,
        object: &Expr,
        method: &str,
        optional: bool,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let wanted = self.json_type.take();
        let receiver = self.eval(object)?;
        if let Some(read) = self.response_json(wanted.as_ref(), method, &receiver, args, span) {
            return read;
        }
        match &receiver {
            Value::None if optional => return Ok(Value::None),
            Value::None if self.built_in_method(&Value::None, method).is_none() => {
                return Err(might_be_none(object.span));
            }
            Value::Type(type_name) => return self.static_call(type_name, method, args, span),
            Value::Builtin("str") if method == "from_code" => {
                let args = crate::collections::plain(self.eval_args(args)?, method, span)?;
                return self.char_from_code(&args, span);
            }
            Value::Builtin("bytes") if method == "fromhex" => {
                let args = crate::collections::plain(self.eval_args(args)?, method, span)?;
                return self.bytes_from_hex(&args, span);
            }
            Value::Ok(_) | Value::Err(_) if method == "expect" => {
                let args = self.eval_args(args)?;
                return expect(receiver, &args, span);
            }
            Value::Ok(_) | Value::Err(_) => return Err(unhandled_result(object.span)),
            Value::Shared(cell) => {
                let type_name = match &*cell.borrow() {
                    Value::Instance(instance) => Rc::clone(&instance.type_name),
                    _ => return Err(no_method(&receiver, method, span)),
                };
                let function = self.type_info(&type_name).and_then(|info| info.methods.get(method).cloned());
                let function = function.ok_or_else(|| no_method(&receiver, method, span))?;
                self.check_lock(cell, span)?;
                self.locks.push((Rc::clone(cell), self.depth + 1));
                let called = self.call_method(object, &function, args, span);
                self.locks.pop();
                return called;
            }
            Value::Instance(instance) => {
                let function =
                    self.type_info(&instance.type_name).and_then(|info| info.methods.get(method).cloned());
                let noted = instance.is_error && matches!(method, "notes" | "add_note");
                return match function {
                    Some(function) => {
                        drop(receiver);
                        self.call_method(object, &function, args, span)
                    }
                    None if noted => {
                        drop(receiver);
                        self.note_method(object, method, args, span)
                    }
                    None => self.std_instance_method(&receiver, method, args, span),
                };
            }
            Value::Capability(_) | Value::Limited(_) => {
                if let Some(function) = self.capability_fennec_method(&receiver, method)? {
                    drop(receiver);
                    return self.call_method(object, &function, args, span);
                }
            }
            _ => {}
        }
        if let Some(function) = self.built_in_method(&receiver, method) {
            drop(receiver);
            return self.call_method(object, &function, args, span);
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

    /// A method std writes in the engines for an instance of one of its types, like `Regex`'s.
    fn std_instance_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if let Value::Instance(instance) = receiver
            && let Some(call) = crate::stdlib::methods_of(&instance.type_name)
        {
            let args = self.eval_args(args)?;
            return call(self, instance, method, args, span);
        }
        Err(no_method(receiver, method, span))
    }

    /// `error.notes()`, the notes added to an error, oldest first, and `error.add_note(text)`,
    /// which adds one to the error a `mut` name holds. Every error type has both, unless it
    /// writes its own.
    fn note_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if method == "notes" {
            let error = self.eval(object)?;
            crate::collections::plain(self.eval_args(args)?, method, span)?;
            let notes = match &error {
                Value::Instance(instance) => instance.notes.as_deref().cloned().unwrap_or_default(),
                _ => Vec::new(),
            };
            return Ok(Value::list(notes.into_iter().map(Value::Str).collect()));
        }
        let what =
            "`add_note` changes it, so call it on something held in a mut name, like `error.add_note(...)`";
        let place = self.mut_place(object, what)?;
        let args = crate::collections::plain(self.eval_args(args)?, method, span)?;
        let [Value::Str(note)] = args.as_slice() else {
            return Err(Diagnostic::new("`add_note` takes the note's text, a `str`", span));
        };
        let mut error = self.take(&place)?;
        if let Value::Instance(instance) = &mut error {
            Rc::make_mut(Rc::make_mut(instance).notes.get_or_insert_default()).push(Rc::clone(note));
        }
        self.put(&place, error)?;
        Ok(Value::Unit)
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
        if info.as_ref().is_some_and(|info| info.def.cases.iter().any(|case| case.name == method)) {
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
                crate::format::failure(&error)
            ),
            span,
        )),
        other => Ok(other),
    }
}

/// Whether calling `method` on `value` changes it.
fn changes_in_place(value: &Value, method: &str) -> bool {
    let methods: &[&str] = match value {
        Value::List(_) => &["append", "clear", "insert", "pop", "remove", "remove_where", "sort"],
        Value::Buffer(_) => &["push", "pop", "reserve", "truncate", "extend", "insert", "remove"],
        Value::Dict(_) => &["clear", "pop"],
        Value::Set(_) => &["add", "clear", "discard", "remove"],
        Value::Table(_) => &["add", "clear", "modify", "remove", "remove_where"],
        _ => &[],
    };
    methods.contains(&method)
}

pub(crate) fn no_method(value: &Value, method: &str, span: Span) -> Diagnostic {
    let name = value.type_name();
    let article =
        if name.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
    let diagnostic = Diagnostic::new(format!("{article} {name} has no method `{method}`"), span);
    match method {
        "length" | "size" | "count" => diagnostic.with_help("use `.len()`"),
        "push" | "add" => diagnostic.with_help("use `append`"),
        "trim" => diagnostic.with_help("use `strip()`"),
        "toUpperCase" | "to_uppercase" => diagnostic.with_help("use `upper()`"),
        "toLowerCase" | "to_lowercase" => diagnostic.with_help("use `lower()`"),
        "log" | "println" => diagnostic.with_help("use `stdout.print(...)`"),
        "copy" | "clone" => diagnostic.with_help("values are copied automatically when assigned: `b = a`"),
        _ => diagnostic,
    }
}
