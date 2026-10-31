//! Closures, the twin of `closures.fen`.

fn apply_all(values: &[i64], change: impl Fn(i64) -> i64) -> i64 {
    values.iter().map(|value| change(*value)).sum()
}

fn twice(value: i64, change: impl Fn(i64) -> i64) -> i64 {
    change(change(value))
}

fn main() {
    let count = std::hint::black_box(10_000);
    let values: Vec<i64> = (0..count).map(|i| i % 1000).collect();
    let mut total = 0;
    for round in 0..20 {
        let offset = round % 17;
        total += apply_all(&values, |n| n + offset);
        total += apply_all(&values, |n| twice(n, |m| m * 3 % 1001 + offset));
    }
    println!("{total}");
}
