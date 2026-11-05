//! The twin of `queue_text.fen`.
use std::sync::Mutex;

use rayon::prelude::*;

fn main() {
    let queue: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let ops: Vec<i64> = (0..300_000).collect();
    let total: usize = ops
        .par_iter()
        .map(|n| {
            queue.lock().unwrap().push(format!("a piece of work with a long enough name, number {n}"));
            queue.lock().unwrap().pop().unwrap_or_default().len() % 2
        })
        .sum();
    println!("{total}");
}
