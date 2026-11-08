//! `std.concurrent`: channels between tasks, written in Fennec over `std.intrinsics`' channel
//! operations, which each engine supplies.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "concurrent",
    names: &["Channel", "Inbox", "Sender", "Closed", "Timeout", "Crashed"],
    source: include_str!("concurrent.fen"),
    gives: &[],
};
