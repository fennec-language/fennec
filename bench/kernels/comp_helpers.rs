//! Iterators that call helper functions, the twin of `comp_helpers.fen`.

fn a(i: usize, j: usize) -> f64 {
    1.0 / ((i + j) * (i + j + 1) / 2 + i + 1) as f64
}

fn row(v: &[f64], i: usize, transpose: bool) -> f64 {
    v.iter().enumerate().map(|(j, x)| (if transpose { a(j, i) } else { a(i, j) }) * x).sum()
}

fn times(v: &[f64], transpose: bool) -> Vec<f64> {
    (0..v.len()).map(|i| row(v, i, transpose)).collect()
}

fn times_transposed(v: &[f64]) -> Vec<f64> {
    times(&times(v, false), true)
}

fn main() {
    let n = std::hint::black_box(100);
    let mut u = vec![1.0; n];
    let mut v = vec![0.0; n];
    for _ in 0..300 {
        v = times_transposed(&u);
        u = times_transposed(&v);
    }
    let vbv: f64 = u.iter().zip(&v).map(|(x, y)| x * y).sum();
    let vv: f64 = v.iter().map(|x| x * x).sum();
    println!("{:.9}", (vbv / vv).sqrt());
}
