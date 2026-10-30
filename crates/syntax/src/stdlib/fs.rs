//! `std.fs`: the `Fs` capability, which reads and writes files and folders, and the
//! `Metadata` it gives. Its methods are written in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "fs",
    names: &["Fs", "Metadata"],
    source: include_str!("fs.fen"),
    gives: &[("metadata", "Metadata"), ("list_dir", "Path")],
};
