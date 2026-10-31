//! Chained method calls on small structs, the twin of `method_chain.fen`.

#[derive(Clone, Copy)]
struct Complex {
    re: f64,
    im: f64,
}

impl Complex {
    fn plus(self, other: Complex) -> Complex {
        Complex { re: self.re + other.re, im: self.im + other.im }
    }

    fn times(self, other: Complex) -> Complex {
        Complex { re: self.re * other.re - self.im * other.im, im: self.re * other.im + self.im * other.re }
    }

    fn norm(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

fn iterations(c: Complex, limit: i64) -> i64 {
    let mut z = Complex { re: 0.0, im: 0.0 };
    for i in 0..limit {
        if z.norm() > 4.0 {
            return i;
        }
        z = z.times(z).plus(c);
    }
    limit
}

fn main() {
    let size = std::hint::black_box(500);
    let mut total = 0;
    for y in 0..size {
        let ci = 2.0 * f64::from(y) / f64::from(size) - 1.0;
        for x in 0..size {
            let cr = 2.0 * f64::from(x) / f64::from(size) - 1.5;
            total += iterations(Complex { re: cr, im: ci }, 100);
        }
    }
    println!("{total}");
}
