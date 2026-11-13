//! A particle simulation, the twin of `physics.fen`: positions and velocities in `Vec`s of a
//! small `Vec3` with its operators, radii in a `Vec<f64>`, a counting sort into a grid each step
//! for the broad phase, and overlapping pairs pushed apart and bounced.

use std::hint::black_box;
use std::ops::{Add, Mul, Sub};

const SIDE: i64 = 24;
const BOX: f64 = 24.0;
const STEP: f64 = 0.01;
const PULL: f64 = 9.8;
const BOUNCE: f64 = 0.8;

#[derive(Clone, Copy, Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, other: Vec3) -> Vec3 {
        Vec3 { x: self.x + other.x, y: self.y + other.y, z: self.z + other.z }
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, other: Vec3) -> Vec3 {
        Vec3 { x: self.x - other.x, y: self.y - other.y, z: self.z - other.z }
    }
}

impl Mul<f64> for Vec3 {
    type Output = Vec3;
    fn mul(self, scale: f64) -> Vec3 {
        Vec3 { x: self.x * scale, y: self.y * scale, z: self.z * scale }
    }
}

impl Vec3 {
    fn dot(self, other: Vec3) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn length_squared(self) -> f64 {
        self.dot(self)
    }
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A float from 0 up to 1 taken from a generator's state.
fn fraction(seed: i64) -> f64 {
    (seed / 64 % 1_000_000) as f64 / 1_000_000.0
}

/// Which cell of the grid a point is in.
fn cell_of(at: Vec3) -> usize {
    let x = (at.x as i64).min(SIDE - 1);
    let y = (at.y as i64).min(SIDE - 1);
    let z = (at.z as i64).min(SIDE - 1);
    ((z * SIDE + y) * SIDE + x) as usize
}

/// Keeps one coordinate inside the box, bouncing its speed back if it was going out.
fn kept(at: f64, speed: f64, radius: f64) -> (f64, f64) {
    if at < radius {
        return (radius, if speed < 0.0 { -speed * BOUNCE } else { speed });
    }
    if at > BOX - radius {
        return (BOX - radius, if speed > 0.0 { -speed * BOUNCE } else { speed });
    }
    (at, speed)
}

/// Moves every ball for one step, under gravity, and keeps it in the box.
fn integrate(positions: &mut [Vec3], velocities: &mut [Vec3], radii: &[f64]) {
    let pull = Vec3 { x: 0.0, y: 0.0, z: -PULL * STEP };
    for ball in 0..positions.len() {
        let velocity = velocities[ball] + pull;
        let at = positions[ball] + velocity * STEP;
        let radius = radii[ball];
        let (x, vx) = kept(at.x, velocity.x, radius);
        let (y, vy) = kept(at.y, velocity.y, radius);
        let (z, vz) = kept(at.z, velocity.z, radius);
        positions[ball] = Vec3 { x, y, z };
        velocities[ball] = Vec3 { x: vx, y: vy, z: vz };
    }
}

/// Files every ball by its cell: `order` holds the balls cell by cell, and the balls of cell `c`
/// are at `starts[c]` up to `starts[c + 1]` in it.
fn file(cells: &[usize], starts: &mut [usize], order: &mut [usize]) {
    starts.fill(0);
    for &cell in cells {
        starts[cell + 1] += 1;
    }
    for at in 1..starts.len() {
        starts[at] += starts[at - 1];
    }
    let mut filled = starts.to_vec();
    for (ball, &cell) in cells.iter().enumerate() {
        order[filled[cell]] = ball;
        filled[cell] += 1;
    }
}

/// Pushes apart and bounces every pair of balls that overlap, each pair once, and gives how many
/// pairs touched.
fn collide(
    positions: &mut [Vec3],
    velocities: &mut [Vec3],
    radii: &[f64],
    cells: &[usize],
    starts: &[usize],
    order: &[usize],
) -> i64 {
    let side = SIDE as usize;
    let mut touched = 0;
    for ball in 0..positions.len() {
        let cell = cells[ball];
        let cx = cell % side;
        let cy = cell / side % side;
        let cz = cell / (side * side);
        for z in cz.saturating_sub(1)..(cz + 2).min(side) {
            for y in cy.saturating_sub(1)..(cy + 2).min(side) {
                for x in cx.saturating_sub(1)..(cx + 2).min(side) {
                    let near = (z * side + y) * side + x;
                    for &other in &order[starts[near]..starts[near + 1]] {
                        if other <= ball {
                            continue;
                        }
                        let apart = positions[other] - positions[ball];
                        let squared = apart.length_squared();
                        let reach = radii[ball] + radii[other];
                        if squared >= reach * reach || squared == 0.0 {
                            continue;
                        }
                        let gap = squared.sqrt();
                        let normal = apart * (1.0 / gap);
                        let push = normal * ((reach - gap) * 0.5);
                        positions[ball] = positions[ball] - push;
                        positions[other] = positions[other] + push;
                        let closing = (velocities[other] - velocities[ball]).dot(normal);
                        if closing < 0.0 {
                            let kick = normal * (closing * 0.95);
                            velocities[ball] = velocities[ball] + kick;
                            velocities[other] = velocities[other] - kick;
                        }
                        touched += 1;
                    }
                }
            }
        }
    }
    touched
}

fn main() {
    let started = std::time::Instant::now();
    let balls: i64 = black_box(5_000);
    let steps = black_box(50);
    let mut positions = Vec::new();
    let mut velocities = Vec::new();
    let mut radii = Vec::new();
    let mut state = 5;
    for ball in 0..balls {
        let radius = 0.25 + 0.05 * (ball % 5) as f64;
        state = next(state);
        let x = radius + fraction(state) * (BOX - 2.0 * radius);
        state = next(state);
        let y = radius + fraction(state) * (BOX - 2.0 * radius);
        state = next(state);
        let z = radius + fraction(state) * (BOX - 2.0 * radius);
        state = next(state);
        let vx = fraction(state) * 4.0 - 2.0;
        state = next(state);
        let vy = fraction(state) * 4.0 - 2.0;
        state = next(state);
        let vz = fraction(state) * 4.0 - 2.0;
        positions.push(Vec3 { x, y, z });
        velocities.push(Vec3 { x: vx, y: vy, z: vz });
        radii.push(radius);
    }
    let mut starts = vec![0; (SIDE * SIDE * SIDE + 1) as usize];
    let mut order = vec![0; balls as usize];
    let mut touched = 0;
    for _ in 0..steps {
        integrate(&mut positions, &mut velocities, &radii);
        let cells: Vec<usize> = positions.iter().map(|&at| cell_of(at)).collect();
        file(&cells, &mut starts, &mut order);
        touched += collide(&mut positions, &mut velocities, &radii, &cells, &starts, &order);
    }
    let mut energy = 0.0;
    let mut spread = 0.0;
    for ball in 0..balls as usize {
        energy += 0.5 * velocities[ball].length_squared() + PULL * positions[ball].z;
        spread += positions[ball].x + 2.0 * positions[ball].y;
    }
    println!("{} {} {touched}", (energy * 1000.0) as i64, (spread * 1000.0) as i64);
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
