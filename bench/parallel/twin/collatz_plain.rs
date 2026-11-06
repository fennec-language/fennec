//! The twin of `collatz_plain.fen`: plain Rust on one thread, with no rayon.

#[expect(dead_code, reason = "each twin uses only its own kernel's part of what they share")]
mod kernels;

fn main() {
    let counts: Vec<i64> = (1..=1_000_000).map(kernels::steps).collect();
    println!("{}", kernels::collatz_line(&counts));
}
