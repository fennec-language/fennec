//! The sieve of Eratosthenes, the twin of `sieve.fen`.

fn count_primes(limit: usize) -> i64 {
    let mut composite = vec![0_i64; limit + 1];
    let mut count = 0;
    for n in 2..=limit {
        if composite[n] == 0 {
            count += 1;
            let mut multiple = n * n;
            while multiple <= limit {
                composite[multiple] = 1;
                multiple += n;
            }
        }
    }
    count
}

fn main() {
    println!("{}", count_primes(std::hint::black_box(5_000_000)));
}
