//! `bytes` for native code. A block holds its owner count, its shape, its length in bytes, then
//! the bytes, packed into words. They don't change once it's filled in. Native code reads the
//! length and the bytes itself, and everything that makes a new block happens here.

use fennec_syntax::{Diagnostic, Span};

use super::heap::{block_text, text_address};
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

/// A new block of this length for native code to copy bytes into.
pub extern "C" fn new(length: i64) -> i64 {
    with(|state| {
        let block = with_room(state, usize::try_from(length).unwrap_or(0));
        state.heap.insert(block)
    })
}

/// `bytes(items)`: each item has to be an int from 0 to 255.
pub extern "C" fn of_list(list: i64, start: i64, end: i64) -> i64 {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    let items: Vec<u64> = with(|state| {
        state.heap.block(list.cast_unsigned()).map(|block| super::lists::items(block, 1).to_vec())
    })
    .unwrap_or_default();
    let mut data = Vec::with_capacity(items.len());
    for item in items {
        let item = item.cast_signed();
        let Ok(byte) = u8::try_from(item) else {
            let message = format!("a byte is an int from 0 to 255, not {item}");
            fail_with(Diagnostic::new(message, span));
            return 0;
        };
        data.push(byte);
    }
    with(|state| make(state, &data))
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
        // Only the part is copied out, so taking a small part of a long run costs only the part.
        let part = part_of(block, from, to.max(from));
        make(state, &part)
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
    with(|state| {
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
    with(|state| {
        let held = text_at(state, text);
        make(state, held.as_bytes())
    })
}

/// A block from an interpreter value, when it crosses back.
pub(super) fn write(state: &mut State, value: &Value) -> Option<i64> {
    let Value::Bytes(data) = value else { return None };
    Some(make(state, data))
}
