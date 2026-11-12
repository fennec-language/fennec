//! Pathfinding for a game's AI, the twin of `pathfind.fen`: A* on a 512 by 512 map with a
//! `BinaryHeap` of open cells, ordered by a derived `Ord` and reversed for the cheapest first,
//! and a `HashMap` of the cells visited.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hint::black_box;

const SIDE: i64 = 512;
const REACH: i64 = 120;
const MUD: i64 = 3;
const GROUND: i64 = 1;

/// A cell waiting to be looked at: its estimated total cost, its estimate of what's left, and
/// where it is, compared in that order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Open {
    total: i64,
    left: i64,
    cell: i64,
}

/// A cell a search has reached: the cheapest cost so far, and the cell before it on that way.
#[derive(Clone, Copy)]
struct Visit {
    cost: i64,
    came_from: i64,
}

/// What one search found.
struct Found {
    steps: i64,
    cost: i64,
    looked: i64,
    passed: i64,
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// The map: what it costs to step onto each cell, 0 for a wall.
fn world(seed: i64) -> Vec<i64> {
    let mut costs = vec![GROUND; (SIDE * SIDE) as usize];
    let mut state = seed;
    for cost in costs.iter_mut() {
        state = next(state);
        let roll = below(state, 100);
        if roll < 18 {
            *cost = 0;
        } else if roll < 30 {
            *cost = MUD;
        }
    }
    for _ in 0..220 {
        state = next(state);
        let left = below(state, SIDE - 40);
        state = next(state);
        let top = below(state, SIDE - 40);
        state = next(state);
        let size = 8 + below(state, 24);
        for at in 0..=size {
            costs[(top * SIDE + left + at) as usize] = 0;
            costs[((top + size) * SIDE + left + at) as usize] = 0;
            costs[((top + at) * SIDE + left) as usize] = 0;
            costs[((top + at) * SIDE + left + size) as usize] = 0;
        }
        let half = size / 2;
        costs[(top * SIDE + left + half) as usize] = GROUND;
        costs[((top + size) * SIDE + left + half) as usize] = GROUND;
        costs[((top + half) * SIDE + left) as usize] = GROUND;
        costs[((top + half) * SIDE + left + size) as usize] = GROUND;
    }
    costs
}

/// How far apart two cells are in steps, if nothing were in the way.
fn distance(a: i64, b: i64) -> i64 {
    (a % SIDE - b % SIDE).abs() + (a / SIDE - b / SIDE).abs()
}

/// The way from `start` to `goal` by A*, or a `Found` with no steps if there's none.
fn search(costs: &[i64], start: i64, goal: i64) -> Found {
    let mut open = BinaryHeap::new();
    let mut seen: HashMap<i64, Visit> = HashMap::new();
    seen.insert(start, Visit { cost: 0, came_from: -1 });
    open.push(Reverse(Open { total: distance(start, goal), left: distance(start, goal), cell: start }));
    let mut looked = 0;
    while let Some(Reverse(here)) = open.pop() {
        let cost = here.total - here.left;
        if cost > seen[&here.cell].cost {
            continue;
        }
        looked += 1;
        if here.cell == goal {
            let mut steps = 0;
            let mut passed = 0;
            let mut at = goal;
            while at != start {
                steps += 1;
                passed = (passed * 31 + at) % 1_000_000_007;
                at = seen[&at].came_from;
            }
            return Found { steps, cost, looked, passed };
        }
        let x = here.cell % SIDE;
        let y = here.cell / SIDE;
        let neighbours = [
            (x + 1 < SIDE, here.cell + 1),
            (x > 0, here.cell - 1),
            (y + 1 < SIDE, here.cell + SIDE),
            (y > 0, here.cell - SIDE),
        ];
        for (inside, there) in neighbours {
            if !inside || costs[there as usize] == 0 {
                continue;
            }
            let reached = cost + costs[there as usize];
            if let Some(known) = seen.get(&there) {
                if known.cost <= reached {
                    continue;
                }
            }
            seen.insert(there, Visit { cost: reached, came_from: here.cell });
            let left = distance(there, goal);
            open.push(Reverse(Open { total: reached + left, left, cell: there }));
        }
    }
    Found { steps: 0, cost: 0, looked, passed: 0 }
}

/// Where a unit stands or goes: an open cell near `around`, found from a generator's state.
fn open_cell(costs: &[i64], around: i64, state: i64) -> i64 {
    let x = (around % SIDE + below(state, 2 * REACH) - REACH + SIDE) % SIDE;
    let y = (around / SIDE + below(next(state), 2 * REACH) - REACH + SIDE) % SIDE;
    let mut cell = y * SIDE + x;
    while costs[cell as usize] == 0 {
        cell = (cell + 1) % (SIDE * SIDE);
    }
    cell
}

fn main() {
    let started = std::time::Instant::now();
    let costs = world(black_box(2026));
    let searches = black_box(16);
    let mut state = 99;
    let mut checksum = 0;
    let mut steps = 0;
    let mut looked = 0;
    let mut stuck = 0;
    for search_number in 0..searches {
        state = next(state);
        let start = open_cell(&costs, below(state, SIDE * SIDE), next(state));
        state = next(next(state));
        let goal = open_cell(&costs, start, state);
        state = next(state);
        let found = search(&costs, start, goal);
        if found.steps == 0 && start != goal {
            stuck += 1;
        }
        steps += found.steps;
        looked += found.looked;
        checksum = (checksum * 7 + found.cost * 1_009 + found.passed + search_number) % 1_000_000_007;
    }
    println!("{checksum} {steps} {looked} {stuck}");
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
