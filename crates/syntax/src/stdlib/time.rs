//! `std.time`: the `Clock` capability, and the instants and durations it gives.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "time",
    names: &["Clock", "Duration", "Instant"],
    source: include_str!("time.fen"),
    gives: &[("now", "Instant")],
};
