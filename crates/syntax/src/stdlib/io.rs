//! `std.io`: standard output, input and error, the capabilities a program talks to its terminal
//! through, whose methods are written in Rust; and, written in Fennec, the `Writer` and `Reader`
//! interfaces any stream of bytes has, with `MemoryStream`, one in memory.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "io",
    names: &["Stdout", "Stdin", "Stderr", "StdoutWriter", "Writer", "Reader", "MemoryStream", "read_all"],
    source: include_str!("io.fen"),
    gives: &[],
};
