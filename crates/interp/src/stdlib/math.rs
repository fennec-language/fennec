//! `std.math`: `PI` and the math functions. `Vec3` is written in Fennec.

use fennec_syntax::{Diagnostic, Span};

use crate::builtins::to_int;
use crate::embed::checks;
use crate::ops;
use crate::value::Value;


fn constant(name: &str) -> Option<Value> {
    (name == "PI").then_some(Value::Float(std::f64::consts::PI))
}

