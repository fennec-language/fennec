// A dict in a struct's field, the Go twin of dict_field.fen.
package main

import (
	"fmt"
	"os"
	"time"
)

type counter struct {
	counts map[int64]int64
}

func (c *counter) bump(key int64) {
	c.counts[key]++
}

func main() {
	started := time.Now()
	c := &counter{counts: map[int64]int64{}}
	for i := int64(0); i < 2_500_000; i++ {
		c.bump(i * 7919 % 64)
	}
	fmt.Printf("%d %d %d\n", len(c.counts), c.counts[0], c.counts[63])
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
