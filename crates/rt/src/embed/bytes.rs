//! `bytes` for native code. A block holds its owner count, its shape, its length in bytes, its room
//! in bytes, then the bytes, packed into words. They don't change once it's filled in, but for a
//! byte builder's, which std's own intrinsics write into. Native code reads the length and the
//! bytes itself, and everything that makes a new block happens here.
//!
//! The room is the words after the header, in bytes, as a list's room is (#1941): a builder can set
//! room aside and write less, and test for room with one load and one compare.

use fennec_syntax::{Diagnostic, Span};

use super::heap::{NEVER_FREED_COUNT, block_text, text_address};
use super::{State, fail_with, with};
use crate::collections::{from_hex, hex, hex_problem};
use crate::value::Value;

/// Words before the bytes: the owner count, the shape, the length in bytes and the room in bytes.
const HEADER_WORDS: usize = 4;
/// Where the bytes start in a block, for native code.
pub const DATA_OFFSET: u32 = 32;

/// A block's header for `length` bytes in `words` words of room after it.
fn header(length: usize, words: usize) -> [u64; HEADER_WORDS] {
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    [1, super::heap::BYTES_SHAPE, number(length), number(words * 8)]
}

/// What the length of a block with this many words of bytes needs.
fn words_for(length: usize) -> usize {
    HEADER_WORDS + length.div_ceil(8)
}

/// A new block with room for this many bytes, all zero.
fn with_room(state: &mut State, length: usize) -> Box<[u64]> {
    written_from(state, length, 0)
}

/// A new block with room for `length` bytes, whose first `written` words of bytes the caller
/// writes, and the rest zero: each word is zeroed once, if at all, by the heap as it gives the
/// block out (`Heap::fresh_written`).
fn written_from(state: &mut State, length: usize, written: usize) -> Box<[u64]> {
    let words = words_for(length);
    let mut block = state.heap.fresh_written(words, HEADER_WORDS + written);
    if let Some(start) = block.get_mut(..HEADER_WORDS) {
        start.copy_from_slice(&header(length, words - HEADER_WORDS));
    }
    block
}

/// A new block holding these bytes.
pub(super) fn make(state: &mut State, data: &[u8]) -> i64 {
    // Every word of bytes is written below, the last with zeros after its bytes.
    let mut block = written_from(state, data.len(), data.len().div_ceil(8));
    for (word, chunk) in block.iter_mut().skip(HEADER_WORDS).zip(data.chunks(8)) {
        let mut packed = [0; 8];
        packed.get_mut(..chunk.len()).unwrap_or_default().copy_from_slice(chunk);
        *word = u64::from_le_bytes(packed);
    }
    state.heap.insert(block)
}

/// The bytes in a block.
pub(super) fn data_of(block: &[u64]) -> Vec<u8> {
    let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
    let mut data: Vec<u8> = block.iter().skip(HEADER_WORDS).flat_map(|word| word.to_le_bytes()).collect();
    data.truncate(length);
    data
}

/// The words two blocks of bytes are compared and hashed by: the length, then the words its bytes
/// take up, the last with nothing after its bytes. A builder's spare room, and whatever's left in
/// it, never counts, so equal bytes are equal however their block was made.
pub(super) fn words_of(block: &[u64]) -> Vec<u64> {
    let length = block.get(2).copied().unwrap_or(0);
    let bytes = usize::try_from(length).unwrap_or(0);
    let mut words = Vec::with_capacity(1 + bytes.div_ceil(8));
    words.push(length);
    words.extend_from_slice(block.get(HEADER_WORDS..HEADER_WORDS + bytes.div_ceil(8)).unwrap_or_default());
    let used = bytes % 8;
    if used != 0
        && let Some(last) = words.last_mut()
    {
        *last &= u64::MAX >> (64 - 8 * used);
    }
    words
}

/// Whether two blocks of bytes hold the same bytes (see `words_of`).
pub(super) fn equal(left: &[u64], right: &[u64]) -> bool {
    left.get(2) == right.get(2) && words_of(left) == words_of(right)
}

/// The bytes from `from` up to `to` in a block, which are inside its length.
fn part_of(block: &[u64], from: usize, to: usize) -> Vec<u8> {
    let words = block.get(HEADER_WORDS + from / 8..HEADER_WORDS + to.div_ceil(8)).unwrap_or_default();
    let skip = from % 8;
    words.iter().flat_map(|word| word.to_le_bytes()).skip(skip).take(to - from).collect()
}

fn data_at(state: &State, at: i64) -> Vec<u8> {
    state.heap.block(at.cast_unsigned()).map(data_of).unwrap_or_default()
}

fn text_at(state: &State, at: i64) -> String {
    state.heap.block(at.cast_unsigned()).map(block_text).unwrap_or_default()
}

/// The room a byte builder's block has for `length` bytes: the next power of two, and at least 16.
/// Native code reads the room word, so `grow` could give room as a list's grows; this is what it
/// gives for now. Only std's own builder intrinsics write into one, and it's copied to its exact
/// size before anything else sees it. Equality and hashing go by the length anyway (`words_of`),
/// so one that isn't still compares and hashes as the same bytes would.
fn builder_room(length: usize) -> usize {
    length.max(16).next_power_of_two()
}

/// An empty byte builder, with room for 16 bytes.
pub extern "C" fn builder() -> i64 {
    super::made_with("fennec_bytes_builder", |state| {
        let mut block = with_room(state, builder_room(0));
        if let Some(length) = block.get_mut(2) {
            *length = 0;
        }
        state.heap.insert(block)
    })
}

/// A byte builder with room for at least `wanted` bytes, holding what `bytes` does. It takes over
/// the caller's owner of `bytes`, and lets go of it, so what it gives back is the caller's alone.
pub extern "C" fn grow(bytes: i64, wanted: i64) -> i64 {
    super::made_with("fennec_bytes_grow", |state| {
        let length = state
            .heap
            .block(bytes.cast_unsigned())
            .and_then(|block| block.get(2))
            .and_then(|n| usize::try_from(*n).ok())
            .unwrap_or(0);
        let used = length.div_ceil(8);
        let room = builder_room(usize::try_from(wanted).unwrap_or(0).max(length));
        // Made first and filled from the old block where it is, with no copy of its bytes between.
        let mut grown = written_from(state, room, used);
        if let Some(header) = grown.get_mut(2) {
            *header = u64::try_from(length).unwrap_or(0);
        }
        let kept = state.heap.block(bytes.cast_unsigned()).unwrap_or_default();
        let kept = kept.get(HEADER_WORDS..HEADER_WORDS + used).unwrap_or_default();
        if let Some(words) = grown.get_mut(HEADER_WORDS..HEADER_WORDS + kept.len()) {
            words.copy_from_slice(kept);
        }
        // Words the old block didn't have, were it ever short of its length, aren't left as they were.
        if let Some(rest) = grown.get_mut(HEADER_WORDS + kept.len()..HEADER_WORDS + used) {
            rest.fill(0);
        }
        super::heap::release(state, bytes.cast_unsigned());
        state.heap.insert(grown)
    })
}

/// A new block of this length for native code to copy bytes into.
pub extern "C" fn new(length: i64) -> i64 {
    with(|state| {
        let block = with_room(state, usize::try_from(length).unwrap_or(0));
        state.heap.insert(block)
    })
}

/// A block of bytes written into the program, by the global native code keeps it in: one being
/// filled in by the thread that asked first, or one filled in.
enum Embedded {
    Filling(&'static mut [u64]),
    Filled(i64),
}

/// Every block of embedded bytes there is, by its global, and the wait for one being filled in.
static EMBEDDED: std::sync::Mutex<Vec<(i64, Embedded)>> = std::sync::Mutex::new(Vec::new());
static FILLED: std::sync::Condvar = std::sync::Condvar::new();

/// `fennec_bytes_embedded(global, length)`: the one block, in the whole process, of a bytes
/// literal or of `embed_file`'s or `embed_folder`'s bytes, with room for `length`, which native
/// code keeps in `global` and never lets go of.
///
/// The thread that asks first gets it empty, with the lowest bit of its address set: it copies the
/// bytes in from the program's own, then says so with `fennec_bytes_embedded_filled(global)`. One
/// asking meanwhile waits for that, which is a copy's while, and gets it filled.
///
/// Nothing of the thread that makes it changes: it's made outside its heap, never freed and owned
/// by no thread (`NEVER_FREED_OWNER`), so it takes no task's mark and isn't in any thread's counts,
/// accounts or audit. A program, and its replay, see the same whichever thread got there first.
pub extern "C" fn embedded(global: i64, length: i64) -> i64 {
    let mut made = EMBEDDED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        match made.iter().find(|(at, _)| *at == global) {
            None => break,
            Some((_, Embedded::Filled(block))) => return *block,
            Some((_, Embedded::Filling(_))) => {
                made = FILLED.wait(made).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
    let length = usize::try_from(length).unwrap_or(0);
    let words = words_for(length);
    let mut block = vec![0; words].into_boxed_slice();
    if let Some(start) = block.get_mut(..HEADER_WORDS) {
        start.copy_from_slice(&header(length, words - HEADER_WORDS));
    }
    if let Some(count) = block.first_mut() {
        *count = NEVER_FREED_COUNT;
    }
    super::heap::mark_never_freed(&mut block);
    let block = Box::leak(block);
    let address = i64::try_from(block.as_ptr().addr()).unwrap_or(0);
    made.push((global, Embedded::Filling(block)));
    address | 1
}

/// `fennec_bytes_embedded_filled(global)`: the block `fennec_bytes_embedded` gave this thread to
/// fill in is filled in, so every thread can read it.
pub extern "C" fn embedded_filled(global: i64) {
    let mut made = EMBEDDED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entry) = made.iter_mut().find(|(at, _)| *at == global) else { return };
    let Embedded::Filling(_) = entry.1 else { return };
    let Embedded::Filling(block) = std::mem::replace(&mut entry.1, Embedded::Filled(0)) else { return };
    let block: &'static [u64] = block;
    entry.1 = Embedded::Filled(i64::try_from(block.as_ptr().addr()).unwrap_or(0));
    super::heap::add_embedded(block);
    drop(made);
    FILLED.notify_all();
}

/// `bytes(items)`: each item has to be an int from 0 to 255.
pub extern "C" fn of_list(list: i64, start: i64, end: i64) -> i64 {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    // Packed eight to a word as they're checked, so the items are read once and nothing as big as
    // the list is copied on the way.
    let packed: Option<Result<(Vec<u64>, usize), i64>> = with(|state| {
        let items = state
            .heap
            .block(list.cast_unsigned())
            .map(|block| super::lists::items(block, 1))
            .unwrap_or_default();
        let mut words = Vec::with_capacity(items.len().div_ceil(8));
        for chunk in items.chunks(8) {
            let mut word = 0;
            for (at, item) in chunk.iter().enumerate() {
                if *item > 255 {
                    return Some(Err(item.cast_signed()));
                }
                word |= item << (8 * at);
            }
            words.push(word);
        }
        Some(Ok((words, items.len())))
    });
    match packed.unwrap_or(Ok((Vec::new(), 0))) {
        Ok((words, length)) => with(|state| {
            let mut block = state.heap.fresh(HEADER_WORDS + words.len());
            let header = header(length, words.len());
            for (slot, word) in block.iter_mut().zip(header.iter().chain(&words)) {
                *slot = *word;
            }
            state.heap.insert(block)
        }),
        Err(item) => {
            fail_with(Diagnostic::new(format!("a byte is an int from 0 to 255, not {item}"), span));
            0
        }
    }
}

/// `a + b`.
pub extern "C" fn join(a: i64, b: i64) -> i64 {
    with(|state| {
        let mut data = data_at(state, a);
        data.extend(data_at(state, b));
        make(state, &data)
    })
}

/// `data[start:end]` with Python's rules, `i64::MIN` for a bound that isn't there.
pub extern "C" fn slice(bytes: i64, start: i64, end: i64) -> i64 {
    with(|state| {
        let block = state.heap.block(bytes.cast_unsigned()).unwrap_or_default();
        let length = block.get(2).and_then(|n| i64::try_from(*n).ok()).unwrap_or(0);
        let bound = |at: i64, default: i64| {
            let at = if at == i64::MIN {
                default
            } else if at < 0 {
                at.saturating_add(length)
            } else {
                at
            };
            usize::try_from(at.clamp(0, length)).unwrap_or(0)
        };
        let (from, to) = (bound(start, 0), bound(end, length));
        let to = to.max(from);
        // A part starting at a word, as a whole run does, is copied a word at a time, with what's
        // past its end in the last word cleared, since bytes compare and hash by their words.
        if from % 8 == 0 {
            let words: Vec<u64> = block
                .get(HEADER_WORDS + from / 8..HEADER_WORDS + to.div_ceil(8))
                .unwrap_or_default()
                .to_vec();
            let size = to - from;
            // Every word of bytes is written below, the last cleared past its end.
            let mut made = written_from(state, size, size.div_ceil(8));
            for (word, held) in made.iter_mut().skip(HEADER_WORDS).zip(words) {
                *word = held;
            }
            if size % 8 != 0
                && let Some(last) = made.last_mut()
            {
                *last &= (1u64 << (8 * (size % 8))) - 1;
            }
            return state.heap.insert(made);
        }
        // Only the part is copied out, so taking a small part of a long run costs only the part.
        let part = part_of(block, from, to);
        make(state, &part)
    })
}

/// The text in bytes `start` up to `end` of a run of bytes, made straight from them, or the empty
/// text when they aren't UTF-8. Both places are inside it, and a format checks that first.
pub extern "C" fn text_part(bytes: i64, start: i64, end: i64) -> i64 {
    with(|state| {
        let block = state.heap.block(bytes.cast_unsigned()).unwrap_or_default();
        let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let from = usize::try_from(start).unwrap_or(0).min(length);
        let to = usize::try_from(end).unwrap_or(0).clamp(from, length);
        let part = part_of(block, from, to);
        match std::str::from_utf8(&part) {
            Ok(text) => {
                let chars = text.chars().count();
                super::heap::text_from_bytes(state, &part, chars)
            }
            Err(_) => text_address(state, ""),
        }
    })
}

/// How two compare, as below zero, zero or above.
pub extern "C" fn order(a: i64, b: i64) -> i64 {
    with(|state| match data_at(state, a).cmp(&data_at(state, b)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

/// `byte in data`.
pub extern "C" fn contains(bytes: i64, byte: i64) -> i8 {
    with(|state| i8::from(u8::try_from(byte).is_ok_and(|byte| data_at(state, bytes).contains(&byte))))
}

/// `data.hex(sep)`, with 0 for no separator.
pub extern "C" fn hex_text(bytes: i64, sep: i64) -> i64 {
    with(|state| {
        let sep = if sep == 0 { String::new() } else { text_at(state, sep) };
        let text = hex(&data_at(state, bytes), &sep);
        text_address(state, &text)
    })
}

/// `response.text()`: a new string, with what isn't text replaced.
pub extern "C" fn text(bytes: i64) -> i64 {
    with(|state| {
        let data = data_at(state, bytes);
        let made = String::from_utf8_lossy(&data).into_owned();
        text_address(state, &made)
    })
}

/// `data.decode()`: a new string, or 0 when the bytes aren't UTF-8 text.
pub extern "C" fn decode(bytes: i64) -> i64 {
    with(|state| match String::from_utf8(data_at(state, bytes)) {
        Ok(text) => text_address(state, &text),
        Err(_) => 0,
    })
}

/// What's wrong with bytes that aren't UTF-8 text.
pub extern "C" fn decode_problem(bytes: i64) -> i64 {
    super::made_with("fennec_bytes_decode_problem", |state| {
        let data = data_at(state, bytes);
        let message = std::str::from_utf8(&data)
            .err()
            .map(|error| format!("these bytes aren't UTF-8 text: {error}"))
            .unwrap_or_default();
        text_address(state, &message)
    })
}

/// `bytes.fromhex(text)`: a new block, or 0 when the text doesn't spell any.
pub extern "C" fn from_hex_text(text: i64) -> i64 {
    with(|state| match from_hex(&text_at(state, text)) {
        Ok(data) => make(state, &data),
        Err(_) => 0,
    })
}

/// What's wrong with text that doesn't spell bytes.
pub extern "C" fn from_hex_problem(text: i64) -> i64 {
    with(|state| {
        let held = text_at(state, text);
        let message = from_hex(&held).err().map(|at| hex_problem(&held, at)).unwrap_or_default();
        text_address(state, &message)
    })
}

/// `text.encode()`.
pub extern "C" fn encode(text: i64) -> i64 {
    super::made_with("fennec_str_encode", |state| {
        let held = text_at(state, text);
        make(state, held.as_bytes())
    })
}

/// A block from a `Value`, when it crosses back.
pub(super) fn write(state: &mut State, value: &Value) -> Option<i64> {
    let Value::Bytes(data) = value else { return None };
    Some(make(state, data))
}

#[cfg(test)]
mod tests {
    use super::{
        HEADER_WORDS, builder, builder_room, data_at, grow, join, make, new, slice, text_at, text_part,
    };
    use crate::embed::{Tables, dicts, failed, finish, heap, start, with};

    /// A block's room word says how many bytes its words after the header hold, and its length is
    /// within it, however the block was made: exactly the size of its bytes, a builder empty or
    /// grown, a slice and a join. Native code writes into a builder up to its room word without
    /// asking (#1941).
    #[test]
    fn every_bytes_block_says_its_room_and_a_builders_has_what_native_code_counts_on() {
        start(Tables::default(), crate::Inputs::default());
        let header = |block: i64| {
            with(|state| {
                let Some(words) = state.heap.block(block.cast_unsigned()) else { panic!("no block") };
                let (length, room) = (words[2], words[3]);
                assert_eq!(
                    room,
                    u64::try_from((words.len() - HEADER_WORDS) * 8).unwrap(),
                    "the room is its words'"
                );
                assert!(length <= room, "{length} bytes in {room}");
                (usize::try_from(length).unwrap(), usize::try_from(room).unwrap())
            })
        };
        let mut made = Vec::new();
        for length in [0_u8, 1, 7, 8, 9, 16, 17, 100] {
            let data: Vec<u8> = (0..length).collect();
            let exact = with(|state| make(state, &data));
            assert_eq!(
                header(exact),
                (data.len(), data.len().div_ceil(8) * 8),
                "{length} bytes made exactly"
            );
            let part = slice(exact, 1, i64::MIN);
            header(part);
            let both = join(exact, part);
            header(both);
            let fresh = new(i64::from(length));
            header(fresh);
            for wanted in [0, 1, 10, 50] {
                let copy = with(|state| make(state, &data));
                let grown = grow(copy, wanted);
                let (length, room) = header(grown);
                assert!(
                    room >= builder_room(length.max(usize::try_from(wanted).unwrap())),
                    "grown for {wanted}"
                );
                made.push(grown);
            }
            made.extend([exact, part, both, fresh]);
        }
        let empty = builder();
        let (length, room) = header(empty);
        assert_eq!((length, room), (0, builder_room(0)), "an empty builder");
        made.push(empty);
        for block in made {
            heap::free(block);
        }
        finish();
    }

    /// A builder's block for these bytes, with its room past them filled with junk, as nothing
    /// promises it's zero.
    fn built(data: &[u8]) -> i64 {
        let exact = with(|state| make(state, data));
        let built = grow(exact, 0);
        with(|state| {
            let Some(words) = state.heap.block_mut(built.cast_unsigned()) else { panic!("no builder") };
            let used = HEADER_WORDS + data.len().div_ceil(8);
            assert!((words.len() - HEADER_WORDS) * 8 > data.len(), "a builder has room past its bytes");
            for word in words.iter_mut().skip(used) {
                *word = u64::MAX;
            }
            let tail = data.len() % 8;
            if tail != 0
                && let Some(last) = words.get_mut(used - 1)
            {
                *last |= u64::MAX << (8 * tail);
            }
        });
        built
    }

    fn hashes(bytes: i64) -> (u64, u64) {
        with(|state| dicts::block_key_hashes(state, bytes.cast_unsigned()))
    }

    /// Equal bytes are equal, and hash alike, whether their block is a builder's, with room and
    /// junk after them, or exactly their size: empty, and either side of a word's end. Bytes that
    /// differ only by a zero on the end are not.
    #[test]
    fn equal_bytes_from_a_builder_and_an_exact_block_are_equal_and_hash_alike() {
        start(Tables::default(), crate::Inputs::default());
        for length in [0_u8, 7, 8, 9, 17] {
            let data: Vec<u8> = (1..=length).collect();
            let exact = with(|state| make(state, &data));
            let from_builder = built(&data);
            assert_eq!(heap::equal(exact, from_builder), 1, "{length} bytes");
            assert_eq!(heap::equal(from_builder, exact), 1, "{length} bytes");
            assert_eq!(hashes(exact), hashes(from_builder), "{length} bytes");
            let mut longer = data.clone();
            longer.push(0);
            let longer = with(|state| make(state, &longer));
            assert_eq!(heap::equal(longer, from_builder), 0, "{length} bytes and a zero");
            for block in [exact, from_builder, longer] {
                heap::free(block);
            }
        }
        finish();
    }
    /// A slice, a part read as text and a join never set the failure flag, whatever they're given:
    /// bounds below zero, past the end, the wrong way round, as far out as an `int` goes, bytes
    /// that aren't UTF-8, and an address that's no block. A slice is what Python's would be.
    #[test]
    fn a_slice_a_text_part_and_a_join_never_fail_whatever_theyre_given() {
        start(Tables::default(), crate::Inputs::default());
        let data: Vec<u8> = (0..19).map(|n| b'a' + n).collect();
        let whole = with(|state| make(state, &data));
        let length = i64::try_from(data.len()).unwrap_or(0);
        let bounds =
            [i64::MIN, i64::MIN + 1, -100, -19, -18, -8, -1, 0, 1, 7, 8, 9, 18, 19, 20, 100, i64::MAX];
        for start_at in bounds {
            for end_at in bounds {
                let part = slice(whole, start_at, end_at);
                assert!(!failed(), "a slice from {start_at} to {end_at} failed");
                // Python's rule: a missing bound is its end, one below zero counts from the back,
                // and each is held to the bytes.
                let place = |at: i64, missing: i64| {
                    let at = if at == i64::MIN {
                        missing
                    } else if at < 0 {
                        at.saturating_add(length)
                    } else {
                        at
                    };
                    usize::try_from(at.clamp(0, length)).unwrap_or(0)
                };
                let (from, to) = (place(start_at, 0), place(end_at, length));
                let wanted = data.get(from..to.max(from)).unwrap_or_default();
                assert_eq!(with(|state| data_at(state, part)), wanted, "from {start_at} to {end_at}");
                let text = text_part(whole, start_at, end_at);
                assert!(!failed() && text != 0, "a part as text from {start_at} to {end_at} failed");
                heap::free(part);
                heap::free(text);
            }
        }
        // Bytes that aren't UTF-8 read as the empty text, and that's an answer, not a failure.
        let broken = with(|state| make(state, &[0xff, 0xfe, b'a']));
        let text = text_part(broken, 0, 3);
        assert!(!failed());
        assert_eq!(with(|state| text_at(state, text)), "");
        // A join of anything, the empty bytes and an address that's no block among them.
        let empty = slice(whole, 5, 5);
        for (a, b, wanted) in [
            (whole, empty, data.len()),
            (empty, empty, 0),
            (whole, whole, data.len() * 2),
            (0, whole, data.len()),
            (whole, 12_345, data.len()),
        ] {
            let both = join(a, b);
            assert!(!failed(), "a join failed");
            assert_eq!(with(|state| data_at(state, both)).len(), wanted);
            heap::free(both);
        }
        // An address that's no block is no bytes, to a slice and to a part as text too.
        for nothing in [0, 12_345] {
            let (part, text) = (slice(nothing, 0, 4), text_part(nothing, 0, 4));
            assert!(!failed());
            assert_eq!(with(|state| data_at(state, part)), Vec::<u8>::new());
            heap::free(part);
            heap::free(text);
        }
        for block in [whole, broken, text, empty] {
            heap::free(block);
        }
        finish();
    }

    /// Eight threads asking for the same embedded bytes at once: one is given the block to fill,
    /// and the rest wait until it's filled and get that block. Every thread's runtime finds it
    /// after, reads it and keeps it out of its own counts, and it's never freed (#2429).
    #[test]
    fn embedded_bytes_are_made_once_and_every_thread_waits_for_them_filled() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        const GLOBAL: i64 = 0x5EED_2429;
        let (filled, firsts, ready) =
            (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)), Arc::new(Barrier::new(8)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (filled, firsts, ready) = (Arc::clone(&filled), Arc::clone(&firsts), Arc::clone(&ready));
                std::thread::spawn(move || {
                    start(Tables::default(), crate::Inputs::default());
                    ready.wait();
                    let given = super::embedded(GLOBAL, 20);
                    let block = given & !1;
                    if given & 1 == 1 {
                        firsts.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        filled.store(true, Ordering::SeqCst);
                        super::embedded_filled(GLOBAL);
                    } else {
                        assert!(filled.load(Ordering::SeqCst), "given before it was filled");
                    }
                    let (length, never_freed) = with(|state| {
                        let words = state.heap.block(block.cast_unsigned()).expect("found on every thread");
                        (words[2], heap::is_never_freed(words))
                    });
                    assert_eq!((length, never_freed), (20, true));
                    let part = slice(block, 2, 5);
                    assert_eq!(with(|state| data_at(state, part)), vec![0; 3]);
                    heap::free(part);
                    // In no thread's table, so no thread's count of its blocks.
                    assert!(with(|state| state.heap.block_mut(block.cast_unsigned()).is_none()));
                    finish();
                    block
                })
            })
            .collect();
        let blocks: Vec<i64> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
        assert_eq!(firsts.load(Ordering::SeqCst), 1, "one thread fills it in");
        assert!(blocks.iter().all(|block| *block == blocks[0]), "one block: {blocks:?}");
        assert_eq!(super::embedded(GLOBAL, 20), blocks[0], "asked again, it's the one filled");
    }
}
