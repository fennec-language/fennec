//! `std.concurrent`: channels between tasks, written in Fennec over `std.intrinsics`' channel
//! operations, which each engine supplies.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "concurrent",
    names: &["channel", "Channel", "Inbox", "Sender", "Closed", "Timeout", "Crashed", "Delivery", "Behind", "pool", "Pool", "Pooled", "Explained", "PoolError", "Borrowed"],
    source: include_str!("concurrent.fen"),
    gives: &[],
};
