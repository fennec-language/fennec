//! `std.toml`: reading and writing TOML, which the interpreter does in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "toml",
    names: &["loads", "dumps", "ParseError"],
    source: "",
    gives: &[],
};
