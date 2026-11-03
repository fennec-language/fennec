// Binary trees, the Go twin of binary_trees.fen.
package main

import "fmt"

// Tree is a leaf when both children are nil, and a node otherwise.
type Tree struct {
	left, right *Tree
}

func build(depth int) *Tree {
	if depth == 0 {
		return &Tree{}
	}
	return &Tree{build(depth - 1), build(depth - 1)}
}

func count(tree *Tree) int64 {
	if tree.left == nil {
		return 1
	}
	return 1 + count(tree.left) + count(tree.right)
}

func main() {
	most := 14
	longest := build(most)
	var total int64
	for depth := 4; depth <= most; depth += 4 {
		for range 1 << (most - depth + 4) {
			total += count(build(depth))
		}
	}
	fmt.Printf("%d %d\n", total, count(longest))
}
