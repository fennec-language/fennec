//! The built-in types' methods, each modeled here on its own: the machine operations the
//! runtime provides, and the ones the standard library writes in Fennec.

use fennec_syntax::{Expr, Span};

use super::builtins::{invalid, not_a_number};
use super::calls::{ArgVal, Receiver};
use super::expr::{dict_insert, set_insert, sized};
use super::{Interp, MOST_ITEMS, Run, as_int, crash, position, unsupported};
use crate::value::{Cap, Held, Instance, Text, Value};

impl Interp {
    /// `receiver.name(...)` for a built-in value.
    #[expect(clippy::too_many_lines, reason = "one step for each kind of value a method is called on")]
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
        if let Some(done) = self.format_method(&receiver, name, (&args, &named), expr)? {
            return Ok(done);
        }
        // The standard library's own code has `len(x)` and `abs(x)` written as these methods
        // when it's parsed, which a program can't call on a built-in value.
        if matches!(name, "len" | "abs") && args.is_empty() && named.is_empty() {
            let value = match &receiver {
                Receiver::Place(place) => self.read_place(place, span)?,
                Receiver::Value(value) => value.clone(),
            };
            if !matches!(value, Value::Instance(_)) {
                return self.builtin(name, vec![ArgVal::Value(None, value)], expr);
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
            && let Some(done) = self
                .std_instance_method(instance, name, &args, &named, span)
                .or_else(|| self.error_method(&receiver, instance, name, &args, span))
        {
            return done;
        }
        match value {
            Value::Cap(Cap::Fs(reach)) => self.fs_method(&reach, name, &args, &named, span),
            // A generator's text joined: each is asked for in turn, and a failure passes up from
            // there, as compiled code's loop does.
            Value::Str(text)
                if name == "join" && args.first().is_some_and(|over| !matches!(over, Value::List(_))) =>
            {
                let over = args.first().cloned().unwrap_or(Value::None);
                let parts = self.all_items(over, span)?;
                str_method(&text, name, &[Value::List(parts.into_iter().collect())], &named, span)
            }
            Value::Str(text) => str_method(&text, name, &args, &named, span),
            Value::Bytes(data) if named.is_empty() => bytes_method(&data, name, &args, span),
            Value::List(items) => self.list_method(receiver, items.into_inner(), name, (&args, named), expr),
            // The methods below take no named argument.
            _ if !named.is_empty() => unsupported(format!("the method `{name}` with a named argument")),
            Value::Cap(cap) => self.cap_method(cap, name, &args, span),
            Value::Dict(pairs) => dict_method(&pairs, name, &args, span),
            Value::Set(items) => self.set_method(items.into_inner(), name, &args),
            Value::Int(_) | Value::Sized(..) => int_method(&value, name, &args, span),
            Value::Float(x) => match name {
                "is_nan" if args.is_empty() => Ok(Value::Bool(x.is_nan())),
                _ => unsupported(format!("the float method `{name}`")),
            },
            Value::Channel(end) => self.channel_method(&end, name, &args),
            other => unsupported(format!("the method `{name}` of a {}", other.kind())),
        }
    }

    /// A method of JSON's `Serializer` or `Deserializer`, given to a type's own `def serialize`
    /// or `def deserialize`, when that's what `receiver` holds.
    fn format_method(
        &mut self,
        receiver: &Receiver,
        name: &str,
        (args, named): (&[Value], &[(String, Value)]),
        expr: &'static Expr,
    ) -> Run<Option<Value>> {
        let format = match receiver {
            Receiver::Place(place) => self.peek_place(place, expr.span, |held| match held {
                Value::Cap(cap @ (Cap::Saver | Cap::Loader)) => Some(cap.clone()),
                _ => None,
            })?,
            Receiver::Value(Value::Cap(cap @ (Cap::Saver | Cap::Loader))) => Some(cap.clone()),
            Receiver::Value(_) => None,
        };
        match format {
            Some(_) if !named.is_empty() => unsupported(format!("`{name}` with a named argument")),
            Some(Cap::Saver) => self.saver_method(name, args, expr).map(Some),
            Some(_) => self.loader_method(name, args, expr).map(Some),
            None => Ok(None),
        }
    }

    /// A `Buffer`'s methods, on the list it's held as. Its room is known only as far as
    /// `docs/language.md` promises it: at least what it holds, and right after `reserve(n)`, at
    /// least `n` more.
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
        if name == "capacity" && args.is_empty() {
            let held = match self.read_place(&place, span)? {
                Value::List(items) => i64::try_from(items.len()).unwrap_or(i64::MAX),
                _ => return unsupported("a Buffer that isn't a list"),
            };
            let promised = match &self.reserved {
                Some((frame, local, room))
                    if place.path.is_empty() && *frame == place.frame && *local == place.name =>
                {
                    *room
                }
                _ => 0,
            };
            return Ok(Value::Room(held.max(promised)));
        }
        if let ("reserve", [Value::Int(more)]) = (name, args.as_slice()) {
            let more = *more;
            let held = match self.read_place(&place, span)? {
                Value::List(items) => i64::try_from(items.len()).unwrap_or(i64::MAX),
                _ => return unsupported("a Buffer that isn't a list"),
            };
            if more < 0 {
                return unsupported("`reserve` of fewer than none");
            }
            self.reserved =
                place.path.is_empty().then(|| (place.frame, place.name.clone(), held.saturating_add(more)));
            return Ok(Value::Unit);
        }
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
                _ => unsupported(format!("the Buffer method `{name}`")),
            }
        })
    }

    /// `int.parse(text)`, `float.parse(text)`, `f32.parse(text)` and `str.from_code(n)`.
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
            // Read straight to the nearest `f32`, rounded once; text that isn't one fails as `float`'s does.
            ("f32", "parse", [Value::Str(text)]) => match text.trim().parse::<f32>() {
                Ok(x) => Ok(Value::F32(x)),
                Err(_) => Err(not_a_number(text, "a float", span)),
            },
            ("bytes", "fromhex", [Value::Str(text)]) => match from_hex(text) {
                Ok(data) => Ok(Value::Bytes(data)),
                Err(at) => Err(invalid(hex_problem(text, at), span)),
            },
            ("str", "from_code", [Value::Int(code)]) => {
                match u32::try_from(*code).ok().and_then(char::from_u32) {
                    Some(c) => Ok(Value::Str(Text::from(c.to_string()))),
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
        instance: &Instance,
        name: &str,
        args: &[Value],
        named: &[(String, Value)],
        span: Span,
    ) -> Option<Run<Value>> {
        let ty = instance.ty.as_str();
        // `std.env`'s `Env`, run from its own source: its `get` is written in Rust. It reads the
        // variables the oracle was started with, as the compiled program reads its own.
        if ty == "Env" && self.program.from_std.get("std.env.Env").is_some_and(|found| found == "Env") {
            return Some(match (name, args) {
                ("get", [Value::Str(variable)]) if named.is_empty() => {
                    Ok(std::env::var(variable.as_str()).map_or(Value::None, |found| Value::Str(found.into())))
                }
                _ => unsupported(format!("the Env method `{name}`")),
            });
        }
        // `std.sqlite`'s `Database` and `Transaction`, run from their own source: the methods
        // their own call, written in Rust.
        if matches!(ty, "Database" | "Transaction")
            && self.program.from_std.get(&format!("std.sqlite.{ty}")).is_some_and(|found| found == ty)
        {
            return self.sqlite_method(instance, name, args, span);
        }
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
            "File" => self.file_method(&Value::Instance(Held::new(instance.clone())), name, args, span),
            _ => match (name, args) {
                ("message", []) => super::show::io_words(instance).map(|words| Value::Str(words.into())),
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
            (Cap::Stdin, "lines") => Ok(Value::List(Held::new(Vec::new()))),
            (Cap::Stdin, "read_line") => Ok(Value::None),
            (Cap::Args, "len") => Ok(Value::Int(0)),
            // Standard error is written as the program writes it, and never compared.
            (Cap::Stderr, "write_line" | "write_text") => match args {
                [Value::Str(text)] => {
                    let mut text = text.as_str().to_owned();
                    if name == "write_line" {
                        text.push('\n');
                    }
                    if !self.shared.quiet.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = std::io::Write::write_all(&mut std::io::stderr(), text.as_bytes());
                    }
                    Ok(Value::Unit)
                }
                _ => unsupported(format!("`stderr.{name}` of something other than text")),
            },
            (Cap::Stderr, "flush") => Ok(Value::Unit),
            // The system's clocks, as `std.time`'s `Instant` and `Time`, each moved on by every
            // `sleep` so far, which takes no time here. What they read can't be known, so a
            // program's output can't depend on more than their order.
            (Cap::Clock, "now" | "time") if args.is_empty() => {
                let slept = self.shared.slept.load(std::sync::atomic::Ordering::Relaxed);
                let (ty, read) = if name == "now" {
                    ("Instant", self.shared.started.elapsed().as_nanos())
                } else {
                    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
                    ("Time", since.map_or(0, |since| since.as_nanos()))
                };
                let nanos = i64::try_from(read.saturating_add(u128::from(slept))).unwrap_or(i64::MAX);
                Ok(Value::instance(ty, None, vec![("nanos".to_owned(), Value::Int(nanos))]))
            }
            (Cap::Clock, "sleep") => match args {
                [Value::Instance(duration)] if duration.ty == "Duration" => {
                    let Some(Value::Int(nanos)) = duration.field("nanos") else {
                        return unsupported("a Duration without its nanoseconds");
                    };
                    let nanos = u64::try_from(*nanos).unwrap_or(0);
                    self.shared.slept.fetch_add(nanos, std::sync::atomic::Ordering::Relaxed);
                    Ok(Value::Unit)
                }
                _ => unsupported("`clock.sleep` of something other than a Duration"),
            },
            // Bytes and seeds that differ every run, as the compiled program's do, so its output
            // can't depend on them. A count below 0 stops the program, as the runtime's does.
            (Cap::Entropy, "bytes") => match args {
                [Value::Int(count)] => match usize::try_from(*count) {
                    Ok(count) => {
                        Ok(Value::Bytes((0..count).map(|_| random_word().to_le_bytes()[0]).collect()))
                    }
                    Err(_) => crash(format!("`bytes` can't give {count} bytes"), span),
                },
                _ => unsupported("`entropy.bytes` of something other than an int"),
            },
            (Cap::Entropy, "seed") if args.is_empty() => Ok(Value::Int(random_word().cast_signed())),
            // No signal comes while the oracle runs a program, so it's never asked to stop.
            (Cap::Signals, "stopping") => Ok(Value::Bool(false)),
            (Cap::Signals, "wait") => unsupported("waiting for a signal, which never comes"),
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
            ("value", [value]) => Text::from(self.display(value)?),
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
                return Ok(Value::Str(Text::from(text)));
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

/// Whether calling the built-in method `name` changes the value it's called on: one of `CHANGES`,
/// or a list's or a table's `sort`, `remove_where` and `modify`, which are worked out from the
/// whole value and put back.
pub(crate) fn changes(name: &str) -> bool {
    CHANGES.contains(&name) || matches!(name, "sort" | "remove_where" | "modify")
}

/// Whether calling the built-in method `name` on what's held only reads it: `only_reads`, or a
/// table's `get`, `values`, `ids` and `items`.
pub(crate) fn reads(held: &Value, name: &str) -> bool {
    only_reads(held, name)
        || matches!(held, Value::Table(_)) && matches!(name, "get" | "values" | "ids" | "items")
}

/// The methods that change a list, dict or set where it is.
pub(crate) const CHANGES: &[&str] = &[
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

/// Whether calling `name` on what's held only reads it: any method of a string, and a dict's or a
/// list's that look things up. Others, `sort` and `remove_where` among them, change it where it is.
pub(crate) fn only_reads(held: &Value, name: &str) -> bool {
    match held {
        Value::Str(_) => true,
        Value::Dict(_) => matches!(name, "get" | "items" | "keys" | "values"),
        Value::List(_) => matches!(name, "count" | "get" | "index"),
        _ => false,
    }
}

/// A method that only reads a string, a dict or a list, worked out where the value is: what it
/// gives, or `None` for one that isn't, or that's worked out from the whole value instead.
fn read_only(
    value: &Value,
    name: &str,
    args: &[Value],
    named: &[(String, Value)],
    span: Span,
) -> Option<Run<Value>> {
    if !only_reads(value, name) {
        return None;
    }
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
        [(label, Value::Str(fill))] if label == "fill" => fill.as_str().to_owned(),
        _ => return unsupported(format!("`{name}` with a named argument")),
    };
    let string = |value: Option<&Value>| -> Run<String> {
        match value {
            Some(Value::Str(text)) => Ok(text.as_str().to_owned()),
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
        ("upper", 0) => Value::Str(Text::from(text.to_uppercase())),
        ("encode", 0) => Value::Bytes(text.as_bytes().to_vec()),
        ("lower", 0) => Value::Str(Text::from(text.to_lowercase())),
        ("strip", 0) => Value::Str(Text::from(text.trim().to_owned())),
        ("lstrip", 0) => Value::Str(Text::from(text.trim_start().to_owned())),
        ("rstrip", 0) => Value::Str(Text::from(text.trim_end().to_owned())),
        ("split", 0) => {
            Value::List(text.split_whitespace().map(|part| Value::Str(Text::from(part.to_owned()))).collect())
        }
        ("split", 1) => {
            let between = string(args.first())?;
            if between.is_empty() {
                return crash("can't split by an empty string", span);
            }
            Value::List(
                text.split(between.as_str()).map(|part| Value::Str(Text::from(part.to_owned()))).collect(),
            )
        }
        ("replace", 2) => {
            Value::Str(Text::from(text.replace(string(args.first())?.as_str(), &string(args.get(1))?)))
        }
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
            Value::Str(Text::from(joined.join(text)))
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
                        found.push(Value::Str(Text::from(part.to_owned())));
                    }
                } else {
                    found.push(Value::Str(Text::from(part.strip_suffix('\r').unwrap_or(part).to_owned())));
                }
            }
            Value::List(Held::new(found))
        }
        ("ljust" | "rjust" | "center", 1) => {
            let width = int(args.first())?;
            let pad = width - chars(text);
            let padding = |count: i64| -> Run<String> { padding(&fill, count, span) };
            match name {
                "ljust" => Value::Str(Text::from(format!("{text}{}", padding(pad)?))),
                "rjust" => Value::Str(Text::from(format!("{}{text}", padding(pad)?))),
                _ if pad <= 0 => Value::Str(Text::from(text.to_owned())),
                _ => {
                    let left = pad / 2 + i64::from(pad % 2 == 1 && width % 2 == 1);
                    Value::Str(Text::from(format!("{}{text}{}", padding(left)?, padding(pad - left)?)))
                }
            }
        }
        ("zfill", 1) => {
            let width = int(args.first())?;
            let zeros = "0".repeat(usize::try_from(width - chars(text)).unwrap_or(0));
            if text.starts_with('-') || text.starts_with('+') {
                let (sign, rest) = text.split_at(1);
                Value::Str(Text::from(format!("{sign}{zeros}{rest}")))
            } else {
                Value::Str(Text::from(format!("{zeros}{text}")))
            }
        }
        ("removeprefix", 1) => {
            let prefix = string(args.first())?;
            Value::Str(Text::from(text.strip_prefix(prefix.as_str()).unwrap_or(text).to_owned()))
        }
        ("removesuffix", 1) => {
            let suffix = string(args.first())?;
            Value::Str(Text::from(
                if suffix.is_empty() { text } else { text.strip_suffix(suffix.as_str()).unwrap_or(text) }
                    .to_owned(),
            ))
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
            return self.put_back(receiver, Value::List(Held::new(sorted)), span);
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
                self.put_back(receiver, Value::List(Held::new(kept)), span)
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
            // The first item equal to `item`, taken out: `remove` gives it back, or `None` when
            // there's none, and `discard` gives nothing.
            ("remove" | "discard", [item]) => {
                let mut items = items;
                let mut found = None;
                for (at, each) in items.iter().enumerate() {
                    if self.equal(each, item)? {
                        found = Some(at);
                        break;
                    }
                }
                let Some(at) = found else {
                    return Ok(if name == "remove" { Value::None } else { Value::Unit });
                };
                let removed = items.remove(at);
                self.put_back(receiver, Value::List(Held::new(items)), span)?;
                Ok(if name == "remove" { removed } else { Value::Unit })
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
                Ok(Value::Set(Held::new(found)))
            }
            "intersection" | "difference" => {
                let mut found = Vec::new();
                for item in items {
                    if self.contains(&set, &item)? == (name == "intersection") {
                        found.push(item);
                    }
                }
                Ok(Value::Set(Held::new(found)))
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
                let mine = Value::Set(Held::new(items));
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
        Value::Set(items) => set_change(items, name, args),
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
        ("discard", [key]) => found(pairs, key).map(|at| {
            if let Some(at) = at {
                pairs.remove(at);
            }
            Value::Unit
        }),
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

fn set_change(items: &mut Vec<Value>, name: &str, args: &mut [Value]) -> Option<Run<Value>> {
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
            Ok(Some(found)) => Ok(items.remove(found)),
            Ok(None) => Ok(Value::None),
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
            pairs
                .iter()
                .map(|(key, value)| Value::Tuple(Held::new(vec![key.clone(), value.clone()])))
                .collect(),
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
            // Past 128 bits only the low ones matter, and a size's are the same in the product
            // of the two's low 128 bits, as they are in the whole one.
            None => a.cast_unsigned().wrapping_mul(b.cast_unsigned()).cast_signed(),
        },
        _ => return unsupported(format!("the int method `{name}`")),
    };
    let result = if name.starts_with("saturating") {
        exact.clamp(least, most)
    } else {
        let modulus = most - least + 1;
        // Wrapped at 128 bits, a multiple of the size's, which leaves what's kept the same.
        exact.wrapping_sub(least).rem_euclid(modulus) + least
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

/// A method of `bytes`: `decode()`, its UTF-8 text or a failure saying why it isn't, and `hex()`
/// or `hex(sep)`, two lowercase hex digits a byte, with `sep` between bytes.
fn bytes_method(data: &[u8], name: &str, args: &[Value], span: Span) -> Run<Value> {
    match (name, args) {
        ("decode", []) => match std::str::from_utf8(data) {
            Ok(text) => Ok(Value::Str(Text::from(text.to_owned()))),
            Err(error) => Err(invalid(format!("these bytes aren't UTF-8 text: {error}"), span)),
        },
        ("hex", []) => Ok(Value::Str(Text::from(hex(data, "")))),
        ("hex", [Value::Str(sep)]) => Ok(Value::Str(Text::from(hex(data, sep)))),
        _ => unsupported(format!("the bytes method `{name}`")),
    }
}

/// Each byte as two lowercase hex digits, with `sep` between bytes.
fn hex(data: &[u8], sep: &str) -> String {
    let digits: Vec<String> = data.iter().map(|byte| format!("{byte:02x}")).collect();
    digits.join(sep)
}

/// The bytes that pairs of hex digits spell, with ASCII whitespace allowed between pairs, or the
/// place, in characters, of what doesn't fit.
fn from_hex(text: &str) -> Result<Vec<u8>, usize> {
    let mut data = Vec::new();
    let mut chars = text.chars().enumerate();
    while let Some((at, c)) = chars.next() {
        if c.is_ascii_whitespace() {
            continue;
        }
        let high = c.to_digit(16).ok_or(at)?;
        let Some((next, low)) = chars.next() else { return Err(at + 1) };
        let low = low.to_digit(16).ok_or(next)?;
        data.push(u8::try_from(high * 16 + low).map_err(|_| at)?);
    }
    Ok(data)
}

/// What `bytes.fromhex` fails with, stopped at `at`.
fn hex_problem(text: &str, at: usize) -> String {
    match text.chars().nth(at) {
        Some(c) => format!(
            "`fromhex` needs two hex digits for each byte, but found {} at {at}",
            crate::text::quoted(&c.to_string())
        ),
        None => {
            "`fromhex` needs two hex digits for each byte, but the text ends halfway through one".to_owned()
        }
    }
}

/// A word no run can know beforehand: hashed with keys `std` draws from the system for each new
/// hasher, so it's new every time. It's for an `Entropy`'s bytes and seeds, which a program's
/// output can't depend on, never for secrets.
fn random_word() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static DRAWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(DRAWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    hasher.finish()
}

impl Interp {
    /// An error's notes: `notes()` reads them, and `add_note(text)` adds one where it's held.
    /// `None` for any other method, or a value that isn't an error. A type of the program's own may
    /// have methods of those names, which were found first.
    fn error_method(
        &mut self,
        receiver: &Receiver,
        instance: &Instance,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Option<Run<Value>> {
        if !self.is_error(&instance.ty) {
            return None;
        }
        match (name, args) {
            ("message", []) => Some(self.words(instance).map(|words| Value::Str(Text::from(words.as_str())))),
            ("with_note", [Value::Str(note)]) => {
                let mut noted = instance.clone();
                noted.notes.push(note.as_str().to_owned());
                Some(Ok(Value::Instance(Held::new(noted))))
            }
            ("notes", []) => {
                let notes = instance.notes.iter().map(|note| Value::Str(Text::from(note.as_str())));
                Some(Ok(Value::List(notes.collect())))
            }
            ("add_note", [Value::Str(note)]) => {
                let Receiver::Place(place) = receiver else {
                    return Some(unsupported("`add_note` on an error that isn't in a place"));
                };
                let note = note.as_str().to_owned();
                let added = self.change_place(&place.clone(), span, false, |held| {
                    if let Value::Instance(held) = held {
                        held.notes.push(note);
                    }
                    Ok(())
                });
                Some(added.map(|()| Value::Unit))
            }
            _ => None,
        }
    }

    /// Whether a type is an error: one declared with `error`, the program's own or the standard
    /// library's, or one of the core's the oracle models.
    pub(crate) fn is_error(&self, ty: &str) -> bool {
        self.program.types.get(ty).is_some_and(|declared| declared.kind == crate::load::Kind::Error)
            || matches!(ty, "InvalidValue" | "ParseError" | "IoError")
    }
}
