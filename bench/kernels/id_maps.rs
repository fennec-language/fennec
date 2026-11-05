//! Filling a map of generational slots, the twin of `id_maps.fen`.

#[derive(Clone, Copy)]
struct Unit {
    x: i64,
    hp: i64,
}

#[derive(Clone, Copy)]
struct Id {
    slot: usize,
    generation: u32,
}

#[derive(Default)]
struct Table {
    slots: Vec<(u32, Option<Unit>)>,
    free_from: usize,
    live: usize,
}

impl Table {
    fn insert(&mut self, unit: Unit) -> Id {
        self.live += 1;
        let free = (self.free_from..self.slots.len()).find(|at| self.slots[*at].1.is_none());
        if let Some(at) = free {
            self.free_from = at + 1;
            let slot = &mut self.slots[at];
            slot.0 += 1;
            slot.1 = Some(unit);
            return Id { slot: at, generation: slot.0 };
        }
        self.slots.push((0, Some(unit)));
        self.free_from = self.slots.len();
        Id { slot: self.slots.len() - 1, generation: 0 }
    }

    fn remove(&mut self, id: Id) {
        let slot = &mut self.slots[id.slot];
        if slot.0 == id.generation && slot.1.take().is_some() {
            self.live -= 1;
            self.free_from = self.free_from.min(id.slot);
        }
    }
}

fn main() {
    let started = std::time::Instant::now();
    let mut units = Table::default();
    let mut ids = Vec::new();
    for i in 0..std::hint::black_box(200_000) {
        ids.push(units.insert(Unit { x: i, hp: 1 }));
    }
    for round in 0..4 {
        let mut at = round;
        while at < ids.len() {
            units.remove(ids[at]);
            at += 5;
        }
        at = round;
        while at < ids.len() {
            ids[at] = units.insert(Unit { x: at as i64, hp: round as i64 });
            at += 5;
        }
    }
    let total: i64 = units.slots.iter().filter_map(|slot| slot.1).map(|unit| unit.x + unit.hp).sum();
    println!("{} {total}", units.live);
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
