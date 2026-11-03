// Counting keys in a hash map, the Go twin of dicts.fen.
package main

import "fmt"

func main() {
	counts := make(map[int64]int64)
	for i := int64(0); i < 2_000_000; i++ {
		counts[i*7919%10_007]++
	}
	fmt.Println(len(counts), counts[0], counts[10_006])
}
