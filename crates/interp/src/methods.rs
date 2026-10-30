//! Methods on built-in values: `stdout.print`, string methods, and list methods. Methods
//! that change a list need it held in a `mut` name.

use std::cmp::Ordering;
use std::rc::Rc;

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Span};

use crate::call::CallArg;
use crate::interp::Interp;
use crate::value::Value;
use crate::{format, ops};

const CHANGES_LIST: [&str; 8] = [
    "append", "clear", "extend", "insert", "pop", "remove", "reverse", "sort",
];

impl Interp<'_> {
    pub(crate) fn method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let receiver = self.eval(object)?;
        let args = self.eval_args(args)?;
        if matches!(receiver, Value::List(_)) && CHANGES_LIST.contains(&method) {
            let what =
                format!("`{method}` changes the list, so call it on a mut name, like `items.{method}(...)`");
            self.check_mut_place(object, &what)?;
            drop(receiver);
            let ExprKind::Name(name) = &object.kind else {
                return Err(Diagnostic::new(what, object.span));
            };
            return self.change_list(name, method, args, span);
        }
        match receiver {
            Value::Stdout => self.stdout_method(method, args, span),
            Value::Str(text) => str_method(&text, method, &plain_args(args, method, span)?, span),
            other => Err(no_method(&other, method, span)),
        }
    }

    fn stdout_method(&mut self, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let ending = match method {
            "print" => "\n",
            "write" => "",
            _ => return Err(no_method(&Value::Stdout, method, span)),
        };
        let text = plain_args(args, method, span)?
            .iter()
            .map(format::display)
            .collect::<Vec<_>>()
            .join(" ");
        write!(self.out, "{text}{ending}")
            .map_err(|err| Diagnostic::new(format!("couldn't write output: {err}"), span))?;
        Ok(Value::Unit)
    }

    fn change_list(
        &mut self,
        name: &str,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let Some(binding) = self.frame.get_mut(name) else {
            return Err(Diagnostic::new(format!("`{name}` doesn't exist"), span));
        };
        let Value::List(mut items) = std::mem::replace(&mut binding.value, Value::Unit) else {
            return Err(Diagnostic::new(format!("`{name}` isn't a list"), span));
        };
        let result = self.apply_list_change(Rc::make_mut(&mut items), method, args, span);
        if let Some(binding) = self.frame.get_mut(name) {
            binding.value = Value::List(items);
        }
        result
    }

    fn apply_list_change(
        &mut self,
        items: &mut Vec<Value>,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let (args, key) = split_key(args, method)?;
        if key.is_some() && method != "sort" {
            return Err(Diagnostic::new(format!("`{method}` doesn't take `key`"), span));
        }
        match (method, args.as_slice()) {
            ("append", [value]) => items.push(value.clone()),
            ("pop", []) => {
                return items
                    .pop()
                    .ok_or_else(|| Diagnostic::new("can't pop from an empty list", span));
            }
            ("pop", [Value::Int(at)]) => return Ok(items.remove(ops::position(*at, items.len(), span)?)),
            ("insert", [Value::Int(at), value]) => {
                let at = usize::try_from(*at)
                    .ok()
                    .filter(|&at| at <= items.len())
                    .ok_or_else(|| {
                        Diagnostic::new(
                            format!("can't insert at {at} in a list of length {}", items.len()),
                            span,
                        )
                    })?;
                items.insert(at, value.clone());
            }
            ("extend", [Value::List(more)]) => items.extend(more.iter().cloned()),
            ("clear", []) => items.clear(),
            ("reverse", []) => items.reverse(),
            ("remove", [value]) => {
                let at = items.iter().position(|item| item == value).ok_or_else(|| {
                    Diagnostic::new(format!("{} isn't in the list", format::repr(value)), span)
                })?;
                items.remove(at);
            }
            ("sort", []) => self.sort_values(items, key.as_ref(), span)?,
            _ => return Err(bad_args(method, span)),
        }
        Ok(Value::Unit)
    }

    /// Sorts in place, by `key` when given. Values that can't be compared are an error.
    pub(crate) fn sort_values(
        &mut self,
        items: &mut Vec<Value>,
        key: Option<&Value>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let keys = match key {
            Some(key) => items
                .iter()
                .map(|item| self.call_value(key, vec![CallArg::plain(item.clone(), span)], span))
                .collect::<Result<Vec<_>, _>>()?,
            None => items.clone(),
        };
        let mut pairs: Vec<(Value, Value)> = keys.into_iter().zip(items.drain(..)).collect();
        let mut failure = None;
        pairs.sort_by(|(a, _), (b, _)| {
            ops::compare(a, b, span).unwrap_or_else(|err| {
                failure.get_or_insert(err);
                Ordering::Equal
            })
        });
        items.extend(pairs.into_iter().map(|(_, item)| item));
        failure.map_or(Ok(()), Err)
    }
}

/// Splits off a `key=` argument; any other named or `mut` argument is an error.
pub(crate) fn split_key(
    args: Vec<CallArg>,
    function: &str,
) -> Result<(Vec<Value>, Option<Value>), Diagnostic> {
    let mut values = Vec::with_capacity(args.len());
    let mut key = None;
    for arg in args {
        match arg.name.as_deref() {
            Some("key") => key = Some(arg.value),
            Some(other) => {
                return Err(Diagnostic::new(
                    format!("`{function}` has no parameter called `{other}`"),
                    arg.span,
                ));
            }
            None if arg.mut_place.is_some() => {
                return Err(Diagnostic::new(
                    format!("`{function}` doesn't change its arguments, so don't pass `mut`"),
                    arg.span,
                ));
            }
            None => values.push(arg.value),
        }
    }
    Ok((values, key))
}

fn plain_args(args: Vec<CallArg>, method: &str, span: Span) -> Result<Vec<Value>, Diagnostic> {
    let (values, key) = split_key(args, method)?;
    match key {
        Some(_) => Err(Diagnostic::new(format!("`{method}` doesn't take `key`"), span)),
        None => Ok(values),
    }
}

fn str_method(text: &str, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let words = |parts: Vec<&str>| Value::list(parts.into_iter().map(Value::str).collect());
    Ok(match (method, args) {
        ("upper", []) => Value::str(&text.to_uppercase()),
        ("lower", []) => Value::str(&text.to_lowercase()),
        ("strip", []) => Value::str(text.trim()),
        ("lstrip", []) => Value::str(text.trim_start()),
        ("rstrip", []) => Value::str(text.trim_end()),
        ("split", []) => words(text.split_whitespace().collect()),
        ("split", [Value::Str(sep)]) if !sep.is_empty() => words(text.split(sep.as_ref()).collect()),
        ("startswith", [Value::Str(prefix)]) => Value::Bool(text.starts_with(prefix.as_ref())),
        ("endswith", [Value::Str(suffix)]) => Value::Bool(text.ends_with(suffix.as_ref())),
        ("replace", [Value::Str(from), Value::Str(to)]) => Value::str(&text.replace(from.as_ref(), to)),
        ("join", [Value::List(items)]) => {
            let parts = items
                .iter()
                .map(|item| match item {
                    Value::Str(part) => Ok(part.as_ref()),
                    other => Err(Diagnostic::new(
                        format!("`join` needs a list of str, but it has a {}", other.type_name()),
                        span,
                    )
                    .with_help("convert the items first: [str(x) for x in items]")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Value::str(&parts.join(text))
        }
        _ if [
            "upper",
            "lower",
            "strip",
            "lstrip",
            "rstrip",
            "split",
            "startswith",
            "endswith",
            "replace",
            "join",
        ]
        .contains(&method) =>
        {
            return Err(bad_args(method, span));
        }
        _ => return Err(no_method(&Value::str(text), method, span)),
    })
}

fn bad_args(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}

fn no_method(value: &Value, method: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("a {} has no method `{method}`", value.type_name()), span);
    match method {
        "len" | "length" | "size" | "count" => diagnostic.with_help("use `len(x)`"),
        "push" | "add" => diagnostic.with_help("use `append`"),
        "trim" => diagnostic.with_help("use `strip()`"),
        "toUpperCase" | "to_uppercase" => diagnostic.with_help("use `upper()`"),
        "toLowerCase" | "to_lowercase" => diagnostic.with_help("use `lower()`"),
        "log" | "println" => diagnostic.with_help("use `stdout.print(...)`"),
        _ => diagnostic,
    }
}
