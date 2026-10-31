//! `std.collections`: queues, heaps, counting and trees.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "collections",
    names: &["Deque", "Heap", "PriorityQueue", "Counter", "Tree", "TreeNode", "TreeCycle"],
    source: include_str!("collections.fen"),
    gives: &[],
};
