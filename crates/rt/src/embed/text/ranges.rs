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
//!
//! `split` by a separator of up to eight bytes and `find` of a part of up to eight read the text's
//! words where they are, from the range's first byte to its last: the two positions are made byte
//! offsets, which for ASCII they already are. Anything else copies the range's bytes, and only
//! those, out first.

use std::cell::RefCell;

use super::super::{State, attempt, span, strings, with};
use super::bytes_of;

thread_local! {
    /// The bytes of the slice read, and of the other text a call reads with it, kept to save
    /// making them anew for each call.
    static SLICE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OTHER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Each byte's top bit.
const HIGH_BITS: u64 = u64::from_ne_bytes([0x80; 8]);

/// Where a slice is in its text's bytes: the byte its first character starts at and the one after
/// its last, the same two for an empty slice, and whether the text is ASCII, its characters its
/// bytes.
#[derive(Clone, Copy)]
struct Bytes {
    first: usize,
    last: usize,
    ascii: bool,
}

/// A text's bytes where they are, how many, and how many characters they make, `text` given as its
/// header: a block is looked up once for all three, as its own words keep its count.
fn held<'a>(state: &'a State, text: &'a [u64; 3]) -> (&'a [u64], usize, usize) {
    let chars = match super::block_of(text) {
        Some(address) => {
            let ((words, length), ascii) = strings::ascii_bytes_in_block(state, address.cast_signed());
            if ascii {
                return (words, length, length);
            }
            state.heap.block(address).and_then(|block| block.get(3)).copied().unwrap_or(0)
        }
        None => super::chars_of(state, text),
    };
    let (words, length) = bytes_of(state, text);
    (words, length, usize::try_from(chars).unwrap_or(0))
}

/// Where `text[from:to]` is in a text's bytes, given as [`held`] gives them.
fn byte_range((words, length, chars): (&[u64], usize, usize), (from, to): (i64, i64)) -> Bytes {
    let ascii = chars == length;
    let count = i64::try_from(chars).unwrap_or(i64::MAX);
    let bound = |at: i64, missing: i64| {
        let at = if at == i64::MIN { missing } else { at };
        let at = if at < 0 { at.saturating_add(count) } else { at };
        usize::try_from(at.clamp(0, count)).unwrap_or(0)
    };
    let (from, to) = (bound(from, 0), bound(to, count));
    if from >= to {
        return Bytes { first: 0, last: 0, ascii };
    }
    if ascii {
        return Bytes { first: from, last: to, ascii };
    }
    let last = if to == chars { length } else { byte_of_char(words, length, to) };
    Bytes { first: byte_of_char(words, length, from), last, ascii }
}

/// Where character `wanted` starts in the `length` bytes packed in `words`, UTF-8, counted a word
/// at a time, as `chars_before` counts. `wanted` is one of the characters there are, so the bytes
/// past the last word's last aren't counted before it's found; one past them all is `length`.
fn byte_of_char(words: &[u64], length: usize, wanted: usize) -> usize {
    let mut seen = 0;
    for (at, word) in words.iter().enumerate().take(length.div_ceil(8)) {
        let word = u64::from_le(*word);
        // A byte that carries on a character has its top bit set and the one below it clear; each
        // other byte starts one, marked at its top bit.
        let mut starts = !(word & !(word << 1)) & HIGH_BITS;
        let here = usize::try_from(starts.count_ones()).unwrap_or(0);
        if seen + here > wanted {
            for _ in 0..wanted - seen {
                starts &= starts - 1;
            }
            return (at * 8 + usize::try_from(starts.trailing_zeros() / 8).unwrap_or(0)).min(length);
        }
        seen += here;
    }
    length
}

/// The bytes of `text[from:to]`, `text` given as its header, into `into`: only the range's.
fn sliced(state: &State, text: &[u64; 3], range: (i64, i64), into: &mut Vec<u8>) {
    let held = held(state, text);
    let Bytes { first, last, .. } = byte_range(held, range);
    let (words, ..) = held;
    let base = first / 8;
    strings::copy_words((words.get(base..).unwrap_or_default(), last - base * 8), into);
    into.drain(..first - base * 8);
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
        let (text, separator) = ([t0, t1, t2], [s0, s1, s2]);
        let separated = separator != [0; 3];
        let (between, size) = bytes_of(state, &separator);
        if separated && (1..=strings::WORD_BYTES).contains(&size) && state.tables.text_headers {
            let between = between.first().map_or(0, |word| u64::from_le(*word));
            // The range's words are copied out, whole words at a time, so making a long part's
            // block can't move them.
            let mut words = std::mem::take(&mut state.heap.scratch);
            words.clear();
            let held = held(state, &text);
            let Bytes { first, last, .. } = byte_range(held, (from, to));
            words.extend_from_slice(held.0.get(first / 8..last.div_ceil(8)).unwrap_or_default());
            let base = first / 8 * 8;
            let ascii = held.1 == held.2;
            let list =
                strings::split_words(state, (&words, ascii), (first - base, last - base), (between, size));
            state.heap.keep_scratch(words);
            return Ok(list);
        }
        let (mut slice, mut parted) = (SLICE.take(), OTHER.take());
        sliced(state, &text, (from, to), &mut slice);
        if separated {
            strings::copy_words(bytes_of(state, &separator), &mut parted);
        } else {
            parted.clear();
        }
        let split =
            strings::split_bytes(state, &slice, separated.then_some(parted.as_slice()), span(start, end));
        SLICE.set(slice);
        OTHER.set(parted);
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
        let (text, part) = ([t0, t1, t2], [p0, p1, p2]);
        let held = held(state, &text);
        let range = byte_range(held, (from, to));
        let (words, ..) = held;
        let (looked, size) = bytes_of(state, &part);
        if size == 0 {
            return 0;
        }
        if size > range.last - range.first {
            return -1;
        }
        if size > strings::WORD_BYTES {
            // Longer ones are rare, and `str`'s search stays linear however the two repeat
            // themselves.
            return with_both(state, &text, (from, to), &part, |slice, part| {
                let found = match (std::str::from_utf8(slice), std::str::from_utf8(part)) {
                    (Ok(slice), Ok(part)) => slice.find(part).and_then(|at| slice.get(..at)),
                    _ => None,
                };
                found.map_or(-1, |before| i64::try_from(before.chars().count()).unwrap_or(-1))
            });
        }
        let Some(at) = strings::place_from((words, range.last), (looked, size), range.first) else {
            return -1;
        };
        let before = if range.ascii {
            at - range.first
        } else {
            // Counted from the word the range starts in.
            let base = range.first / 8;
            let words = words.get(base..).unwrap_or_default();
            strings::chars_before(words, at - base * 8) - strings::chars_before(words, range.first - base * 8)
        };
        i64::try_from(before).unwrap_or(-1)
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
