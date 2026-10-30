//! `std.random`: seeded random numbers.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "random",
    names: &["Random", "random_int", "random_float", "random_choice"],
    source: include_str!("random.fen"),
    gives: &[],
};
