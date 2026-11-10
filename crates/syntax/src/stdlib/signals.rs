//! `std.signals`: the `Signals` capability, which says when the program's been asked to stop. Its
//! methods are written in Rust.

use super::Module;

pub(crate) const MODULE: Module =
    Module { name: "signals", names: &["Signals"], source: include_str!("signals.fen"), gives: &[] };
