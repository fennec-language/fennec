//! `std.string`: wrapping and indenting text, written in Fennec. `str`'s own methods are in
//! `str.fen`.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "string",
    names: &["wrap", "fill", "dedent", "indent"],
    source: include_str!("string.fen"),
    gives: &[],
};
