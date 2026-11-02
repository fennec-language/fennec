//! `std.process`: running other programs through the `Process` capability, which `main` names
//! like `Fs`. Its methods are written in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "process",
    names: &["Process", "Output", "ProcessError"],
    source: include_str!("process.fen"),
    gives: &[("run", "Output")],
};
