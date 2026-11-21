//! The `Saved` shape: a program's own values saved and loaded back through JSON, `std.pack` and
//! TOML, as data on disk and in transit is. A box of unions, an enum, a `where` rule, two
//! `Table`s with `Id`s into them, an optional `Id` and an optional text is changed a step at a
//! time, sent through a format and taken back, and texts that don't hold one are refused. What
//! each step prints is worked out here, by the same model the generator picks steps with, so a
//! step only ever names an item that's there and a saved `Id` never points at one taken out.

use std::fmt::Write as _;

/// A `Saved` statement: `saved{id}(stdout)`.
#[derive(Clone, Debug)]
pub(crate) struct Saving {
    pub(crate) id: usize,
    pub(crate) steps: Vec<SaveStep>,
}

/// A step of a `Saved` statement. `units` and `tags` are the program's lists of the `Id`s it was
/// handed, which a step names by place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveStep {
    /// `units.append(box.units.add(Unit{id}(name=..., target=units[k])))`, `k` a unit that's in, and
    /// the `Id` it's given printed.
    Hired(Option<usize>),
    /// `box.units.remove(units[k]) else ...`, printed: one no unit in the box names.
    Fired(usize),
    /// `box.units.get(units[k]) else ...`, printed.
    Looked(usize),
    /// `box.picked = units[k]`, a unit that's in, or `None`.
    Picked(Option<usize>),
    /// `box.note = NOTES[k]`, or `None`.
    Noted(Option<usize>),
    /// `box.kept.mark = MARKS[k]`.
    Marked(usize),
    /// `box.kept.marks.append(MARKS[k])`.
    Marks(usize),
    /// `box.moves.append(.Step(by=by))`, or `.Wait`.
    Moved(Option<i64>),
    /// `box.kept.gauge = Gauge{id}(level=level)`, never below 0.
    Gauged(i64),
    /// `tags.append(box.kept.tags.add(Tag{id}(name=..., near=[tags[k], ...])))`, each a tag that's in,
    /// and the `Id` it's given printed.
    Tagged(Vec<usize>),
    /// `box.kept.tags.remove(tags[k]) else ...`, printed: one no tag and no `best` names.
    Untagged(usize),
    /// `box.kept.best.append(tags[k])`, a tag that's in.
    Best(usize),
    /// The box sent through a format and taken back in its place, then printed. TOML holds only
    /// `box.kept`, as it has no `None` and no enums.
    Trip(SaveFormat),
    /// `json.dumps(box)`, printed.
    Shown,
    /// `json.loads[Box{id}]` of `BROKEN[k]`'s text, which it refuses, in compiled code's words.
    Broken(usize),
    /// `pack.loads[Gauge{id}](pack.dumps(Loose{id}(level=-PACKED[k])))`: refused for breaking the
    /// rule, saying at which byte, except for 0.
    Packed(usize),
}

/// The formats a `Trip` goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveFormat {
    Json,
    /// `std.pack`, which saves a `Table`'s items in order, as JSON and TOML do.
    Pack,
    /// `std.pack` with `exact=True`, which saves a `Table` slot for slot, generations and all.
    Exact,
    /// `box.kept` through TOML.
    Toml,
}

/// Every `SaveFormat`, for the generator to pick from.
pub(crate) const SAVE_FORMATS: [SaveFormat; 4] =
    [SaveFormat::Json, SaveFormat::Pack, SaveFormat::Exact, SaveFormat::Toml];

/// The marks a step sets or adds, for statement `id`: as written, as printed, and as JSON writes
/// it, tagged with its member's name.
fn marks(id: usize) -> [(String, String, String); 5] {
    let pt = |x: i64, tag: &str, written: String| {
        (
            written,
            format!("Pt{id}(x={x}, tag=\"{tag}\")"),
            format!("{{\"Pt{id}\": {{\"x\": {x}, \"tag\": \"{tag}\"}}}}"),
        )
    };
    let disc = |radius: &str| {
        (
            format!("Disc{id}(radius={radius})"),
            format!("Disc{id}(radius={radius})"),
            format!("{{\"Disc{id}\": {{\"radius\": {radius}}}}}"),
        )
    };
    [
        pt(0, "p", format!("Pt{id}(x=0)")),
        pt(-3, "q", format!("Pt{id}(x=-3, tag=\"q\")")),
        disc("1.5"),
        disc("0.25"),
        pt(12, "two words", format!("Pt{id}(x=12, tag=\"two words\")")),
    ]
}

/// How many marks a step can pick from.
pub(crate) const MARK_COUNT: usize = 5;

/// The notes a step sets.
pub(crate) const NOTES: [&str; 3] = ["n", "two words", ""];

/// What `Packed` steps save as a `Loose{id}`'s level, made negative.
pub(crate) const PACKED: [i64; 6] = [0, 1, 64, 65, 300, 70000];

/// The JSON texts a `Broken` step loads, as Fennec literals, for statement `id`, each with what
/// the refusal says, as printed inside the `ParseError`.
fn broken(id: usize) -> [(String, String); 12] {
    let q = |text: &str| text.replace('"', "\\\"");
    [
        ("{}".to_owned(), String::new()),
        (
            q(&format!("{{\"kept\": {{\"mark\": {{\"Cone{id}\": {{\"x\": 1}}}}}}}}")),
            format!("`kept.mark` is \\\"Cone{id}\\\", which is none of `Disc{id} | Pt{id}`"),
        ),
        (
            q("{\"kept\": {\"mark\": 3}}"),
            format!("`kept.mark` should be a Disc{id} | Pt{id}, but it's an int"),
        ),
        (
            q("{\"kept\": {\"gauge\": {\"level\": -1}}}"),
            format!("in `kept.gauge`, Gauge{id}.level breaks its `where` rule"),
        ),
        (
            q("{\"picked\": 5}"),
            format!("`picked` is an Id, but the text has no `Table[Unit{id}]` for it to be in"),
        ),
        (q("{\"picked\": -1}"), "`picked` is out of range".to_owned()),
        (
            q("{\"units\": [{\"name\": \"a\", \"target\": 3}]}"),
            "`units[0].target` is item 3 of `units`, which has only 1".to_owned(),
        ),
        (q("{\"moves\": [\"Run\"]}"), format!("`moves[0]` is \\\"Run\\\", which isn't a case of `Move{id}`")),
        (
            q("{\"moves\": [{\"Step\": {\"by\": \"x\"}}]}"),
            "`moves[0].by` should be an int, but it's a str".to_owned(),
        ),
        (q("{\"note\": 4}"), "`note` should be a str, but it's an int".to_owned()),
        (
            q(&format!("{{\"kept\": {{\"marks\": [{{\"Pt{id}\": {{}}}}]}}}}")),
            format!("`kept.marks[0].Pt{id}.x` is missing"),
        ),
        (
            q("{\"kept\": {\"best\": [0]}}"),
            format!("`kept.best[0]` is an Id, but the text has no `Table[Tag{id}]` for it to be in"),
        ),
    ]
}

/// How many texts a `Broken` step can pick from.
pub(crate) const BROKEN_COUNT: usize = 12;

/// An `Id`: its `Table`, by the number the model gave it, its slot and the slot's generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    table: usize,
    slot: usize,
    generation: u64,
}

/// A `Table` as compiled code keeps it: each slot's generation and item, if it holds one. An item
/// goes in the lowest slot that's free, and taking one out counts its slot on a generation.
#[derive(Clone, Debug)]
struct Slots<T> {
    table: usize,
    slots: Vec<(u64, Option<T>)>,
}

impl<T: Clone> Slots<T> {
    fn add(&mut self, item: T) -> Key {
        let table = self.table;
        if let Some(slot) = self.slots.iter().position(|(_, held)| held.is_none())
            && let Some((generation, held)) = self.slots.get_mut(slot)
        {
            *held = Some(item);
            return Key { table, slot, generation: *generation };
        }
        self.slots.push((0, Some(item)));
        Key { table, slot: self.slots.len() - 1, generation: 0 }
    }

    fn get(&self, key: Key) -> Option<&T> {
        if key.table != self.table {
            return None;
        }
        match self.slots.get(key.slot) {
            Some((generation, Some(item))) if *generation == key.generation => Some(item),
            _ => None,
        }
    }

    fn remove(&mut self, key: Key) -> Option<T> {
        self.get(key)?;
        let (generation, held) = self.slots.get_mut(key.slot)?;
        *generation += 1;
        held.take()
    }

    /// The `Id`s of the items held, in slot order, as `ids()` gives them.
    fn keys(&self) -> Vec<Key> {
        let table = self.table;
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, (_, held))| held.is_some())
            .map(|(slot, (generation, _))| Key { table, slot, generation: *generation })
            .collect()
    }

    fn items(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|(_, held)| held.as_ref())
    }

    /// Where the item `key` names is among the items held, as a format that isn't exact saves it.
    fn position(&self, key: Key) -> usize {
        self.keys().iter().position(|held| *held == key).unwrap_or(0)
    }
}

/// A unit: the step that hired it, and whom it targets.
type Unit = (usize, Option<Key>);
/// A tag: the step that made it, and the tags near it.
type Tag = (usize, Vec<Key>);

/// What a `Saved` statement's box and its lists of `Id`s hold, as the program runs.
#[derive(Clone, Debug)]
pub(crate) struct SavedBox {
    id: usize,
    mark: usize,
    marks: Vec<usize>,
    gauge: i64,
    tags: Slots<Tag>,
    best: Vec<Key>,
    moves: Vec<Option<i64>>,
    units: Slots<Unit>,
    picked: Option<Key>,
    note: Option<usize>,
    unit_ids: Vec<Key>,
    tag_ids: Vec<Key>,
    /// How many `Table`s have been made, so each new one gets a number of its own.
    tables: usize,
}

impl SavedBox {
    pub(crate) fn new(id: usize) -> SavedBox {
        SavedBox {
            id,
            mark: 0,
            marks: Vec::new(),
            gauge: 0,
            tags: Slots { table: 0, slots: Vec::new() },
            best: Vec::new(),
            moves: Vec::new(),
            units: Slots { table: 1, slots: Vec::new() },
            picked: None,
            note: None,
            unit_ids: Vec::new(),
            tag_ids: Vec::new(),
            tables: 2,
        }
    }

    /// The places in `units` of the units still in the box.
    pub(crate) fn units_in(&self) -> Vec<usize> {
        (0..self.unit_ids.len())
            .filter(|at| self.unit_ids.get(*at).is_some_and(|key| self.units.get(*key).is_some()))
            .collect()
    }

    /// The places in `tags` of the tags still in the box.
    pub(crate) fn tags_in(&self) -> Vec<usize> {
        (0..self.tag_ids.len())
            .filter(|at| self.tag_ids.get(*at).is_some_and(|key| self.tags.get(*key).is_some()))
            .collect()
    }

    /// Whether taking out the unit at place `at` in `units` leaves every saved `Id` naming one in.
    pub(crate) fn can_fire(&self, at: usize) -> bool {
        let Some(key) = self.unit_ids.get(at).copied() else { return false };
        self.picked != Some(key) && self.units.items().all(|(_, target)| *target != Some(key))
    }

    /// Whether taking out the tag at place `at` in `tags` leaves every saved `Id` naming one in.
    pub(crate) fn can_untag(&self, at: usize) -> bool {
        let Some(key) = self.tag_ids.get(at).copied() else { return false };
        !self.best.contains(&key) && self.tags.items().all(|(_, near)| !near.contains(&key))
    }

    fn unit_shown(&self, unit: Option<&Unit>) -> String {
        let id = self.id;
        match unit {
            Some((n, target)) => {
                let target = target.map_or_else(|| "None".to_owned(), |key| format!("Id({})", key.slot));
                format!("Unit{id}(name=\"u{n}\", target={target})")
            }
            None => format!("Unit{id}(name=\"gone\", target=None)"),
        }
    }

    fn tag_shown(&self, tag: Option<&Tag>) -> String {
        let id = self.id;
        match tag {
            Some((n, near)) => {
                let near: Vec<String> = near.iter().map(|key| format!("Id({})", key.slot)).collect();
                format!("Tag{id}(name=\"t{n}\", near=[{}])", near.join(", "))
            }
            None => format!("Tag{id}(name=\"gone\", near=[])"),
        }
    }

    fn kept_shown(&self) -> String {
        let id = self.id;
        let marks = marks(id);
        let mark = |at: usize| marks.get(at).map_or(String::new(), |(_, shown, _)| shown.clone());
        let listed: Vec<String> = self.marks.iter().map(|at| mark(*at)).collect();
        let tags: Vec<String> = self.tags.items().map(|tag| self.tag_shown(Some(tag))).collect();
        let best: Vec<String> = self.best.iter().map(|key| format!("Id({})", key.slot)).collect();
        format!(
            "Kept{id}(mark={}, marks=[{}], gauge=Gauge{id}(level={}), tags=Table([{}]), best=[{}])",
            mark(self.mark),
            listed.join(", "),
            self.gauge,
            tags.join(", "),
            best.join(", ")
        )
    }

    fn shown(&self) -> String {
        let id = self.id;
        let moves: Vec<String> = self
            .moves
            .iter()
            .map(|by| by.map_or_else(|| format!("Move{id}.Wait"), |by| format!("Move{id}.Step(by={by})")))
            .collect();
        let units: Vec<String> = self.units.items().map(|unit| self.unit_shown(Some(unit))).collect();
        let picked = self.picked.map_or_else(|| "None".to_owned(), |key| format!("Id({})", key.slot));
        let note = self
            .note
            .and_then(|at| NOTES.get(at))
            .map_or_else(|| "None".to_owned(), |note| format!("\"{note}\""));
        format!(
            "Box{id}(kept={}, moves=[{}], units=Table([{}]), picked={picked}, note={note})",
            self.kept_shown(),
            moves.join(", "),
            units.join(", ")
        )
    }

    /// `json.dumps(box)`: each `Id` as where its item is among its `Table`'s items.
    fn json(&self) -> String {
        let id = self.id;
        let marks = marks(id);
        let mark = |at: usize| marks.get(at).map_or(String::new(), |(_, _, json)| json.clone());
        let listed: Vec<String> = self.marks.iter().map(|at| mark(*at)).collect();
        let tag_at = |key: &Key| self.tags.position(*key).to_string();
        let tags: Vec<String> = self
            .tags
            .items()
            .map(|(n, near)| {
                let near: Vec<String> = near.iter().map(tag_at).collect();
                format!("{{\"name\": \"t{n}\", \"near\": [{}]}}", near.join(", "))
            })
            .collect();
        let best: Vec<String> = self.best.iter().map(tag_at).collect();
        let moves: Vec<String> = self
            .moves
            .iter()
            .map(|by| {
                by.map_or_else(|| "\"Wait\"".to_owned(), |by| format!("{{\"Step\": {{\"by\": {by}}}}}"))
            })
            .collect();
        let unit_at = |key: Option<Key>| {
            key.map_or_else(|| "null".to_owned(), |key| self.units.position(key).to_string())
        };
        let units: Vec<String> = self
            .units
            .items()
            .map(|(n, target)| format!("{{\"name\": \"u{n}\", \"target\": {}}}", unit_at(*target)))
            .collect();
        let note = self
            .note
            .and_then(|at| NOTES.get(at))
            .map_or_else(|| "null".to_owned(), |note| format!("\"{note}\""));
        format!(
            "{{\"kept\": {{\"mark\": {}, \"marks\": [{}], \"gauge\": {{\"level\": {}}}, \"tags\": [{}], \"best\": [{}]}}, \
             \"moves\": [{}], \"units\": [{}], \"picked\": {}, \"note\": {note}}}",
            mark(self.mark),
            listed.join(", "),
            self.gauge,
            tags.join(", "),
            best.join(", "),
            moves.join(", "),
            units.join(", "),
            unit_at(self.picked)
        )
    }

    /// The box's `Table`s as a load gives them back: new `Table`s, slot for slot when `exact`,
    /// and otherwise each item in the slot where it is among the items, at generation 0, with
    /// every `Id` inside the box moved to match.
    fn loaded(&mut self, exact: bool, kept_only: bool) {
        let fresh = |tables: &mut usize| {
            *tables += 1;
            *tables
        };
        let tag_table = fresh(&mut self.tables);
        let tag_move = |key: Key, tags: &Slots<Tag>| {
            if exact {
                Key { table: tag_table, ..key }
            } else {
                Key { table: tag_table, slot: tags.position(key), generation: 0 }
            }
        };
        let old_tags = self.tags.clone();
        let mut tags = Slots { table: tag_table, slots: Vec::new() };
        for (generation, held) in &old_tags.slots {
            let held = held
                .as_ref()
                .map(|(n, near)| (*n, near.iter().map(|key| tag_move(*key, &old_tags)).collect()));
            match (exact, held) {
                (true, held) => tags.slots.push((*generation, held)),
                (false, Some(held)) => tags.slots.push((0, Some(held))),
                (false, None) => {}
            }
        }
        self.best = self.best.iter().map(|key| tag_move(*key, &old_tags)).collect();
        self.tags = tags;
        self.tag_ids = self.tags.keys();
        if kept_only {
            return;
        }
        let unit_table = fresh(&mut self.tables);
        let unit_move = |key: Key, units: &Slots<Unit>| {
            if exact {
                Key { table: unit_table, ..key }
            } else {
                Key { table: unit_table, slot: units.position(key), generation: 0 }
            }
        };
        let old_units = self.units.clone();
        let mut units = Slots { table: unit_table, slots: Vec::new() };
        for (generation, held) in &old_units.slots {
            let held = held.as_ref().map(|(n, target)| (*n, target.map(|key| unit_move(key, &old_units))));
            match (exact, held) {
                (true, held) => units.slots.push((*generation, held)),
                (false, Some(held)) => units.slots.push((0, Some(held))),
                (false, None) => {}
            }
        }
        self.picked = self.picked.map(|key| unit_move(key, &old_units));
        self.units = units;
        self.unit_ids = self.units.keys();
    }

    /// Takes step `n`, giving the lines it prints.
    pub(crate) fn step(&mut self, n: usize, step: &SaveStep) -> Vec<String> {
        let id = self.id;
        let mut lines = Vec::new();
        match step {
            SaveStep::Hired(target) => {
                let target = target.and_then(|at| self.unit_ids.get(at).copied());
                let key = self.units.add((n, target));
                self.unit_ids.push(key);
                lines.push(format!("Id({})", key.slot));
            }
            SaveStep::Fired(at) => {
                let gone = self.unit_ids.get(*at).copied().and_then(|key| self.units.remove(key));
                lines.push(self.unit_shown(gone.as_ref()));
            }
            SaveStep::Looked(at) => {
                let found = self.unit_ids.get(*at).and_then(|key| self.units.get(*key));
                lines.push(self.unit_shown(found));
            }
            SaveStep::Picked(at) => self.picked = at.and_then(|at| self.unit_ids.get(at).copied()),
            SaveStep::Noted(at) => self.note = *at,
            SaveStep::Marked(at) => self.mark = *at,
            SaveStep::Marks(at) => self.marks.push(*at),
            SaveStep::Moved(by) => self.moves.push(*by),
            SaveStep::Gauged(level) => self.gauge = *level,
            SaveStep::Tagged(near) => {
                let near = near.iter().filter_map(|at| self.tag_ids.get(*at).copied()).collect();
                let key = self.tags.add((n, near));
                self.tag_ids.push(key);
                lines.push(format!("Id({})", key.slot));
            }
            SaveStep::Untagged(at) => {
                let gone = self.tag_ids.get(*at).copied().and_then(|key| self.tags.remove(key));
                lines.push(self.tag_shown(gone.as_ref()));
            }
            SaveStep::Best(at) => {
                if let Some(key) = self.tag_ids.get(*at) {
                    self.best.push(*key);
                }
            }
            SaveStep::Trip(format) => {
                let kept_only = *format == SaveFormat::Toml;
                self.loaded(*format == SaveFormat::Exact, kept_only);
                if kept_only {
                    lines.push(self.kept_shown());
                    lines.push(self.tag_ids.len().to_string());
                } else {
                    lines.push(self.shown());
                    lines.push(format!("{} {}", self.unit_ids.len(), self.tag_ids.len()));
                }
            }
            SaveStep::Shown => lines.push(self.json()),
            SaveStep::Broken(at) => {
                let said = broken(id).get(*at).map(|(_, said)| said.clone()).unwrap_or_default();
                if *at == 0 {
                    lines.push(SavedBox::new(id).shown());
                } else {
                    lines.push(format!("refused ParseError(message=\"{said}\", line=0, column=0)"));
                }
            }
            SaveStep::Packed(at) => {
                let level = PACKED.get(*at).copied().unwrap_or(0);
                if level == 0 {
                    lines.push(format!("Gauge{id}(level=0)"));
                } else {
                    // The level is the last thing written, after a header and the layout of 24
                    // bytes, as a zigzag varint, and it's read whole before the rule is checked.
                    let mut zigzag = 2 * level.unsigned_abs() - 1;
                    let mut length = 1;
                    while zigzag >= 128 {
                        zigzag >>= 7;
                        length += 1;
                    }
                    lines.push(format!(
                        "refused ParseError(message=\"Gauge{id}.level breaks its `where` rule, at byte {}\", line=0, \
                         column=0)",
                        24 + length
                    ));
                }
            }
        }
        lines
    }
}

/// What `saved{id}` prints, step by step.
pub(crate) fn printed(saving: &Saving) -> Vec<String> {
    let id = saving.id;
    let mut held = SavedBox::new(id);
    let mut lines = vec![format!("Unit{id}(name=\"seen\", target=None) Tag{id}(name=\"seen\", near=[])")];
    for (n, step) in saving.steps.iter().enumerate() {
        lines.extend(held.step(n, step));
    }
    lines.push(held.shown());
    lines.push(format!("{} {}", held.unit_ids.len(), held.tag_ids.len()));
    lines
}

/// Picks 3 to 14 steps, each naming only what's there: `pick(n)` is from 0 to below `n`.
pub(crate) fn picked_steps(id: usize, pick: &mut dyn FnMut(usize) -> usize) -> Vec<SaveStep> {
    let mut held = SavedBox::new(id);
    let mut steps = Vec::new();
    for n in 0..3 + pick(12) {
        let one_of = |pick: &mut dyn FnMut(usize) -> usize, places: &[usize]| {
            if places.is_empty() { None } else { places.get(pick(places.len())).copied() }
        };
        let units = held.units_in();
        let tags = held.tags_in();
        let step = match pick(17) {
            0 | 1 => SaveStep::Hired(one_of(pick, &units)),
            2 => match one_of(
                pick,
                &(0..held.unit_ids.len()).filter(|at| held.can_fire(*at)).collect::<Vec<_>>(),
            ) {
                Some(at) => SaveStep::Fired(at),
                None => SaveStep::Hired(None),
            },
            3 if !held.unit_ids.is_empty() => SaveStep::Looked(pick(held.unit_ids.len())),
            4 => SaveStep::Picked(if pick(4) == 0 { None } else { one_of(pick, &units) }),
            5 => SaveStep::Noted(if pick(4) == 0 { None } else { Some(pick(NOTES.len())) }),
            6 => SaveStep::Marked(pick(MARK_COUNT)),
            7 => SaveStep::Marks(pick(MARK_COUNT)),
            8 => SaveStep::Moved(if pick(3) == 0 {
                None
            } else {
                Some(i64::try_from(pick(13)).unwrap_or(0) - 3)
            }),
            9 => SaveStep::Gauged(i64::try_from(pick(10)).unwrap_or(0)),
            10 => {
                let near = (0..pick(3)).filter_map(|_| one_of(pick, &tags)).collect();
                SaveStep::Tagged(near)
            }
            11 => match one_of(
                pick,
                &(0..held.tag_ids.len()).filter(|at| held.can_untag(*at)).collect::<Vec<_>>(),
            ) {
                Some(at) => SaveStep::Untagged(at),
                None => SaveStep::Tagged(Vec::new()),
            },
            12 => one_of(pick, &tags).map_or(SaveStep::Tagged(Vec::new()), SaveStep::Best),
            13 | 14 => SaveStep::Trip(
                SAVE_FORMATS.get(pick(SAVE_FORMATS.len())).copied().unwrap_or(SaveFormat::Json),
            ),
            15 => SaveStep::Shown,
            _ if pick(2) == 0 => SaveStep::Broken(pick(BROKEN_COUNT)),
            _ => SaveStep::Packed(pick(PACKED.len())),
        };
        let _ = held.step(n, &step);
        steps.push(step);
    }
    steps
}

/// The types a `Saved` statement `id` saves, the functions it loads texts and packed gauges with,
/// and the start of `saved{id}`, making the box and its lists of `Id`s. It prints a unit and a
/// tag first: until #2334 is fixed, `ids()` on a `Table` of a type no value of was made doesn't
/// build.
fn types_written(id: usize) -> String {
    format!(
        "struct Pt{id}:\n    x: int\n    tag = \"p\"\n\nstruct Disc{id}:\n    radius: float\n\n\
         type Mark{id} = Pt{id} | Disc{id}\n\nenum Move{id}:\n    Wait\n    Step(by: int)\n\n\
         struct Gauge{id}:\n    level: int where level >= 0\n\nstruct Loose{id}:\n    level: int\n\n\
         struct Tag{id}:\n    name: str\n    near = list[Id[Tag{id}]]()\n\n\
         struct Unit{id}:\n    name: str\n    target: Id[Unit{id}]? = None\n\n\
         struct Kept{id}:\n    mark: Mark{id} = Pt{id}(x=0)\n    marks = list[Mark{id}]()\n    gauge = Gauge{id}(level=0)\n    \
         tags = Table[Tag{id}]()\n    best = list[Id[Tag{id}]]()\n\n\
         struct Box{id}:\n    kept = Kept{id}()\n    moves = list[Move{id}]()\n    units = Table[Unit{id}]()\n    \
         picked: Id[Unit{id}]? = None\n    note: str? = None\n\n\
         def tried{id}(text: str, stdout: Stdout):\n    got = json.loads[Box{id}](text) except why:\n        \
         stdout.print(\"refused\", why)\n        return\n    stdout.print(got)\n\n\
         def packed{id}(level: int, stdout: Stdout):\n    \
         got = pack.loads[Gauge{id}](pack.dumps(Loose{id}(level=level))) except why:\n        \
         stdout.print(\"refused\", why)\n        return\n    stdout.print(got)\n\n\
         def saved{id}(stdout: Stdout):\n    mut box = Box{id}()\n    mut units = list[Id[Unit{id}]]()\n    \
         mut tags = list[Id[Tag{id}]]()\n    \
         stdout.print(Unit{id}(name=\"seen\"), Tag{id}(name=\"seen\"))\n"
    )
}

/// `saved{id}`, the function a `Saved` calls, after its types and the functions it loads texts
/// and packed gauges with.
pub(crate) fn written(saving: &Saving) -> String {
    let id = saving.id;
    let mut out = types_written(id);
    let marks = marks(id);
    let mark = |at: usize| marks.get(at).map_or(String::new(), |(written, _, _)| written.clone());
    let gone_unit = format!("Unit{id}(name=\"gone\")");
    let gone_tag = format!("Tag{id}(name=\"gone\")");
    let texts = broken(id);
    for (n, step) in saving.steps.iter().enumerate() {
        let line = match step {
            SaveStep::Hired(target) => {
                let target = target.map_or_else(String::new, |at| format!(", target=units[{at}]"));
                format!(
                    "units.append(box.units.add(Unit{id}(name=\"u{n}\"{target})))\n    stdout.print(units[-1])"
                )
            }
            SaveStep::Fired(at) => format!("stdout.print(box.units.remove(units[{at}]) else {gone_unit})"),
            SaveStep::Looked(at) => format!("stdout.print(box.units.get(units[{at}]) else {gone_unit})"),
            SaveStep::Picked(at) => {
                format!("box.picked = {}", at.map_or_else(|| "None".to_owned(), |at| format!("units[{at}]")))
            }
            SaveStep::Noted(at) => format!(
                "box.note = {}",
                at.and_then(|at| NOTES.get(at))
                    .map_or_else(|| "None".to_owned(), |note| format!("\"{note}\""))
            ),
            SaveStep::Marked(at) => format!("box.kept.mark = {}", mark(*at)),
            SaveStep::Marks(at) => format!("box.kept.marks.append({})", mark(*at)),
            SaveStep::Moved(by) => {
                format!(
                    "box.moves.append({})",
                    by.map_or_else(|| ".Wait".to_owned(), |by| format!(".Step(by={by})"))
                )
            }
            SaveStep::Gauged(level) => format!("box.kept.gauge = Gauge{id}(level={level})"),
            SaveStep::Tagged(near) => {
                let near: Vec<String> = near.iter().map(|at| format!("tags[{at}]")).collect();
                let near =
                    if near.is_empty() { String::new() } else { format!(", near=[{}]", near.join(", ")) };
                format!(
                    "tags.append(box.kept.tags.add(Tag{id}(name=\"t{n}\"{near})))\n    stdout.print(tags[-1])"
                )
            }
            SaveStep::Untagged(at) => {
                format!("stdout.print(box.kept.tags.remove(tags[{at}]) else {gone_tag})")
            }
            SaveStep::Best(at) => format!("box.kept.best.append(tags[{at}])"),
            SaveStep::Trip(SaveFormat::Toml) => format!(
                "kept{n} = toml.loads[Kept{id}](toml.dumps(box.kept)) except why:\n        stdout.print(\"refused\", why)\n        \
                 return\n    box.kept = kept{n}\n    tags = box.kept.tags.ids()\n    stdout.print(box.kept)\n    \
                 stdout.print(len(tags))"
            ),
            SaveStep::Trip(format) => {
                let (module, exact) = match format {
                    SaveFormat::Json => ("json", ""),
                    SaveFormat::Exact => ("pack", ", exact=True"),
                    SaveFormat::Pack | SaveFormat::Toml => ("pack", ""),
                };
                format!(
                    "loaded{n} = {module}.loads[Box{id}]({module}.dumps(box{exact})) except why:\n        \
                     stdout.print(\"refused\", why)\n        return\n    box = loaded{n}\n    units = box.units.ids()\n    \
                     tags = box.kept.tags.ids()\n    stdout.print(box)\n    \
                     stdout.print(len(units), len(tags))"
                )
            }
            SaveStep::Shown => "stdout.print(json.dumps(box))".to_owned(),
            SaveStep::Broken(at) => {
                format!("tried{id}(\"{}\", stdout)", texts.get(*at).map_or("{}", |(text, _)| text.as_str()))
            }
            SaveStep::Packed(at) => format!("packed{id}({}, stdout)", -PACKED.get(*at).copied().unwrap_or(0)),
        };
        let _ = writeln!(out, "    {line}");
    }
    // The lists of `Id`s are printed by length, so a refresh of them after the last load is read.
    out.push_str("    stdout.print(box)\n    stdout.print(len(units), len(tags))\n\n");
    out
}
