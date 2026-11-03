// Structs in a game loop, the Go twin of particles.fen.
package main

import "fmt"

type Vec2 struct {
	x, y float64
}

type Particle struct {
	position Vec2
	velocity Vec2
	bounces  int64
}

func (p *Particle) step(dt float64) {
	p.velocity.y -= 9.8 * dt
	p.position.x += p.velocity.x * dt
	p.position.y += p.velocity.y * dt
	if p.position.y < 0.0 {
		p.position.y = -p.position.y
		p.velocity.y = -p.velocity.y * 0.9
		p.bounces++
	}
}

func main() {
	count := 2000
	particles := make([]Particle, 0, count)
	for i := 0; i < count; i++ {
		particles = append(particles, Particle{
			position: Vec2{x: float64(i), y: float64(i % 50)},
			velocity: Vec2{x: float64(i%7) - 3.0, y: float64(i % 11)},
		})
	}
	for range 2500 {
		for i := range particles {
			particles[i].step(0.01)
		}
	}
	var bounces int64
	for _, particle := range particles {
		bounces += particle.bounces
	}
	height := 0.0
	for _, particle := range particles {
		height += particle.position.y
	}
	fmt.Printf("%d %.6f\n", bounces, height)
}
