//! Comprehensions and iterators, the twin of `comprehensions.fen`.

use std::collections::{HashMap, HashSet};

fn main() {
    let size = std::hint::black_box(100_000);
    let mut total: i64 = 0;
    for round in 0..10 {
        let numbers: Vec<i64> = (0..size).map(|i| i * 2 + round).collect();
        let evens: Vec<i64> = numbers.iter().copied().filter(|n| n % 4 == 0).collect();
        let squares: Vec<i64> = evens.iter().map(|n| n * n % 1000).collect();
        total += squares.iter().filter(|n| **n > 500).sum::<i64>();
        total += squares.iter().map(|n| n % 100).collect::<HashSet<i64>>().len() as i64;
        total += evens.iter().map(|n| (n % 50, *n)).collect::<HashMap<i64, i64>>().len() as i64;
        total += numbers.iter().zip(&squares).map(|(a, b)| a * b % 7).sum::<i64>();
    }
    println!("{total}");
}
