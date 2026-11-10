//! Putting back what a crash changed inside `lock x as ref s:`. Before each write inside such a block,
//! generated code saves the word it's about to overwrite. The runtime keeps the saves in a log, one
//! for each lock held, innermost last.
//!
//! - **A normal end,** by `fail` and `return` too, keeps the changes (`kept`). The log goes, with the
//!   counts it held of what was taken out.
//! - **A crash** puts every saved word back as it was before the lock was taken (`undo`). Then the
//!   lock's let go, as at any other end.
//!
//! The runtime only reaches a block through the tables this thread holds. So it puts back the words
//! of blocks in them itself. The lock's own cell is in no table it holds, on any thread but the one
//! that made it. So native code puts the cell's words back, a word at a time, with what `undo_word`
//! gives it.
//!
//! A save names a block by its address, and a word by its index among the block's words, from its
//! count. Saving never fails, so generated code has no flag to check after each one.
//!
//! A list about to be changed where it is, like a shared queue's, isn't held as it was: holding it
//! would make the change copy all of it, every time the lock's taken. The log marks it instead (see
//! `lists::MARKED`), which native code takes as another owner, so every change asks the runtime
//! first. An append or a pop there is made in place and noted here, for a crash to undo in turn; any
//! other change copies the list, as for one held.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;

use super::super::heap::AddressHasher;
use super::super::{State, attempt, heap, lists, with};

/// One word as it was before a block's code changed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Saved {
    block: u64,
    word: usize,
    old: u64,
    /// Whether the log holds a count of `old`, a block's address: one it took over from the word,
    /// or one of its own.
    held: bool,
    /// Whether `old` is a list the log marked, where it holds the mark in place of a count.
    marked: bool,
}

/// A change made in place to a list a log marked, for a crash to undo.
enum Change {
    /// Items went on the end of a list that had `before` of them.
    Grew { before: usize },
    /// The item at `at`, the last, was taken off, and the log holds its words, with an owner of
    /// each block in them.
    Popped { at: usize, words: Vec<u64> },
}

/// What's saved for one lock held.
struct Log {
    /// The lock's own block.
    cell: u64,
    /// How many tables were behind the current one when the lock was taken. One more than the log
    /// around it says means the lock brought a table of its own. A value of only numbers has none.
    depth: usize,
    saved: Vec<Saved>,
    /// Where each save is for, so that a place written again and again is saved once.
    places: HashSet<u64, BuildHasherDefault<AddressHasher>>,
    /// What was changed in place in the lists it marked, by list, oldest first.
    changes: Vec<(u64, Change)>,
}

/// The address of the word a save is for, which only that word has.
fn place(saved: &Saved) -> u64 {
    saved.block.wrapping_add(u64::try_from(saved.word).unwrap_or(0).wrapping_mul(8))
}

/// What became of a save.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pushed {
    Added,
    /// The place was saved already, and only its first save goes back.
    Repeated,
    /// No lock is held that may crash.
    NoLog,
}

thread_local! {
    /// The logs of the locks this thread holds that may crash, innermost last.
    static LOGS: RefCell<Vec<Log>> = const { RefCell::new(Vec::new()) };
    /// For a crash, what goes back in the lock's own cell, by word, for native code to write.
    static CELL: RefCell<HashMap<usize, Saved>> = RefCell::new(HashMap::new());
}

fn address(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// `fennec_lock_begin(cell)`: a lock that may crash is taken, after its table, if it has one.
pub extern "C" fn begin(cell: i64) {
    attempt(|state: &mut State| {
        let depth = state.heap.tables_behind();
        LOGS.with_borrow_mut(|logs| {
            logs.push(Log {
                cell: address(cell),
                depth,
                saved: Vec::new(),
                places: HashSet::default(),
                changes: Vec::new(),
            });
        });
        Ok(())
    });
}

/// Adds a save to the log of the lock it's for: the log whose cell it is, or the innermost.
fn push(saved: Saved) -> Pushed {
    LOGS.with_borrow_mut(|logs| {
        let at = logs.iter().rposition(|log| log.cell == saved.block).or(logs.len().checked_sub(1));
        let Some(log) = at.and_then(|at| logs.get_mut(at)) else { return Pushed::NoLog };
        // A loop writes the same place, so the last save is looked at first.
        let last = log.saved.last().is_some_and(|last| last.block == saved.block && last.word == saved.word);
        if last || !log.places.insert(place(&saved)) {
            return Pushed::Repeated;
        }
        log.saved.push(saved);
        Pushed::Added
    })
}

/// `fennec_lock_save(block, word, old, holds)`: the word at `word` in `block` is about to change from
/// `old`. With `holds` 1, `old` is a block's address, and the log takes over the word's count of it:
/// generated code doesn't let go of it at the write.
pub extern "C" fn save(block: i64, word: i64, old: i64, holds: i64) {
    let Ok(word) = usize::try_from(word) else { return };
    let saved = Saved {
        block: address(block),
        word,
        old: old.cast_unsigned(),
        held: holds != 0 && old != 0,
        marked: false,
    };
    if push(saved) != Pushed::Added && saved.held {
        // What a repeated save names was made under the lock, and nothing will put it back. With no
        // lock held that may crash, which would be a bug in generated code, it's the same. Either
        // way the count is let go of, as the write would have.
        attempt(|state: &mut State| {
            heap::release(state, saved.old);
            Ok(())
        });
    }
}

/// `fennec_lock_save_text(block, word, w0, w1, w2)`: a string's three-word header at `word` is about
/// to change. Its first word is a block's address when the top bit of its last is clear, and the log
/// takes over the header's count of that block.
pub extern "C" fn save_text(block: i64, word: i64, w0: i64, w1: i64, w2: i64) {
    let Ok(at) = usize::try_from(word) else { return };
    let in_block = i64::from(w2 >= 0);
    save(block, word, w0, in_block);
    save(block, i64::try_from(at + 1).unwrap_or(-1), w1, 0);
    save(block, i64::try_from(at + 2).unwrap_or(-1), w2, 0);
}

/// `fennec_lock_save_ref(block, word, old)`: the value at `word`, the block `old`, is about to be
/// changed where it is, by a call it's passed to as `ref` or one of the runtime's own. The log takes a
/// count of its own, so the change copies it rather than change what the crash would put back. Once a
/// word's saved, what's there since was made under the lock, so a second save of it does nothing,
/// and changing it again doesn't copy.
pub extern "C" fn save_ref(block: i64, word: i64, old: i64) {
    let Ok(word) = usize::try_from(word) else { return };
    let saved = Saved { block: address(block), word, old: address(old), held: old != 0, marked: false };
    let known = LOGS.with_borrow(|logs| logs.iter().any(|log| log.places.contains(&place(&saved))));
    if known {
        return;
    }
    // A list is marked rather than held, so that an append or a pop changes it in place.
    if with(|state| lists::markable(state, saved.old)) {
        let saved = Saved { held: false, marked: true, ..saved };
        if push(saved) == Pushed::Added {
            attempt(|state: &mut State| {
                lists::mark(state, saved.old);
                Ok(())
            });
        }
        return;
    }
    if push(saved) != Pushed::Added || !saved.held {
        return;
    }
    attempt(|state: &mut State| {
        heap::retain(state, saved.old);
        Ok(())
    });
}

/// `fennec_lock_kept()`: the innermost lock that may crash ends normally, keeping what it changed.
/// What the log held is let go of. Saves of what's not this lock's, written while it was held, go to
/// the log of the lock around it, for its own crash to put back.
pub extern "C" fn kept() {
    attempt(|state: &mut State| {
        let Some(log) = LOGS.with_borrow_mut(Vec::pop) else { return Ok(()) };
        let outer = LOGS.with_borrow(|logs| logs.last().map(|outer| outer.depth));
        let own_table = outer.is_none_or(|depth| log.depth > depth);
        let mut passed = Vec::new();
        let mut changes = log.changes;
        for saved in log.saved {
            let in_own_table =
                own_table && usize::try_from(saved.block).is_ok_and(|at| state.heap.blocks.get(at).is_some());
            if outer.is_none() || saved.block == log.cell || in_own_table {
                if saved.held {
                    heap::release(state, saved.old);
                }
                if saved.marked {
                    // What it changed stays: the popped items it held go, and so does the mark.
                    for (_, change) in take_changes(&mut changes, saved.old) {
                        if let Change::Popped { words, .. } = change {
                            lists::let_go_of_words(state, saved.old, &words);
                        }
                    }
                    lists::unmark(state, saved.old, false);
                }
            } else {
                passed.push(saved);
            }
        }
        let (repeated, marked) = LOGS.with_borrow_mut(|logs| {
            let (mut repeated, mut marked) = (Vec::new(), Vec::new());
            if let Some(outer) = logs.last_mut() {
                for saved in passed {
                    if outer.places.insert(place(&saved)) {
                        // A list this lock marked is the outer one's to undo now, with its changes.
                        if saved.marked {
                            outer.changes.extend(take_changes(&mut changes, saved.old));
                        }
                        outer.saved.push(saved);
                    } else if saved.held {
                        repeated.push(saved.old);
                    } else if saved.marked {
                        marked.push((saved.old, take_changes(&mut changes, saved.old)));
                    }
                }
            }
            (repeated, marked)
        });
        for old in repeated {
            heap::release(state, old);
        }
        // The outer lock saved the place before this one marked what's there, so what it puts back
        // is its own: what this lock changed stays.
        for (list, changes) in marked {
            for (_, change) in changes {
                if let Change::Popped { words, .. } = change {
                    lists::let_go_of_words(state, list, &words);
                }
            }
            lists::unmark(state, list, false);
        }
        Ok(())
    });
}

/// `fennec_lock_undo()`: the innermost lock that may crash is left by a crash. Every saved word of a
/// block in the tables this thread holds goes back as it was before the lock was taken, and what's
/// there now, and what the log held between, is let go of. What goes back in the lock's own cell
/// waits for `undo_word`, which native code calls for each of the cell's words, before the lock's
/// table goes.
pub extern "C" fn undo() {
    attempt(|state: &mut State| {
        let Some(log) = LOGS.with_borrow_mut(Vec::pop) else { return Ok(()) };
        // Each list it marked is put back as it was, its changes undone newest first, and its mark
        // becomes an owner, for the word that held it to hold again: what's there now, the same list
        // or a copy, is let go of below as any saved word's is.
        let mut changes = log.changes;
        for saved in log.saved.iter().filter(|saved| saved.marked) {
            for (_, change) in take_changes(&mut changes, saved.old).into_iter().rev() {
                match change {
                    Change::Grew { before } => lists::shorten_to(state, saved.old, before),
                    Change::Popped { at, words } => lists::put_back(state, saved.old, at, &words),
                }
            }
            lists::unmark(state, saved.old, true);
        }
        let mut cell = HashMap::new();
        let mut first = Vec::new();
        let mut seen = HashSet::new();
        let mut released = Vec::new();
        for saved in log.saved {
            if !seen.insert((saved.block, saved.word)) {
                // A value made under the lock: what goes back is the first save's.
                if saved.held {
                    released.push(saved.old);
                }
            } else if saved.block == log.cell {
                cell.insert(saved.word, saved);
            } else {
                first.push(saved);
            }
        }
        // What each word holds now is read before any goes back, while what the value holds is as the
        // crash left it, all of it there to be read.
        let mut places: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut back = Vec::with_capacity(first.len());
        for saved in first {
            let Some(now) =
                state.heap.block_mut(saved.block).and_then(|words| words.get(saved.word).copied())
            else {
                // A block no table here holds: a save there's nothing to put back for.
                heap::not_native(saved.block, "put back, after a crash under a lock");
                if saved.held {
                    released.push(saved.old);
                }
                continue;
            };
            let holds = places.entry(saved.block).or_insert_with(|| places_in(state, saved.block));
            if now != 0 && holds.contains(&saved.word) {
                released.push(now);
            }
            back.push(saved);
        }
        for saved in back {
            if let Some(word) = state.heap.block_mut(saved.block).and_then(|words| words.get_mut(saved.word))
            {
                *word = saved.old;
            }
        }
        for old in released {
            heap::release(state, old);
        }
        let left = CELL.with_borrow_mut(|waiting| std::mem::replace(waiting, cell));
        for saved in left.into_values().filter(|saved| saved.held) {
            heap::release(state, saved.old);
        }
        Ok(())
    });
}

/// The changes noted for `list`, oldest first, taken out of `changes`.
fn take_changes(changes: &mut Vec<(u64, Change)>, list: u64) -> Vec<(u64, Change)> {
    let (taken, kept): (Vec<_>, Vec<_>) =
        std::mem::take(changes).into_iter().partition(|(at, _)| *at == list);
    *changes = kept;
    taken
}

/// Notes a change in place to a list a log marked, in that log. Appends one after another undo as
/// one, back to the length before the first.
fn note(list: u64, change: Change) -> bool {
    LOGS.with_borrow_mut(|logs| {
        let Some(log) =
            logs.iter_mut().rev().find(|log| log.saved.iter().any(|saved| saved.marked && saved.old == list))
        else {
            return false;
        };
        let again = matches!(change, Change::Grew { .. })
            && log
                .changes
                .iter()
                .rev()
                .find(|(at, _)| *at == list)
                .is_some_and(|(_, last)| matches!(last, Change::Grew { .. }));
        if !again {
            log.changes.push((list, change));
        }
        true
    })
}

/// A marked list moved from `from` to `to`, to have room: the log that marked it goes by where it is.
pub(in crate::embed) fn moved(from: u64, to: u64) {
    LOGS.with_borrow_mut(|logs| {
        for log in logs.iter_mut() {
            for saved in log.saved.iter_mut().filter(|saved| saved.marked && saved.old == from) {
                saved.old = to;
            }
            for (list, _) in &mut log.changes {
                if *list == from {
                    *list = to;
                }
            }
        }
    });
}

/// A marked list is about to have items go on its end, from `before`. Says whether a log has it.
pub(in crate::embed) fn grew(list: u64, before: usize) -> bool {
    note(list, Change::Grew { before })
}

/// A marked list's last item, at `at`, is about to be taken off: the log holds `words`, with an
/// owner of each block in them, already taken. Says whether a log has it.
pub(in crate::embed) fn popped(list: u64, at: usize, words: Vec<u64>) -> bool {
    note(list, Change::Popped { at, words })
}

/// Where a block holds others, or nowhere if it can't say.
fn places_in(state: &State, block: u64) -> Vec<usize> {
    let mut places = Vec::new();
    if let Some(words) = state.heap.block(block) {
        let _ = heap::child_places(state, words, &mut places);
    }
    places
}

/// `fennec_lock_undo_word(word, now, now_holds)`, after `undo`: the word to write back at `word` in
/// the lock's own cell, where `now` is, a block's address if `now_holds` is 1. A word that wasn't
/// saved stays as it is. A word that goes back lets go of what's there now.
pub extern "C" fn undo_word(word: i64, now: i64, now_holds: i64) -> i64 {
    let Ok(at) = usize::try_from(word) else { return now };
    let Some(saved) = CELL.with_borrow_mut(|waiting| waiting.remove(&at)) else { return now };
    if now_holds != 0 && now != 0 {
        attempt(|state: &mut State| {
            heap::release(state, address(now));
            Ok(())
        });
    }
    saved.old.cast_signed()
}

/// How many locks that may crash this thread holds, for tests.
#[cfg(test)]
fn held() -> usize {
    LOGS.with_borrow(Vec::len)
}

/// How many saves the logs hold.
#[cfg(test)]
fn saved_entries() -> usize {
    LOGS.with_borrow(|logs| logs.iter().map(|log| log.saved.len()).sum())
}

#[cfg(test)]
mod tests;
