//! A binary format as a real Rust program uses one: loading bytes into typed structs with serde
//! and bincode, using them, and writing them back. The stats are a `BTreeMap`, whose keys come
//! out in the order Fennec's dict keeps them, so writing back gives the same bytes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum Kind {
    Player,
    Monster,
    Trader,
}

#[derive(Serialize, Deserialize)]
enum Order {
    Hold,
    Move { x: i64, y: i64 },
    Say { text: String },
}

#[derive(Serialize, Deserialize)]
struct Spot {
    x: f64,
    y: f64,
}

#[derive(Serialize, Deserialize)]
struct Item {
    name: String,
    count: i64,
    tags: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Unit {
    id: i64,
    name: String,
    active: bool,
    health: f64,
    kind: Kind,
    pet: Option<String>,
    at: Spot,
    tags: Vec<String>,
    bag: Vec<Item>,
    stats: BTreeMap<String, i64>,
    orders: Vec<Order>,
}

fn order(i: i64) -> Order {
    match i % 3 {
        0 => Order::Hold,
        1 => Order::Move { x: i, y: -i },
        _ => Order::Say { text: format!("unit {i} says \"hello\"\n") },
    }
}

fn unit(i: i64) -> Unit {
    let kind = match i % 3 {
        0 => Kind::Player,
        1 => Kind::Monster,
        _ => Kind::Trader,
    };
    Unit {
        id: i,
        name: format!("fox-{i}"),
        active: i % 2 == 0,
        health: (i % 100) as f64 + 0.5,
        kind,
        pet: if i % 4 == 0 { Some(format!("pet-{i}")) } else { None },
        at: Spot { x: i as f64 * 0.25, y: i as f64 * -0.5 },
        tags: vec!["fast".to_owned(), "été".to_owned(), format!("group-{}", i % 7)],
        bag: (0..i % 5)
            .map(|at| Item {
                name: format!("item-{at}"),
                count: i + at,
                tags: vec!["a".to_owned(), "b".to_owned()],
            })
            .collect(),
        stats: BTreeMap::from([
            ("armor".to_owned(), i % 11),
            ("attack".to_owned(), i % 13),
            ("speed".to_owned(), i % 17),
        ]),
        orders: (0..3).map(|at| order(i + at)).collect(),
    }
}

fn score(units: &[Unit]) -> i64 {
    let mut total = 0;
    for unit in units {
        // Fennec's `len` counts characters, and these names and tags are ASCII.
        total += unit.id + unit.name.len() as i64 + unit.tags.len() as i64 + unit.stats["attack"];
        if unit.active {
            total += 1;
        }
        if unit.pet.is_some() {
            total += 2;
        }
        for item in &unit.bag {
            total += item.count;
        }
        for order in &unit.orders {
            match order {
                Order::Hold => total += 1,
                Order::Move { x, y } => total += x - y,
                Order::Say { text } => total += text.len() as i64,
            }
        }
    }
    total
}

fn main() {
    let started = std::time::Instant::now();
    let units: Vec<Unit> = (0..std::hint::black_box(300)).map(unit).collect();
    let data = bincode::serialize(&units).expect("it writes");
    let mut scored = 0;
    let mut same = 0;
    for _ in 0..std::hint::black_box(40) {
        let units: Vec<Unit> = bincode::deserialize(&data).expect("it loads");
        scored += score(&units);
        if bincode::serialize(&units).expect("it writes") == data {
            same += 1;
        }
    }
    println!("{scored} {same}");
    if std::env::args().any(|arg| arg == "--time") {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
