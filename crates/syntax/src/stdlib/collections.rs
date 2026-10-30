//! `std.collections`: queues, heaps and counting.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "collections",
    names: &["Deque", "Heap", "PriorityQueue", "Counter"],
    source: include_str!("collections.fen"),
    gives: &[],
};
