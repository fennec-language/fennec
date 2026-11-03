// The sieve of Eratosthenes, the Go twin of sieve.fen.
package main

import "fmt"

func countPrimes(limit int) int64 {
	composite := make([]int64, limit+1)
	var count int64
	for n := 2; n <= limit; n++ {
		if composite[n] == 0 {
			count++
			for multiple := n * n; multiple <= limit; multiple += n {
				composite[multiple] = 1
			}
		}
	}
	return count
}

func main() {
	fmt.Println(countPrimes(5_000_000))
}
