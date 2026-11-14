//! The builtin functions: `len`, `sum`, `sorted` and the rest, each modeled here on its own.

use std::cmp::Ordering;

use fennec_check::{Num, Type};
use fennec_syntax::{Expr, ExprKind, Span};

use super::calls::ArgVal;
use super::expr::{set_insert, sized};
use super::stream::Source;
use super::{Flow, Interp, MOST_ITEMS, Run, as_int, crash, unsupported};
use crate::text;
use crate::value::{Cap, Value};

/// The arguments of a builtin call, by position and by name.
struct Given {
    positional: Vec<ArgVal>,
    named: Vec<(String, ArgVal)>,
}

impl Given {
    fn of(given: Vec<ArgVal>) -> Given {
        let mut positional = Vec::new();
        let mut named = Vec::new();
        for arg in given {
            match arg.name().map(str::to_owned) {
                Some(name) => named.push((name, arg)),
                None => positional.push(arg),
            }
        }
        Given { positional, named }
    }

    fn take_named(&mut self, name: &str) -> Option<ArgVal> {
        let at = self.named.iter().position(|(given, _)| given == name)?;
        Some(self.named.remove(at).1)
    }
}

impl Interp {
    #[expect(clippy::too_many_lines, reason = "one arm for each builtin")]
    pub(crate) fn builtin(&mut self, name: &str, given: Vec<ArgVal>, expr: &'static Expr) -> Run<Value> {
        let span = expr.span;
        if name.starts_with("std.") {
            let mut args = Vec::with_capacity(given.len());
            for arg in given {
                if arg.name().is_some() {
                    return unsupported(format!("{name} with a named argument"));
                }
                args.push(self.arg_as_value(arg)?);
            }
            return super::library::call(name, &args, span);
        }
        let mut given = Given::of(given);
        match name {
            "len" => {
                let value = self.one(&mut given, name)?;
                self.length(value, span).map(Value::Int)
            }
            "abs" => match self.one(&mut given, name)? {
                Value::Int(n) => n
                    .checked_abs()
                    .map_or_else(|| crash("this calculation overflows an int", span), |n| Ok(Value::Int(n))),
                Value::Float(x) => Ok(Value::Float(x.abs())),
                Value::Sized(num, n) => sized(num, n.abs(), span),
                Value::Instance(instance) => match self.program.method(&instance.ty, "abs") {
                    Some(def) => {
                        let receiver = super::calls::Receiver::Value(Value::Instance(instance));
                        self.call_def(def, None, Some(receiver), Vec::new(), span)
                    }
                    None => unsupported(format!("`abs` of a {}", instance.ty)),
                },
                other => unsupported(format!("`abs` of a {}", other.kind())),
            },
            "str" => {
                let value = self.one(&mut given, name)?;
                self.display(&value).map(Value::Str)
            }
            "int" | "i64" => match self.one(&mut given, name)? {
                Value::Int(n) => Ok(Value::Int(n)),
                Value::Float(x) => float_to_int(x, span).map(Value::Int),
                Value::Str(text) => parsed_int(&text, span),
                Value::Sized(_, n) => match i64::try_from(n) {
                    Ok(n) => Ok(Value::Int(n)),
                    Err(_) => crash("this doesn't fit in an int", span),
                },
                other => unsupported(format!("`int` of a {}", other.kind())),
            },
            "float" | "f64" => match self.one(&mut given, name)? {
                Value::Int(n) => Ok(Value::Float(text::int_as_float(n))),
                Value::Float(x) => Ok(Value::Float(x)),
                Value::Str(text) => parsed_float(&text, span),
                Value::Sized(num, n) if !num.is_float() => Ok(Value::Float(super::expr::int128_as_float(n))),
                other => unsupported(format!("`float` of a {}", other.kind())),
            },
            "i8" | "i16" | "i32" | "u8" | "u16" | "u32" | "u64" => {
                let Some(num) = Num::named(name) else { return unsupported(name.to_owned()) };
                match self.one(&mut given, name)? {
                    Value::Int(n) => fitting(num, i128::from(n), span),
                    Value::Sized(_, n) => fitting(num, n, span),
                    other => unsupported(format!("`{name}` of a {}", other.kind())),
                }
            }
            "range" => {
                let mut bounds = Vec::new();
                for arg in given.positional {
                    match self.arg_as_value(arg)? {
                        Value::Int(n) => bounds.push(n),
                        other => return unsupported(format!("`range` over a {}", other.kind())),
                    }
                }
                let (first, end, by) = match bounds.as_slice() {
                    [end] => (0, *end, 1),
                    [first, end] => (*first, *end, 1),
                    [first, end, by] => (*first, *end, *by),
                    _ => return unsupported("`range` with these arguments"),
                };
                range(first, end, by, span)
            }
            "enumerate" => {
                if !given.named.is_empty() {
                    return unsupported("`enumerate` with a named argument");
                }
                let over = Self::first(&mut given, name)?;
                if over.is_lazy() {
                    let source = self.source_of_arg(over, span)?;
                    return Ok(Interp::stream_of(Source::Numbered(Box::new(source), 0), None));
                }
                let items = self.collect(over, span)?;
                Ok(Value::List(
                    items
                        .into_iter()
                        .enumerate()
                        .map(|(at, item)| Value::Tuple(vec![Value::Int(as_int(at)), item]))
                        .collect(),
                ))
            }
            "zip" => {
                let left = Self::first(&mut given, name)?;
                let right = Self::first(&mut given, name)?;
                if left.is_lazy() || right.is_lazy() {
                    let left = self.source_of_arg(left, span)?;
                    let right = self.source_of_arg(right, span)?;
                    return Ok(Interp::stream_of(Source::Paired(Box::new(left), Box::new(right)), None));
                }
                let left = self.collect(left, span)?;
                let right = self.collect(right, span)?;
                Ok(Value::List(left.into_iter().zip(right).map(|(a, b)| Value::Tuple(vec![a, b])).collect()))
            }
            "sorted" => {
                let over = Self::first(&mut given, name)?;
                let key = given.take_named("key").map(|key| self.arg_as_value(key)).transpose()?;
                let reverse =
                    match given.take_named("reverse").map(|arg| self.arg_as_value(arg)).transpose()? {
                        None => false,
                        Some(Value::Bool(reverse)) => reverse,
                        Some(_) => return unsupported("`sorted` with a `reverse` that isn't a bool"),
                    };
                let items = self.collect(over, span)?;
                self.sorted(items, key.as_ref(), reverse, expr).map(Value::List)
            }
            "reversed" => match self.one(&mut given, name)? {
                Value::List(mut items) => {
                    items.reverse();
                    Ok(Value::List(items))
                }
                Value::Tuple(mut items) => {
                    items.reverse();
                    Ok(Value::Tuple(items))
                }
                Value::Str(text) => Ok(Value::Str(text.chars().rev().collect())),
                other => unsupported(format!("`reversed` of a {}", other.kind())),
            },
            "min" | "max" => self.extreme(name == "max", given, expr),
            "sum" => self.sum(given, expr),
            "any" | "all" => {
                let want = name == "any";
                let over = Self::first(&mut given, name)?;
                let mut found = !want;
                self.each_in(over, span, &mut |_, item| match item {
                    Value::Bool(truth) if truth == want => {
                        found = want;
                        Ok(false)
                    }
                    Value::Bool(_) => Ok(true),
                    other => unsupported(format!("`any` or `all` over a {}", other.kind())),
                })?;
                Ok(Value::Bool(found))
            }
            "list" => {
                let over = Self::first(&mut given, name)?;
                self.collect(over, span).map(Value::List)
            }
            "set" => {
                if given.positional.is_empty() {
                    return Ok(Value::Set(Vec::new()));
                }
                let over = Self::first(&mut given, name)?;
                let mut set = Vec::new();
                for item in self.collect(over, span)? {
                    set_insert(&mut set, item)?;
                }
                Ok(Value::Set(set))
            }
            "copy" => {
                let Value::Instance(mut instance) = self.one_keeping_named(&mut given, name)? else {
                    return unsupported("`copy` of something that isn't a struct");
                };
                for (field, arg) in std::mem::take(&mut given.named) {
                    let value = self.arg_as_value(arg)?;
                    match instance.field_mut(&field) {
                        Some(slot) => *slot = value,
                        None => return unsupported(format!("`copy` with the field `{field}`")),
                    }
                }
                self.ruled(*instance, span)
            }
            "round" => {
                let value = Self::first(&mut given, name)?;
                let value = self.arg_as_value(value)?;
                let places = match given.positional.pop() {
                    Some(arg) => Some(self.arg_as_value(arg)?),
                    None => given.take_named("ndigits").map(|arg| self.arg_as_value(arg)).transpose()?,
                };
                match (value, places) {
                    (Value::Float(x), None) => float_to_int(x.round_ties_even(), span).map(Value::Int),
                    (Value::Float(x), Some(Value::Int(places))) => Ok(Value::Float(round_places(x, places))),
                    _ => unsupported("`round` of something that isn't a float"),
                }
            }
            other => unsupported(format!("the builtin `{other}`")),
        }
    }

    /// The one argument a builtin takes, as a value.
    fn one(&mut self, given: &mut Given, name: &str) -> Run<Value> {
        if given.positional.len() != 1 || !given.named.is_empty() {
            return unsupported(format!("`{name}` with these arguments"));
        }
        self.one_keeping_named(given, name)
    }

    fn one_keeping_named(&mut self, given: &mut Given, name: &str) -> Run<Value> {
        let arg = Self::first(given, name)?;
        self.arg_as_value(arg)
    }

    /// The first argument by position.
    fn first(given: &mut Given, name: &str) -> Run<ArgVal> {
        if given.positional.is_empty() {
            return unsupported(format!("`{name}` with no argument"));
        }
        Ok(given.positional.remove(0))
    }

    /// How many items something holds, as `len` says.
    pub(crate) fn length(&mut self, value: Value, span: Span) -> Run<i64> {
        Ok(as_int(match value {
            Value::Str(text) => text.chars().count(),
            Value::List(items) | Value::Tuple(items) | Value::Set(items) => items.len(),
            Value::Dict(pairs) => pairs.len(),
            Value::Table(table) => table.len(),
            Value::Cap(Cap::Args) => 0,
            Value::Instance(instance) => match self.program.method(&instance.ty, "len") {
                Some(def) => {
                    let receiver = super::calls::Receiver::Value(Value::Instance(instance));
                    return match self.call_def(def, None, Some(receiver), Vec::new(), span)? {
                        Value::Int(n) => Ok(n),
                        _ => unsupported("a `def len` that doesn't give an int"),
                    };
                }
                None => return unsupported(format!("`len` of a {}", instance.ty)),
            },
            other => return unsupported(format!("`len` of a {}", other.kind())),
        }))
    }

    /// Runs `each` on each item an argument goes over, until it says to stop.
    fn each_in(
        &mut self,
        arg: ArgVal,
        span: Span,
        each: &mut dyn FnMut(&mut Interp, Value) -> Run<bool>,
    ) -> Run<()> {
        if let ArgVal::Lazy(_, expr) = arg {
            let ExprKind::Comprehension { elem, names, iter, cond, .. } = &expr.kind else {
                return unsupported("a generator that isn't one");
            };
            return self.each_of(elem, names, iter, cond.as_deref(), each);
        }
        let over = self.arg_as_value(arg)?;
        let mut items = self.items_of(over, span)?;
        while let Some(item) = self.next_item(&mut items)? {
            if !each(self, item)? {
                break;
            }
        }
        Ok(())
    }

    /// Every item an argument goes over.
    fn collect(&mut self, arg: ArgVal, span: Span) -> Run<Vec<Value>> {
        let mut all = Vec::new();
        self.each_in(arg, span, &mut |_, item| {
            all.push(item);
            if all.len() > MOST_ITEMS {
                return unsupported("ran too long: too many items");
            }
            Ok(true)
        })?;
        Ok(all)
    }

    /// Where `enumerate` or `zip` reads an argument's items from, as they're asked for.
    fn source_of_arg(&mut self, arg: ArgVal, span: Span) -> Run<Source> {
        let value = match arg {
            ArgVal::Lazy(_, expr) => self.generator_of(expr)?,
            other => self.arg_as_value(other)?,
        };
        self.source_of(value, span)
    }

    /// Sorted, steadily: items that order the same keep their order, the other way round too.
    fn sorted(
        &mut self,
        items: Vec<Value>,
        key: Option<&Value>,
        reverse: bool,
        expr: &'static Expr,
    ) -> Run<Vec<Value>> {
        let mut keyed = Vec::with_capacity(items.len());
        for item in items {
            let by = match key {
                Some(key) => self.call_value(key, vec![item.clone()], expr)?,
                None => item.clone(),
            };
            keyed.push((by, item));
        }
        let sorted = self.merge_sort(keyed, reverse)?;
        Ok(sorted.into_iter().map(|(_, item)| item).collect())
    }

    /// A merge sort, which never puts an item before an equal one that came before it.
    pub(crate) fn merge_sort(
        &mut self,
        items: Vec<(Value, Value)>,
        reverse: bool,
    ) -> Run<Vec<(Value, Value)>> {
        if items.len() <= 1 {
            return Ok(items);
        }
        let mut left = items;
        let right = left.split_off(left.len() / 2);
        let left = self.merge_sort(left, reverse)?;
        let right = self.merge_sort(right, reverse)?;
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let mut left = left.into_iter().peekable();
        let mut right = right.into_iter().peekable();
        loop {
            let take_right = match (left.peek(), right.peek()) {
                (Some((a, _)), Some((b, _))) => {
                    let order = self.compare(a, b)?;
                    if reverse { order == Ordering::Less } else { order == Ordering::Greater }
                }
                (Some(_), None) => false,
                (None, Some(_)) => true,
                (None, None) => break,
            };
            let next = if take_right { right.next() } else { left.next() };
            merged.extend(next);
        }
        Ok(merged)
    }

    /// `min` or `max`: of an argument's items, or of several arguments, by `key=` if given. The
    /// first of equal ones wins.
    fn extreme(&mut self, most: bool, mut given: Given, expr: &'static Expr) -> Run<Value> {
        let key = given.take_named("key").map(|key| self.arg_as_value(key)).transpose()?;
        if !given.named.is_empty() {
            return unsupported("`min` or `max` with a named argument");
        }
        let items = if given.positional.len() == 1 {
            let over = Self::first(&mut given, "max")?;
            self.collect(over, expr.span)?
        } else {
            let mut items = Vec::new();
            for arg in given.positional {
                items.push(self.arg_as_value(arg)?);
            }
            items
        };
        let mut best: Option<(Value, Value)> = None;
        for item in items {
            let by = match &key {
                Some(key) => self.call_value(key, vec![item.clone()], expr)?,
                None => item.clone(),
            };
            let better = match &best {
                None => true,
                Some((held, _)) => {
                    let order = self.compare(&by, held)?;
                    if most { order == Ordering::Greater } else { order == Ordering::Less }
                }
            };
            if better {
                best = Some((by, item));
            }
        }
        match best {
            Some((_, item)) => Ok(item),
            None => crash(format!("`{}` of nothing", if most { "max" } else { "min" }), expr.span),
        }
    }

    /// `sum`: ints added one at a time and checked, sized numbers in their size, and floats into
    /// eight running totals, item `at` into total `at % 8`, added pairwise at the end, as every
    /// engine adds them.
    fn sum(&mut self, mut given: Given, expr: &'static Expr) -> Run<Value> {
        let over = Self::first(&mut given, "sum")?;
        let start = match given.positional.pop() {
            Some(start) => Some(self.arg_as_value(start)?),
            None => given.take_named("start").map(|start| self.arg_as_value(start)).transpose()?,
        };
        let floats = match self.program.type_at(expr.span) {
            Some(Type::Float) => true,
            Some(Type::Sized(num)) if num.is_float() => return unsupported("f32"),
            Some(Type::Int | Type::Sized(_)) => false,
            // Anything else is added with its `+`, from the start it's given.
            _ if start.is_some() => false,
            _ => return unsupported("`sum` of something other than numbers, with no start"),
        };
        let span = expr.span;
        if floats {
            let mut lanes = [0.0_f64; 8];
            let mut at = 0_usize;
            self.each_in(over, span, &mut |_, item| match item {
                Value::Float(x) => {
                    if let Some(lane) = lanes.get_mut(at % 8) {
                        *lane += x;
                    }
                    at += 1;
                    Ok(true)
                }
                other => unsupported(format!("`sum` of a {} among floats", other.kind())),
            })?;
            for (into, from) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (4, 6), (0, 4)] {
                let added = lanes.get(from).copied().unwrap_or(0.0);
                if let Some(lane) = lanes.get_mut(into) {
                    *lane += added;
                }
            }
            let start = match start {
                None => 0.0,
                Some(Value::Float(x)) => x,
                Some(_) => return unsupported("`sum` of floats from something else"),
            };
            return Ok(Value::Float(start + lanes.first().copied().unwrap_or(0.0)));
        }
        let mut total = start;
        self.each_in(over, span, &mut |interp, item| {
            total = Some(match total.take() {
                None => item,
                Some(so_far) => interp.binary_values(fennec_syntax::BinOp::Add, so_far, item, span)?,
            });
            Ok(true)
        })?;
        match (total, self.program.type_at(expr.span)) {
            (Some(total), _) => Ok(total),
            (None, Some(Type::Sized(num))) => Ok(Value::Sized(*num, 0)),
            (None, _) => Ok(Value::Int(0)),
        }
    }
}

impl ArgVal {
    /// Whether an argument's items are made as they're asked for.
    fn is_lazy(&self) -> bool {
        match self {
            ArgVal::Lazy(..) => true,
            ArgVal::Value(_, value) => Interp::is_stream(value),
            ArgVal::Ref(..) => false,
        }
    }
}

/// `range(first, end, by)`, as a list.
fn range(first: i64, end: i64, by: i64, span: Span) -> Run<Value> {
    if by == 0 {
        return crash("`range` can't step by 0", span);
    }
    let count = if by > 0 {
        (i128::from(end) - i128::from(first) + i128::from(by) - 1) / i128::from(by)
    } else {
        (i128::from(first) - i128::from(end) - i128::from(by) - 1) / -i128::from(by)
    };
    let count = usize::try_from(count.max(0)).unwrap_or(usize::MAX);
    if count > MOST_ITEMS {
        return unsupported("ran too long: a range too long");
    }
    let mut items = Vec::with_capacity(count);
    let mut at = i128::from(first);
    for _ in 0..count {
        items.push(Value::Int(i64::try_from(at).unwrap_or(0)));
        at += i128::from(by);
    }
    Ok(Value::List(items))
}

/// A sized number made from another, which has to fit: one that doesn't is a bug.
fn fitting(num: Num, n: i128, span: Span) -> Run<Value> {
    let Some((least, most)) = num.range() else { return unsupported("f32") };
    if (least..=most).contains(&n) {
        Ok(Value::Sized(num, n))
    } else {
        crash(format!("{n} doesn't fit in a {}", num.name()), span)
    }
}

/// A float's whole part as an int, which has to be one.
#[expect(clippy::cast_possible_truncation, reason = "the float is checked to be inside an int's range first")]
pub(crate) fn float_to_int(x: f64, span: Span) -> Run<i64> {
    let whole = x.trunc();
    if !whole.is_finite() || whole < -9_223_372_036_854_775_808.0 || whole >= 9_223_372_036_854_775_808.0 {
        return crash("can't turn a float into an int", span);
    }
    Ok(whole as i64)
}

/// `int(text)`: the int it writes, with the whitespace around it ignored, or an `InvalidValue`.
fn parsed_int(text: &str, span: Span) -> Run<Value> {
    match text.trim().parse::<i64>() {
        Ok(n) => Ok(Value::Int(n)),
        Err(_) => Err(not_a_number(text, "an int", span)),
    }
}

fn parsed_float(text: &str, span: Span) -> Run<Value> {
    match text.trim().parse::<f64>() {
        Ok(x) => Ok(Value::Float(x)),
        Err(_) => Err(not_a_number(text, "a float", span)),
    }
}

/// The `InvalidValue` text that isn't a number fails with, quoting text from outside whole or,
/// past 40 characters, its first 20.
pub(crate) fn not_a_number(input: &str, what: &str, span: Span) -> Flow {
    let length = input.chars().count();
    let quoted = if length <= 40 {
        text::quoted(input)
    } else {
        let start: String = input.chars().take(20).collect();
        let mut count = length.to_string();
        let mut grouped = String::new();
        while count.len() > 3 {
            let tail = count.split_off(count.len() - 3);
            grouped = format!(",{tail}{grouped}");
        }
        format!("\"{}…\" ({count}{grouped} characters)", start.escape_debug())
    };
    invalid(format!("{quoted} isn't {what}"), span)
}

/// A failure with an `InvalidValue` saying `message`.
pub(crate) fn invalid(message: String, span: Span) -> Flow {
    let error = Value::instance("InvalidValue", None, vec![("message".to_owned(), Value::Str(message))]);
    Flow::Fail(error, span)
}

/// `round(x, places)`, by the number `x` really is, as the runtime rounds it.
#[expect(clippy::cast_possible_truncation, reason = "places is checked to be small first")]
fn round_places(x: f64, places: i64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if places < 0 {
        let power = places.checked_neg().and_then(|power| i32::try_from(power).ok());
        return match power.filter(|power| *power <= 308) {
            Some(power) => {
                let scale = 10_f64.powi(power);
                (x / scale).round_ties_even() * scale
            }
            None => 0.0_f64.copysign(x),
        };
    }
    if places > 1100 {
        return x;
    }
    if places <= 22 {
        let scale = 10_f64.powi(places as i32);
        let scaled = x * scale;
        if scaled.abs() < 4_503_599_627_370_496.0 && (scaled.fract().abs() - 0.5).abs() > 1e-6 {
            return scaled.round_ties_even() / scale;
        }
    }
    let digits = usize::try_from(places).unwrap_or(0);
    format!("{x:.digits$}").parse().unwrap_or(x)
}
