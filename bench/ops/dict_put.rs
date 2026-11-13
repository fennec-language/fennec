//! The twin of `dict_put.fen`, done the idiomatic way: `counts[key] = at` on a dict of 16 str keys.

use std::collections::HashMap;
use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let keys: Vec<String> = (shift..shift + 16).map(|i| format!("key-{i}")).collect();
    let mut counts: HashMap<String, i64> = HashMap::new();
    let mut total: i64 = 0;
    for at in 0..ops {
        let key = &keys[((at + shift) % 16) as usize];
        if let Some(count) = counts.get_mut(key) {
            *count = at + shift;
        } else {
            counts.insert(key.clone(), at + shift);
        }
        total += counts.len() as i64;
    }
    println!("{total}");
}
