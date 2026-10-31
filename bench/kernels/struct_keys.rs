//! A dict keyed by a plain struct, the twin of `struct_keys.fen`.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
struct Mix(u64);

impl Hasher for Mix {
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }

    fn write_i64(&mut self, n: i64) {
        self.0 = (self.0.rotate_left(5) ^ n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }
}

#[derive(PartialEq, Eq, Hash)]
struct Cell {
    x: i64,
    y: i64,
}

fn main() {
    let mut seen: HashMap<Cell, i64, BuildHasherDefault<Mix>> = HashMap::default();
    for i in 0..std::hint::black_box(5_000_i64) {
        *seen.entry(Cell { x: i * 31 % 101, y: i * 17 % 103 }).or_insert(0) += 1;
    }
    println!("{} {}", seen.len(), seen[&Cell { x: 0, y: 0 }]);
}
