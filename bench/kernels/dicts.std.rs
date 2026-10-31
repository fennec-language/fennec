//! Counting keys in a hash map with Rust's default hash, SipHash, which resists attacks that
//! Fennec's dicts don't try to. Shown beside `dicts`, and never enforced.

use std::collections::HashMap;

fn main() {
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for i in 0..std::hint::black_box(2_000_000_i64) {
        *counts.entry(i * 7919 % 10_007).or_insert(0) += 1;
    }
    println!("{} {} {}", counts.len(), counts[&0], counts[&10_006]);
}
