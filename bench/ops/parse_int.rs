//! The twin of `parse_int.fen`, done the idiomatic way: `int.parse(text)` of a number of up to 3 digits.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let texts: Vec<String> = (shift..shift + 16).map(|i| (i * 37).to_string()).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        total += texts[((at + shift) % 16) as usize].parse::<i64>().unwrap_or(-1);
    }
    println!("{total}");
}
