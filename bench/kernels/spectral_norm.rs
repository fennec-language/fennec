//! The spectral norm from the Benchmarks Game, the twin of `spectral_norm.fen`.

fn a(i: usize, j: usize) -> f64 {
    1.0 / ((i + j) * (i + j + 1) / 2 + i + 1) as f64
}

fn times(v: &[f64], transpose: bool) -> Vec<f64> {
    (0..v.len())
        .map(|i| v.iter().enumerate().map(|(j, x)| if transpose { a(j, i) } else { a(i, j) } * x).sum())
        .collect()
}

fn times_transposed(v: &[f64]) -> Vec<f64> {
    times(&times(v, false), true)
}

fn main() {
    let n = std::hint::black_box(400);
    let mut u = vec![1.0; n];
    let mut v = vec![0.0; n];
    for _ in 0..10 {
        v = times_transposed(&u);
        u = times_transposed(&v);
    }
    let vbv: f64 = u.iter().zip(&v).map(|(u, v)| u * v).sum();
    let vv: f64 = v.iter().map(|v| v * v).sum();
    println!("{:.9}", (vbv / vv).sqrt());
}
