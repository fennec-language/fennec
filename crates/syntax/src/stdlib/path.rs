//! `std.path`: `Path`, which only looks at its text.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "path",
    names: &["Path"],
    source: include_str!("path.fen"),
    gives: &[],
};
