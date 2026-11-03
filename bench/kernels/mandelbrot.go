// The Mandelbrot set, the Go twin of mandelbrot.fen.
package main

import (
	"fmt"
	"os"
	"time"
)

func iterations(cr, ci float64, limit int64) int64 {
	zr, zi := 0.0, 0.0
	for i := int64(0); i < limit; i++ {
		zr2, zi2 := zr*zr, zi*zi
		if zr2+zi2 > 4.0 {
			return i
		}
		zi = 2.0*zr*zi + ci
		zr = zr2 - zi2 + cr
	}
	return limit
}

func main() {
	started := time.Now()
	size := 400
	var total, inside int64
	for y := 0; y < size; y++ {
		ci := 2.0*float64(y)/float64(size) - 1.0
		for x := 0; x < size; x++ {
			cr := 2.0*float64(x)/float64(size) - 1.5
			count := iterations(cr, ci, 100)
			total += count
			if count == 100 {
				inside++
			}
		}
	}
	fmt.Printf("%d %d\n", total, inside)
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
