//! The Mandelbrot set, the twin of `mandelbrot.fen`.

fn iterations(cr: f64, ci: f64, limit: i64) -> i64 {
    let (mut zr, mut zi) = (0.0, 0.0);
    for i in 0..limit {
        let (zr2, zi2) = (zr * zr, zi * zi);
        if zr2 + zi2 > 4.0 {
            return i;
        }
        zi = 2.0 * zr * zi + ci;
        zr = zr2 - zi2 + cr;
    }
    limit
}

fn main() {
    let size = std::hint::black_box(400);
    let (mut total, mut inside) = (0, 0);
    for y in 0..size {
        let ci = 2.0 * f64::from(y) / f64::from(size) - 1.0;
        for x in 0..size {
            let cr = 2.0 * f64::from(x) / f64::from(size) - 1.5;
            let count = iterations(cr, ci, 100);
            total += count;
            if count == 100 {
                inside += 1;
            }
        }
    }
    println!("{total} {inside}");
}
