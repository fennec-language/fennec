//! The twin of `slice.fen`, done the idiomatic way: `stamp[11:16]`, the minute out of a timestamp, compared with another.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let stamps: Vec<String> = (shift + 10..shift + 26).map(|i| format!("2026-11-12T09:{i}:00.000Z")).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let minute = &stamps[((at + shift) % 16) as usize][11..16];
        total += minute.len() as i64;
        if minute == "09:15" {
            total += 1;
        }
    }
    println!("{total}");
}
