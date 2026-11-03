// Counting keys in a hash map, the Go twin of dicts.fen.
package main

import (
	"fmt"
	"os"
	"time"
)

func main() {
	started := time.Now()
	counts := make(map[int64]int64)
	for i := int64(0); i < 2_000_000; i++ {
		counts[i*7919%10_007]++
	}
	fmt.Println(len(counts), counts[0], counts[10_006])
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
