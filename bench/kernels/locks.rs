//! A lock taken in a loop, the twin of `locks.fen`: a counter and a dict, each behind a mutex,
//! with the same hash as Fennec's dicts.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Mutex;

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

struct Hits {
    count: i64,
}

struct Table {
    counts: HashMap<i64, i64, BuildHasherDefault<Mix>>,
}

fn main() {
    let started = std::time::Instant::now();
    let hits = Mutex::new(Hits { count: 0 });
    let table = Mutex::new(Table { counts: HashMap::default() });
    for i in 0..std::hint::black_box(1_000_000_i64) {
        hits.lock().unwrap().count += 1;
        let key = i * 7919 % 64;
        *table.lock().unwrap().counts.entry(key).or_insert(0) += 1;
    }
    println!("{}", hits.lock().unwrap().count);
    let table = table.lock().unwrap();
    println!("{} {} {}", table.counts.len(), table.counts[&0], table.counts[&63]);
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
