//! Methods on built-in values: `stdout`, `fs`, strings, lists, dicts, sets and `Table`.

use std::fmt::Write;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::format;
use crate::methods::no_method;
use crate::ops;
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

/// When the monotonic clock started: the first time any program read it.
fn clock_start() -> std::time::Instant {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

/// What a test's fake `Clock` says the time is before it sleeps: 2000-01-01T00:00:00Z, as
/// nanoseconds since 1970 began in UTC. It moves on as the test sleeps.
pub(crate) const FAKE_TIME: i64 = 946_684_800_000_000_000;

/// The wall clock's reading, in nanoseconds since 1970 began in UTC, negative before then.
pub(crate) fn wall_nanos() -> i64 {
    let now = std::time::SystemTime::now();
    match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos()).map_or(i64::MIN, |nanos| -nanos),
    }
}

/// The monotonic clock's reading, in nanoseconds since it started.
pub(crate) fn monotonic_nanos() -> i64 {
    i64::try_from(clock_start().elapsed().as_nanos()).unwrap_or(i64::MAX)
}

fn lookup(pairs: &crate::pairs::Pairs, key: &Value) -> Option<Value> {
    pairs.get(key).cloned()
}

pub(crate) fn str_method(text: &str, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
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

/// `data.hex()` and `data.hex(sep)`.
fn bytes_hex(data: &[u8], args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    match args {
        [] => Ok(Value::str(&hex(data, ""))),
        [Value::Str(sep)] => Ok(Value::str(&hex(data, sep))),
        _ => Err(wrong_args("hex", span)),
    }
}

/// Each byte as two lowercase hex digits, with `sep` between bytes.
pub(crate) fn hex(data: &[u8], sep: &str) -> String {
    let mut out = String::with_capacity(data.len() * (2 + sep.len()));
    for (at, byte) in data.iter().enumerate() {
        if at > 0 {
            out.push_str(sep);
        }
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What's wrong with text that `from_hex` stopped reading at `at`.
pub(crate) fn hex_problem(text: &str, at: usize) -> String {
    match text.chars().nth(at) {
        Some(c) => format!(
            "`fromhex` needs two hex digits for each byte, but found {} at {at}",
            format::repr(&Value::str(c.encode_utf8(&mut [0; 4])))
        ),
        None => {
            "`fromhex` needs two hex digits for each byte, but the text ends halfway through one".to_owned()
        }
    }
}

/// The bytes that pairs of hex digits spell, with ASCII whitespace allowed between pairs. A
/// failure gives the position, in characters, of what doesn't fit.
pub(crate) fn from_hex(text: &str) -> Result<Vec<u8>, usize> {
    let mut bytes = Vec::new();
    let mut chars = text.chars().enumerate();
    while let Some((at, c)) = chars.next() {
        if c.is_ascii_whitespace() {
            continue;
        }
        let high = c.to_digit(16).ok_or(at)?;
        let Some((next, low)) = chars.next() else { return Err(at + 1) };
        let low = low.to_digit(16).ok_or(next)?;
        bytes.push(u8::try_from(high * 16 + low).map_err(|_| at)?);
    }
    Ok(bytes)
}

/// `x.abs()` or `x.is_nan()`.
fn float_method(x: f64, method: &str) -> Value {
    if method == "abs" { Value::Float(x.abs()) } else { Value::Bool(x.is_nan()) }
}
