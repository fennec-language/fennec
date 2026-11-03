// Sorting, the Go twin of sort.fen.
package main

import (
	"cmp"
	"fmt"
	"slices"
)

func main() {
	var seed int64 = 42
	numbers := make([]int64, 0, 300_000)
	for i := 0; i < 300_000; i++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		numbers = append(numbers, seed)
	}
	ordered := slices.Clone(numbers)
	slices.Sort(ordered)
	byDigit := slices.Clone(numbers)
	slices.SortStableFunc(byDigit, func(a, b int64) int { return cmp.Compare(a%1000, b%1000) })
	words := make([]string, 0, 50_000)
	for _, n := range numbers[:50_000] {
		words = append(words, fmt.Sprintf("fox%d", n%100000))
	}
	slices.Sort(words)
	fmt.Printf("%d %d %d %d %s\n", ordered[0], ordered[150_000], byDigit[0], byDigit[150_000], words[25_000])
}
