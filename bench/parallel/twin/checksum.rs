//! The twin of `checksum.fen`: the same stirring, added up by rayon.

use rayon::prelude::*;

#[expect(dead_code, reason = "each twin uses only its own kernel's part of what they share")]
mod kernels;

fn main() {
    println!("{}", (0..2_000_000).into_par_iter().map(kernels::stirred).sum::<i64>());
}
