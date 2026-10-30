//! `std.math`: the math functions. `Vec3` is written in Fennec, and `PI` is a float.

use fennec_syntax::Span;

use super::{Function, Module, no_checks, no_methods, takes};
use crate::function::FnCx;
use crate::types::Type;

pub(crate) const MODULE: Module = Module {
    functions: &FUNCTIONS,
    result,
    method: no_methods,
    construct: no_checks,
};

const FUNCTIONS: [Function; 5] = [
    takes("math.ceil", (1, 1)),
    takes("math.cos", (1, 1)),
    takes("math.floor", (1, 1)),
    takes("math.sin", (1, 1)),
    takes("math.sqrt", (1, 1)),
];

/// Each takes a number. `floor` and `ceil` give ints; the rest give floats.
fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
    let types: Vec<Type> = plain.iter().map(|(ty, _)| ty.clone()).collect();
    cx.math(name, &types, span)
}
