//! Naive recursive Fibonacci, the twin of `fib.fen`.

fn fib(n: i64) -> i64 {
    if n < 2 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

fn main() {
    let started = std::time::Instant::now();
    println!("{}", fib(std::hint::black_box(35)));
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
