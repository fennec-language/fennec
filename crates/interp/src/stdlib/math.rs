//! `std.math`: `PI` and the math functions. `Vec3` is written in Fennec.

use fennec_syntax::{Diagnostic, Span};

use super::Module;
use crate::builtins::to_int;
use crate::call::CallArg;
use crate::collections::plain;
use crate::interp::Interp;
use crate::ops;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &["math.ceil", "math.cos", "math.floor", "math.sin", "math.sqrt"],
    call,
    constant,
    methods: &[],
};

fn constant(name: &str) -> Option<Value> {
    (name == "PI").then_some(Value::Float(std::f64::consts::PI))
}

fn call(_: &mut Interp<'_>, name: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
    let values = plain(args, name, span)?;
    let x = match values.as_slice() {
        [Value::Float(x)] => *x,
        [Value::Int(n)] => ops::to_float(*n),
        [other] => {
            return Err(Diagnostic::new(
                format!("`{name}` needs a number, not a {}", other.type_name()),
                span,
            ));
        }
        _ => return Err(Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span)),
    };
    match name {
        "math.floor" => to_int(&Value::Float(x.floor()), span),
        "math.ceil" => to_int(&Value::Float(x.ceil()), span),
        "math.sin" => Ok(Value::Float(x.sin())),
        "math.cos" => Ok(Value::Float(x.cos())),
        _ if x < 0.0 => Err(Diagnostic::new("can't take the square root of a negative number", span)),
        _ => Ok(Value::Float(x.sqrt())),
    }
}
