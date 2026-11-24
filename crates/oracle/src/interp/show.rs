//! How values print, compare and order, as the compiled program does: a type's own `def str`,
//! `def ==` and `def <` are called where it has them.

use std::cmp::Ordering;
use std::fmt::Write as _;

use super::calls::{ArgVal, Receiver};
use super::{Binding, Interp, Place, Run, unsupported};
use crate::text;
use crate::value::{Cell, Func, Held, Instance, Value};

/// Why a `Buffer`'s room, beyond what's promised of it, is refused.
pub(crate) const ROOM_UNPROMISED: &str = "a Buffer's exact room, which `docs/language.md` promises only to be at least what it holds, and n more right after `reserve(n)`";

impl Interp {
    /// What `stdout.print` and `str()` show: a string as it is, a failure's words, and anything
    /// else as it shows inside a collection.
    pub(crate) fn display(&mut self, value: &Value) -> Run<String> {
        match value {
            Value::Str(text) => Ok(text.as_str().to_owned()),
            Value::Some(inner) => self.display(inner),
            Value::Instance(instance) if self.is_error(&instance.ty) => self.words(instance),
            other => self.repr(other),
        }
    }

    /// An error's words: its own `def message(self) -> str`, or else the error as it shows.
    pub(crate) fn words(&mut self, instance: &Instance) -> Run<String> {
        if let Some(words) = self.own_words(instance) {
            return words;
        }
        let mut out = String::new();
        self.instance_repr_into(instance, &mut out)?;
        Ok(out)
    }

    /// An error's own words, when it has a `def message`: the program's, or for an error the
    /// oracle models, std's, as crates/syntax/src/stdlib/errors.fen writes them: `IoError`'s by
    /// `io_words`, and `InvalidValue`'s and `ParseError`'s their `message`.
    fn own_words(&mut self, instance: &Instance) -> Option<Run<String>> {
        if let Some(def) = self.program.method(&instance.ty, "message") {
            let receiver = Receiver::Value(Value::Instance(Held::new(instance.clone())));
            return Some(match self.call_def(def, None, Some(receiver), Vec::new(), def.span) {
                Ok(Value::Str(text)) => Ok(text.as_str().to_owned()),
                Ok(other) => unsupported(format!("a `def message` that gave back a {}", other.kind())),
                Err(flow) => Err(flow),
            });
        }
        if instance.ty == "IoError" {
            return Some(io_words(instance));
        }
        let field = match instance.ty.as_str() {
            "InvalidValue" | "ParseError" => "message",
            _ => return None,
        };
        Some(match instance.field(field) {
            Some(Value::Str(words)) => Ok(words.as_str().to_owned()),
            _ => unsupported(format!("an `{}` without its `{field}`", instance.ty)),
        })
    }

    /// How a failure that ends the program is shown after "`main` failed: ": its own words with
    /// it as it is on the next line, or just it, when it has none.
    pub(crate) fn shown_ending_main(&mut self, error: &Value) -> Run<String> {
        let shown = self.repr(error)?;
        let Value::Instance(instance) = error else { return Ok(shown) };
        match self.own_words(instance) {
            Some(words) => Ok(format!("{}\n  {shown}", words?)),
            None => Ok(shown),
        }
    }

    /// A new failure raised inside `except why:` carries `why`'s words as a note. One that was made
    /// already, as `why`, a copy of it, or `why.with_note(...)`, carries what it had. A `fail` in a
    /// function the block calls isn't in the block.
    pub(crate) fn with_cause_noted(&mut self, raised: &fennec_syntax::Expr, error: Value) -> Run<Value> {
        let Some((frame, _, why)) = self.excepts.last().cloned() else { return Ok(error) };
        if frame != self.cur || raised.made_already() {
            return Ok(error);
        }
        let Value::Instance(instance) = error else { return Ok(error) };
        if !self.is_error(&instance.ty) {
            return Ok(Value::Instance(instance));
        }
        let words = self.display(&why)?;
        let mut noted = (*instance).clone();
        noted.notes.push(words);
        Ok(Value::Instance(Held::new(noted)))
    }

    /// A value as it shows inside a collection, strings in quotes.
    pub(crate) fn repr(&mut self, value: &Value) -> Run<String> {
        let mut out = String::new();
        self.repr_into(value, &mut out)?;
        Ok(out)
    }

    /// Writes a value as it shows inside a collection onto `out`. Each part is written onto the
    /// same text, so a value nested deep takes time in proportion to what it prints.
    fn repr_into(&mut self, value: &Value, out: &mut String) -> Run<()> {
        let _deeper = super::deeper()?;
        match value {
            Value::Unit => {}
            Value::None => out.push_str("None"),
            // An optional holding an optional shows as what it holds.
            Value::Some(inner) => self.repr_into(inner, out)?,
            Value::Bool(true) => out.push_str("True"),
            Value::Bool(false) => out.push_str("False"),
            Value::Int(n) => out.push_str(&n.to_string()),
            Value::Sized(_, n) => out.push_str(&n.to_string()),
            Value::Float(x) => out.push_str(&text::float(*x)),
            Value::F32(x) => out.push_str(&text::float32(*x)),
            Value::Str(text) => out.push_str(&text::quoted(text)),
            Value::Bytes(data) => {
                let shown: Vec<String> = data.iter().map(u8::to_string).collect();
                let _ = write!(out, "bytes([{}])", shown.join(", "));
            }
            Value::List(items) => self.joined_into(("[", items, "]"), out)?,
            Value::Tuple(items) if items.len() == 1 => self.joined_into(("(", items, ",)"), out)?,
            Value::Tuple(items) => self.joined_into(("(", items, ")"), out)?,
            Value::Set(items) if items.is_empty() => out.push_str("set()"),
            Value::Set(items) => self.joined_into(("{", items, "}"), out)?,
            Value::Dict(pairs) => {
                out.push('{');
                for (at, (key, value)) in pairs.iter().enumerate() {
                    if at > 0 {
                        out.push_str(", ");
                    }
                    self.repr_into(key, out)?;
                    out.push_str(": ");
                    self.repr_into(value, out)?;
                }
                out.push('}');
            }
            Value::Instance(instance) => self.instance_repr_into(instance, out)?,
            Value::Stream(stream) => match &stream.shown {
                Some(shown) => {
                    let _ = write!(out, "<{shown}>");
                }
                None => return unsupported("printing a generator whose type isn't known"),
            },
            Value::Table(table) => {
                let items: Vec<Value> = table.live().map(|(_, item)| item.clone()).collect();
                self.joined_into(("Table([", &items, "])"), out)?;
            }
            Value::Id(id) => {
                let _ = write!(out, "Id({})", id.slot);
            }
            Value::Func(_) => return unsupported("printing a function"),
            Value::Cap(_) => return unsupported("printing a capability"),
            Value::Shared(_) => return unsupported("printing a `shared` value"),
            Value::Channel(_) => return unsupported("printing a channel"),
            Value::Room(_) => return unsupported(ROOM_UNPROMISED),
            Value::Unknown(why) => return unsupported(why.clone()),
        }
        Ok(())
    }

    /// Writes items between an opening and a closing, each as it shows inside a collection,
    /// with `, ` between them.
    fn joined_into(&mut self, (open, items, close): (&str, &[Value], &str), out: &mut String) -> Run<()> {
        out.push_str(open);
        for (at, item) in items.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            self.repr_into(item, out)?;
        }
        out.push_str(close);
        Ok(())
    }

    fn instance_repr_into(&mut self, instance: &Instance, out: &mut String) -> Run<()> {
        if let Some(def) = self.program.method(&instance.ty, "str") {
            let shown = self.own_str(def, instance)?;
            out.push_str(&shown);
            return Ok(());
        }
        // A generic type's value prints with no type arguments, as it's written, `Holder(item=3)`,
        // whatever `T` is: decided 2026-11-14, after the compiled program was found printing
        // `Holder[int](item=3)` but `Holder(item="x")`.
        let printed_by_std =
            is_core_error(&instance.ty) || matches!(instance.ty.as_str(), "Path" | "Metadata");
        if !self.program.types.contains_key(instance.ty.as_str()) && !printed_by_std {
            return unsupported(format!("printing a {}", instance.ty));
        }
        out.push_str(fennec_syntax::rename::shown_type_name(&instance.ty));
        if let Some(case) = &instance.case {
            out.push('.');
            out.push_str(case);
        }
        if instance.fields.is_empty() && instance.case.is_some() {
            return Ok(());
        }
        out.push('(');
        for (at, (field, value)) in instance.fields.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            out.push_str(field);
            out.push('=');
            self.repr_into(value, out)?;
        }
        out.push(')');
        Ok(())
    }

    /// A type's own `def str(self, ref out: StrWriter)`, called with a writer of its own.
    fn own_str(&mut self, def: &'static fennec_syntax::FnDef, instance: &Instance) -> Run<String> {
        let writer = Value::instance(
            "StrWriter",
            None,
            vec![("_parts".to_owned(), Value::List(Held::new(Vec::new())))],
        );
        let previous = self.push_frame(vec![("out".to_owned(), Binding::Value(writer))], None)?;
        let place = Place { frame: self.cur, name: "out".to_owned(), path: Vec::new(), lent: 0 };
        let receiver = Receiver::Value(Value::Instance(Held::new(instance.clone())));
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
        let _deeper = super::deeper()?;
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
                if !a.notes.is_empty() || !b.notes.is_empty() {
                    return unsupported("`==` on errors that carry notes");
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
            (Value::Shared(a), Value::Shared(b)) => self.shared_equal(a, b),
            // An optional `shared` value that's there is never equal to one that's missing.
            (Value::Shared(_), Value::None) | (Value::None, Value::Shared(_)) => Ok(false),
            _ => plain_equal(left, right),
        }
    }

    /// Two `shared` values compare by what they hold, as `==` takes both their locks in a fixed
    /// order and compares the values (decided 2026-11-14), with the type's own `def ==` where it
    /// has one. That holds for a cell compared with itself too: there's no shortcut, so a `def ==`
    /// that says no is what `a == a` gives, as native code gives. A cell a `lock` holds now is
    /// refused, since `==` would wait there for that lock.
    fn shared_equal(&mut self, a: &Cell, b: &Cell) -> Run<bool> {
        let locked = std::sync::atomic::Ordering::SeqCst;
        if a.locked.load(locked) || b.locked.load(locked) {
            return unsupported("comparing a `shared` value a `lock` holds");
        }
        let (Some(x), Some(y)) = (held_now(a), held_now(b)) else {
            return unsupported("comparing a `shared` value whose cell couldn't be read");
        };
        self.equal(&x, &y)
    }

    /// The order of two values, with a type's own `def <` where it has one.
    pub(crate) fn compare(&mut self, left: &Value, right: &Value) -> Run<Ordering> {
        let _deeper = super::deeper()?;
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
            (Value::Sized(_, a), Value::Sized(_, b)) => Ok(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => Ok(float_order(*a, *b)),
            (Value::F32(a), Value::F32(b)) => Ok(float_order(f64::from(*a), f64::from(*b))),
            (Value::Str(a), Value::Str(b)) => Ok(a.cmp(b)),
            (Value::Bytes(a), Value::Bytes(b)) => Ok(a.cmp(b)),
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

/// An `IoError`'s words, as its `def message` in crates/syntax/src/stdlib/errors.fen says them:
/// the path it was at, and for `PermissionDenied` and `Other` its reason.
pub(crate) fn io_words(instance: &Instance) -> Run<String> {
    let text = |field: &str| match instance.field(field) {
        Some(Value::Str(text)) => Ok(text.as_str().to_owned()),
        _ => unsupported(format!("an `IoError` without its `{field}`")),
    };
    Ok(match instance.case.as_deref() {
        Some("NotFound") => match text("path")? {
            path if path.is_empty() => "nothing is there".to_owned(),
            path => format!("nothing is at `{path}`"),
        },
        Some("PermissionDenied") => match (text("path")?, text("reason")?) {
            (path, reason) if path.is_empty() => reason,
            (path, reason) => format!("`{path}`: {reason}"),
        },
        Some("AlreadyExists") => match text("path")? {
            path if path.is_empty() => "something is there already".to_owned(),
            path => format!("something is at `{path}` already"),
        },
        _ => text("reason")?,
    })
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

/// A copy of what a `shared` cell holds, taken with its lock held only for the copy.
fn held_now(cell: &Cell) -> Option<Value> {
    cell.held.lock().ok().map(|value| value.clone())
}

/// Equality with no type's own `def ==`: what a dict's keys and a set's items are told apart by.
pub(crate) fn plain_equal(left: &Value, right: &Value) -> Run<bool> {
    let _deeper = super::deeper()?;
    Ok(match (left, right) {
        (Value::Unit, Value::Unit) | (Value::None, Value::None) => true,
        (Value::Some(a), Value::Some(b)) => plain_equal(a, b)?,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Sized(x, a), Value::Sized(y, b)) => x == y && a == b,
        (Value::Float(a), Value::Float(b)) => float_order(*a, *b) == Ordering::Equal,
        (Value::F32(a), Value::F32(b)) => float_order(f64::from(*a), f64::from(*b)) == Ordering::Equal,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Bytes(a), Value::Bytes(b)) => a == b,
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
            // Whether `==` reads an error's notes isn't measured: errors that carry any are
            // refused rather than guessed at.
            if !a.notes.is_empty() || !b.notes.is_empty() {
                return unsupported("`==` on errors that carry notes");
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
        (Value::Room(_), _) | (_, Value::Room(_)) => return unsupported(ROOM_UNPROMISED),
        (Value::Shared(_) | Value::Channel(_), _) | (_, Value::Shared(_) | Value::Channel(_)) => {
            return unsupported("comparing `shared` values or channels");
        }
        _ => false,
    })
}
