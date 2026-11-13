//! The twin of `dict_get.fen`, done the idiomatic way: `counts.get(key)` on a dict of 16 str keys.

use std::collections::HashMap;
use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let keys: Vec<String> = (shift..shift + 16).map(|i| format!("key-{i}")).collect();
    let counts: HashMap<String, i64> = keys.iter().cloned().zip(shift..).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        total += counts.get(&keys[((at + shift) * 7 % 16) as usize]).copied().unwrap_or(0);
    }
    println!("{total}");
}
