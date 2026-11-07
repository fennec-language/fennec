//! A graph as a compiler's call graph or a build's dependencies use one, with petgraph: 100,000
//! nodes and about 500,000 weighted edges, always from a lower node to a higher one, then a
//! topological order and the cheapest path from the first node to the last.

use petgraph::algo::{astar, toposort};
use petgraph::graph::DiGraph;

fn main() {
    let count: i64 = 100_000;
    let mut graph = DiGraph::<i64, f64>::new();
    let ids: Vec<_> = (0..count).map(|at| graph.add_node(at)).collect();
    let mut seed: i64 = 7;
    let mut next = || {
        seed = (seed * 1_103_515_245 + 12345) % 2_147_483_648;
        seed
    };
    for _ in 0..count * 5 {
        let first = (next() / 4096) % count;
        let second = (next() / 4096) % count;
        let weight = (next() % 1_000_003) as f64 / 997.0 + 0.5;
        let (low, high) = match first.cmp(&second) {
            std::cmp::Ordering::Less => (first, second),
            std::cmp::Ordering::Greater => (second, first),
            std::cmp::Ordering::Equal => continue,
        };
        graph.add_edge(ids[low as usize], ids[high as usize], weight);
    }
    let order = toposort(&graph, None).unwrap_or_default();
    let (start, end) = (ids[0], ids[count as usize - 1]);
    let found = astar(&graph, start, |node| node == end, |edge| *edge.weight(), |_| 0.0);
    let (cost, path) = found.unwrap_or((0.0, Vec::new()));
    println!("{} {} {} {cost:?}", graph.edge_count(), order.len(), path.len());
}
