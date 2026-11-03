// Generic code over an interface, the Go twin of generics.fen.
package main

import (
	"fmt"
	"strconv"
	"strings"
)

type Shape interface {
	area() float64
	scaled(by float64) float64
}

type Circle struct {
	radius float64
}

func (c Circle) area() float64 {
	return 3.14159 * c.radius * c.radius
}

func (c Circle) scaled(by float64) float64 {
	return c.area() * by * by
}

type Square struct {
	side float64
}

func (s Square) area() float64 {
	return s.side * s.side
}

func (s Square) scaled(by float64) float64 {
	return s.area() * by * by
}

func total[T Shape](shapes []T, by float64) float64 {
	sum := 0.0
	for _, shape := range shapes {
		sum += shape.area() + shape.scaled(by)
	}
	return sum
}

func largest[T any](items []T, better func(a, b *T) bool) *T {
	best := &items[0]
	for i := range items {
		if better(&items[i], best) {
			best = &items[i]
		}
	}
	return best
}

// debugFloat formats like Rust's `{:?}` for a float: shortest digits, always with a decimal point.
func debugFloat(x float64) string {
	text := strconv.FormatFloat(x, 'f', -1, 64)
	if !strings.Contains(text, ".") {
		text += ".0"
	}
	return text
}

func main() {
	count := 5000
	circles := make([]Circle, 0, count)
	squares := make([]Square, 0, count)
	for i := 0; i < count; i++ {
		circles = append(circles, Circle{radius: float64(i % 10)})
	}
	for i := 0; i < count; i++ {
		squares = append(squares, Square{side: float64(i % 13)})
	}
	sum := 0.0
	for round := 0; round < 2500; round++ {
		by := float64(round%3) + 1.0
		sum += total(circles, by) + total(squares, by)
	}
	biggest := largest(circles, func(a, b *Circle) bool { return a.radius > b.radius })
	fmt.Printf("%.3f %s\n", sum, debugFloat(biggest.radius))
}
