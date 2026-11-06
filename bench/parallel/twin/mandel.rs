//! The twin of `mandel.fen`: each pixel's escape count written in place, the pixels shared out by
//! rayon.

use rayon::prelude::*;

#[expect(dead_code, reason = "each twin uses only its own kernel's part of what they share")]
mod kernels;

fn main() {
    let mut pixels = kernels::pixels();
    pixels.par_iter_mut().for_each(|pixel| pixel.escaped = kernels::escape(pixel.x, pixel.y));
    println!("{}", kernels::mandel_line(&pixels));
}
