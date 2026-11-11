//! `std.math`: vectors, quaternions and matrices, `PI`, `NAN`, `INFINITY` and the math functions.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "math",
    names: &["Vec2", "Vec3", "Vec4", "Quat", "Mat3", "Mat4", "PI", "NAN", "INFINITY", "sqrt", "floor", "ceil", "sin", "cos", "tan", "acos", "exp", "log", "lerp", "dot"],
    source: include_str!("math.fen"),
    gives: &[],
};
