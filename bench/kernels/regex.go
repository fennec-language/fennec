// Regex-redux, the Go twin of regex.fen, on Go's standard regexp engine.
package main

import (
	"fmt"
	"os"
	"regexp"
	"strings"
	"time"
)

func sequence(length int) string {
	seed := int64(42)
	var letters strings.Builder
	letters.Grow(length)
	for range length {
		seed = (seed*3877 + 29573) % 139968
		letters.WriteByte("acgt"[seed*4/139968])
	}
	return letters.String()
}

func main() {
	started := time.Now()
	dna := sequence(50_000)
	patterns := []string{
		"agggtaaa|tttaccct",
		"[cgt]gggtaaa|tttaccc[acg]",
		"a[act]ggtaaa|tttacc[agt]t",
		"agg[act]taaa|ttta[agt]cct",
		"aggg[acg]aaa|ttt[cgt]ccct",
	}
	for _, pattern := range patterns {
		fmt.Println(pattern, len(regexp.MustCompile(pattern).FindAllStringIndex(dna, -1)))
	}
	text := dna
	for _, change := range [][2]string{{"tHa[Nt]", "<4>"}, {"aND|caN|Ha[DS]|WaS", "<3>"}, {"a[NSt]|BY", "<2>"}, {"<[^>]*>", "|"}} {
		text = regexp.MustCompile(change[0]).ReplaceAllLiteralString(text, change[1])
	}
	fmt.Println(len(text))
	fmt.Println(len(regexp.MustCompile("ta+c").ReplaceAllLiteralString(dna, "x")))
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
