//! Binary trees, the twin of `binary_trees.fen`.

enum Tree {
    Leaf,
    Node(Box<Tree>, Box<Tree>),
}

fn build(depth: u32) -> Tree {
    if depth == 0 {
        return Tree::Leaf;
    }
    Tree::Node(Box::new(build(depth - 1)), Box::new(build(depth - 1)))
}

fn count(tree: &Tree) -> i64 {
    match tree {
        Tree::Leaf => 1,
        Tree::Node(left, right) => 1 + count(left) + count(right),
    }
}

fn main() {
    let most = std::hint::black_box(14);
    let longest = build(most);
    let mut total = 0;
    let mut depth = 4;
    while depth <= most {
        for _ in 0..1 << (most - depth + 4) {
            total += count(&build(depth));
        }
        depth += 4;
    }
    println!("{total} {}", count(&longest));
}
