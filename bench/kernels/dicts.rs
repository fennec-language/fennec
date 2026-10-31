//! Counting keys in a hash map, the twin of `dicts.fen`, with the same hash as Fennec's dicts, so
//! it measures the table rather than the hash.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
struct Mix(u64);

impl Hasher for Mix {
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }

    fn write_i64(&mut self, n: i64) {
        let mixed = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = mixed ^ (mixed >> 29);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn main() {
    let mut counts: HashMap<i64, i64, BuildHasherDefault<Mix>> = HashMap::default();
    for i in 0..std::hint::black_box(2_000_000_i64) {
        *counts.entry(i * 7919 % 10_007).or_insert(0) += 1;
    }
    println!("{} {} {}", counts.len(), counts[&0], counts[&10_006]);
}
