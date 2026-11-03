// Naive recursive Fibonacci, the Go twin of fib.fen.
package main

import (
	"fmt"
	"os"
	"time"
)

func fib(n int64) int64 {
	if n < 2 {
		return n
	}
	return fib(n-1) + fib(n-2)
}

func main() {
	started := time.Now()
	fmt.Println(fib(35))
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
