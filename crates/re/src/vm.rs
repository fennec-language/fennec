//! The Pike VM. Every thread is a way the pattern could still match. All of them take each
//! character together, and two threads at the same instruction can't do anything
//! different from then on, so the one the pattern prefers is kept and the other dropped.
//! That bounds the threads by the program's size, and the work by its size times the
//! text's length.

use crate::compile::{Inst, Program};
use crate::parse::{Look, is_word};
use crate::{Anchor, Captures};

/// Where each group started and ended, as byte offsets: group `n` uses slots `2n` and `2n+1`.
type Slots = Vec<Option<usize>>;

/// The threads at one position, in the order the pattern prefers them.
struct Threads {
    /// Which instructions already have a thread, so each is followed once.
    seen: Vec<bool>,
    list: Vec<(usize, Slots)>,
}

impl Threads {
    fn new(size: usize) -> Threads {
        Threads { seen: vec![false; size], list: Vec::new() }
    }

    fn clear(&mut self) {
        self.seen.fill(false);
        self.list.clear();
    }

    /// Adds a thread at `pc`, following jumps, splits, saves and looks right away, so the
    /// list holds only threads waiting for a character, or a match.
    fn add(&mut self, program: &Program, pc: usize, slots: Slots, text: &str, pos: usize) {
        let mut stack = vec![(pc, slots)];
        while let Some((pc, mut slots)) = stack.pop() {
            match self.seen.get_mut(pc) {
                Some(seen) if !*seen => *seen = true,
                _ => continue,
            }
            match program.insts.get(pc) {
                Some(Inst::Jump(to)) => stack.push((*to, slots)),
                Some(Inst::Split(first, second)) => {
                    stack.push((*second, slots.clone()));
                    stack.push((*first, slots));
                }
                Some(Inst::Save(slot)) => {
                    if let Some(slot) = slots.get_mut(*slot) {
                        *slot = Some(pos);
                    }
                    stack.push((pc + 1, slots));
                }
                Some(Inst::Look(look)) => {
                    if holds(*look, text, pos) {
                        stack.push((pc + 1, slots));
                    }
                }
                Some(_) => self.list.push((pc, slots)),
                None => {}
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
    let size = program.insts.len();
    let slots = (program.groups + 1) * 2;
    let (mut current, mut next) = (Threads::new(size), Threads::new(size));
    let mut found: Option<Slots> = None;
    let mut pos = start;
    loop {
        if found.is_none() && (anchor == Anchor::Search || pos == start) {
            current.add(program, 0, vec![None; slots], text, pos);
        }
        // With no thread alive, only a search that hasn't matched yet can still start one.
        if current.list.is_empty() && (found.is_some() || anchor != Anchor::Search) {
            break;
        }
        let c = text.get(pos..).and_then(|rest| rest.chars().next());
        let after = pos + c.map_or(0, char::len_utf8);
        for (pc, slots) in std::mem::take(&mut current.list) {
            let takes = match (program.insts.get(pc), c) {
                (Some(Inst::Match), _) => {
                    if anchor == Anchor::Both && pos != text.len() {
                        continue;
                    }
                    // Threads after this one are ones the pattern likes less.
                    found = Some(slots);
                    break;
                }
                (Some(Inst::Char(want)), Some(c)) => *want == c,
                (Some(Inst::Any), Some(c)) => c != '\n',
                (Some(Inst::Class(class)), Some(c)) => class.matches(c),
                _ => false,
            };
            if takes {
                next.add(program, pc + 1, slots, text, after);
            }
        }
        if c.is_none() {
            break;
        }
        std::mem::swap(&mut current, &mut next);
        next.clear();
        pos = after;
    }
    found.map(|slots| Captures { groups: slots.chunks(2).map(bounds).collect() })
}

fn bounds(pair: &[Option<usize>]) -> Option<(usize, usize)> {
    match pair {
        [Some(from), Some(to)] => Some((*from, *to)),
        _ => None,
    }
}
