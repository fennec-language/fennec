//! `std.collections`: queues, heaps, counting, trees, and sorted maps and sets.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "collections",
    names: &["Deque", "Heap", "PriorityQueue", "Counter", "Tree", "TreeNode", "TreeCycle", "SortedMap", "SortedSet", "Grid"],
    source: include_str!("collections.fen"),
    gives: &[],
};
