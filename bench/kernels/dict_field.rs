//! A dict in a struct's field, the twin of `dict_field.fen`, with the same hash as Fennec's dicts.

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

struct Counter {
    counts: HashMap<i64, i64, BuildHasherDefault<Mix>>,
}

impl Counter {
    fn bump(&mut self, key: i64) {
        *self.counts.entry(key).or_insert(0) += 1;
    }
}

fn main() {
    let mut counter = Counter { counts: HashMap::default() };
    for i in 0..std::hint::black_box(50_000_i64) {
        counter.bump(i * 7919 % 64);
    }
    println!("{} {} {}", counter.counts.len(), counter.counts[&0], counter.counts[&63]);
}
