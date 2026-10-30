//! `std.hash`: BLAKE3 hashes of text, which the interpreter does in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "hash",
    names: &["blake3", "keyed_blake3", "derive_key"],
    source: "",
    gives: &[],
};
