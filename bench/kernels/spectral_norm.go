// The spectral norm from the Benchmarks Game, the Go twin of spectral_norm.fen.
package main

import (
	"fmt"
	"math"
	"os"
	"strconv"
	"time"
)

func a(i, j int) float64 {
	return 1.0 / float64((i+j)*(i+j+1)/2+i+1)
}

func times(v []float64, transpose bool) []float64 {
	result := make([]float64, len(v))
	for i := range v {
		sum := 0.0
		for j, x := range v {
			if transpose {
				sum += a(j, i) * x
			} else {
				sum += a(i, j) * x
			}
		}
		result[i] = sum
	}
	return result
}

func timesTransposed(v []float64) []float64 {
	return times(times(v, false), true)
}

func main() {
	started := time.Now()
	n := 400
	u := make([]float64, n)
	for i := range u {
		u[i] = 1.0
	}
	v := make([]float64, n)
	for range 10 {
		v = timesTransposed(u)
		u = timesTransposed(v)
	}
	vbv, vv := 0.0, 0.0
	for i := range u {
		vbv += u[i] * v[i]
	}
	for _, x := range v {
		vv += x * x
	}
	fmt.Println(strconv.FormatFloat(math.Sqrt(vbv/vv), 'f', 9, 64))
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
