//! `std.json`: reading and writing JSON, which the interpreter does in Rust. The options it
//! takes are enums written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "json",
    names: &["loads", "dumps", "ParseError", "JsonNames", "JsonExtra", "native_dumps", "native_loads", "article", "escape_of", "float_text", "base64_of", "from_base64", "quoted", "IdsSeen", "tables_in", "found_tables"],
    source: include_str!("json.fen"),
    gives: &[],
};
