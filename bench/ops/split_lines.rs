//! The twin of `split_lines.fen`, done the idiomatic way: `text.split("\r\n")` of a request's
//! head into its 4 lines.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let texts: Vec<String> = (shift..shift + 16)
        .map(|i| format!("POST /orders/{i} HTTP/1.1\r\nHost: fennec.example\r\nContent-Type: application/json\r\nX-Request: {}", i * 7))
        .collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let lines: Vec<&str> = texts[((at + shift) % 16) as usize].split("\r\n").collect();
        total += (lines.len() + lines[2].len()) as i64;
    }
    println!("{total}");
}
