//! The twin of `parse_float.fen`, done the idiomatic way: `text.parse::<f64>()` of a price like
//! "1234.56".

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let texts: Vec<String> = (shift..shift + 16).map(|i| format!("{}.{}", i * 37, i + 10)).collect();
    let mut total: f64 = 0.0;
    for at in 0..ops {
        total += texts[((at + shift) % 16) as usize].parse::<f64>().unwrap_or(-1.0);
    }
    println!("{total}");
}
