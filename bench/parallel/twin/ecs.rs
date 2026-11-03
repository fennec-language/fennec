//! The twin of `ecs.fen`: the same ticks over the same units, as Rust with rayon writes them.

use rayon::prelude::*;

mod world;

fn main() {
    let (mut units, hazards) = world::world();
    for _ in 0..200 {
        units.par_iter_mut().for_each(|unit| world::advance(unit, &hazards, 0.5));
    }
    let alive: i64 = units.par_iter().map(|unit| i64::from(unit.health > 0.0)).sum();
    let health: f64 = units.par_iter().map(|unit| unit.health).sum();
    println!("{alive} {health:?}");
}
