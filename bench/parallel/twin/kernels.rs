//! What the twins of `collatz.fen`, `checksum.fen` and `mandel.fen` share: the work done for each
//! item, the same as the Fennec. Only how they go over the items differs.

/// How many Collatz steps `start` takes to reach 1.
pub fn steps(start: i64) -> i64 {
    let (mut n, mut count) = (start, 0);
    while n != 1 {
        n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
        count += 1;
    }
    count
}

/// The total of `counts` and the longest one, with its number, as `collatz.fen` prints them.
pub fn collatz_line(counts: &[i64]) -> String {
    let mut longest = (0, 0);
    for (at, count) in counts.iter().enumerate() {
        if *count > longest.0 {
            longest = (*count, at + 1);
        }
    }
    format!("{} {} {}", counts.iter().sum::<i64>(), longest.0, longest.1)
}

/// `n` stirred a hundred times with a Lehmer generator's step, then cut to below a thousand.
pub fn stirred(n: i64) -> i64 {
    let mut x = n % 2_147_483_646 + 1;
    for _ in 0..100 {
        x = x * 48_271 % 2_147_483_647;
    }
    x % 1_000
}

pub const WIDTH: i64 = 1200;
pub const HEIGHT: i64 = 900;
pub const LIMIT: i64 = 1000;

pub struct Pixel {
    pub x: f64,
    pub y: f64,
    pub escaped: i64,
}

pub fn pixels() -> Vec<Pixel> {
    (0..WIDTH * HEIGHT)
        .map(|at| Pixel {
            x: (at % WIDTH) as f64 / WIDTH as f64 * 3.0 - 2.0,
            y: (at / WIDTH) as f64 / HEIGHT as f64 * 3.0 - 1.5,
            escaped: 0,
        })
        .collect()
}

/// How many steps the point takes to leave the circle of radius 2, up to `LIMIT`.
pub fn escape(cx: f64, cy: f64) -> i64 {
    let (mut x, mut y, mut count) = (0.0_f64, 0.0_f64, 0);
    while count < LIMIT && x * x + y * y <= 4.0 {
        let next = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = next;
        count += 1;
    }
    count
}

/// The total of the escape counts and how many pixels never escaped, as `mandel.fen` prints them.
pub fn mandel_line(pixels: &[Pixel]) -> String {
    let total: i64 = pixels.iter().map(|pixel| pixel.escaped).sum();
    let inside = pixels.iter().filter(|pixel| pixel.escaped == LIMIT).count();
    format!("{total} {inside}")
}
