//! The twin of `cache.fen`.
use std::collections::HashMap;
use std::sync::RwLock;

use rayon::prelude::*;

const KEYS: i64 = 1000;

fn main() {
    let cache: RwLock<HashMap<String, i64>> = RwLock::new(HashMap::new());
    let ops: Vec<i64> = (0..1_000_000).collect();
    let total: i64 = ops
        .par_iter()
        .map(|n| {
            let key = format!("a key with a long enough name to have a block, number {}", n % KEYS);
            let found = cache.read().unwrap().get(&key).copied();
            match found {
                Some(found) => found,
                None => {
                    cache.write().unwrap().insert(key, n % KEYS);
                    0
                }
            }
        })
        .sum();
    println!("{}", total >= 0);
}
