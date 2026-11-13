//! The twin of `find.fen`, done the idiomatic way: `line.find(": ")` on a header line of about 20 bytes.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let lines: Vec<String> = (shift..shift + 16).map(|i| format!("Header-{i}: value-{}", i * 7)).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let found = lines[((at + shift) % 16) as usize].find(": ").map_or(-1, |found| found as i64);
        total += found;
    }
    println!("{total}");
}
