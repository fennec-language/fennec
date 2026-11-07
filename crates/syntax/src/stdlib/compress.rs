//! `std.compress`: DEFLATE, and zlib and gzip around it, which the interpreter does in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "compress",
    names: &["deflate", "inflate", "zlib", "unzlib", "gzip", "gunzip"],
    source: "",
    gives: &[],
};
