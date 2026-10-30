//! Operators on values. Ints and floats never mix silently, and int arithmetic that
//! overflows is an error rather than a wrapped number.

use std::cmp::Ordering;
use std::rc::Rc;

use fennec_syntax::{BinOp, Diagnostic, Span, UnaryOp};

use crate::value::Value;

pub(crate) fn to_float(n: i64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "int to float is allowed to round, as in Python")]
    let x = n as f64;
    x
}

pub(crate) fn unary(op: UnaryOp, value: Value, span: Span) -> Result<Value, Diagnostic> {
    match (op, value) {
        (UnaryOp::Neg, Value::Int(n)) => n.checked_neg().map(Value::Int).ok_or_else(|| overflow(span)),
        (UnaryOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
        (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (UnaryOp::Not, other) => {
            Err(Diagnostic::new(format!("`not` needs a bool, but this is a {}", other.type_name()), span)
                .with_help("Fennec has no truthiness: compare explicitly, like `len(items) == 0`"))
        }
        (UnaryOp::Neg, other) => Err(Diagnostic::new(format!("can't negate a {}", other.type_name()), span)),
    }
}

pub(crate) fn binary(op: BinOp, lhs: Value, rhs: Value, span: Span) -> Result<Value, Diagnostic> {
    match op {
        BinOp::Eq => Ok(Value::Bool(lhs == rhs)),
        BinOp::NotEq => Ok(Value::Bool(lhs != rhs)),
        BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            let ordering = compare(&lhs, &rhs, span)?;
            Ok(Value::Bool(match op {
                BinOp::Lt => ordering.is_lt(),
                BinOp::LtEq => ordering.is_le(),
                BinOp::Gt => ordering.is_gt(),
                _ => ordering.is_ge(),
            }))
        }
        BinOp::Is | BinOp::IsNot => is_none(op, &lhs, &rhs, span),
        BinOp::In => contains(&rhs, &lhs, span).map(Value::Bool),
        BinOp::NotIn => contains(&rhs, &lhs, span).map(|found| Value::Bool(!found)),
        // `and` and `or` never get here: they short-circuit, so `Interp::eval` runs them.
        _ => arithmetic(op, lhs, rhs, span),
    }
}

fn is_none(op: BinOp, lhs: &Value, rhs: &Value, span: Span) -> Result<Value, Diagnostic> {
    if *rhs != Value::None {
        return Err(
            Diagnostic::new("`is` only compares with None", span).with_help("use `==` to compare values")
        );
    }
    Ok(Value::Bool((*lhs == Value::None) == (op == BinOp::Is)))
}

fn arithmetic(op: BinOp, lhs: Value, rhs: Value, span: Span) -> Result<Value, Diagnostic> {
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => int_arithmetic(op, a, b, span),
        (Value::Float(a), Value::Float(b)) => float_arithmetic(op, a, b, span),
        (Value::Str(a), Value::Str(b)) if op == BinOp::Add => Ok(Value::Str(Rc::from(format!("{a}{b}")))),
        (Value::Bytes(a), Value::Bytes(b)) if op == BinOp::Add => {
            Ok(Value::Bytes(Rc::from([&a[..], &b[..]].concat())))
        }
        (Value::List(a), Value::List(b)) if op == BinOp::Add => {
            Ok(Value::list(a.iter().chain(b.iter()).cloned().collect()))
        }
        (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
            Err(Diagnostic::new(format!("can't mix int and float in `{}`", op.symbol()), span)
                .with_help("convert one side with float(...) or int(...)"))
        }
        (lhs, rhs) => Err(Diagnostic::new(
            format!("can't use `{}` on a {} and a {}", op.symbol(), lhs.type_name(), rhs.type_name()),
            span,
        )),
    }
}

fn int_arithmetic(op: BinOp, a: i64, b: i64, span: Span) -> Result<Value, Diagnostic> {
    if b == 0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return Err(Diagnostic::new("division by zero", span));
    }
    let result = match op {
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        BinOp::Div => return Ok(Value::Float(to_float(a) / to_float(b))),
        BinOp::FloorDiv => {
            a.checked_div_euclid(b).map(|q| if b.is_negative() && a.rem_euclid(b) != 0 { q - 1 } else { q })
        }
        BinOp::Mod => a.checked_rem_euclid(b).map(|r| if b.is_negative() && r != 0 { r + b } else { r }),
        BinOp::Pow => {
            let exponent = u32::try_from(b).map_err(|_| {
                Diagnostic::new("an int can only be raised to a non-negative power", span)
                    .with_help("for fractional results, use floats: float(a) ** float(b)")
            })?;
            a.checked_pow(exponent)
        }
        _ => None,
    };
    result.map(Value::Int).ok_or_else(|| overflow(span))
}

fn float_arithmetic(op: BinOp, a: f64, b: f64, span: Span) -> Result<Value, Diagnostic> {
    if b == 0.0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return Err(Diagnostic::new("division by zero", span));
    }
    Ok(Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::FloorDiv => (a / b).floor(),
        BinOp::Mod => a - b * (a / b).floor(),
        _ => a.powf(b),
    }))
}

fn overflow(span: Span) -> Diagnostic {
    Diagnostic::new("this calculation overflows an int", span)
        .with_help("ints are 64-bit; check the values involved, or use floats for very large numbers")
}

/// Orders two values of the same kind; anything else is an error.
pub(crate) fn compare(lhs: &Value, rhs: &Value, span: Span) -> Result<Ordering, Diagnostic> {
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => Ok(a.total_cmp(b)),
        (Value::Str(a), Value::Str(b)) => Ok(a.cmp(b)),
        (Value::Bytes(a), Value::Bytes(b)) => Ok(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
        (Value::Tuple(a), Value::Tuple(b)) | (Value::List(a), Value::List(b)) => {
            for (x, y) in a.iter().zip(b.iter()) {
                let ordering = compare(x, y, span)?;
                if ordering.is_ne() {
                    return Ok(ordering);
                }
            }
            Ok(a.len().cmp(&b.len()))
        }
        _ => Err(Diagnostic::new(
            format!("can't compare a {} with a {}", lhs.type_name(), rhs.type_name()),
            span,
        )),
    }
}

fn contains(container: &Value, item: &Value, span: Span) -> Result<bool, Diagnostic> {
    match (container, item) {
        (Value::List(items) | Value::Tuple(items) | Value::Set(items), item) => Ok(items.contains(item)),
        (Value::Dict(pairs), key) => Ok(pairs.iter().any(|(existing, _)| existing == key)),
        (Value::Str(text), Value::Str(part)) => Ok(text.contains(part.as_ref())),
        (Value::Bytes(data), Value::Int(byte)) => {
            Ok(u8::try_from(*byte).is_ok_and(|byte| data.contains(&byte)))
        }
        _ => Err(Diagnostic::new(
            format!("can't look for a {} in a {}", item.type_name(), container.type_name()),
            span,
        )),
    }
}

/// `list[i]` and `str[i]`, with negative indexes counting from the end as in Python.
pub(crate) fn index(object: &Value, index: &Value, span: Span) -> Result<Value, Diagnostic> {
    let Value::Int(at) = index else {
        return Err(Diagnostic::new(format!("an index must be an int, not a {}", index.type_name()), span));
    };
    match object {
        Value::List(items) => position(*at, items.len(), span)
            .and_then(|at| items.get(at).cloned().ok_or_else(|| Diagnostic::new("index out of range", span))),
        Value::Str(text) => {
            let count = text.chars().count();
            let at = position(*at, count, span)?;
            text.chars()
                .nth(at)
                .map(|c| Value::str(c.encode_utf8(&mut [0; 4])))
                .ok_or_else(|| Diagnostic::new("index out of range", span))
        }
        Value::Bytes(data) => position(*at, data.len(), span).and_then(|at| {
            data.get(at)
                .map(|byte| Value::Int(i64::from(*byte)))
                .ok_or_else(|| Diagnostic::new("index out of range", span))
        }),
        other => Err(Diagnostic::new(format!("a {} can't be indexed", other.type_name()), span)),
    }
}

/// Resolves a possibly negative index against a length.
pub(crate) fn position(at: i64, len: usize, span: Span) -> Result<usize, Diagnostic> {
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    let resolved = if at < 0 { at + len_i } else { at };
    usize::try_from(resolved)
        .ok()
        .filter(|&resolved| resolved < len)
        .ok_or_else(|| Diagnostic::new(format!("index {at} is out of range for length {len}"), span))
}

/// `value[start:end]`, with Python's rules: negative positions count from the end, and
/// positions past either end are clamped.
pub(crate) fn slice(
    value: &Value,
    start: Option<&Value>,
    end: Option<&Value>,
    span: Span,
) -> Result<Value, Diagnostic> {
    let len = match value {
        Value::List(items) | Value::Tuple(items) => items.len(),
        Value::Str(text) => text.chars().count(),
        Value::Bytes(data) => data.len(),
        other => return Err(Diagnostic::new(format!("a {} can't be sliced", other.type_name()), span)),
    };
    let bound = |at: Option<&Value>, default: usize| -> Result<usize, Diagnostic> {
        match at {
            None => Ok(default),
            Some(Value::Int(at)) => {
                let len_i = i64::try_from(len).unwrap_or(i64::MAX);
                let at = if *at < 0 { at + len_i } else { *at };
                Ok(usize::try_from(at.clamp(0, len_i)).unwrap_or(0))
            }
            Some(other) => Err(Diagnostic::new(
                format!("a slice position must be an int, not a {}", other.type_name()),
                span,
            )),
        }
    };
    let (from, to) = (bound(start, 0)?, bound(end, len)?);
    let to = to.max(from);
    Ok(match value {
        Value::List(items) => Value::list(items.get(from..to).unwrap_or(&[]).to_vec()),
        Value::Tuple(items) => Value::tuple(items.get(from..to).unwrap_or(&[]).to_vec()),
        Value::Str(text) => Value::str(&text.chars().skip(from).take(to - from).collect::<String>()),
        Value::Bytes(data) => Value::Bytes(Rc::from(data.get(from..to).unwrap_or(&[]))),
        _ => Value::Unit,
    })
}
