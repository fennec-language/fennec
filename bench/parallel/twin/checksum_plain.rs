//! The twin of `checksum_plain.fen`: plain Rust on one thread, with no rayon.

#[expect(dead_code, reason = "each twin uses only its own kernel's part of what they share")]
mod kernels;

fn main() {
    println!("{}", (0..2_000_000).map(kernels::stirred).sum::<i64>());
}
