//! Structs in a game loop, the twin of `particles.fen`.

struct Vec2 {
    x: f64,
    y: f64,
}

struct Particle {
    position: Vec2,
    velocity: Vec2,
    bounces: i64,
}

impl Particle {
    fn step(&mut self, dt: f64) {
        self.velocity.y -= 9.8 * dt;
        self.position.x += self.velocity.x * dt;
        self.position.y += self.velocity.y * dt;
        if self.position.y < 0.0 {
            self.position.y = -self.position.y;
            self.velocity.y = -self.velocity.y * 0.9;
            self.bounces += 1;
        }
    }
}

fn main() {
    let count = std::hint::black_box(2000);
    let mut particles: Vec<Particle> = (0..count)
        .map(|i| Particle {
            position: Vec2 { x: f64::from(i), y: f64::from(i % 50) },
            velocity: Vec2 { x: f64::from(i % 7) - 3.0, y: f64::from(i % 11) },
            bounces: 0,
        })
        .collect();
    for _ in 0..2500 {
        for particle in &mut particles {
            particle.step(0.01);
        }
    }
    let bounces: i64 = particles.iter().map(|particle| particle.bounces).sum();
    let height: f64 = particles.iter().map(|particle| particle.position.y).sum();
    println!("{bounces} {height:.6}");
}
