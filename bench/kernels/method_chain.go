// Chained method calls on small structs, the Go twin of method_chain.fen.
package main

import "fmt"

type Complex struct {
	re, im float64
}

func (c Complex) plus(other Complex) Complex {
	return Complex{re: c.re + other.re, im: c.im + other.im}
}

func (c Complex) times(other Complex) Complex {
	return Complex{re: c.re*other.re - c.im*other.im, im: c.re*other.im + c.im*other.re}
}

func (c Complex) norm() float64 {
	return c.re*c.re + c.im*c.im
}

func iterations(c Complex, limit int64) int64 {
	z := Complex{re: 0.0, im: 0.0}
	for i := int64(0); i < limit; i++ {
		if z.norm() > 4.0 {
			return i
		}
		z = z.times(z).plus(c)
	}
	return limit
}

func main() {
	size := 500
	var total int64
	for y := 0; y < size; y++ {
		ci := 2.0*float64(y)/float64(size) - 1.0
		for x := 0; x < size; x++ {
			cr := 2.0*float64(x)/float64(size) - 1.5
			total += iterations(Complex{re: cr, im: ci}, 100)
		}
	}
	fmt.Println(total)
}
