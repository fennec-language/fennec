//! A game server's tick, the twin of `game_tick.fen`: entities in a `Vec`, each with an inventory
//! in a `HashMap`, the players' commands through a bounded channel from their own thread, the
//! grid's cells split across scoped threads and put back together in order, and a snapshot of
//! what changed written in `std.pack`'s compact form: zigzag varints, after a count.

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::mpsc::{sync_channel, SyncSender};

const TICKS: i64 = 40;
const COMMANDS: i64 = 64;
const SIZE: i64 = 2048;
const CELL: i64 = 32;
const SIDE: i64 = 64;
const RANGE: i64 = 1024;
const ITEMS: [&str; 8] = ["gold", "arrow", "potion", "bone", "herb", "gem", "scroll", "key"];

struct Entity {
    number: i64,
    x: i64,
    y: i64,
    vx: i64,
    vy: i64,
    health: i64,
    inventory: HashMap<&'static str, i64>,
    changed: bool,
}

/// What a client is sent about an entity that changed.
struct Snap {
    number: i64,
    x: i64,
    y: i64,
    health: i64,
    carried: i64,
}

enum Command {
    Move { who: usize, vx: i64, vy: i64 },
    Give { who: usize, item: usize, count: i64 },
    Heal { who: usize, amount: i64 },
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// The players: every tick's commands, sent in order from their own thread.
fn players(out: SyncSender<Command>, seed: i64, entities: i64) {
    let mut state = seed;
    for _ in 0..TICKS * COMMANDS {
        state = next(state);
        let who = below(state, entities) as usize;
        state = next(state);
        let kind = below(state, 3);
        state = next(state);
        let a = below(state, 17);
        state = next(state);
        let b = below(state, 17);
        let command = match kind {
            0 => Command::Move { who, vx: a - 8, vy: b - 8 },
            1 => Command::Give { who, item: a as usize % ITEMS.len(), count: b + 1 },
            _ => Command::Heal { who, amount: a },
        };
        if out.send(command).is_err() {
            return;
        }
    }
}

fn apply(entities: &mut [Entity], command: Command) {
    match command {
        Command::Move { who, vx, vy } => {
            entities[who].vx = vx;
            entities[who].vy = vy;
        }
        Command::Give { who, item, count } => {
            let entity = &mut entities[who];
            *entity.inventory.entry(ITEMS[item]).or_insert(0) += count;
            entity.changed = true;
        }
        Command::Heal { who, amount } => {
            let entity = &mut entities[who];
            entity.health = (entity.health + amount).min(100);
            entity.changed = true;
        }
    }
}

/// Moves one axis, bouncing off the world's edges.
fn moved(at: i64, speed: i64) -> (i64, i64) {
    let there = at + speed;
    if there < 0 {
        (-there, -speed)
    } else if there >= SIZE {
        (2 * (SIZE - 1) - there, -speed)
    } else {
        (there, speed)
    }
}

fn integrate(entities: &mut [Entity]) {
    for entity in entities {
        entity.changed = false;
        if entity.vx != 0 || entity.vy != 0 {
            (entity.x, entity.vx) = moved(entity.x, entity.vx);
            (entity.y, entity.vy) = moved(entity.y, entity.vy);
            entity.changed = true;
        }
    }
}

/// What the entities in one cell take from their neighbours within range: damage from other
/// teams, closer hurting more, and a little healing from their own.
fn cell_health(cell: i64, cells: &[Vec<usize>], xs: &[i64], ys: &[i64]) -> Vec<i64> {
    let (gx, gy) = (cell % SIDE, cell / SIDE);
    let mut changes = Vec::new();
    for &me in &cells[cell as usize] {
        let mut change = 0;
        for ny in (gy - 1).max(0)..(gy + 2).min(SIDE) {
            for nx in (gx - 1).max(0)..(gx + 2).min(SIDE) {
                for &other in &cells[(ny * SIDE + nx) as usize] {
                    let dx = xs[other] - xs[me];
                    let dy = ys[other] - ys[me];
                    let near = dx * dx + dy * dy;
                    if other != me && near < RANGE {
                        if other % 4 == me % 4 {
                            change += 1;
                        } else {
                            change -= 1 + (RANGE - near) / 256;
                        }
                    }
                }
            }
        }
        changes.push(change);
    }
    changes
}

/// `std.pack`'s form for an int: zigzag, then 7 bits a byte, low first.
fn varint(out: &mut Vec<u8>, value: i64) {
    let mut rest = ((value << 1) ^ (value >> 63)) as u64;
    while rest >= 128 {
        out.push((rest & 127) as u8 | 128);
        rest >>= 7;
    }
    out.push(rest as u8);
}

fn packed(snaps: &[Snap]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut count = snaps.len() as u64;
    while count >= 128 {
        out.push((count & 127) as u8 | 128);
        count >>= 7;
    }
    out.push(count as u8);
    for snap in snaps {
        varint(&mut out, snap.number);
        varint(&mut out, snap.x);
        varint(&mut out, snap.y);
        varint(&mut out, snap.health);
        varint(&mut out, snap.carried);
    }
    out
}

/// One tick after the commands: move, file everyone in the grid, take health on threads, and
/// pack what changed. Gives the snapshot's checksum.
fn tick(entities: &mut [Entity], cells: &mut [Vec<usize>], threads: usize, turn: i64) -> i64 {
    integrate(entities);
    for cell in cells.iter_mut() {
        cell.clear();
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for entity in entities.iter() {
        xs.push(entity.x);
        ys.push(entity.y);
        cells[(entity.y / CELL * SIDE + entity.x / CELL) as usize].push(entity.number as usize);
    }
    let total = SIDE * SIDE;
    let share = (total + threads as i64 - 1) / threads as i64;
    let changes: Vec<Vec<i64>> = std::thread::scope(|scope| {
        let (cells, xs, ys) = (&*cells, &xs, &ys);
        let parts: Vec<_> = (0..threads as i64)
            .map(|part| {
                scope.spawn(move || {
                    (part * share..((part + 1) * share).min(total))
                        .map(|cell| cell_health(cell, cells, xs, ys))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        parts.into_iter().flat_map(|part| part.join().unwrap()).collect()
    });
    for (cell, members) in cells.iter().enumerate() {
        for (at, &me) in members.iter().enumerate() {
            let change = changes[cell][at];
            if change != 0 {
                let entity = &mut entities[me];
                entity.health = (entity.health + change).min(100);
                if entity.health <= 0 {
                    // Dies, drops half its gold, and comes back somewhere else.
                    let gold = entity.inventory.get("gold").copied().unwrap_or(0);
                    entity.inventory.insert("gold", gold / 2);
                    *entity.inventory.entry("bone").or_insert(0) += 1;
                    entity.health = 100;
                    let me = me as i64;
                    entity.x = (me * 7_919 + turn * 104_729) % SIZE;
                    entity.y = (me * 104_729 + turn * 7_919) % SIZE;
                }
                entity.changed = true;
            }
        }
    }
    let snaps: Vec<Snap> = entities
        .iter()
        .filter(|entity| entity.changed)
        .map(|entity| Snap {
            number: entity.number,
            x: entity.x,
            y: entity.y,
            health: entity.health,
            carried: entity.inventory.values().sum(),
        })
        .collect();
    let data = packed(&snaps);
    let mut hash = data.len() as i64;
    for &byte in &data {
        hash = (hash * 31 + i64::from(byte)) % 1_000_000_007;
    }
    hash
}

fn main() {
    let started = std::time::Instant::now();
    let count = black_box(10_000_i64);
    // As many threads as Fennec's pool has: `FENNEC_THREADS` when it's set, so a run at any
    // count compares like with like, and otherwise one for each core, as the pool does.
    let threads = std::env::var("FENNEC_THREADS")
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .filter(|&count| count > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get));
    let mut entities = Vec::new();
    let mut state = 42;
    for number in 0..count {
        state = next(state);
        let x = below(state, SIZE);
        state = next(state);
        let y = below(state, SIZE);
        state = next(state);
        let speed = if number % 4 == 0 { below(state, 9) - 4 } else { 0 };
        entities.push(Entity {
            number,
            x,
            y,
            vx: speed,
            vy: -speed,
            health: 100,
            inventory: HashMap::from([("gold", number % 50)]),
            changed: false,
        });
    }
    let mut cells = vec![Vec::new(); (SIDE * SIDE) as usize];
    let mut checksum = 0;
    let (sender, commands) = sync_channel(64);
    std::thread::scope(|scope| {
        scope.spawn(move || players(sender, 7, count));
        for turn in 0..TICKS {
            for command in commands.iter().take(COMMANDS as usize) {
                apply(&mut entities, command);
            }
            checksum = (checksum * 7 + tick(&mut entities, &mut cells, threads, turn)) % 1_000_000_007;
        }
    });
    let mut health = 0;
    let mut carried = 0;
    for entity in &entities {
        health += entity.health;
        carried += entity.inventory.values().sum::<i64>() * (entity.number % 7 + 1);
    }
    println!("{checksum} {health} {carried}");
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
