// Fannkuch, the Go twin of fannkuch.fen.
package main

import (
	"fmt"
	"slices"
)

func fannkuch(n int) (int64, int64) {
	perm := make([]int, n)
	for i := range perm {
		perm[i] = i
	}
	count := make([]int, n)
	flips := make([]int, n)
	var most, checksum int64
	sign := int64(1)
	for {
		copy(flips, perm)
		var total int64
		first := flips[0]
		for first != 0 {
			slices.Reverse(flips[:first+1])
			total++
			first = flips[0]
		}
		most = max(most, total)
		checksum += sign * total
		sign = -sign
		r := 1
		for {
			if r == n {
				return checksum, most
			}
			head := perm[0]
			copy(perm[:r], perm[1:r+1])
			perm[r] = head
			count[r]++
			if count[r] <= r {
				break
			}
			count[r] = 0
			r++
		}
	}
}

func main() {
	checksum, most := fannkuch(9)
	fmt.Printf("%d %d\n", checksum, most)
}
