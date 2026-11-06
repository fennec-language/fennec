//! The twin of `mandel_plain.fen`: plain Rust on one thread, with no rayon.

#[expect(dead_code, reason = "each twin uses only its own kernel's part of what they share")]
mod kernels;

fn main() {
    let mut pixels = kernels::pixels();
    for pixel in &mut pixels {
        pixel.escaped = kernels::escape(pixel.x, pixel.y);
    }
    println!("{}", kernels::mandel_line(&pixels));
}
