//! `crewed{id}(stdout)`: a `shared Bag` of its own, changed by launched tasks one after another.
//! Each task takes every number it's sent through two or three locks of the bag in turn. Each lock
//! makes changes of its own:
//! - appends and pops;
//! - the dict's entries set, removed and popped;
//! - `add_twice`, a `ref self` method, with an argument worked out from the number sent or from the
//!   bag itself.
//!
//! A change can crash partway through a lock: a place outside the list, a key popped that isn't
//! there, or an overflow. That lock's changes are put back, the earlier locks' stay, and the task
//! ends. After each task, `main` locks the bag at once, calls `add_twice` with an argument of its
//! own working out, and reads the bag. #2516's segfault came at just such a lock, after a task
//! crashed inside one following a `ref self` call with a computed argument in an earlier lock.
//!
//! What each step prints is worked out here, from a model of the bag.

use std::fmt::Write as _;

/// The keys a bag's dict may hold.
const KEYS: [&str; 3] = ["a", "b", "c"];

/// The places a change may set in the list: past the end, or before its start, crashes.
const PLACES: [i64; 4] = [0, 1, -1, 3];

/// An argument written into a lock's body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arg {
    Literal(i64),
    /// `(number * by)`, from the number the task was sent.
    Times(i64),
    /// `(number + by)`.
    Plus(i64),
    /// `(len(held.items) * by)`, from the bag itself.
    Length(i64),
    /// `(len(held.items) - number)`, from both.
    LengthLess,
}

/// A change to the bag under `lock box as ref held:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// `held.items.append(arg)`.
    Append(Arg),
    /// `_ = held.items.pop()`, which takes nothing from an empty list.
    Pop,
    /// `held.add_twice(arg)`, a `ref self` method appending it twice.
    AddTwice(Arg),
    /// `held.items[at] += arg`, which crashes for a place outside the list.
    Bump(i64, Arg),
    /// `held.counts[key] = arg`, its key by place in `KEYS`.
    SetCount(usize, Arg),
    /// `_ = held.counts.remove(key)`, which takes nothing for a key that isn't there.
    Remove(usize),
    /// `_ = held.counts.pop(key)`, which crashes for a key that isn't there.
    PopCount(usize),
}

/// One `Crewed` statement.
#[derive(Clone, Debug)]
pub(crate) struct Crew {
    pub(crate) id: usize,
    /// The bag's list and dict to start with, each key by place in `KEYS`.
    pub(crate) items: Vec<i64>,
    pub(crate) counts: Vec<(usize, i64)>,
    /// Each lock a task takes for every number it's sent, in turn, and its changes.
    pub(crate) locks: Vec<Vec<Change>>,
    /// The numbers each task is sent, the tasks one after another.
    pub(crate) tasks: Vec<Vec<i64>>,
    /// What `main` passes `add_twice` after each task: `(len(held.items) * by)`.
    pub(crate) by: i64,
}

fn key(at: usize) -> &'static str {
    KEYS.get(at).copied().unwrap_or("a")
}

fn small(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(0)
}

/// An argument, a fifth each of every kind.
fn arg_picked(choose: &mut impl FnMut(usize) -> usize) -> Arg {
    match choose(5) {
        0 => Arg::Literal(small(choose(9)) - 3),
        1 => Arg::Times(small(choose(4))),
        2 => Arg::Plus(small(choose(5)) - 2),
        3 => Arg::Length(small(choose(3))),
        _ => Arg::LengthLess,
    }
}

/// A change, `add_twice` twice as often as each other kind.
fn change_picked(choose: &mut impl FnMut(usize) -> usize) -> Change {
    match choose(8) {
        0 => Change::Append(arg_picked(choose)),
        1 => Change::Pop,
        2 | 3 => Change::AddTwice(arg_picked(choose)),
        4 => {
            let at = PLACES.get(choose(PLACES.len())).copied().unwrap_or(0);
            Change::Bump(at, arg_picked(choose))
        }
        5 => Change::SetCount(choose(KEYS.len()), arg_picked(choose)),
        6 => Change::Remove(choose(KEYS.len())),
        _ => Change::PopCount(choose(KEYS.len())),
    }
}

/// A `Crewed` statement: a bag of up to 3 items and 3 keys, 2 or 3 locks of 1 to 3 changes each,
/// and 1 to 3 tasks sent 1 to 4 numbers each.
pub(crate) fn picked(id: usize, choose: &mut impl FnMut(usize) -> usize) -> Crew {
    let items = (0..choose(4)).map(|_| small(choose(10)) - 2).collect();
    let mut counts = Vec::new();
    for at in 0..KEYS.len() {
        if choose(3) != 0 {
            counts.push((at, small(choose(9))));
        }
    }
    let locks =
        (0..2 + choose(2)).map(|_| (0..=choose(3)).map(|_| change_picked(choose)).collect()).collect();
    let tasks = (0..=choose(3)).map(|_| (0..=choose(4)).map(|_| small(choose(8)) - 2).collect()).collect();
    Crew { id, items, counts, locks, tasks, by: small(choose(3)) }
}

/// The bag, as the model has it: its dict's entries in the order they went in.
#[derive(Clone)]
struct Bag {
    items: Vec<i64>,
    counts: Vec<(&'static str, i64)>,
}

impl Bag {
    fn counts_shown(&self) -> String {
        let counts: Vec<String> = self.counts.iter().map(|(key, n)| format!("\"{key}\": {n}")).collect();
        format!("{{{}}}", counts.join(", "))
    }

    fn shown(&self) -> String {
        let items: Vec<String> = self.items.iter().map(i64::to_string).collect();
        format!("Bag(items=[{}], counts={})", items.join(", "), self.counts_shown())
    }

    fn length(&self) -> Option<i64> {
        i64::try_from(self.items.len()).ok()
    }

    /// What `arg` comes to, with `number` what the task was sent, or None where it overflows.
    fn arg(&self, arg: Arg, number: i64) -> Option<i64> {
        match arg {
            Arg::Literal(n) => Some(n),
            Arg::Times(by) => number.checked_mul(by),
            Arg::Plus(by) => number.checked_add(by),
            Arg::Length(by) => self.length()?.checked_mul(by),
            Arg::LengthLess => self.length()?.checked_sub(number),
        }
    }

    /// The bag after one lock's changes, or None where one crashes, which puts them all back.
    fn changed(&self, changes: &[Change], number: i64) -> Option<Bag> {
        let mut bag = self.clone();
        for change in changes {
            match change {
                Change::Append(arg) => {
                    let value = bag.arg(*arg, number)?;
                    bag.items.push(value);
                }
                Change::Pop => {
                    bag.items.pop();
                }
                Change::AddTwice(arg) => {
                    let value = bag.arg(*arg, number)?;
                    bag.items.extend([value, value]);
                }
                Change::Bump(at, arg) => {
                    let value = bag.arg(*arg, number)?;
                    let at = if *at < 0 { at + bag.length()? } else { *at };
                    let item = bag.items.get_mut(usize::try_from(at).ok()?)?;
                    *item = item.checked_add(value)?;
                }
                Change::SetCount(at, arg) => {
                    let value = bag.arg(*arg, number)?;
                    match bag.counts.iter_mut().find(|(held, _)| *held == key(*at)) {
                        Some((_, n)) => *n = value,
                        None => bag.counts.push((key(*at), value)),
                    }
                }
                Change::Remove(at) => bag.counts.retain(|(held, _)| *held != key(*at)),
                Change::PopCount(at) => {
                    let place = bag.counts.iter().position(|(held, _)| *held == key(*at))?;
                    bag.counts.remove(place);
                }
            }
        }
        Some(bag)
    }
}

/// What `crewed{id}` prints.
pub(crate) fn printed(crew: &Crew) -> Vec<String> {
    let mut bag =
        Bag { items: crew.items.clone(), counts: crew.counts.iter().map(|(at, n)| (key(*at), *n)).collect() };
    let mut lines = Vec::new();
    for numbers in &crew.tasks {
        // A crash puts back its own lock's changes and ends the task, the locks before it kept.
        let crashed = numbers.iter().any(|number| {
            crew.locks.iter().any(|changes| match bag.changed(changes, *number) {
                Some(changed) => {
                    bag = changed;
                    false
                }
                None => true,
            })
        });
        if crashed {
            lines.push("crashed".to_owned());
        }
        let added = bag.length().unwrap_or(0).saturating_mul(crew.by);
        bag.items.extend([added, added]);
        lines.push(format!("{} {}", bag.items.len(), bag.counts_shown()));
        lines.push(bag.shown());
    }
    lines
}

fn arg_written(arg: Arg) -> String {
    let signed = |by: i64| if by < 0 { format!("- {}", -by) } else { format!("+ {by}") };
    match arg {
        Arg::Literal(n) => n.to_string(),
        Arg::Times(by) => format!("(number * {by})"),
        Arg::Plus(by) => format!("(number {})", signed(by)),
        Arg::Length(by) => format!("(len(held.items) * {by})"),
        Arg::LengthLess => "(len(held.items) - number)".to_owned(),
    }
}

fn change_written(change: &Change) -> String {
    match change {
        Change::Append(arg) => format!("held.items.append({})", arg_written(*arg)),
        Change::Pop => "_ = held.items.pop()".to_owned(),
        Change::AddTwice(arg) => format!("held.add_twice({})", arg_written(*arg)),
        Change::Bump(at, arg) => format!("held.items[{at}] += {}", arg_written(*arg)),
        Change::SetCount(at, arg) => format!("held.counts[\"{}\"] = {}", key(*at), arg_written(*arg)),
        Change::Remove(at) => format!("_ = held.counts.remove(\"{}\")", key(*at)),
        Change::PopCount(at) => format!("_ = held.counts.pop(\"{}\")", key(*at)),
    }
}

fn ints_written(items: &[i64]) -> String {
    let items: Vec<String> = items.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(", "))
}

/// The task's function, what sends to it, and `crewed{id}` itself.
pub(crate) fn written(crew: &Crew) -> String {
    let id = crew.id;
    let mut out =
        format!("def crew_worker{id}(inbox: Inbox[int], box: shared Bag):\n    for number in inbox:\n");
    for changes in &crew.locks {
        out.push_str("        lock box as ref held:\n");
        for change in changes {
            let _ = writeln!(out, "            {}", change_written(change));
        }
    }
    let _ = write!(
        out,
        "\ndef crew_sent{id}(box: shared Bag, items: list[int]) fails Crashed | Closed | Timeout:\n    with \
         launch((inbox) => crew_worker{id}(inbox, box))? as sends:\n        for item in items:\n            \
         sends.send(item)?\n\n"
    );
    let counts: Vec<String> = crew.counts.iter().map(|(at, n)| format!("\"{}\": {n}", key(*at))).collect();
    let _ = writeln!(
        out,
        "def crewed{id}(stdout: Stdout):\n    shared box = Bag(items={}, counts={{{}}})",
        ints_written(&crew.items),
        counts.join(", ")
    );
    for numbers in &crew.tasks {
        let _ = write!(
            out,
            "    crew_sent{id}(box, items={}) except why:\n        case Crashed():\n            stdout.print(\"crashed\")\n        \
             case Closed():\n            stdout.print(\"closed\")\n        case Timeout():\n            \
             stdout.print(\"timeout\")\n    lock box as ref held:\n        held.add_twice((len(held.items) * {}))\n        \
             stdout.print(len(held.items), held.counts)\n    lock box as held:\n        stdout.print(held)\n",
            ints_written(numbers),
            crew.by
        );
    }
    out.push('\n');
    out
}
