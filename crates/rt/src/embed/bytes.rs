//! `bytes` for native code. A block holds its owner count, its shape, its length in bytes, then
//! the bytes, packed into words. They don't change once it's filled in. Native code reads the
//! length and the bytes itself, and everything that makes a new block happens here.

use fennec_syntax::{Diagnostic, Span};

use super::heap::{IMMORTAL, block_text, text_address};
use super::{State, fail_with, with};
use crate::collections::{from_hex, hex, hex_problem};
use crate::value::Value;

/// Words before the bytes.
const HEADER_WORDS: usize = 3;
/// Where the bytes start in a block, for native code.
pub const DATA_OFFSET: u32 = 24;

/// What the length of a block with this many words of bytes needs.
fn words_for(length: usize) -> usize {
    HEADER_WORDS + length.div_ceil(8)
}

/// A new block with room for this many bytes, all zero.
fn with_room(state: &mut State, length: usize) -> Box<[u64]> {
    let mut block = state.heap.fresh(words_for(length));
    let header = [1, super::heap::BYTES_SHAPE, u64::try_from(length).unwrap_or(0)];
    if let Some(start) = block.get_mut(..HEADER_WORDS) {
        start.copy_from_slice(&header);
    }
    block.iter_mut().skip(HEADER_WORDS).for_each(|word| *word = 0);
    block
}

/// A new block holding these bytes.
pub(super) fn make(state: &mut State, data: &[u8]) -> i64 {
    let mut block = with_room(state, data.len());
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
/// Native code works it out from the length alone, so a builder needs no word for it. Only std's
/// own builder intrinsics write into one, and it's copied to its exact size before anything else
/// sees it. Equality and hashing go by the length anyway (`words_of`), so one that isn't still
/// compares and hashes as the same bytes would.
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
        let block = state.heap.block(bytes.cast_unsigned()).unwrap_or_default();
        let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let kept: Vec<u64> =
            block.get(HEADER_WORDS..HEADER_WORDS + length.div_ceil(8)).unwrap_or_default().to_vec();
        let room = builder_room(usize::try_from(wanted).unwrap_or(0).max(length));
        let mut grown = with_room(state, room);
        if let Some(header) = grown.get_mut(2) {
            *header = u64::try_from(length).unwrap_or(0);
        }
        for (word, held) in grown.iter_mut().skip(HEADER_WORDS).zip(kept) {
            *word = held;
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

/// `fennec_bytes_constant(length)`: a block for `embed`'s bytes, with room for `length`, which
/// lives for the whole run on the thread that asks: native code keeps one per thread and never
/// lets it go, so no task's end may take it away.
pub extern "C" fn constant(length: i64) -> i64 {
    with(|state| {
        let mut block = with_room(state, usize::try_from(length).unwrap_or(0));
        if let Some(count) = block.first_mut() {
            *count = IMMORTAL;
        }
        let address = i64::try_from(block.as_ptr().addr()).unwrap_or(0);
        state.heap.adopt(&mut vec![block]);
        address
    })
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
            let header = [1, super::heap::BYTES_SHAPE, u64::try_from(length).unwrap_or(0)];
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
            let mut made = with_room(state, size);
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
    use super::{HEADER_WORDS, grow, make};
    use crate::embed::{Tables, dicts, finish, heap, start, with};

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
}
