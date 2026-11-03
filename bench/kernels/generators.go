// Generators: stages chained through names and through functions, the Go twin of generators.fen.
package main

import (
	"fmt"
	"iter"
	"strings"
	"unicode/utf8"
)

type Row struct {
	amount int64
	weight int64
}

func parse(line string) Row {
	length := int64(utf8.RuneCountInString(line))
	return Row{amount: length, weight: length % 7}
}

func makeLines(count int64) []string {
	lines := make([]string, 0, count)
	for at := int64(0); at < count; at++ {
		if at%5 == 0 {
			lines = append(lines, "   ")
		} else if at%4 == 0 {
			lines = append(lines, fmt.Sprintf("  item %d ", at))
		} else {
			lines = append(lines, fmt.Sprintf("item %d", at))
		}
	}
	return lines
}

func trimmed(src []string) iter.Seq[string] {
	return func(yield func(string) bool) {
		for _, line := range src {
			if !yield(strings.TrimSpace(line)) {
				return
			}
		}
	}
}

func parsed(lines iter.Seq[string]) iter.Seq[Row] {
	return func(yield func(Row) bool) {
		for line := range lines {
			if line != "" && !yield(parse(line)) {
				return
			}
		}
	}
}

func pipeline(src []string) int64 {
	lines := trimmed(src)
	rows := parsed(lines)
	var sum int64
	for r := range rows {
		sum += r.amount + r.weight
	}
	return sum
}

func countUp(limit int64) iter.Seq[int64] {
	return func(yield func(int64) bool) {
		for n := int64(0); n < limit; n++ {
			if !yield(n) {
				return
			}
		}
	}
}

func oddSquares(numbers iter.Seq[int64]) iter.Seq[int64] {
	return func(yield func(int64) bool) {
		for n := range numbers {
			if n%2 == 1 && !yield(n*n%1000) {
				return
			}
		}
	}
}

func running(numbers iter.Seq[int64], size int64) iter.Seq[int64] {
	return func(yield func(int64) bool) {
		var total, at int64
		for n := range numbers {
			total += n
			if at%size == 0 {
				total %= 1000
			}
			at++
			if !yield(total) {
				return
			}
		}
	}
}

func firstOver(limit, over int64) int64 {
	for n := range oddSquares(countUp(limit)) {
		if n > over {
			return n
		}
	}
	return 0
}

func main() {
	src := makeLines(20_000)
	var result int64
	for round := int64(0); round < 40; round++ {
		result += pipeline(src)
		chain := running(oddSquares(countUp(200_000+round)), 16)
		var sum int64
		for n := range chain {
			sum += n % 7
		}
		result += sum
		result += firstOver(1_000, 990-round)
	}
	fmt.Println(result)
}
