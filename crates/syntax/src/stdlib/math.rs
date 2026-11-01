//! `std.math`: `Vec3`, `PI` and the math functions.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "math",
    names: &["Vec3", "PI", "sqrt", "floor", "ceil", "sin", "cos", "lerp"],
    source: include_str!("math.fen"),
    gives: &[],
};
