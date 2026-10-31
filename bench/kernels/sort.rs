//! Sorting, the twin of `sort.fen`.

fn main() {
    let mut seed: i64 = std::hint::black_box(42);
    let mut numbers = Vec::with_capacity(300_000);
    for _ in 0..300_000 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        numbers.push(seed);
    }
    let mut ordered = numbers.clone();
    ordered.sort();
    let mut by_digit = numbers.clone();
    by_digit.sort_by_key(|n| n % 1000);
    let mut words: Vec<String> = numbers[..50_000].iter().map(|n| format!("fox{}", n % 100000)).collect();
    words.sort();
    println!("{} {} {} {} {}", ordered[0], ordered[150_000], by_digit[0], by_digit[150_000], words[25_000]);
}
