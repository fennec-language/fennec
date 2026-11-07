//! `std.math`: vectors, quaternions and matrices, `PI` and the math functions.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "math",
    names: &["Vec2", "Vec3", "Vec4", "Quat", "Mat3", "Mat4", "PI", "sqrt", "floor", "ceil", "sin", "cos", "tan", "acos", "exp", "log", "lerp"],
    source: include_str!("math.fen"),
    gives: &[],
};
