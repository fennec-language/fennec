//! The engines' builtins a constant can call while compiling: conversions, the math functions,
//! and the plain methods of strs, lists, sets and dicts. Each does what the native one does.
//! Any other is left for the program to work out when it starts.

use std::cmp::Ordering;
use std::rc::Rc;

use fennec_syntax::Span;

use super::value::Value;
use super::{Machine, Ran, Stop, fail, not_here, text};

/// What a builtin gives, and the new value of its first argument when it changes that.
type Called = (Value, Option<Value>);

/// 2^63: a float converts to an int when it's at least -2^63 and below 2^63.
const INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// Calls the builtin `name` with these arguments, its object first for a method.
pub(super) fn call(machine: &mut Machine<'_>, name: &str, args: &[Value], span: Span) -> Ran<Called> {
    if let Some(changed) = changing(machine, name, args)? {
        return Ok(changed);
    }
    if let Some(worked) = math_call(name, args, span) {
        return worked.map(|value| (value, None));
    }
    let value = match (name, args) {
        ("PI", []) => Value::Float(std::f64::consts::PI),
        ("copy", [value]) => value.clone(),
        ("Ok", [value]) => Value::Ok(Rc::new(value.clone())),
        ("Err", [value]) => Value::Err(Rc::new(value.clone())),
        ("float", [Value::Int(n)]) => Value::Float(to_float(*n)),
        ("float", [Value::Float(x)]) => Value::Float(*x),
        ("int", [Value::Int(n)]) => Value::Int(*n),
        ("int", [Value::Float(x)]) => to_int(*x)?,
        ("int", [Value::Bool(b)]) => Value::Int(i64::from(*b)),
        ("str", [value]) => match text::shown(value) {
            Some(shown) => Value::str(&shown),
            None => return not_here("`str` of this value"),
        },
        ("round", [Value::Float(x)]) => to_int(x.round_ties_even())?,
        ("abs" | "int.abs", [Value::Int(n)]) => {
            n.checked_abs().map(Value::Int).ok_or_else(|| super::overflow(span))?
        }
        ("abs" | "float.abs", [Value::Float(x)]) => Value::Float(x.abs()),
        (
            "len" | "list.len" | "str.len" | "dict.len" | "set.len" | "bytes.len" | "tuple.len" | "array.len",
            [value],
        ) => match value.count() {
            Some(len) => Value::Int(i64::try_from(len).unwrap_or(i64::MAX)),
            None => return not_here(format!("`{name}` of this value")),
        },
        ("range", bounds) => range(machine, bounds)?,
        ("list", [items]) => Value::list(items_of(items)?),
        ("set", []) => Value::set([]),
        ("set", [items]) => Value::set(items_of(items)?),
        ("reversed", [Value::Str(text)]) => Value::str(&text.chars().rev().collect::<String>()),
        ("reversed", [items]) => Value::list(items_of(items)?.into_iter().rev().collect()),
        ("enumerate", [items]) => Value::list(
            items_of(items)?
                .into_iter()
                .zip(0_i64..)
                .map(|(item, at)| Value::Tuple(Rc::new(vec![Value::Int(at), item])))
                .collect(),
        ),
        ("zip", [first, second]) => Value::list(
            items_of(first)?
                .into_iter()
                .zip(items_of(second)?)
                .map(|(a, b)| Value::Tuple(Rc::new(vec![a, b])))
                .collect(),
        ),
        ("any" | "all", [items]) => {
            let mut truths = Vec::new();
            for item in items_of(items)? {
                let Value::Bool(truth) = item else {
                    return not_here(format!("`{name}` of what isn't bools"));
                };
                truths.push(truth);
            }
            Value::Bool(if name == "any" {
                truths.contains(&true)
            } else {
                truths.iter().all(|truth| *truth)
            })
        }
        ("sum", [items]) => sum(items_of(items)?, None, span)?,
        ("sum" | "sum.start", [items, start]) => sum(items_of(items)?, Some(start.clone()), span)?,
        ("min" | "max", [items]) => extreme(name, items_of(items)?)?,
        ("min" | "max", [first, second, ..])
            if !matches!(first, Value::List(_)) || matches!(second, Value::List(_)) =>
        {
            extreme(name, args.to_vec())?
        }
        ("sorted", [items]) => {
            let mut items = items_of(items)?;
            sort(&mut items)?;
            Value::list(items)
        }
        ("sorted.key", [items, key]) => {
            let items = items_of(items)?;
            let mut keyed = Vec::new();
            for item in items {
                let found = machine.call_closure(key, vec![item.clone()])?;
                keyed.push((found, item));
            }
            sort_keyed(&mut keyed)?;
            Value::list(keyed.into_iter().map(|(_, item)| item).collect())
        }
        ("dict.get", [Value::Dict(pairs), key]) => pairs.get(key).cloned().unwrap_or(Value::None),
        ("dict.get", [Value::Dict(pairs), key, default]) => {
            pairs.get(key).cloned().unwrap_or_else(|| default.clone())
        }
        ("dict.contains", [Value::Dict(pairs), key]) => Value::Bool(pairs.contains(key)),
        ("set.contains", [Value::Set(items), key]) => Value::Bool(items.contains(key)),
        ("list.contains", [Value::List(items), item]) => Value::Bool(items.contains(item)),
        ("dict.keys" | "keys", [Value::Dict(pairs)]) => Value::list(pairs.keys().cloned().collect()),
        ("dict.values" | "values", [Value::Dict(pairs)]) => {
            Value::list(pairs.pairs.iter().map(|(_, value)| value.clone()).collect())
        }
        ("dict.items" | "items", [Value::Dict(pairs)]) => Value::list(
            pairs
                .pairs
                .iter()
                .map(|(key, value)| Value::Tuple(Rc::new(vec![key.clone(), value.clone()])))
                .collect(),
        ),
        _ => return strs(machine, name, args),
    };
    Ok((value, None))
}

/// The builtins that change their first argument, giving it back changed.
fn changing(machine: &mut Machine<'_>, name: &str, args: &[Value]) -> Ran<Option<Called>> {
    let mut args = args.iter().cloned();
    let (Some(first), rest) = (args.next(), args.collect::<Vec<_>>()) else {
        return Ok(None);
    };
    let changed = match (name, first, rest.as_slice()) {
        ("list.append", Value::List(mut items), [item]) => {
            Rc::make_mut(&mut items).push(item.clone());
            (Value::Unit, Value::List(items))
        }
        ("list.extend", Value::List(mut items), [more]) => {
            let more = items_of(more)?;
            machine.charge(more.len())?;
            Rc::make_mut(&mut items).extend(more);
            (Value::Unit, Value::List(items))
        }
        ("list.insert", Value::List(mut items), [Value::Int(at), item]) => {
            let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
            let place = if *at < 0 { at.saturating_add(length) } else { *at }.clamp(0, length);
            Rc::make_mut(&mut items).insert(usize::try_from(place).unwrap_or(0), item.clone());
            (Value::Unit, Value::List(items))
        }
        ("list.sort", Value::List(mut items), []) => {
            sort(Rc::make_mut(&mut items).as_mut_slice())?;
            (Value::Unit, Value::List(items))
        }
        ("list.reverse", Value::List(mut items), []) => {
            Rc::make_mut(&mut items).reverse();
            (Value::Unit, Value::List(items))
        }
        ("set.add", Value::Set(mut items), [item]) => {
            Rc::make_mut(&mut items).insert(item.clone(), Value::Unit);
            (Value::Unit, Value::Set(items))
        }
        ("set.update", Value::Set(mut items), [more]) => {
            for item in items_of(more)? {
                Rc::make_mut(&mut items).insert(item, Value::Unit);
            }
            (Value::Unit, Value::Set(items))
        }
        ("dict.update", Value::Dict(mut pairs), [Value::Dict(more)]) => {
            for (key, value) in &more.pairs {
                Rc::make_mut(&mut pairs).insert(key.clone(), value.clone());
            }
            (Value::Unit, Value::Dict(pairs))
        }
        _ => return Ok(None),
    };
    Ok(Some((changed.0, Some(changed.1))))
}

/// The plain methods of strs.
fn strs(machine: &mut Machine<'_>, name: &str, args: &[Value]) -> Ran<Called> {
    let Some((Value::Str(text), rest)) = args.split_first() else {
        return not_here(format!("`{name}`"));
    };
    let value = match (name, rest) {
        ("str.upper", []) => Value::str(&text.to_uppercase()),
        ("str.lower", []) => Value::str(&text.to_lowercase()),
        ("str.strip", []) => Value::str(text.trim()),
        ("str.lstrip", []) => Value::str(text.trim_start()),
        ("str.rstrip", []) => Value::str(text.trim_end()),
        ("str.contains", [Value::Str(part)]) => Value::Bool(text.contains(part.as_ref())),
        ("str.startswith" | "str.starts_with", [Value::Str(part)]) => {
            Value::Bool(text.starts_with(part.as_ref()))
        }
        ("str.endswith" | "str.ends_with", [Value::Str(part)]) => Value::Bool(text.ends_with(part.as_ref())),
        ("str.replace", [Value::Str(old), Value::Str(new)]) if !old.is_empty() => {
            Value::str(&text.replace(old.as_ref(), new))
        }
        ("str.split", []) => Value::list(text.split_whitespace().map(Value::str).collect()),
        ("str.split", [Value::Str(by)]) if !by.is_empty() => {
            Value::list(text.split(by.as_ref()).map(Value::str).collect())
        }
        ("str.join", [items]) => {
            let mut parts = Vec::new();
            for item in items_of(items)? {
                let Value::Str(part) = item else { return not_here("joining what isn't strs") };
                parts.push(part);
            }
            let joined = parts.iter().map(AsRef::as_ref).collect::<Vec<&str>>().join(text);
            machine.charge(joined.len() / 16)?;
            Value::str(&joined)
        }
        _ => return not_here(format!("`{name}`")),
    };
    Ok((value, None))
}

/// The math functions, through the same library the engines use, so the bits are the same.
fn math(name: &str, x: f64, span: Span) -> Ran<Value> {
    Ok(Value::Float(match name {
        "math.floor" => return to_int(x.floor()),
        "math.ceil" => return to_int(x.ceil()),
        "math.sin" => libm::sin(x),
        "math.cos" => libm::cos(x),
        "math.tan" => libm::tan(x),
        "math.acos" => libm::acos(x),
        "math.exp" => libm::exp(x),
        "math.log" => libm::log(x),
        "math.sqrt" if x < 0.0 => return fail("can't take the square root of a negative number", span),
        "math.sqrt" => x.sqrt(),
        _ => return not_here(format!("`{name}`")),
    }))
}

/// Calls a math function, if `name` is one.
fn math_call(name: &str, args: &[Value], span: Span) -> Option<Ran<Value>> {
    if !name.starts_with("math.") {
        return None;
    }
    let x = match args {
        [Value::Float(x)] => *x,
        [Value::Int(n)] => to_float(*n),
        _ => return Some(not_here(format!("`{name}` of this"))),
    };
    Some(math(name, x, span))
}

#[expect(clippy::cast_precision_loss, reason = "int to float is allowed to round, as in Python")]
pub(super) fn to_float(n: i64) -> f64 {
    n as f64
}

/// A float as an int, toward zero. One out of an int's range is left for the program.
fn to_int(x: f64) -> Ran<Value> {
    if !(-INT_LIMIT..INT_LIMIT).contains(&x) {
        return not_here("a float too big for an int");
    }
    #[expect(clippy::cast_possible_truncation, reason = "int() truncates toward zero, as in Python")]
    let n = x.trunc() as i64;
    Ok(Value::Int(n))
}

/// `a ** b` on floats, as the engines compute it.
pub(super) fn float_power(a: f64, b: f64) -> f64 {
    if b.to_bits() == 2.0_f64.to_bits() {
        a * a
    } else if b.to_bits() == 0.5_f64.to_bits() {
        // `sqrt(-0)` is -0 and `sqrt(-inf)` is NaN, where `pow` gives +0 and +inf.
        if a == 0.0 {
            0.0
        } else if a == f64::NEG_INFINITY {
            f64::INFINITY
        } else {
            a.sqrt()
        }
    } else if b.to_bits() == 1.0_f64.to_bits() {
        a
    } else if b == 0.0 {
        1.0
    } else if b.to_bits() == (-1.0_f64).to_bits() {
        1.0 / a
    } else {
        libm::pow(a, b)
    }
}

/// The items of what holds them in order: a list, a tuple, a set or a dict's keys.
fn items_of(value: &Value) -> Ran<Vec<Value>> {
    value.items().ok_or(Stop::NotHere("going through this value".to_owned()))
}

/// `range(stop)`, `range(start, stop)` or `range(start, stop, step)` as a list.
fn range(machine: &mut Machine<'_>, bounds: &[Value]) -> Ran<Value> {
    let (first, limit, by) = match bounds {
        [Value::Int(limit)] => (0, *limit, 1),
        [Value::Int(first), Value::Int(limit)] => (*first, *limit, 1),
        [Value::Int(first), Value::Int(limit), Value::Int(by)] if *by != 0 => (*first, *limit, *by),
        _ => return not_here("this `range`"),
    };
    let mut items = Vec::new();
    let mut at = first;
    while (by > 0 && at < limit) || (by < 0 && at > limit) {
        machine.charge(1)?;
        items.push(Value::Int(at));
        let Some(next) = at.checked_add(by) else { break };
        at = next;
    }
    Ok(Value::list(items))
}

fn sum(items: Vec<Value>, start: Option<Value>, span: Span) -> Ran<Value> {
    // Floats add in the one fixed order every engine uses.
    let floats: Option<Vec<f64>> =
        items.iter().map(|item| if let Value::Float(x) = item { Some(*x) } else { None }).collect();
    match (&floats, &start) {
        (Some(floats), None) if !floats.is_empty() => {
            return Ok(Value::Float(fennec_syntax::fixed_sum::added(0.0, floats.iter().copied())));
        }
        (Some(floats), Some(Value::Float(first))) => {
            return Ok(Value::Float(fennec_syntax::fixed_sum::added(*first, floats.iter().copied())));
        }
        _ => {}
    }
    let mut total = start.unwrap_or_else(|| {
        if matches!(items.first(), Some(Value::Float(_))) { Value::Float(0.0) } else { Value::Int(0) }
    });
    for item in items {
        total = match (&total, &item) {
            (Value::Int(a), Value::Int(b)) => {
                Value::Int(a.checked_add(*b).ok_or_else(|| super::overflow(span))?)
            }
            (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
            _ => return not_here("`sum` of these"),
        };
    }
    Ok(total)
}

/// The least or greatest of some items: the first of those that tie.
fn extreme(name: &str, items: Vec<Value>) -> Ran<Value> {
    let mut items = items.into_iter();
    let Some(mut best) = items.next() else {
        return not_here(format!("`{name}` of nothing"));
    };
    for item in items {
        let ordering = order(&item, &best)?;
        if (name == "min" && ordering.is_lt()) || (name == "max" && ordering.is_gt()) {
            best = item;
        }
    }
    Ok(best)
}

/// Sorts in place, keeping the order of items that tie.
fn sort(items: &mut [Value]) -> Ran<()> {
    let mut problem = None;
    items.sort_by(|a, b| {
        order(a, b).unwrap_or_else(|stop| {
            problem.get_or_insert(stop);
            Ordering::Equal
        })
    });
    problem.map_or(Ok(()), Err)
}

fn sort_keyed(items: &mut [(Value, Value)]) -> Ran<()> {
    let mut problem = None;
    items.sort_by(|(a, _), (b, _)| {
        order(a, b).unwrap_or_else(|stop| {
            problem.get_or_insert(stop);
            Ordering::Equal
        })
    });
    problem.map_or(Ok(()), Err)
}

/// How two values of one type order. A type with its own `<` is a call in the IR, not this.
pub(super) fn order(lhs: &Value, rhs: &Value) -> Ran<Ordering> {
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => Ok(float_order(*a, *b)),
        (Value::Str(a), Value::Str(b)) => Ok(a.cmp(b)),
        (Value::Bytes(a), Value::Bytes(b)) => Ok(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
        (Value::Tuple(a), Value::Tuple(b)) | (Value::List(a), Value::List(b)) => {
            for (x, y) in a.iter().zip(b.iter()) {
                let ordering = order(x, y)?;
                if ordering.is_ne() {
                    return Ok(ordering);
                }
            }
            Ok(a.len().cmp(&b.len()))
        }
        _ => not_here("ordering these values"),
    }
}

/// Floats order as numbers, with `0.0` the same as `-0.0`, and NaN after everything.
fn float_order(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}
