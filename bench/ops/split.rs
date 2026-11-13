//! The twin of `split.fen`, done the idiomatic way: `line.split(" ")` of a request line into its 3 parts.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let lines: Vec<String> = (shift..shift + 16).map(|i| format!("GET /users/{i} HTTP/1.1")).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let parts: Vec<&str> = lines[((at + shift) % 16) as usize].split(' ').collect();
        total += (parts.len() + parts[1].len()) as i64;
    }
    println!("{total}");
}
