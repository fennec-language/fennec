//! The twin of `queue.fen`.
use std::sync::Mutex;

use rayon::prelude::*;

fn main() {
    let queue: Mutex<Vec<i64>> = Mutex::new(Vec::new());
    let ops: Vec<i64> = (0..1_000_000).collect();
    let total: i64 = ops
        .par_iter()
        .map(|n| {
            queue.lock().unwrap().push(*n);
            queue.lock().unwrap().pop().unwrap_or(0) % 2
        })
        .sum();
    println!("{total}");
}
