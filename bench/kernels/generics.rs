//! Generic code over an interface, the twin of `generics.fen`.

trait Shape {
    fn area(&self) -> f64;
    fn scaled(&self, by: f64) -> f64;
}

struct Circle {
    radius: f64,
}

impl Shape for Circle {
    fn area(&self) -> f64 {
        3.14159 * self.radius * self.radius
    }

    fn scaled(&self, by: f64) -> f64 {
        self.area() * by * by
    }
}

struct Square {
    side: f64,
}

impl Shape for Square {
    fn area(&self) -> f64 {
        self.side * self.side
    }

    fn scaled(&self, by: f64) -> f64 {
        self.area() * by * by
    }
}

fn total<T: Shape>(shapes: &[T], by: f64) -> f64 {
    shapes.iter().map(|shape| shape.area() + shape.scaled(by)).sum()
}

fn largest<'a, T>(items: &'a [T], better: impl Fn(&T, &T) -> bool) -> &'a T {
    let mut best = &items[0];
    for item in items {
        if better(item, best) {
            best = item;
        }
    }
    best
}

fn main() {
    let count = std::hint::black_box(5000);
    let circles: Vec<Circle> = (0..count).map(|i| Circle { radius: f64::from(i % 10) }).collect();
    let squares: Vec<Square> = (0..count).map(|i| Square { side: f64::from(i % 13) }).collect();
    let mut sum = 0.0;
    for round in 0..2500 {
        let by = f64::from(round % 3) + 1.0;
        sum += total(&circles, by) + total(&squares, by);
    }
    let biggest = largest(&circles, |a, b| a.radius > b.radius);
    println!("{sum:.3} {:?}", biggest.radius);
}
