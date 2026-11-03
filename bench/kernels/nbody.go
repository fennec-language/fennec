// The n-body simulation from the Benchmarks Game, the Go twin of nbody.fen.
package main

import (
	"fmt"
	"math"
	"os"
	"strconv"
	"time"
)

type Body struct {
	x, y, z, vx, vy, vz, mass float64
}

func energy(bodies []Body) float64 {
	total := 0.0
	for i, a := range bodies {
		total += 0.5 * a.mass * (a.vx*a.vx + a.vy*a.vy + a.vz*a.vz)
		for _, b := range bodies[i+1:] {
			dx, dy, dz := a.x-b.x, a.y-b.y, a.z-b.z
			total -= a.mass * b.mass / math.Sqrt(dx*dx+dy*dy+dz*dz)
		}
	}
	return total
}

func advance(bodies []Body, dt float64) {
	count := len(bodies)
	for i := 0; i < count; i++ {
		for j := i + 1; j < count; j++ {
			dx, dy, dz := bodies[i].x-bodies[j].x, bodies[i].y-bodies[j].y, bodies[i].z-bodies[j].z
			squared := dx*dx + dy*dy + dz*dz
			magnitude := dt / (squared * math.Sqrt(squared))
			massI, massJ := bodies[i].mass, bodies[j].mass
			bodies[i].vx -= dx * massJ * magnitude
			bodies[i].vy -= dy * massJ * magnitude
			bodies[i].vz -= dz * massJ * magnitude
			bodies[j].vx += dx * massI * magnitude
			bodies[j].vy += dy * massI * magnitude
			bodies[j].vz += dz * massI * magnitude
		}
	}
	for i := range bodies {
		body := &bodies[i]
		body.x += dt * body.vx
		body.y += dt * body.vy
		body.z += dt * body.vz
	}
}

func main() {
	started := time.Now()
	pi := math.Pi
	solarMass := 4.0 * pi * pi
	days := 365.24
	planet := func(x, y, z, vx, vy, vz, mass float64) Body {
		return Body{x, y, z, vx * days, vy * days, vz * days, mass * solarMass}
	}
	bodies := []Body{
		{0.0, 0.0, 0.0, 0.0, 0.0, 0.0, solarMass},
		planet(4.84143144246472090e+00, -1.16032004402742839e+00, -1.03622044471123109e-01, 1.66007664274403694e-03, 7.69901118419740425e-03, -6.90460016972063023e-05, 9.54791938424326609e-04),
		planet(8.34336671824457987e+00, 4.12479856412430479e+00, -4.03523417114321381e-01, -2.76742510726862411e-03, 4.99852801234917238e-03, 2.30417297573763929e-05, 2.85885980666130812e-04),
		planet(1.28943695621391310e+01, -1.51111514016986312e+01, -2.23307578892655734e-01, 2.96460137564761618e-03, 2.37847173959480950e-03, -2.96589568540237556e-05, 4.36624404335156298e-05),
		planet(1.53796971148509165e+01, -2.59193146099879641e+01, 1.79258772950371181e-01, 2.68067772490389322e-03, 1.62824170038242295e-03, -9.51592254519715870e-05, 5.15138902046611451e-05),
	}
	px, py, pz := 0.0, 0.0, 0.0
	for _, body := range bodies {
		px += body.vx * body.mass
		py += body.vy * body.mass
		pz += body.vz * body.mass
	}
	bodies[0].vx = -px / solarMass
	bodies[0].vy = -py / solarMass
	bodies[0].vz = -pz / solarMass
	fmt.Println(strconv.FormatFloat(energy(bodies), 'f', 9, 64))
	for range 200_000 {
		advance(bodies, 0.01)
	}
	fmt.Println(strconv.FormatFloat(energy(bodies), 'f', 9, 64))
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
