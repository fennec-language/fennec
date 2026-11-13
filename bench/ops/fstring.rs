//! The twin of `fstring.fen`, done the idiomatic way: `f"user-{at}"`, an f-string with one int.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let mut total: i64 = 0;
    for at in 0..ops {
        let text = format!("user-{}", at + shift);
        total += text.len() as i64;
    }
    println!("{total}");
}
