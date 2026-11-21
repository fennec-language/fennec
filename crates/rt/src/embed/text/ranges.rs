//! What a call reads of a slice of a text, read where it is: `text[from:to].split(x)`, `.find(x)`,
//! `.startswith(x)` and `== x`, with no string made for the slice. A slice of a long text makes a
//! block and copies its part in, about 540 instructions with the block's making and freeing, for a
//! string the call reads once and lets go (`runtime-lane-perf/str-slice-options-2026-11-21.md` in
//! the coordinator's notes). The compiler calls these where a slice's only use is the text the next
//! call reads, so what a program sees is the same: the same answer, and no failure a slice
//! wouldn't have.
//!
//! The range is the slice's own, read as `fennec_str_slice` reads it: positions count characters,
//! a negative one counts from the end, a missing one is `i64::MIN`, and both are clamped into the
//! text, so a start at or past the end gives the empty string. A slice never fails for a text.

use std::cell::RefCell;

use super::super::{State, attempt, span, strings, with};
use super::bytes_of;

thread_local! {
    /// The bytes of the slice read, and of the other text a call reads with it, kept to save
    /// making them anew for each call.
    static SLICE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OTHER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The bytes of `text[from:to]`, `text` given as its header, into `into`.
fn sliced(state: &State, text: &[u64; 3], (from, to): (i64, i64), into: &mut Vec<u8>) {
    strings::copy_words(bytes_of(state, text), into);
    let chars = into.iter().filter(|byte| **byte & 0xC0 != 0x80).count();
    let count = i64::try_from(chars).unwrap_or(i64::MAX);
    let bound = |at: i64, missing: i64| {
        let at = if at == i64::MIN { missing } else { at };
        let at = if at < 0 { at.saturating_add(count) } else { at };
        usize::try_from(at.clamp(0, count)).unwrap_or(0)
    };
    let (from, to) = (bound(from, 0), bound(to, count));
    if from >= to {
        into.clear();
        return;
    }
    let (first, last) =
        if chars == into.len() { (from, to) } else { (byte_of_char(into, from), byte_of_char(into, to)) };
    into.truncate(last);
    into.drain(..first);
}

/// Where character `wanted` starts in `bytes`, UTF-8, or their length past the last.
fn byte_of_char(bytes: &[u8], wanted: usize) -> usize {
    bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte & 0xC0 != 0x80)
        .nth(wanted)
        .map_or(bytes.len(), |(at, _)| at)
}

/// Runs `read` on the bytes of `text[from:to]` and of `other`, both headers, each copied into the
/// bytes kept for them.
fn with_both<T>(
    state: &State,
    text: &[u64; 3],
    range: (i64, i64),
    other: &[u64; 3],
    read: impl FnOnce(&[u8], &[u8]) -> T,
) -> T {
    let (mut slice, mut part) = (SLICE.take(), OTHER.take());
    sliced(state, text, range, &mut slice);
    strings::copy_words(bytes_of(state, other), &mut part);
    let answer = read(&slice, &part);
    SLICE.set(slice);
    OTHER.set(part);
    answer
}

/// `fennec_str_split_range(text, from, to, separator, start, end)`: `text[from:to].split(separator)`
/// for a text and a separator given as their headers, with no string made for the slice. A
/// separator of three zeros is none, `split()`, as for `fennec_str_split_header`.
pub extern "C" fn split_range(
    t0: u64,
    t1: u64,
    t2: u64,
    from: i64,
    to: i64,
    s0: u64,
    s1: u64,
    s2: u64,
    start: i64,
    end: i64,
) -> i64 {
    attempt(|state: &mut State| {
        let (mut slice, mut between) = (SLICE.take(), OTHER.take());
        sliced(state, &[t0, t1, t2], (from, to), &mut slice);
        let separated = (s0, s1, s2) != (0, 0, 0);
        if separated {
            strings::copy_words(bytes_of(state, &[s0, s1, s2]), &mut between);
        } else {
            between.clear();
        }
        let split =
            strings::split_bytes(state, &slice, separated.then_some(between.as_slice()), span(start, end));
        SLICE.set(slice);
        OTHER.set(between);
        split
    })
}

/// `fennec_str_find_range(text, from, to, part)`: `text[from:to].find(part)`, which character of the
/// slice `part` first starts at, or -1 when it isn't there.
pub extern "C" fn find_range(
    t0: u64,
    t1: u64,
    t2: u64,
    from: i64,
    to: i64,
    p0: u64,
    p1: u64,
    p2: u64,
) -> i64 {
    with(|state| {
        with_both(state, &[t0, t1, t2], (from, to), &[p0, p1, p2], |slice, part| {
            let found = match (std::str::from_utf8(slice), std::str::from_utf8(part)) {
                (Ok(slice), Ok(part)) => slice.find(part).and_then(|at| slice.get(..at)),
                _ => None,
            };
            found.map_or(-1, |before| i64::try_from(before.chars().count()).unwrap_or(-1))
        })
    })
}

/// `fennec_text_starts_with_range(text, from, to, part)`: `text[from:to].startswith(part)`.
pub extern "C" fn starts_with_range(
    t0: u64,
    t1: u64,
    t2: u64,
    from: i64,
    to: i64,
    p0: u64,
    p1: u64,
    p2: u64,
) -> i8 {
    with(|state| {
        i8::from(with_both(state, &[t0, t1, t2], (from, to), &[p0, p1, p2], |slice, part| {
            slice.starts_with(part)
        }))
    })
}

/// `fennec_text_equals_range(text, from, to, other)`: `text[from:to] == other`.
pub extern "C" fn equals_range(
    t0: u64,
    t1: u64,
    t2: u64,
    from: i64,
    to: i64,
    o0: u64,
    o1: u64,
    o2: u64,
) -> i8 {
    with(|state| {
        i8::from(with_both(state, &[t0, t1, t2], (from, to), &[o0, o1, o2], |slice, other| slice == other))
    })
}

#[cfg(test)]
mod tests;
