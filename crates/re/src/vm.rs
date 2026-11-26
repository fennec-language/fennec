//! The Pike VM. Every thread is a way the pattern could still match. All of them take each
//! character together, and two threads at the same instruction can't do anything
//! different from then on, so the one the pattern prefers is kept and the other dropped.
//! That bounds the threads by the program's size, and the work by its size times the
//! text's length.
//!
//! A search allocates nothing once its thread has searched before: the threads, their slots
//! and the stack `add` follows with live in scratch kept between searches, and the only
//! allocation left is the `Captures` a match gives back.

use std::cell::RefCell;

use crate::compile::{Inst, Program};
use crate::parse::{Look, is_word};
use crate::{Anchor, Captures};

/// Where a group started or ended, as a byte offset: group `n` uses slots `2n` and `2n+1`.
type Slot = Option<usize>;

/// The most slots, stack steps included, the scratch keeps after a search. A pattern with many
/// groups and many threads alive at once can need far more for a while; past this, the thread
/// lets its scratch go rather than hold that much for good.
const KEPT_SLOTS: usize = 1 << 16;

thread_local! {
    /// The scratch each thread's searches reuse.
    static SCRATCH: RefCell<Scratch> = const { RefCell::new(Scratch::new()) };
}

/// What a search works in, kept between searches.
struct Scratch {
    current: Threads,
    next: Threads,
    /// The slots of the thread `add` is following.
    slots: Vec<Slot>,
    /// The slots of the match the pattern prefers so far.
    found: Vec<Slot>,
}

impl Scratch {
    const fn new() -> Scratch {
        Scratch { current: Threads::new(), next: Threads::new(), slots: Vec::new(), found: Vec::new() }
    }

    /// How many slots and steps it holds room for, all told.
    fn held(&self) -> usize {
        self.current.held() + self.next.held()
    }
}

/// The threads at one position, in the order the pattern prefers them.
struct Threads {
    /// The generation in which each instruction last got a thread, so each is followed once.
    /// A new generation clears them all at once.
    seen: Vec<u32>,
    generation: u32,
    /// Each thread's instruction.
    list: Vec<usize>,
    /// Each thread's slots, one after another, in the list's order.
    slots: Vec<Slot>,
    /// What `add` still has to do.
    stack: Vec<Step>,
}

/// What `add` does next: follow the instructions from `pc`, or put a slot back as it was
/// before a `Save`, once everything after that `Save` has been followed.
#[derive(Clone, Copy)]
enum Step {
    Follow(usize),
    Restore(usize, Slot),
}

impl Threads {
    const fn new() -> Threads {
        Threads { seen: Vec::new(), generation: 0, list: Vec::new(), slots: Vec::new(), stack: Vec::new() }
    }

    /// No threads, ready for a program of `size` instructions.
    fn start(&mut self, size: usize) {
        if self.seen.len() < size {
            self.seen.resize(size, 0);
        }
        self.clear();
    }

    fn clear(&mut self) {
        self.list.clear();
        self.slots.clear();
        self.stack.clear();
        if self.generation == u32::MAX {
            self.seen.fill(0);
            self.generation = 0;
        }
        self.generation += 1;
    }

    /// How many slots and steps it holds room for.
    fn held(&self) -> usize {
        self.slots.capacity() + self.stack.capacity()
    }

    /// The slots of the `at`th thread, `width` of them.
    fn slots_of(&self, at: usize, width: usize) -> &[Slot] {
        self.slots.get(at * width..(at + 1) * width).unwrap_or_default()
    }

    /// Adds a thread at `pc` holding `slots`, following jumps, splits, saves and looks right away,
    /// so the list holds only threads waiting for a character, or a match. `slots` changes on the
    /// way, and is as it was again when this returns.
    fn add(&mut self, program: &Program, pc: usize, slots: &mut [Slot], text: &str, pos: usize) {
        self.stack.push(Step::Follow(pc));
        while let Some(step) = self.stack.pop() {
            let mut pc = match step {
                Step::Follow(pc) => pc,
                Step::Restore(slot, was) => {
                    if let Some(slot) = slots.get_mut(slot) {
                        *slot = was;
                    }
                    continue;
                }
            };
            loop {
                match self.seen.get_mut(pc) {
                    Some(seen) if *seen != self.generation => *seen = self.generation,
                    _ => break,
                }
                match program.insts.get(pc) {
                    Some(Inst::Jump(to)) => pc = *to,
                    Some(Inst::Split(first, second)) => {
                        self.stack.push(Step::Follow(*second));
                        pc = *first;
                    }
                    Some(Inst::Save(slot)) => {
                        if let Some(held) = slots.get_mut(*slot) {
                            self.stack.push(Step::Restore(*slot, *held));
                            *held = Some(pos);
                        }
                        pc += 1;
                    }
                    Some(Inst::Look(look)) => {
                        if !holds(*look, text, pos) {
                            break;
                        }
                        pc += 1;
                    }
                    Some(_) => {
                        self.list.push(pc);
                        self.slots.extend_from_slice(slots);
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

fn holds(look: Look, text: &str, pos: usize) -> bool {
    let before = text.get(..pos).and_then(|text| text.chars().next_back()).is_some_and(is_word);
    let after = text.get(pos..).and_then(|text| text.chars().next()).is_some_and(is_word);
    match look {
        Look::Start => pos == 0,
        Look::End => pos == text.len(),
        Look::WordBoundary => before != after,
        Look::NotWordBoundary => before == after,
    }
}

/// The match the pattern prefers, starting at or after `start`.
pub(crate) fn run(program: &Program, text: &str, start: usize, anchor: Anchor) -> Option<Captures> {
    if !text.is_char_boundary(start) {
        return None;
    }
    SCRATCH
        .try_with(|kept| {
            kept.try_borrow_mut().ok().map(|mut scratch| {
                let found = search(&mut scratch, program, text, start, anchor);
                if scratch.held() > KEPT_SLOTS {
                    *scratch = Scratch::new();
                }
                found
            })
        })
        .ok()
        .flatten()
        // The thread's scratch is gone (the thread is ending) or in use (a search inside a
        // search, which nothing does): a scratch of its own does the same.
        .unwrap_or_else(|| search(&mut Scratch::new(), program, text, start, anchor))
}

/// [`Anchor`]s as numbers, for [`search_as`]'s copies.
const SEARCH: u8 = 0;
const START: u8 = 1;
const BOTH: u8 = 2;

/// [`search_as`] for `anchor`. Each anchor gets a copy of its own, its tests at each position
/// folded away, as a program calling the engine with one anchor gets: one copy for every exported
/// call would test the anchor again at every position.
#[inline]
fn search(
    scratch: &mut Scratch,
    program: &Program,
    text: &str,
    start: usize,
    anchor: Anchor,
) -> Option<Captures> {
    match anchor {
        Anchor::Search => search_as::<SEARCH>(scratch, program, text, start),
        Anchor::Start => search_as::<START>(scratch, program, text, start),
        Anchor::Both => search_as::<BOTH>(scratch, program, text, start),
    }
}

/// The match the pattern prefers from `start`, anchored as `ANCHOR`, one of [`SEARCH`], [`START`]
/// and [`BOTH`], says.
fn search_as<const ANCHOR: u8>(
    scratch: &mut Scratch,
    program: &Program,
    text: &str,
    start: usize,
) -> Option<Captures> {
    let Scratch { current, next, slots, found } = scratch;
    let width = (program.groups + 1) * 2;
    current.start(program.insts.len());
    next.start(program.insts.len());
    slots.clear();
    slots.resize(width, None);
    let mut matched = false;
    let mut pos = start;
    loop {
        if !matched && (ANCHOR == SEARCH || pos == start) {
            slots.fill(None);
            current.add(program, 0, slots, text, pos);
        }
        // With no thread alive, only a search that hasn't matched yet can still start one.
        if current.list.is_empty() && (matched || ANCHOR != SEARCH) {
            break;
        }
        let c = text.get(pos..).and_then(|rest| rest.chars().next());
        let after = pos + c.map_or(0, char::len_utf8);
        for (at, pc) in current.list.iter().enumerate() {
            let takes = match (program.insts.get(*pc), c) {
                (Some(Inst::Match), _) => {
                    if ANCHOR == BOTH && pos != text.len() {
                        continue;
                    }
                    // Threads after this one are ones the pattern likes less.
                    found.clear();
                    found.extend_from_slice(current.slots_of(at, width));
                    matched = true;
                    break;
                }
                (Some(Inst::Char(want)), Some(c)) => *want == c,
                (Some(Inst::Any), Some(c)) => c != '\n',
                (Some(Inst::Class(class)), Some(c)) => class.matches(c),
                _ => false,
            };
            if takes {
                for (slot, held) in slots.iter_mut().zip(current.slots_of(at, width)) {
                    *slot = *held;
                }
                next.add(program, pc + 1, slots, text, after);
            }
        }
        if c.is_none() {
            break;
        }
        std::mem::swap(current, next);
        next.clear();
        pos = after;
    }
    matched.then(|| Captures { groups: found.chunks(2).map(bounds).collect() })
}

fn bounds(pair: &[Option<usize>]) -> Option<(usize, usize)> {
    match pair {
        [Some(from), Some(to)] => Some((*from, *to)),
        _ => None,
    }
}
