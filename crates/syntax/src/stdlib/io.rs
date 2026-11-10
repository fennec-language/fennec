//! `std.io`: the `Writer` and `Reader` interfaces any stream of bytes has, and `MemoryStream`, one in
//! memory, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "io",
    names: &["Writer", "Reader", "MemoryStream", "read_all"],
    source: include_str!("io.fen"),
    gives: &[],
};
