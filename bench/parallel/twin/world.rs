//! What `ecs.fen`'s twins share: the same units, moved the same way. Only how they go over the
//! units differs.

pub const SIZE: f64 = 1000.0;
pub const NEAR: f64 = 400.0;

#[derive(Clone, Copy)]
pub struct Unit {
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub health: f64,
}

fn spread(n: i64, top: f64) -> f64 {
    ((n * 7919 + 104729) % 100003) as f64 / 100003.0 * top
}

pub fn world() -> (Vec<Unit>, Vec<(f64, f64)>) {
    let units = (0..200_000)
        .map(|n| Unit {
            x: spread(n, SIZE),
            y: spread(n * 3, SIZE),
            vx: spread(n * 5, 20.0) - 10.0,
            vy: spread(n * 7, 20.0) - 10.0,
            health: 100.0,
        })
        .collect();
    (units, (0..32).map(|h| (spread(h * 11, SIZE), spread(h * 13, SIZE))).collect())
}

/// One unit's tick.
#[inline(always)]
pub fn advance(unit: &mut Unit, hazards: &[(f64, f64)], dt: f64) {
    unit.x += unit.vx * dt;
    unit.y += unit.vy * dt;
    if unit.x < 0.0 || unit.x > SIZE {
        unit.vx = -unit.vx;
    }
    if unit.y < 0.0 || unit.y > SIZE {
        unit.vy = -unit.vy;
    }
    let mut nearest = SIZE * SIZE * 4.0;
    for &(hx, hy) in hazards {
        let (dx, dy) = (unit.x - hx, unit.y - hy);
        let far = dx * dx + dy * dy;
        if far < nearest {
            nearest = far;
        }
    }
    if nearest < NEAR {
        unit.health -= 1.0;
    }
}
