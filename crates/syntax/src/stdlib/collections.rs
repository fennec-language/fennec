//! `std.collections`: queues, heaps, counting, trees, sorted maps and sets, and `Table` with the
//! `Id`s it hands out, which the language builds in.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "collections",
    names: &[
        "Deque",
        "Heap",
        "PriorityQueue",
        "Counter",
        "Packed",
        "Tree",
        "Graph",
        "Edge",
        "TreeNode",
        "TreeCycle",
        "SortedMap",
        "SortedSet",
        "Grid",
        "Id",
        "Table",
    ],
    source: include_str!("collections.fen"),
    gives: &[],
};
