//! The built-in types' methods, each modeled here on its own: the machine operations the
//! runtime provides, and the ones the standard library writes in Fennec.

use fennec_syntax::{Expr, Span};

use super::builtins::{invalid, not_a_number};
use super::calls::{ArgVal, Receiver};
use super::expr::{dict_insert, set_insert, sized};
use super::{Interp, MOST_ITEMS, Run, as_int, crash, position, unsupported};
use crate::value::{Cap, Value};

impl Interp {
    /// `receiver.name(...)` for a built-in value.
    pub(crate) fn method(
        &mut self,
        receiver: Receiver,
        name: &str,
        given: Vec<ArgVal>,
        expr: &'static Expr,
    ) -> Run<Value> {
        let span = expr.span;
        let mut args = Vec::with_capacity(given.len());
        let mut named = Vec::new();
        for arg in given {
            let label = arg.name().map(str::to_owned);
            let value = self.arg_as_value(arg)?;
            match label {
                Some(label) => named.push((label, value)),
                None => args.push(value),
            }
        }
        let is_table = match &receiver {
            Receiver::Place(place) => self.peek_place(place, span, |held| matches!(held, Value::Table(_)))?,
            Receiver::Value(value) => matches!(value, Value::Table(_)),
        };
        if is_table {
            if !named.is_empty() {
                return unsupported(format!("the Table method `{name}` with a named argument"));
            }
            return self.table_method(receiver, name, args, span);
        }
        if let Receiver::Place(place) = &receiver {
            let place = place.clone();
            if CHANGES.contains(&name) {
                let changed = self.change_place(&place, span, false, |value| {
                    Ok(changed_in_place(value, name, &mut args, &named, span))
                })?;
                if let Some(result) = changed {
                    return result;
                }
            }
            let read = self.peek_place(&place, span, |value| read_only(value, name, &args, &named, span))?;
            if let Some(result) = read {
                return result;
            }
        }
        let value = match &receiver {
            Receiver::Place(place) => self.read_place(place, span)?,
            Receiver::Value(value) => value.clone(),
        };
        if let Value::Instance(instance) = &value
            && instance.ty == "StrWriter"
        {
            return self.writer_method(receiver, name, &args, span);
        }
        if let Value::Instance(instance) = &value
            && let Some(done) = self.std_instance_method(instance, name, &args, &named, span)
        {
            return done;
        }
        match value {
            Value::Cap(Cap::Fs(reach)) => self.fs_method(&reach, name, &args, &named, span),
            Value::Str(text) => str_method(&text, name, &args, &named, span),
            Value::List(items) => self.list_method(receiver, items, name, (&args, named), expr),
            // The methods below take no named argument.
            _ if !named.is_empty() => unsupported(format!("the method `{name}` with a named argument")),
            Value::Cap(cap) => self.cap_method(cap, name, &args, span),
            Value::Dict(pairs) => dict_method(&pairs, name, &args, span),
            Value::Set(items) => self.set_method(items, name, &args),
            Value::Int(_) | Value::Sized(..) => int_method(&value, name, &args, span),
            Value::Float(x) => match name {
                "is_nan" if args.is_empty() => Ok(Value::Bool(x.is_nan())),
                _ => unsupported(format!("the float method `{name}`")),
            },
            other => unsupported(format!("the method `{name}` of a {}", other.kind())),
        }
    }

    /// A `Buffer`'s methods, on the list it's held as. How much room it has isn't modeled.
    pub(crate) fn buffer_method(
        &mut self,
        receiver: Receiver,
        name: &str,
        given: Vec<ArgVal>,
        span: Span,
    ) -> Run<Value> {
        let mut args = Vec::with_capacity(given.len());
        for arg in given {
            if arg.name().is_some() {
                return unsupported(format!("the Buffer method `{name}` with a named argument"));
            }
            args.push(self.arg_as_value(arg)?);
        }
        let Receiver::Place(place) = receiver else {
            return unsupported(format!("the Buffer method `{name}` on a value that isn't a place"));
        };
        self.change_place(&place, span, false, |value| {
            let Value::List(items) = value else { return unsupported("a Buffer that isn't a list") };
            match (name, args.as_mut_slice()) {
                ("push", [item]) => {
                    items.push(std::mem::replace(item, Value::Unit));
                    Ok(Value::Unit)
                }
                ("pop", []) => match items.pop() {
                    Some(item) => Ok(item),
                    None => crash("can't pop from an empty Buffer", span),
                },
                ("remove", [Value::Int(at)]) => position(*at, items.len(), span).map(|at| items.remove(at)),
                ("insert", [Value::Int(at), item]) => {
                    match usize::try_from(*at).ok().filter(|at| *at <= items.len()) {
                        Some(at) => {
                            items.insert(at, std::mem::replace(item, Value::Unit));
                            Ok(Value::Unit)
                        }
                        None => {
                            crash(format!("can't insert at {at} in a Buffer of length {}", items.len()), span)
                        }
                    }
                }
                ("extend", [Value::List(more)]) => {
                    items.append(more);
                    Ok(Value::Unit)
                }
                ("truncate", [Value::Int(keep)]) => {
                    items.truncate(usize::try_from(*keep).unwrap_or(0));
                    Ok(Value::Unit)
                }
                ("reserve", [Value::Int(_)]) => Ok(Value::Unit),
                _ => unsupported(format!("the Buffer method `{name}`")),
            }
        })
    }

    /// `int.parse(text)`, `float.parse(text)` and `str.from_code(n)`.
    pub(crate) fn static_call(&mut self, ty: &str, name: &str, given: Vec<ArgVal>, span: Span) -> Run<Value> {
        let mut args = Vec::new();
        for arg in given {
            if arg.name().is_some() {
                return unsupported(format!("`{ty}.{name}` with a named argument"));
            }
            args.push(self.arg_as_value(arg)?);
        }
        match (ty, name, args.as_slice()) {
            ("int", "parse", [Value::Str(text)]) => match text.trim().parse::<i64>() {
                Ok(n) => Ok(Value::Int(n)),
                Err(_) => Err(not_a_number(text, "an int", span)),
            },
            ("float", "parse", [Value::Str(text)]) => match text.trim().parse::<f64>() {
                Ok(x) => Ok(Value::Float(x)),
                Err(_) => Err(not_a_number(text, "a float", span)),
            },
            ("str", "from_code", [Value::Int(code)]) => {
                match u32::try_from(*code).ok().and_then(char::from_u32) {
                    Some(c) => Ok(Value::Str(c.to_string())),
                    None => Err(invalid(format!("{code} isn't the code of a character"), span)),
                }
            }
            _ => unsupported(format!("`{ty}.{name}`")),
        }
    }

    /// A method of a value of one of the standard library's types the oracle models: a `Path`'s,
    /// a `File`'s, or an `IoError`'s `message()`. `None` for a value of any other type.
    fn std_instance_method(
        &mut self,
        instance: &crate::value::Instance,
        name: &str,
        args: &[Value],
        named: &[(String, Value)],
        span: Span,
    ) -> Option<Run<Value>> {
        let ty = instance.ty.as_str();
        if !matches!(ty, "Path" | "File" | "IoError") || !self.is_std_type(ty) {
            return None;
        }
        if !named.is_empty() {
            return Some(unsupported(format!("the {ty} method `{name}` with a named argument")));
        }
        Some(match ty {
            "Path" => match instance.field("text") {
                Some(Value::Str(text)) => super::paths::method(text, name, args, span),
                _ => unsupported("a Path without its text"),
            },
            "File" => self.file_method(&Value::Instance(Box::new(instance.clone())), name, args, span),
            _ => match (name, args, instance.field("reason")) {
                ("message", [], Some(reason)) => Ok(reason.clone()),
                _ => unsupported(format!("the IoError method `{name}`")),
            },
        })
    }

    fn cap_method(&mut self, cap: Cap, name: &str, args: &[Value], span: Span) -> Run<Value> {
        match (cap, name) {
            (Cap::Stdout, "print") => {
                let mut shown = Vec::with_capacity(args.len());
                for arg in args {
                    shown.push(self.display(arg)?);
                }
                let mut line = shown.join(" ");
                line.push('\n');
                self.print(&line)?;
                Ok(Value::Unit)
            }
            (Cap::Stdout, "write") => match args {
                [one] => {
                    let shown = self.display(one)?;
                    self.print(&shown)?;
                    Ok(Value::Unit)
                }
                _ => unsupported("`stdout.write` of several values"),
            },
            (Cap::Stdin, "lines") => Ok(Value::List(Vec::new())),
            (Cap::Stdin, "read_line") => Ok(Value::None),
            (Cap::Args, "len") => Ok(Value::Int(0)),
            _ => {
                let _ = span;
                unsupported(format!("the capability method `{name}`"))
            }
        }
    }

    /// The methods of the `StrWriter` a type's `def str` writes into.
    fn writer_method(&mut self, receiver: Receiver, name: &str, args: &[Value], span: Span) -> Run<Value> {
        let text = match (name, args) {
            ("write", [Value::Str(text)]) => text.clone(),
            ("value", [value]) => self.display(value)?,
            ("text", []) => {
                let value = match &receiver {
                    Receiver::Place(place) => self.read_place(place, span)?,
                    Receiver::Value(value) => value.clone(),
                };
                let Value::Instance(writer) = value else { return unsupported("a StrWriter") };
                let Some(Value::List(parts)) = writer.field("_parts") else {
                    return unsupported("a StrWriter");
                };
                let mut text = String::new();
                for part in parts {
                    if let Value::Str(part) = part {
                        text.push_str(part);
                    }
                }
                return Ok(Value::Str(text));
            }
            _ => return unsupported(format!("the StrWriter method `{name}`")),
        };
        let Receiver::Place(place) = receiver else {
            return unsupported("writing to a StrWriter that isn't a place");
        };
        self.change_place(&place, span, false, |writer| match writer {
            Value::Instance(writer) => match writer.field_mut("_parts") {
                Some(Value::List(parts)) => {
                    parts.push(Value::Str(text));
                    Ok(Value::Unit)
                }
                _ => unsupported("a StrWriter"),
            },
            _ => unsupported("a StrWriter"),
        })
    }
}

/// The methods that change a list, dict or set where it is.
const CHANGES: &[&str] = &[
    "append",
    "pop",
    "insert",
    "extend",
    "clear",
    "reverse",
    "swap",
    "remove",
    "update",
    "setdefault",
    "add",
    "discard",
];

/// A method that only reads a string, a dict or a list, worked out where the value is: what it
/// gives, or `None` for one that isn't.
fn read_only(
    value: &Value,
    name: &str,
    args: &[Value],
    named: &[(String, Value)],
    span: Span,
) -> Option<Run<Value>> {
    match value {
        Value::Str(text) => Some(str_method(text, name, args, named, span)),
        Value::Dict(pairs) if named.is_empty() => Some(dict_method(pairs, name, args, span)),
        Value::List(items) if name == "get" => match args {
            [Value::Int(at)] => {
                let size = as_int(items.len());
                if *at >= size || *at < -size {
                    return Some(Ok(Value::None));
                }
                Some(position(*at, items.len(), span).map(|at| items.get(at).cloned().unwrap_or(Value::None)))
            }
            _ => None,
        },
        _ => None,
    }
}

#[expect(clippy::too_many_lines, reason = "one arm for each of str's methods")]
fn str_method(text: &str, name: &str, args: &[Value], named: &[(String, Value)], span: Span) -> Run<Value> {
    let fill = match named {
        [] => " ".to_owned(),
        [(label, Value::Str(fill))] if label == "fill" => fill.clone(),
        _ => return unsupported(format!("`{name}` with a named argument")),
    };
    let string = |value: Option<&Value>| -> Run<String> {
        match value {
            Some(Value::Str(text)) => Ok(text.clone()),
            _ => unsupported(format!("`{name}` with an argument that isn't a str")),
        }
    };
    let int = |value: Option<&Value>| -> Run<i64> {
        match value {
            Some(Value::Int(n)) => Ok(*n),
            _ => unsupported(format!("`{name}` with an argument that isn't an int")),
        }
    };
    let chars = |text: &str| as_int(text.chars().count());
    Ok(match (name, args.len()) {
        ("upper", 0) => Value::Str(text.to_uppercase()),
        ("lower", 0) => Value::Str(text.to_lowercase()),
        ("strip", 0) => Value::Str(text.trim().to_owned()),
        ("lstrip", 0) => Value::Str(text.trim_start().to_owned()),
        ("rstrip", 0) => Value::Str(text.trim_end().to_owned()),
        ("split", 0) => {
            Value::List(text.split_whitespace().map(|part| Value::Str(part.to_owned())).collect())
        }
        ("split", 1) => {
            let between = string(args.first())?;
            if between.is_empty() {
                return crash("can't split by an empty string", span);
            }
            Value::List(text.split(between.as_str()).map(|part| Value::Str(part.to_owned())).collect())
        }
        ("replace", 2) => Value::Str(text.replace(string(args.first())?.as_str(), &string(args.get(1))?)),
        ("find", 1) => {
            let part = string(args.first())?;
            match text.find(part.as_str()) {
                Some(at) => Value::Int(chars(text.get(..at).unwrap_or(""))),
                None => Value::None,
            }
        }
        ("rfind", 1) => {
            let part = string(args.first())?;
            match text.rfind(part.as_str()) {
                Some(at) => Value::Int(chars(text.get(..at).unwrap_or(""))),
                None => Value::None,
            }
        }
        ("join", 1) => {
            let Some(Value::List(parts)) = args.first() else {
                return unsupported("`join` of something that isn't a list");
            };
            let mut joined = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    Value::Str(part) => joined.push(part.as_str()),
                    other => return unsupported(format!("`join` of a {}", other.kind())),
                }
            }
            Value::Str(joined.join(text))
        }
        ("code", 0) => {
            let mut all = text.chars();
            match (all.next(), all.next()) {
                (Some(c), None) => Value::Int(i64::from(u32::from(c))),
                _ => {
                    return crash(
                        format!("`code` needs one character, but this has {}", text.chars().count()),
                        span,
                    );
                }
            }
        }
        ("codes", 0) => Value::List(text.chars().map(|c| Value::Int(i64::from(u32::from(c)))).collect()),
        ("startswith", 1) => Value::Bool(text.starts_with(string(args.first())?.as_str())),
        ("endswith", 1) => Value::Bool(text.ends_with(string(args.first())?.as_str())),
        ("isdigit", 0) => Value::Bool(!text.is_empty() && text.chars().all(|c| c.is_ascii_digit())),
        ("lines", 0) => {
            let parts: Vec<&str> = text.split('\n').collect();
            let last = parts.len().saturating_sub(1);
            let mut found = Vec::new();
            for (at, part) in parts.into_iter().enumerate() {
                if at == last {
                    if !part.is_empty() {
                        found.push(Value::Str(part.to_owned()));
                    }
                } else {
                    found.push(Value::Str(part.strip_suffix('\r').unwrap_or(part).to_owned()));
                }
            }
            Value::List(found)
        }
        ("ljust" | "rjust" | "center", 1) => {
            let width = int(args.first())?;
            let pad = width - chars(text);
            let padding = |count: i64| -> Run<String> { padding(&fill, count, span) };
            match name {
                "ljust" => Value::Str(format!("{text}{}", padding(pad)?)),
                "rjust" => Value::Str(format!("{}{text}", padding(pad)?)),
                _ if pad <= 0 => Value::Str(text.to_owned()),
                _ => {
                    let left = pad / 2 + i64::from(pad % 2 == 1 && width % 2 == 1);
                    Value::Str(format!("{}{text}{}", padding(left)?, padding(pad - left)?))
                }
            }
        }
        ("zfill", 1) => {
            let width = int(args.first())?;
            let zeros = "0".repeat(usize::try_from(width - chars(text)).unwrap_or(0));
            if text.starts_with('-') || text.starts_with('+') {
                let (sign, rest) = text.split_at(1);
                Value::Str(format!("{sign}{zeros}{rest}"))
            } else {
                Value::Str(format!("{zeros}{text}"))
            }
        }
        ("removeprefix", 1) => {
            let prefix = string(args.first())?;
            Value::Str(text.strip_prefix(prefix.as_str()).unwrap_or(text).to_owned())
        }
        ("removesuffix", 1) => {
            let suffix = string(args.first())?;
            Value::Str(
                if suffix.is_empty() { text } else { text.strip_suffix(suffix.as_str()).unwrap_or(text) }
                    .to_owned(),
            )
        }
        ("count", 1) => {
            let part = string(args.first())?;
            if part.is_empty() {
                Value::Int(chars(text) + 1)
            } else {
                Value::Int(as_int(text.matches(part.as_str()).count()))
            }
        }
        _ => return unsupported(format!("the str method `{name}`")),
    })
}

impl Interp {
    fn list_method(
        &mut self,
        receiver: Receiver,
        items: Vec<Value>,
        name: &str,
        (args, mut named): (&[Value], Vec<(String, Value)>),
        expr: &'static Expr,
    ) -> Run<Value> {
        let span = expr.span;
        if name == "sort" && args.is_empty() {
            let mut take =
                |label: &str| named.iter().position(|(given, _)| given == label).map(|at| named.remove(at).1);
            let key = take("key");
            let reverse = match take("reverse") {
                None => false,
                Some(Value::Bool(reverse)) => reverse,
                Some(_) => return unsupported("`sort` with a `reverse` that isn't a bool"),
            };
            if !named.is_empty() {
                return unsupported("`sort` with a named argument it doesn't take");
            }
            let sorted = self.sorted(items, key.as_ref(), reverse, expr)?;
            return self.put_back(receiver, Value::List(sorted), span);
        }
        if !named.is_empty() {
            return unsupported(format!("the list method `{name}` with a named argument"));
        }
        match (name, args) {
            // Each item `gone` says yes to is taken out, each asked once, in order.
            ("remove_where", [gone]) => {
                let mut kept = Vec::with_capacity(items.len());
                for item in items {
                    match self.call_value(gone, vec![item.clone()], expr)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => kept.push(item),
                        other => return unsupported(format!("`remove_where` given a {}", other.kind())),
                    }
                }
                self.put_back(receiver, Value::List(kept), span)
            }
            ("get", [Value::Int(at)]) => {
                let size = as_int(items.len());
                if *at >= size || *at < -size {
                    return Ok(Value::None);
                }
                let at = position(*at, items.len(), span)?;
                Ok(items.into_iter().nth(at).unwrap_or(Value::None))
            }
            ("count", [item]) => {
                let mut found = 0;
                for each in &items {
                    if self.equal(each, item)? {
                        found += 1;
                    }
                }
                Ok(Value::Int(found))
            }
            ("index", [item]) => {
                for (at, each) in items.iter().enumerate() {
                    if self.equal(each, item)? {
                        return Ok(Value::Int(as_int(at)));
                    }
                }
                Ok(Value::None)
            }
            ("remove", [item]) => {
                let mut items = items;
                let mut found = None;
                for (at, each) in items.iter().enumerate() {
                    if self.equal(each, item)? {
                        found = Some(at);
                        break;
                    }
                }
                let Some(at) = found else { return crash("the item to remove isn't in the list", span) };
                items.remove(at);
                self.put_back(receiver, Value::List(items), span)
            }
            _ => unsupported(format!("the list method `{name}`")),
        }
    }

    /// Puts a changed value back where its method was called.
    fn put_back(&mut self, receiver: Receiver, value: Value, span: Span) -> Run<Value> {
        match receiver {
            Receiver::Place(place) => {
                self.write_place(&place, value, span)?;
                Ok(Value::Unit)
            }
            Receiver::Value(_) => Ok(Value::Unit),
        }
    }

    fn set_method(&mut self, items: Vec<Value>, name: &str, args: &[Value]) -> Run<Value> {
        let Some(Value::Set(other)) = args.first() else {
            return unsupported(format!("the set method `{name}`"));
        };
        let set = Value::Set(other.clone());
        match name {
            "union" => {
                let mut found = items;
                for item in other {
                    set_insert(&mut found, item.clone())?;
                }
                Ok(Value::Set(found))
            }
            "intersection" | "difference" => {
                let mut found = Vec::new();
                for item in items {
                    if self.contains(&set, &item)? == (name == "intersection") {
                        found.push(item);
                    }
                }
                Ok(Value::Set(found))
            }
            "is_subset" | "is_disjoint" => {
                let mut all = true;
                let mut none = true;
                for item in &items {
                    if self.contains(&set, item)? {
                        none = false;
                    } else {
                        all = false;
                    }
                }
                Ok(Value::Bool(if name == "is_subset" { all } else { none }))
            }
            "is_superset" => {
                let mine = Value::Set(items);
                for item in other {
                    if !self.contains(&mine, item)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            _ => unsupported(format!("the set method `{name}`")),
        }
    }
}

/// A method that changes a list, dict or set where it is, needing nothing but the value: what it
/// gives back, or `None` for one that isn't.
fn changed_in_place(
    value: &mut Value,
    name: &str,
    args: &mut Vec<Value>,
    named: &[(String, Value)],
    span: Span,
) -> Option<Run<Value>> {
    if !named.is_empty() {
        return None;
    }
    match value {
        Value::List(items) => list_change(items, name, args, span),
        Value::Dict(pairs) => dict_change(pairs, name, args, span),
        Value::Set(items) => set_change(items, name, args, span),
        _ => None,
    }
}

fn list_change(items: &mut Vec<Value>, name: &str, args: &mut Vec<Value>, span: Span) -> Option<Run<Value>> {
    let grown = |items: &Vec<Value>| -> Run<Value> {
        if items.len() > MOST_ITEMS { unsupported("ran too long: a list too long") } else { Ok(Value::Unit) }
    };
    Some(match (name, args.as_mut_slice()) {
        ("append", [item]) => {
            items.push(std::mem::replace(item, Value::Unit));
            grown(items)
        }
        ("pop", []) => Ok(items.pop().unwrap_or(Value::None)),
        ("pop", [Value::Int(at)]) => position(*at, items.len(), span).map(|at| items.remove(at)),
        ("insert", [Value::Int(at), item]) => match usize::try_from(*at).ok().filter(|at| *at <= items.len())
        {
            Some(at) => {
                items.insert(at, std::mem::replace(item, Value::Unit));
                grown(items)
            }
            None => crash(format!("can't insert at {at} in a list of length {}", items.len()), span),
        },
        ("extend", [Value::List(more)]) => {
            items.append(more);
            grown(items)
        }
        ("clear", []) => {
            items.clear();
            Ok(Value::Unit)
        }
        ("reverse", []) => {
            items.reverse();
            Ok(Value::Unit)
        }
        ("swap", [Value::Int(i), Value::Int(j)]) => {
            let (i, j) = match (position(*i, items.len(), span), position(*j, items.len(), span)) {
                (Ok(i), Ok(j)) => (i, j),
                (Err(flow), _) | (_, Err(flow)) => return Some(Err(flow)),
            };
            items.swap(i, j);
            Ok(Value::Unit)
        }
        _ => return None,
    })
}

fn dict_change(
    pairs: &mut Vec<(Value, Value)>,
    name: &str,
    args: &mut [Value],
    span: Span,
) -> Option<Run<Value>> {
    let found = |pairs: &Vec<(Value, Value)>, key: &Value| super::find_key(pairs, key);
    Some(match (name, &mut *args) {
        ("pop", [key]) => match found(pairs, key) {
            Ok(Some(at)) => Ok(pairs.remove(at).1),
            Ok(None) => crash("the key to pop isn't in the dict", span),
            Err(flow) => Err(flow),
        },
        ("remove", [key]) => match found(pairs, key) {
            Ok(Some(at)) => Ok(pairs.remove(at).1),
            Ok(None) => Ok(Value::None),
            Err(flow) => Err(flow),
        },
        ("clear", []) => {
            pairs.clear();
            Ok(Value::Unit)
        }
        ("update", [Value::Dict(more)]) => {
            let mut done = Ok(Value::Unit);
            for (key, value) in std::mem::take(more) {
                if let Err(flow) = dict_insert(pairs, key, value) {
                    done = Err(flow);
                    break;
                }
            }
            done
        }
        ("setdefault", [key, default]) => match found(pairs, key) {
            Ok(Some(at)) => Ok(pairs.get(at).map_or(Value::None, |(_, value)| value.clone())),
            Ok(None) => {
                pairs.push((key.clone(), default.clone()));
                Ok(default.clone())
            }
            Err(flow) => Err(flow),
        },
        _ => return None,
    })
}

fn set_change(items: &mut Vec<Value>, name: &str, args: &mut [Value], span: Span) -> Option<Run<Value>> {
    let at = |items: &Vec<Value>, item: &Value| -> Run<Option<usize>> {
        for (at, held) in items.iter().enumerate() {
            if super::show::plain_equal(held, item)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    };
    Some(match (name, &mut *args) {
        ("add", [item]) => set_insert(items, item.clone()).map(|()| Value::Unit),
        ("discard", [item]) => at(items, item).map(|found| {
            if let Some(found) = found {
                items.remove(found);
            }
            Value::Unit
        }),
        ("remove", [item]) => match at(items, item) {
            Ok(Some(found)) => {
                items.remove(found);
                Ok(Value::Unit)
            }
            Ok(None) => crash("the item to remove isn't in the set", span),
            Err(flow) => Err(flow),
        },
        ("clear", []) => {
            items.clear();
            Ok(Value::Unit)
        }
        ("update", [Value::Set(more)]) => {
            let mut done = Ok(Value::Unit);
            for item in std::mem::take(more) {
                if let Err(flow) = set_insert(items, item) {
                    done = Err(flow);
                    break;
                }
            }
            done
        }
        _ => return None,
    })
}

fn dict_method(pairs: &[(Value, Value)], name: &str, args: &[Value], span: Span) -> Run<Value> {
    match (name, args) {
        ("get", [key]) => match super::find_key(pairs, key)? {
            Some(at) => Ok(pairs.get(at).map_or(Value::None, |(_, value)| value.clone())),
            None => Ok(Value::None),
        },
        ("get", [key, default]) => match super::find_key(pairs, key)? {
            Some(at) => Ok(pairs.get(at).map_or(Value::None, |(_, value)| value.clone())),
            None => Ok(default.clone()),
        },
        ("keys", []) => Ok(Value::List(pairs.iter().map(|(key, _)| key.clone()).collect())),
        ("values", []) => Ok(Value::List(pairs.iter().map(|(_, value)| value.clone()).collect())),
        ("items", []) => Ok(Value::List(
            pairs.iter().map(|(key, value)| Value::Tuple(vec![key.clone(), value.clone()])).collect(),
        )),
        _ => {
            let _ = span;
            unsupported(format!("the dict method `{name}`"))
        }
    }
}

/// `wrapping_` and `saturating_` arithmetic on an int of any size.
fn int_method(value: &Value, name: &str, args: &[Value], span: Span) -> Run<Value> {
    let (num, a) = match *value {
        Value::Int(n) => (None, i128::from(n)),
        Value::Sized(num, n) => (Some(num), n),
        _ => return unsupported("an int method"),
    };
    let b = match args {
        [Value::Int(b)] => i128::from(*b),
        [Value::Sized(_, b)] => *b,
        _ => return unsupported(format!("the int method `{name}`")),
    };
    let (least, most) = match num {
        Some(num) => num.range().unwrap_or((0, 0)),
        None => (i128::from(i64::MIN), i128::from(i64::MAX)),
    };
    let exact = match name {
        "wrapping_add" | "saturating_add" => a + b,
        "wrapping_sub" | "saturating_sub" => a - b,
        "wrapping_mul" | "saturating_mul" => match a.checked_mul(b) {
            Some(product) => product,
            None if name.starts_with("saturating") => {
                if (a < 0) == (b < 0) {
                    most
                } else {
                    least
                }
            }
            None => return unsupported("a wrapping product past 128 bits"),
        },
        _ => return unsupported(format!("the int method `{name}`")),
    };
    let result = if name.starts_with("saturating") {
        exact.clamp(least, most)
    } else {
        let modulus = most - least + 1;
        (exact - least).rem_euclid(modulus) + least
    };
    match num {
        Some(num) => sized(num, result, span),
        None => Ok(Value::Int(i64::try_from(result).unwrap_or(0))),
    }
}

/// `count` characters of `fill` repeated, cut to that many.
fn padding(fill: &str, count: i64, span: Span) -> Run<String> {
    if fill.is_empty() {
        return crash("a fill has to be at least one character, to pad text with", span);
    }
    let Ok(count) = usize::try_from(count) else { return Ok(String::new()) };
    Ok(fill.chars().cycle().take(count).collect())
}
