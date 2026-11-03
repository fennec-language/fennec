// A dict keyed by a plain struct, the Go twin of struct_keys.fen.
package main

import "fmt"

type Cell struct {
	x, y int64
}

func main() {
	seen := make(map[Cell]int64)
	for i := int64(0); i < 500_000; i++ {
		seen[Cell{x: i * 31 % 101, y: i * 17 % 103}]++
	}
	fmt.Println(len(seen), seen[Cell{x: 0, y: 0}])
}
