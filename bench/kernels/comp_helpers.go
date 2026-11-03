// Iterators that call helper functions, the Go twin of comp_helpers.fen.
package main

import (
	"fmt"
	"math"
	"os"
	"time"
)

func a(i, j int) float64 {
	return 1.0 / float64((i+j)*(i+j+1)/2+i+1)
}

func row(v []float64, i int, transpose bool) float64 {
	sum := 0.0
	for j, x := range v {
		var entry float64
		if transpose {
			entry = a(j, i)
		} else {
			entry = a(i, j)
		}
		sum += entry * x
	}
	return sum
}

func times(v []float64, transpose bool) []float64 {
	out := make([]float64, 0, len(v))
	for i := range v {
		out = append(out, row(v, i, transpose))
	}
	return out
}

func timesTransposed(v []float64) []float64 {
	return times(times(v, false), true)
}

func main() {
	started := time.Now()
	n := 100
	u := make([]float64, n)
	for i := range u {
		u[i] = 1.0
	}
	v := make([]float64, n)
	for range 300 {
		v = timesTransposed(u)
		u = timesTransposed(v)
	}
	vbv := 0.0
	for i, x := range u {
		vbv += x * v[i]
	}
	vv := 0.0
	for _, x := range v {
		vv += x * x
	}
	fmt.Printf("%.9f\n", math.Sqrt(vbv/vv))
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
