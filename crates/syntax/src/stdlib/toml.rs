//! `std.toml`: reading and writing TOML, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "toml",
    names: &["loads", "dumps", "ParseError", "native_dumps", "native_loads"],
    source: include_str!("toml.fen"),
    gives: &[],
};
