//! The twin of `item_copy_change.fen`, done the idiomatic way: a struct cloned out of a list of 16, then changed.

use std::hint::black_box;

#[derive(Clone)]
#[allow(dead_code)]
enum Outcome {
    Good(i64),
    Bad(String),
}

#[derive(Clone)]
#[allow(dead_code)]
struct Inner {
    label: String,
    weights: Vec<i64>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct Item {
    id: i64,
    name: String,
    tags: Vec<String>,
    scores: Vec<i64>,
    inner: Inner,
    state: Outcome,
}

fn make(n: i64) -> Item {
    Item {
        id: n,
        name: format!("a name long enough for a block {n}"),
        tags: vec!["one".to_owned(), "two".to_owned()],
        scores: vec![n, n + 1],
        inner: Inner { label: format!("a label long enough for a block {n}"), weights: vec![n] },
        state: Outcome::Bad(format!("a reason long enough for a block {n}")),
    }
}

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let items: Vec<Item> = (0..16).map(|n| make(n + shift)).collect();
    let one = make(7 + shift);
    let mut total: i64 = 0;
    for at in 0..ops {
        let mut mine = items[((at + shift) % 16) as usize].clone();
        mine.id += at;
        total += mine.id + mine.name.len() as i64;
    }
    println!("{total} {} {} {}", items.len(), items[shift as usize].id, one.name);
}
