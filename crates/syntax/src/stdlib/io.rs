//! `std.io`: standard output, input and error, the capabilities a program talks to its terminal
//! through. Their methods are written in Rust.

use super::Module;

pub(crate) const MODULE: Module =
    Module { name: "io", names: &["Stdout", "Stdin", "Stderr"], source: include_str!("io.fen"), gives: &[] };
