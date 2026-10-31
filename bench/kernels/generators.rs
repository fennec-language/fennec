//! Generators, the twin of `generators.fen`. Each function collects what the Fennec one yields.

fn count_up(n: i64) -> Vec<i64> {
    (0..n).collect()
}

fn odd_squares(items: &[i64]) -> Vec<i64> {
    items.iter().filter(|item| **item % 2 == 1).map(|item| item * item % 1000).collect()
}

fn windows(items: &[i64], size: usize) -> Vec<i64> {
    let mut total = 0;
    let mut out = Vec::with_capacity(items.len());
    for (at, item) in items.iter().enumerate() {
        total += item;
        if at >= size {
            total -= items[at - size];
        }
        out.push(total);
    }
    out
}

fn main() {
    let mut result = 0;
    for round in 0..std::hint::black_box(30) {
        let squares = odd_squares(&count_up(100_000 + round));
        result += windows(&squares, 16).iter().map(|n| n % 7).sum::<i64>();
    }
    println!("{result}");
}
