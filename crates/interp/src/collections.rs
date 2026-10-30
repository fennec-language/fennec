//! Methods on built-in values: `stdout`, `fs`, strings, lists, dicts, sets and `IdMap`.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::format;
use crate::interp::Interp;
use crate::methods::no_method;
use crate::ops;
use crate::place::Place;
use crate::value::{Capability, Value};

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
            None => values.push(arg.value),
        }
    }
    Ok((values, key))
}

pub(crate) fn plain(args: Vec<CallArg>, method: &str, span: Span) -> Result<Vec<Value>, Diagnostic> {
    match split_key(args, method)? {
        (values, None) => Ok(values),
        (_, Some(_)) => Err(Diagnostic::new(format!("`{method}` doesn't take `key`"), span)),
    }
}

fn wrong_args(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}

impl Interp<'_> {
    /// Methods that only read their receiver.
    pub(crate) fn read_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if args.iter().any(|arg| arg.mut_place.is_some()) {
            return Err(Diagnostic::new(
                format!("`{method}` doesn't change its arguments, so don't pass `mut`"),
                span,
            ));
        }
        let args = plain(args, method, span)?;
        match (receiver, method, args.as_slice()) {
            (Value::Int(n), "abs", []) => {
                return n
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| Diagnostic::new("this overflows an int", span));
            }
            (Value::Float(x), "abs", []) => return Ok(Value::Float(x.abs())),
            _ => {}
        }
        if method == "len" && args.is_empty() {
            // Every collection knows its length, and so do a tuple and the arguments.
            return match receiver {
                Value::Capability(Capability::Args) => {
                    Ok(Value::Int(i64::try_from(self.arguments().len()).unwrap_or(i64::MAX)))
                }
                Value::Packed(packed) => crate::builtins::length(&packed.unpack(), span),
                other => crate::builtins::length(other, span).map_err(|_| no_method(other, method, span)),
            };
        }
        match receiver {
            Value::Capability(Capability::Stdout) => self.stdout_method(method, &args, span),
            Value::Capability(Capability::Fs) => self.fs_method(method, &args, span),
            Value::Capability(Capability::Clock) => self.clock_method(method, &args, span),
            Value::Capability(Capability::Stdin) => self.stdin_method(method, &args, span),
            // The bytes share the string's memory: nothing is copied.
            Value::Str(text) if method == "encode" && args.is_empty() => {
                Ok(Value::Bytes(Rc::from(Rc::clone(text))))
            }
            Value::Str(text) => str_method(text, method, &args, span),
            Value::Bytes(data) if method == "decode" && args.is_empty() => match std::str::from_utf8(data) {
                Ok(text) => Ok(Value::Ok(Rc::new(Value::str(text)))),
                Err(error) => {
                    let message = format!("these bytes aren't UTF-8 text: {error}");
                    let error =
                        self.make_value("InvalidValue", vec![("message", Value::str(&message))], span)?;
                    Ok(Value::Err(Rc::new(error)))
                }
            },
            Value::Dict(pairs) => match (method, args.as_slice()) {
                ("get", [key]) => Ok(lookup(pairs, key).unwrap_or(Value::None)),
                ("get", [key, default]) => Ok(lookup(pairs, key).unwrap_or_else(|| default.clone())),
                ("keys", []) => Ok(Value::list(pairs.iter().map(|(key, _)| key.clone()).collect())),
                ("values", []) => Ok(Value::list(pairs.iter().map(|(_, value)| value.clone()).collect())),
                ("items", []) => Ok(Value::list(
                    pairs.iter().map(|(k, v)| Value::tuple(vec![k.clone(), v.clone()])).collect(),
                )),
                _ => Err(no_method(receiver, method, span)),
            },
            Value::IdMap(map) => match (method, args.as_slice()) {
                ("get", [Value::Id(id)]) => Ok(map.get(*id).cloned().unwrap_or(Value::None)),
                ("items", []) => Ok(Value::list(
                    map.entries().map(|(id, v)| Value::tuple(vec![Value::Id(id), v.clone()])).collect(),
                )),
                ("values", []) => Ok(Value::list(map.entries().map(|(_, v)| v.clone()).collect())),
                ("ids", []) => Ok(Value::list(map.entries().map(|(id, _)| Value::Id(id)).collect())),
                _ => Err(no_method(receiver, method, span)),
            },
            _ => Err(no_method(receiver, method, span)),
        }
    }

    fn stdout_method(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let ending = match method {
            "print" => "\n",
            "write" => "",
            _ => return Err(no_method(&Value::Capability(Capability::Stdout), method, span)),
        };
        let text = args.iter().map(format::display).collect::<Vec<_>>().join(" ");
        write!(self.out, "{text}{ending}")
            .map_err(|err| Diagnostic::new(format!("couldn't write output: {err}"), span))?;
        Ok(Value::Unit)
    }

    /// `str.from_code(n)`: the character with a code. A number that isn't one fails with
    /// an `InvalidValue`.
    pub(crate) fn char_from_code(&mut self, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let [Value::Int(code)] = args else { return Err(wrong_args("from_code", span)) };
        if let Some(c) = u32::try_from(*code).ok().and_then(char::from_u32) {
            return Ok(Value::Ok(Rc::new(Value::str(c.encode_utf8(&mut [0; 4])))));
        }
        let message = format!("{code} isn't the code of a character");
        let error = self.make_value("InvalidValue", vec![("message", Value::str(&message))], span)?;
        Ok(Value::Err(Rc::new(error)))
    }

    /// `clock.now()` reads the monotonic clock; `clock.sleep(duration)` waits.
    fn clock_method(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("now", []) => {
                let nanos = i64::try_from(clock_start().elapsed().as_nanos()).unwrap_or(i64::MAX);
                self.make_value("Instant", vec![("nanos", Value::Int(nanos))], span)
            }
            ("sleep", [Value::Instance(duration)]) if duration.type_name.as_ref() == "Duration" => {
                let nanos = match duration.field("nanos") {
                    Some(Value::Int(nanos)) => u64::try_from(*nanos).unwrap_or(0),
                    _ => 0,
                };
                std::thread::sleep(std::time::Duration::from_nanos(nanos));
                Ok(Value::Unit)
            }
            ("now" | "sleep", _) => Err(wrong_args(method, span)),
            _ => Err(no_method(&Value::Capability(Capability::Clock), method, span)),
        }
    }

    /// Methods that change their receiver, which has been taken out of its place.
    pub(crate) fn change_in_place(
        &mut self,
        value: &mut Value,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let (args, key) = split_key(args, method)?;
        if key.is_some() && method != "sort" {
            return Err(Diagnostic::new(format!("`{method}` doesn't take `key`"), span));
        }
        match value {
            Value::List(items) => self.change_list(Rc::make_mut(items), method, &args, key.as_ref(), span),
            Value::Dict(pairs) => {
                let pairs = Rc::make_mut(pairs);
                match (method, args.as_slice()) {
                    ("clear", []) => pairs.clear(),
                    ("pop", [key]) => {
                        let at = pairs.iter().position(|(existing, _)| existing == key).ok_or_else(|| {
                            Diagnostic::new(format!("the dict has no key {}", format::repr(key)), span)
                        })?;
                        return Ok(pairs.remove(at).1);
                    }
                    _ => return Err(wrong_args(method, span)),
                }
                Ok(Value::Unit)
            }
            Value::Set(items) => {
                let items = Rc::make_mut(items);
                match (method, args.as_slice()) {
                    ("add", [item]) if !items.contains(item) => items.push(item.clone()),
                    ("add", [_]) => {}
                    ("clear", []) => items.clear(),
                    ("discard" | "remove", [item]) => {
                        match items.iter().position(|existing| existing == item) {
                            Some(at) => drop(items.remove(at)),
                            None if method == "remove" => {
                                return Err(Diagnostic::new(
                                    format!("{} isn't in the set", format::repr(item)),
                                    span,
                                ));
                            }
                            None => {}
                        }
                    }
                    _ => return Err(wrong_args(method, span)),
                }
                Ok(Value::Unit)
            }
            Value::IdMap(_) => self.change_id_map(value, method, &args, span),
            other => Err(no_method(other, method, span)),
        }
    }

    fn change_list(
        &mut self,
        items: &mut Vec<Value>,
        method: &str,
        args: &[Value],
        key: Option<&Value>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("append", [value]) => items.push(value.clone()),
            ("pop", []) => {
                return items.pop().ok_or_else(|| Diagnostic::new("can't pop from an empty list", span));
            }
            ("pop", [Value::Int(at)]) => return Ok(items.remove(ops::position(*at, items.len(), span)?)),
            ("insert", [Value::Int(at), value]) => {
                let at = usize::try_from(*at).ok().filter(|&at| at <= items.len()).ok_or_else(|| {
                    Diagnostic::new(format!("can't insert at {at} in a list of length {}", items.len()), span)
                })?;
                items.insert(at, value.clone());
            }
            ("clear", []) => items.clear(),
            ("remove", [value]) => {
                let at = items.iter().position(|item| item == value).ok_or_else(|| {
                    Diagnostic::new(format!("{} isn't in the list", format::repr(value)), span)
                })?;
                items.remove(at);
            }
            ("remove_where", [test]) => {
                let mut kept = Vec::with_capacity(items.len());
                for item in items.drain(..) {
                    if !self.passes(test, &item, span)? {
                        kept.push(item);
                    }
                }
                *items = kept;
            }
            ("sort", []) => self.sort_values(items, key, span)?,
            _ => return Err(wrong_args(method, span)),
        }
        Ok(Value::Unit)
    }

    fn change_id_map(
        &mut self,
        value: &mut Value,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let Value::IdMap(map) = value else { return Err(wrong_args(method, span)) };
        let map = Rc::make_mut(map);
        match (method, args) {
            ("insert", [item]) => Ok(Value::Id(map.insert(item.clone()))),
            ("remove", [Value::Id(id)]) => Ok(map.remove(*id).unwrap_or(Value::None)),
            ("clear", []) => {
                for slot in &mut map.slots {
                    slot.value = None;
                }
                Ok(Value::Unit)
            }
            ("modify", [Value::Id(id), change]) => {
                let Some(current) = map.remove(*id) else { return Ok(Value::Bool(false)) };
                let changed = self.call_changing(change, current, span)?;
                if let Some(slot) = map.slots.get_mut(id.slot) {
                    slot.value = Some(changed);
                }
                Ok(Value::Bool(true))
            }
            ("remove_where", [test]) => {
                let ids: Vec<_> = map.entries().map(|(id, item)| (id, item.clone())).collect();
                for (id, item) in ids {
                    if self.passes(test, &item, span)? {
                        map.remove(id);
                    }
                }
                Ok(Value::Unit)
            }
            _ => Err(wrong_args(method, span)),
        }
    }

    /// Calls a test function on an item; it must return a bool.
    fn passes(&mut self, test: &Value, item: &Value, span: Span) -> Result<bool, Diagnostic> {
        match self.call_value(test, vec![CallArg::plain(item.clone(), span)], span)? {
            Value::Bool(passed) => Ok(passed),
            other => Err(Diagnostic::new(
                format!("this function should return a bool, but returned a {}", other.type_name()),
                span,
            )),
        }
    }

    /// Calls a function that changes one value, like the change passed to `IdMap.modify`,
    /// and returns the changed value.
    fn call_changing(&mut self, change: &Value, value: Value, span: Span) -> Result<Value, Diagnostic> {
        match change {
            Value::Lambda(lambda) => {
                let [param] = lambda.params.as_slice() else {
                    return Err(Diagnostic::new("this function should take one argument", span));
                };
                let caller = std::mem::replace(&mut self.frame, crate::scope::Frame::new());
                for (name, captured) in &lambda.captured {
                    self.frame.declare(name, captured.clone(), false);
                }
                self.frame.push();
                self.frame.declare(param, value, true);
                let result = self.eval(&lambda.body);
                let changed = self.frame.get(param).map_or(Value::Unit, |binding| binding.value.clone());
                self.frame = caller;
                result.map(|_| changed)
            }
            Value::Function(function) => {
                const ITEM: &str = "\0item";
                self.frame.push();
                self.frame.declare(ITEM, Value::Unit, true);
                let place = Place { root: ITEM.to_owned(), steps: Vec::new(), span };
                let arg = CallArg { name: None, value, mut_place: Some(place.clone()), literal: false, span };
                let result = self.call_user(function, vec![arg], span);
                let changed = self.take(&place);
                self.frame.pop();
                result.and(changed)
            }
            other => Err(Diagnostic::new(format!("a {} can't be called", other.type_name()), span)),
        }
    }
}

/// When the monotonic clock started: the first time any program read it.
fn clock_start() -> std::time::Instant {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

fn lookup(pairs: &[(Value, Value)], key: &Value) -> Option<Value> {
    pairs.iter().find(|(existing, _)| existing == key).map(|(_, value)| value.clone())
}

fn str_method(text: &str, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let words = |parts: Vec<&str>| Value::list(parts.into_iter().map(Value::str).collect());
    Ok(match (method, args) {
        ("code", []) => {
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                Value::Int(i64::from(u32::from(c)))
            } else {
                let count = text.chars().count();
                return Err(Diagnostic::new(
                    format!("`code` needs one character, but this has {count}"),
                    span,
                )
                .with_help("use `codes()` for the code of every character"));
            }
        }
        ("codes", []) => Value::list(text.chars().map(|c| Value::Int(i64::from(u32::from(c)))).collect()),
        ("upper", []) => Value::str(&text.to_uppercase()),
        ("lower", []) => Value::str(&text.to_lowercase()),
        ("strip", []) => Value::str(text.trim()),
        ("lstrip", []) => Value::str(text.trim_start()),
        ("rstrip", []) => Value::str(text.trim_end()),
        ("split", []) => words(text.split_whitespace().collect()),
        ("split", [Value::Str(sep)]) if !sep.is_empty() => words(text.split(sep.as_ref()).collect()),
        ("replace", [Value::Str(from), Value::Str(to)]) => Value::str(&text.replace(from.as_ref(), to)),
        ("find", [Value::Str(part)]) => text.find(part.as_ref()).map_or(Value::None, |at| {
            Value::Int(i64::try_from(text.get(..at).map_or(0, |b| b.chars().count())).unwrap_or(0))
        }),
        ("join", [items]) => {
            let items = match items {
                Value::List(items) | Value::Tuple(items) | Value::Set(items) => items.as_ref().clone(),
                other => {
                    return Err(Diagnostic::new(
                        format!("`join` needs a list of str, not a {}", other.type_name()),
                        span,
                    ));
                }
            };
            let parts = items
                .iter()
                .map(|item| match item {
                    Value::Str(part) => Ok(part.to_string()),
                    other => Err(Diagnostic::new(
                        format!("`join` needs a list of str, but it has a {}", other.type_name()),
                        span,
                    )
                    .with_help("convert the items first: [str(x) for x in items]")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Value::str(&parts.join(text))
        }
        ("upper" | "lower" | "strip" | "lstrip" | "rstrip" | "split" | "replace" | "find", _) => {
            return Err(wrong_args(method, span));
        }
        _ => return Err(no_method(&Value::str(text), method, span)),
    })
}
