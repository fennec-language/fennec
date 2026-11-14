//! The few parts of the standard library the oracle models, each on its own: `std.math`'s
//! constants and the functions whose result IEEE 754 fixes exactly, and `std.json`'s `dumps` and
//! `loads`, in `json.rs`. The rest of it, and every function `libm` computes, is unsupported.

use fennec_syntax::Span;

use super::{Run, crash, unsupported};
use crate::value::Value;

/// A constant of the standard library, by its whole name, like `std.math.PI`, or a case of one
/// of `std.json`'s option enums, like `std.json.JsonNames.CamelCase`.
pub(crate) fn constant(path: &str) -> Option<Value> {
    if let Some((ty, case)) = path.strip_prefix("std.json.").and_then(|rest| rest.split_once('.')) {
        return super::json::option_case(ty, case);
    }
    Some(Value::Float(match path {
        "std.math.PI" => std::f64::consts::PI,
        "std.math.INFINITY" => f64::INFINITY,
        "std.math.NAN" => f64::NAN,
        _ => return None,
    }))
}

/// Whether the oracle models a function of the standard library, by its whole name.
pub(crate) fn has_function(path: &str) -> bool {
    matches!(
        path,
        "std.math.sqrt"
            | "std.math.floor"
            | "std.math.ceil"
            | "std.math.lerp"
            | "std.json.dumps"
            | "std.json.loads"
    )
}

/// Calls a function of the standard library the oracle models.
pub(crate) fn call(path: &str, args: &[Value], span: Span) -> Run<Value> {
    match (path, args) {
        ("std.math.sqrt", [Value::Float(x)]) => {
            if *x < 0.0 {
                return crash("can't take the square root of a negative number", span);
            }
            Ok(Value::Float(x.sqrt()))
        }
        ("std.math.floor", [Value::Float(x)]) => {
            super::builtins::float_to_int(x.floor(), span).map(Value::Int)
        }
        ("std.math.ceil", [Value::Float(x)]) => super::builtins::float_to_int(x.ceil(), span).map(Value::Int),
        ("std.math.lerp", [Value::Float(start), Value::Float(end), Value::Float(amount)]) => {
            Ok(Value::Float(start + (end - start) * amount))
        }
        _ => unsupported(path.to_owned()),
    }
}
