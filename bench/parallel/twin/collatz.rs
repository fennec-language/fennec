//! The twin of `collatz.fen`: the same steps, the numbers shared out by rayon.

use rayon::prelude::*;

#[expect(dead_code, reason = "each twin uses only its own kernel's part of what they share")]
mod kernels;

fn main() {
    let counts: Vec<i64> = (1..=1_000_000).into_par_iter().map(kernels::steps).collect();
    println!("{}", kernels::collatz_line(&counts));
}
