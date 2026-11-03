// Building a long string piece by piece, then scanning it: the Go twin of strings.fen.
package main

import (
	"fmt"
	"strings"
)

func main() {
	var builder strings.Builder
	for i := 0; i < 2_000_000; i++ {
		if i%3 == 0 {
			builder.WriteString("fennec ")
		} else {
			builder.WriteString("fox ")
		}
	}
	text := builder.String()
	spaces := 0
	for _, c := range text {
		if c == ' ' {
			spaces++
		}
	}
	fmt.Println(len(text), spaces)
}
