//! `std.json`: reading and writing JSON, written in Fennec, with the options it takes as
//! enums.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "json",
    names: &["loads", "dumps", "ParseError", "JsonNames", "JsonExtra", "native_dumps", "native_loads", "article", "escape_of", "float_text", "quoted", "IdsSeen", "tables_in", "found_tables", "dump", "load"],
    source: include_str!("json.fen"),
    gives: &[],
};
