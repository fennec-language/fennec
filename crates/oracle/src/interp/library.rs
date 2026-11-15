//! The parts of the standard library the oracle models on its own: `std.math`'s constants, the
//! functions whose result IEEE 754 fixes exactly, and those `docs/language.md` says the `libm`
//! crate computes, with that crate, `std.json`'s `dumps` and `loads`, in `json.rs`, `std.path`'s
//! `Path`, in `paths.rs`, and `std.fs`, in `files.rs`. The rest of `std.math`, its vectors and
//! matrices, is run from its own Fennec source (`RUN_FROM_SOURCE` in `load.rs`). The rest of the
//! library is unsupported.

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
            | "std.math.sin"
            | "std.math.cos"
            | "std.math.tan"
            | "std.math.acos"
            | "std.math.exp"
            | "std.math.log"
            | "std.math.floor"
            | "std.math.ceil"
            | "std.math.lerp"
            | "std.json.dumps"
            | "std.json.loads"
            | "std.path.Path"
    )
}

/// Calls a function of the standard library the oracle models.
pub(crate) fn call(path: &str, args: &[Value], span: Span) -> Run<Value> {
    // `std.math`'s functions take floats, and an int given one is taken as the float it is, as
    // `sqrt(16)` is 4.0.
    let floats: Vec<Value>;
    let args = if path.starts_with("std.math.") && args.iter().any(|arg| matches!(arg, Value::Int(_))) {
        floats = args
            .iter()
            .map(|arg| match arg {
                Value::Int(n) => Value::Float(crate::text::int_as_float(*n)),
                other => other.clone(),
            })
            .collect();
        floats.as_slice()
    } else {
        args
    };
    match (path, args) {
        ("std.math.sqrt", [Value::Float(x)]) => {
            if *x < 0.0 {
                return crash("can't take the square root of a negative number", span);
            }
            Ok(Value::Float(x.sqrt()))
        }
        // As `docs/language.md` says, `libm` computes these, the same bits on every platform.
        // Past -1 or 1 `acos` is NaN, and so is `log` below 0: neither stops the program.
        ("std.math.sin", [Value::Float(x)]) => Ok(Value::Float(libm::sin(*x))),
        ("std.math.cos", [Value::Float(x)]) => Ok(Value::Float(libm::cos(*x))),
        ("std.math.tan", [Value::Float(x)]) => Ok(Value::Float(libm::tan(*x))),
        ("std.math.acos", [Value::Float(x)]) => Ok(Value::Float(libm::acos(*x))),
        ("std.math.exp", [Value::Float(x)]) => Ok(Value::Float(libm::exp(*x))),
        ("std.math.log", [Value::Float(x)]) => Ok(Value::Float(libm::log(*x))),
        ("std.math.floor", [Value::Float(x)]) => {
            super::builtins::float_to_int(x.floor(), span).map(Value::Int)
        }
        ("std.math.ceil", [Value::Float(x)]) => super::builtins::float_to_int(x.ceil(), span).map(Value::Int),
        ("std.math.lerp", [Value::Float(start), Value::Float(end), Value::Float(amount)]) => {
            Ok(Value::Float(start + (end - start) * amount))
        }
        ("std.path.Path", [Value::Str(text)]) => Ok(super::files::path_value(text.clone())),
        _ => unsupported(path.to_owned()),
    }
}
