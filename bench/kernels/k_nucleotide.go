// K-nucleotide, the Go twin of k_nucleotide.fen.
package main

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
)

func sequence(length int) string {
	var seed int64 = 42
	var letters strings.Builder
	letters.Grow(length)
	for i := 0; i < length; i++ {
		seed = (seed*3877 + 29573) % 139968
		letters.WriteByte("acgt"[seed*4/139968])
	}
	return letters.String()
}

func counted(dna string, size int) map[string]int64 {
	counts := map[string]int64{}
	for at := 0; at <= len(dna)-size; at++ {
		counts[dna[at:at+size]]++
	}
	return counts
}

type pair struct {
	piece string
	count int64
}

func main() {
	dna := sequence(100_000)
	for _, size := range []int{1, 2} {
		var ordered []pair
		for piece, count := range counted(dna, size) {
			ordered = append(ordered, pair{piece, count})
		}
		slices.SortFunc(ordered, func(a, b pair) int {
			if c := cmp.Compare(b.count, a.count); c != 0 {
				return c
			}
			return strings.Compare(a.piece, b.piece)
		})
		for _, p := range ordered {
			fmt.Printf("%s %d\n", p.piece, p.count)
		}
	}
	for _, piece := range []string{"ggt", "ggta", "ggtatt", "ggtattttaatt", "ggtattttaatttatagt"} {
		fmt.Printf("%d %s\n", counted(dna, len(piece))[piece], piece)
	}
}
