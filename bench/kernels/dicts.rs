//! Counting keys in a hash map, the twin of `dicts.fen`.

use std::collections::HashMap;

fn main() {
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for i in 0..std::hint::black_box(20_000_i64) {
        *counts.entry(i * 7919 % 10_007).or_insert(0) += 1;
    }
    println!("{} {} {}", counts.len(), counts[&0], counts[&10_006]);
}
