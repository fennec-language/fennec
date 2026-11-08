//! The few plain operations the runtime shares among its modules.

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;

/// 2^63: a float converts to an int when it is at least -2^63 and below 2^63.
const INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// The most positions a leaf of a `parallel` reduction's tree holds, by the length alone, so
/// the interpreter and native code combine in the same order (see `embed::parallel`).
pub(crate) const REDUCE_LEAF: usize = 1024;

pub(crate) fn length(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    let len = match value {
        Value::List(items) | Value::Tuple(items) | Value::Set(items) => items.len(),
        Value::Buffer(buffer) => buffer.items.len(),
        Value::Dict(pairs) => pairs.len(),
        Value::Str(text) => text.chars().count(),
        Value::Bytes(data) => data.len(),
        Value::Table(map) => map.entries().count(),
        other => return Err(Diagnostic::new(format!("a {} has no length", other.type_name()), span)),
    };
    i64::try_from(len).map(Value::Int).map_err(|_| Diagnostic::new("this is too long to measure", span))
}

pub(crate) fn to_int(value: &Value, span: Span) -> Result<Value, Diagnostic> {
    match value {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(x) if (-INT_LIMIT..INT_LIMIT).contains(x) => {
            #[expect(clippy::cast_possible_truncation, reason = "int() truncates toward zero, as in Python")]
            let n = x.trunc() as i64;
            Ok(Value::Int(n))
        }
        other => Err(Diagnostic::new(format!("can't turn a {} into an int", other.type_name()), span)),
    }
}
