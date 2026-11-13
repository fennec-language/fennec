//! The twin of `struct_new.fen`, done the idiomatic way: `Spot(name=name, x=at + shift, y=at + 1)`, a struct with a str and two ints.

use std::hint::black_box;

struct Spot<'a> {
    name: &'a str,
    x: i64,
    y: i64,
}

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let names: Vec<String> = (shift..shift + 16).map(|i| format!("spot-{i}")).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let spot = Spot { name: &names[((at + shift) % 16) as usize], x: at + shift, y: at + 1 };
        total += spot.x + spot.y + spot.name.len() as i64;
    }
    println!("{total}");
}
