//! `std.math`: `PI` and the math functions. `Vec3` is written in Fennec.

use fennec_syntax::{Diagnostic, Span};

use super::Module;
use crate::builtins::to_int;
use crate::embed::checks;
use crate::collections::plain;
use crate::ops;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &[
        "math.acos",
        "math.ceil",
        "math.cos",
        "math.exp",
        "math.floor",
        "math.log",
        "math.sin",
        "math.sqrt",
        "math.tan",
    ],
    call,
    constant,
    methods: &[],
};

fn constant(name: &str) -> Option<Value> {
    (name == "PI").then_some(Value::Float(std::f64::consts::PI))
}

