//! The twin of `ecs.fen` without `parallel`: plain Rust on one thread, with no rayon.

mod world;

fn main() {
    let (mut units, hazards) = world::world();
    for _ in 0..200 {
        for unit in &mut units {
            world::advance(unit, &hazards, 0.5);
        }
    }
    let alive: i64 = units.iter().map(|unit| i64::from(unit.health > 0.0)).sum();
    let health: f64 = units.iter().map(|unit| unit.health).sum();
    println!("{alive} {health:?}");
}
