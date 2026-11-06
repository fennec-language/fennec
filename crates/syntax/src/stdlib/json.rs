//! `std.json`: reading and writing JSON, which the interpreter does in Rust. The options it
//! takes are enums written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "json",
    names: &["loads", "dumps", "ParseError", "JsonNames", "JsonExtra", "native_dumps", "native_loads", "_article", "_escaped", "_float_text", "_base64", "_from_base64", "_quoted", "_IdsSeen"],
    source: include_str!("json.fen"),
    gives: &[],
};
