// Closures, the Go twin of closures.fen.
package main

import "fmt"

func applyAll(values []int64, change func(int64) int64) int64 {
	var sum int64
	for _, value := range values {
		sum += change(value)
	}
	return sum
}

func twice(value int64, change func(int64) int64) int64 {
	return change(change(value))
}

func main() {
	count := int64(10_000)
	values := make([]int64, 0, count)
	for i := int64(0); i < count; i++ {
		values = append(values, i%1000)
	}
	var total int64
	for round := int64(0); round < 400; round++ {
		offset := round % 17
		total += applyAll(values, func(n int64) int64 { return n + offset })
		total += applyAll(values, func(n int64) int64 {
			return twice(n, func(m int64) int64 { return m*3%1001 + offset })
		})
	}
	fmt.Println(total)
}
