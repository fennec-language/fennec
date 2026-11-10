//! `Stderr`, standard error, which a program writes what isn't its output to: warnings, errors and
//! logs. Its methods are `Writer`'s: `write(data)` for bytes, `write_text(text)`, `write_line(text)`
//! and `flush()`. Each goes out at once, with nothing held back, and fails with an `IoError` when
//! standard error is closed. A test's `Stderr` is a fake that keeps what's written, for the test's
//! `captured()`.

use std::io::Write as _;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::value::{Capability, Value};

/// What a method of `Stderr` writes, or `None` for `flush`, which writes nothing.
fn what_to_write(method: &str, args: &[Value], at: Span) -> Result<Option<Vec<u8>>, Diagnostic> {
    match (method, args) {
        ("write", [Value::Bytes(data)]) => Ok(Some(data.to_vec())),
        ("write_text", [Value::Str(text)]) => Ok(Some(text.as_bytes().to_vec())),
        ("write_line", [Value::Str(text)]) => Ok(Some(format!("{text}\n").into_bytes())),
        ("flush", []) => Ok(None),
        ("write" | "write_text" | "write_line" | "flush", _) => {
            Err(Diagnostic::new(format!("wrong arguments to `{method}`"), at))
        }
        _ => Err(crate::methods::no_method(&Value::Capability(Capability::Stderr), method, at)),
    }
}

/// A method of the real `Stderr`.
pub(crate) fn written(method: &str, args: &[Value], at: Span) -> Result<Value, Diagnostic> {
    let data = what_to_write(method, args, at)?;
    let mut out = std::io::stderr().lock();
    let done = match data {
        Some(data) => out.write_all(&data).and_then(|()| out.flush()),
        None => out.flush(),
    };
    Ok(match done {
        Ok(()) => Value::Ok(Rc::new(Value::Unit)),
        Err(error) => crate::stdlib::fs::io_error(&error.to_string()),
    })
}

/// A method of a test's fake `Stderr`, which keeps what's written in `errors`.
pub(crate) fn faked(
    errors: &mut String,
    method: &str,
    args: &[Value],
    at: Span,
) -> Result<Value, Diagnostic> {
    if let Some(data) = what_to_write(method, args, at)? {
        errors.push_str(&String::from_utf8_lossy(&data));
    }
    Ok(Value::Ok(Rc::new(Value::Unit)))
}
