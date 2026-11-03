// Comprehensions and iterators, the Go twin of comprehensions.fen.
package main

import (
	"fmt"
	"os"
	"time"
)

func main() {
	started := time.Now()
	size := int64(100_000)
	var total int64
	for round := int64(0); round < 10; round++ {
		numbers := make([]int64, 0, size)
		for i := int64(0); i < size; i++ {
			numbers = append(numbers, i*2+round)
		}
		var evens []int64
		for _, n := range numbers {
			if n%4 == 0 {
				evens = append(evens, n)
			}
		}
		squares := make([]int64, 0, len(evens))
		for _, n := range evens {
			squares = append(squares, n*n%1000)
		}
		for _, n := range squares {
			if n > 500 {
				total += n
			}
		}
		set := make(map[int64]struct{})
		for _, n := range squares {
			set[n%100] = struct{}{}
		}
		total += int64(len(set))
		byRemainder := make(map[int64]int64)
		for _, n := range evens {
			byRemainder[n%50] = n
		}
		total += int64(len(byRemainder))
		for at := 0; at < len(numbers) && at < len(squares); at++ {
			total += numbers[at] * squares[at] % 7
		}
	}
	fmt.Println(total)
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
