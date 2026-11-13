//! The twin of `concat.fen`, done the idiomatic way: `head + body`, joining a 20-byte and a 30-byte str.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let heads: Vec<String> = (shift..shift + 16).map(|i| format!("HTTP/1.1 {} OK\r\n", 200 + i)).collect();
    let bodies: Vec<String> = (shift..shift + 16).map(|i| format!("{{\"id\": {i}, \"name\": \"fox-{i}\"}}")).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let joined = [heads[((at + shift) % 16) as usize].as_str(), bodies[((at + shift + 3) % 16) as usize].as_str()].concat();
        total += joined.len() as i64;
    }
    println!("{total}");
}
