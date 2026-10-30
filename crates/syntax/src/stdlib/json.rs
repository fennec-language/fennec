//! `std.json`: reading and writing JSON, which the interpreter does in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "json",
    names: &["loads", "dumps", "ParseError"],
    source: "",
    gives: &[],
};
