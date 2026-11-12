//! Strings for native code. A string is a block in the heap (see `heap`): its owner count,
//! its shape, its length in bytes and in characters, how many bytes it has room for, then
//! its UTF-8 bytes. Copies share a block. The one change in place is `+=` on a block with
//! one owner, which may have room left after its bytes. Native code reads lengths and ASCII characters itself; everything
//! that makes a new string, or needs to walk UTF-8, happens here.

use fennec_syntax::{Diagnostic, Span};

use super::heap::{
    TEXT_HEADER_WORDS, block_text, mark_owner, release, retain, text_address, text_from_bytes,
};
use super::{State, attempt, span, with};
use crate::packed::Element;
use crate::value::Value;

/// A string's text, when the address is a string's block.
fn text(state: &State, address: i64) -> Option<String> {
    let block = state.heap.block(u64::try_from(address).ok()?)?;
    super::heap::is_text(state, block).then(|| block_text(block))
}

fn lost(start: i64, end: i64) -> Diagnostic {
    Diagnostic::new("the compiled code lost a string", span(start, end))
        .with_help("this is a bug in Fennec; please report it")
}

/// A string block owned once with a temporary's text. Fails when it isn't a string.
pub extern "C" fn from_value(value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| match state.temp(value) {
        Value::Str(text) => Ok(text_address(state, &text)),
        other => Err(Diagnostic::new(
            format!("expected a str here, but this is a {}", other.type_name()),
            span(start, end),
        )),
    })
}

/// A string's block, or no words when the address isn't a string's.
fn text_block(state: &State, address: i64) -> &[u64] {
    u64::try_from(address)
        .ok()
        .and_then(|address| state.heap.block(address))
        .filter(|block| super::heap::is_text(state, block))
        .unwrap_or_default()
}

/// `a + b`. Both are UTF-8 with their characters already counted, so their words go one after the
/// other into a new block and its lengths are theirs added up: nothing is checked or counted again a
/// character at a time.
pub extern "C" fn concat(a: i64, b: i64) -> i64 {
    super::made_with("fennec_str_concat", |state| {
        let (first, first_chars) = lengths(text_block(state, a));
        let (second, second_chars) = lengths(text_block(state, b));
        let bytes = first + second;
        if bytes == 1 {
            // A string of one ASCII character is the one block every such string shares.
            let one = if first == 1 { a } else { b };
            let byte = text_block(state, one).get(TEXT_HEADER_WORDS).map(|word| word & 0xff);
            if let Some(address) =
                byte.and_then(|byte| usize::try_from(byte).ok()).and_then(|byte| state.tables.ascii.get(byte))
            {
                return i64::try_from(*address).unwrap_or(0);
            }
        }
        let words = TEXT_HEADER_WORDS + bytes.div_ceil(8).max(1);
        // Each word with bytes in it is written below, and only an empty string's one word is left
        // to be zero.
        let mut joined = state.heap.fresh_written(words, TEXT_HEADER_WORDS + bytes.div_ceil(8));
        let number = |n: usize| u64::try_from(n).unwrap_or(0);
        let room = number((words - TEXT_HEADER_WORDS) * 8);
        let header = [1, state.tables.str_shape, number(bytes), first_chars + second_chars, room, 0];
        if let Some(start) = joined.get_mut(..TEXT_HEADER_WORDS) {
            start.copy_from_slice(&header);
        }
        add_text(&mut joined, 0, text_block(state, a));
        add_text(&mut joined, first, text_block(state, b));
        state.heap.insert(joined)
    })
}

/// An empty string owned once, with room for at least `bytes` bytes, for code generators
/// that know a string's length before making it, like an f-string's. They copy its bytes in
/// themselves and then set its lengths in bytes and characters.
pub extern "C" fn with_room(bytes: i64) -> i64 {
    super::made_with("fennec_str_with_room", |state| {
        let words = usize::try_from(bytes).unwrap_or(0).div_ceil(8).max(1);
        let mut block = state.heap.fresh(TEXT_HEADER_WORDS + words);
        if let Some([count, shape, _, _, room, _cursor]) = block.get_mut(..TEXT_HEADER_WORDS) {
            *count = 1;
            *shape = state.tables.str_shape;
            *room = u64::try_from(words * 8).unwrap_or(0);
        }
        state.heap.insert(block)
    })
}

/// `text += more`: takes over the owner `text` had and gives back the joined string's.
/// When `text` has no other owner and its block has room, the bytes go on the end in
/// place. Otherwise they're joined in a new block with room to double, so building a
/// string a piece at a time takes time in proportion to its length.
pub extern "C" fn append(text: i64, more: i64) -> i64 {
    super::made_with("fennec_str_append", |state| grow(state, text, more).unwrap_or(text))
}

fn grow(state: &mut State, text: i64, more: i64) -> Option<i64> {
    // Copied first: `text += text` reads the block it writes.
    let mut copied = std::mem::take(&mut state.heap.scratch);
    let lengths = copy_text(state, more, &mut copied);
    let grown = lengths.and_then(|(extra, extra_chars)| grow_by(state, text, &copied, extra, extra_chars));
    state.heap.scratch = copied;
    grown
}

/// Copies a string's words into `words`, giving back its lengths in bytes and characters.
fn copy_text(state: &State, address: i64, words: &mut Vec<u64>) -> Option<(usize, u64)> {
    let block = state.heap.block(u64::try_from(address).ok()?)?;
    if !super::heap::is_text(state, block) {
        return None;
    }
    let (length, chars) = lengths(block);
    words.clear();
    words.extend_from_slice(block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + length.div_ceil(8))?);
    Some((length, chars))
}

/// `grow` with the bytes added already copied out of their block.
fn grow_by(state: &mut State, text: i64, added: &[u64], extra: usize, extra_chars: u64) -> Option<i64> {
    let at = u64::try_from(text).ok()?;
    let block = state.heap.block(at)?;
    let (length, chars) = (usize::try_from(*block.get(2)?).ok()?, *block.get(3)?);
    let total = length + extra;
    let needed = TEXT_HEADER_WORDS + total.div_ceil(8).max(1);
    let fits = state.heap.is_alone(at, block) && block.len() >= needed;
    if fits {
        let block = state.heap.block_mut(at)?;
        put_words(block, length, added, extra);
        set_lengths(block, total, chars + extra_chars);
        return Some(text);
    }
    let room = 2 * total.div_ceil(8).max(1);
    let mut grown = state.heap.fresh(TEXT_HEADER_WORDS + room);
    let used = TEXT_HEADER_WORDS + length.div_ceil(8);
    for (to, from) in grown.iter_mut().zip(state.heap.block(at)?.get(..used)?) {
        *to = *from;
    }
    // What it was copied from may be another thread's, as under a read lock.
    mark_owner(&mut grown, state.heap.owner_bits());
    put_words(&mut grown, length, added, extra);
    set_lengths(&mut grown, total, chars + extra_chars);
    if let Some([count, _, _, _, bytes, _cursor]) = grown.get_mut(..TEXT_HEADER_WORDS) {
        *count = 1;
        *bytes = u64::try_from(room * 8).unwrap_or(0);
    }
    let joined = state.heap.insert(grown);
    release(state, at);
    Some(joined)
}

/// Writes `length` bytes, packed in `words`, into a string's block, starting `from` bytes
/// into its text, a word at a time. What follows them in the block's last word they touch is
/// zero, as native code compares strings' words whole.
fn put_words(block: &mut [u64], from: usize, words: &[u64], length: usize) {
    let Some(to) = block.get_mut(TEXT_HEADER_WORDS..) else { return };
    let tail = u32::try_from(length % 8 * 8).unwrap_or(0);
    let count = length.div_ceil(8);
    if from.is_multiple_of(8)
        && let (Some(to), Some(words)) = (to.get_mut(from / 8..from / 8 + count), words.get(..count))
    {
        // Starting on a word, the words are copied whole and only the last one is cut.
        to.copy_from_slice(words);
        if tail > 0
            && let Some(last) = to.last_mut()
        {
            *last &= u64::MAX >> (64 - tail);
        }
        return;
    }
    let shift = u32::try_from(from % 8 * 8).unwrap_or(0);
    let kept = if shift == 0 { 0 } else { u64::MAX >> (64 - shift) };
    let last = length / 8;
    for (n, word) in words.iter().take(count).enumerate() {
        let word = if n == last && tail > 0 { word & (u64::MAX >> (64 - tail)) } else { *word };
        let at = from / 8 + n;
        if let Some(slot) = to.get_mut(at) {
            *slot = (*slot & kept) | word << shift;
        }
        if shift > 0
            && let Some(slot) = to.get_mut(at + 1)
        {
            *slot = word >> (64 - shift);
        }
    }
}

fn set_lengths(block: &mut [u64], length: usize, chars: u64) {
    if let Some([bytes, count]) = block.get_mut(2..4) {
        *bytes = u64::try_from(length).unwrap_or(0);
        *count = chars;
    }
}

/// The character starting `at` bytes into a string. Each character has one block, which
/// lives for the whole run, as a literal's does.
pub extern "C" fn char_at(text: i64, at: i64) -> i64 {
    super::made_with("fennec_str_char_at", |state| {
        // A character is at most four bytes, so only the words holding those are read.
        let at = usize::try_from(at).unwrap_or(0);
        let block = u64::try_from(text).ok().and_then(|text| state.heap.block(text)).unwrap_or_default();
        let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let words = block.get(TEXT_HEADER_WORDS + at / 8..).unwrap_or_default();
        let near: Vec<u8> = words.iter().take(2).flat_map(|word| word.to_le_bytes()).collect();
        let wanted = length.saturating_sub(at).min(4);
        let start = near.get(at % 8..at % 8 + wanted).unwrap_or_default();
        if let Some(address) = start.first().filter(|byte| byte.is_ascii()).and_then(|byte| {
            state.tables.ascii.get(usize::from(*byte)).and_then(|at| i64::try_from(*at).ok())
        }) {
            return address;
        }
        let valid = match std::str::from_utf8(start) {
            Ok(text) => text,
            Err(err) => {
                start.get(..err.valid_up_to()).and_then(|good| std::str::from_utf8(good).ok()).unwrap_or("")
            }
        };
        let one = valid.chars().next().map(String::from).unwrap_or_default();
        let address = super::heap::literal(&mut state.tables, &one);
        let mut made = std::mem::take(&mut state.tables.statics);
        state.heap.adopt(&mut made);
        address
    })
}

/// `text[at]`, a string of one character, counting characters.
pub extern "C" fn index(address: i64, at: i64, start: i64, end: i64) -> i64 {
    let byte = attempt(|state: &mut State| byte_of_char(state, address, at, (start, end)).map(Some));
    byte.map_or(0, |byte| char_at(address, byte))
}

/// `text[at]` as a header, for native code holding strings as headers: no block is made for the
/// character, as one of four bytes or fewer is always inline, and the block's found only once.
pub extern "C" fn index_text(address: i64, at: i64, start: i64, end: i64) -> super::text::Text {
    attempt(|state: &mut State| {
        let found = u64::try_from(address).ok().and_then(|address| state.heap.block(address));
        let block =
            found.filter(|block| super::heap::is_text(state, block)).ok_or_else(|| lost(start, end))?;
        let (length, chars) = lengths(block);
        let count = i64::try_from(chars).map_err(|_| lost(start, end))?;
        let wanted = if at < 0 { at + count } else { at };
        let Ok(wanted) = usize::try_from(wanted).map(|wanted| wanted.min(usize::MAX - 1)) else {
            return out_of_range(state, address, at, (start, end)).map(|()| super::text::Text::default());
        };
        if wanted >= usize::try_from(chars).unwrap_or(0) {
            return out_of_range(state, address, at, (start, end)).map(|()| super::text::Text::default());
        }
        let words = block.get(TEXT_HEADER_WORDS..).unwrap_or_default();
        let known = state.heap.cursor();
        let kept =
            (known.address == usize::try_from(address).unwrap_or(0)).then_some((known.chars, known.bytes));
        let place = if length == usize::try_from(chars).unwrap_or(0) {
            wanted
        } else {
            walk_to_char(words, (length, usize::try_from(chars).unwrap_or(0)), kept, wanted)
        };
        let byte = |at: usize| words.get(at / 8).and_then(|word| word.to_le_bytes().get(at % 8).copied());
        let lead = byte(place).ok_or_else(|| lost(start, end))?;
        let width: u8 = match lead {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        let mut first = 0_u64;
        for offset in 0..usize::from(width) {
            first |= u64::from(byte(place + offset).unwrap_or(0)) << (offset * 8);
        }
        if length != usize::try_from(chars).unwrap_or(0) {
            let address = usize::try_from(address).unwrap_or(0);
            state.heap.set_cursor(super::heap::Cursor { address, chars: wanted, bytes: place });
        }
        Ok(super::text::Text { words: super::text::inline_word(first, width) })
    })
}

/// The failure for an index past either end of a string, with the language's message.
fn out_of_range(state: &State, address: i64, at: i64, (start, end): (i64, i64)) -> Result<(), Diagnostic> {
    let whole = text(state, address).ok_or_else(|| lost(start, end))?;
    crate::ops::index(&Value::str(&whole), &Value::Int(at), span(start, end))?;
    Err(lost(start, end))
}

/// Where the `at`th character starts, in bytes, reading none of the text for ASCII. An index
/// past either end fails.
fn byte_of_char(
    state: &mut State,
    address: i64,
    at: i64,
    (start, end): (i64, i64),
) -> Result<i64, Diagnostic> {
    let block = u64::try_from(address).ok().and_then(|address| state.heap.block(address));
    let block = block.filter(|block| super::heap::is_text(state, block)).ok_or_else(|| lost(start, end))?;
    let header = |at: usize| block.get(at).and_then(|n| i64::try_from(*n).ok()).unwrap_or(0);
    let (length, chars) = (header(2), header(3));
    let wanted = if at < 0 { at + chars } else { at };
    if !(0..chars).contains(&wanted) {
        let whole = text(state, address).ok_or_else(|| lost(start, end))?;
        crate::ops::index(&Value::str(&whole), &Value::Int(at), span(start, end))?;
        return Err(lost(start, end));
    }
    if length == chars {
        return Ok(wanted);
    }
    let wanted = usize::try_from(wanted).map_err(|_| lost(start, end))?;
    let place = place_of_char(state, address, wanted).ok_or_else(|| lost(start, end))?;
    i64::try_from(place).map_err(|_| lost(start, end))
}

/// Where the `wanted`th character of a string that isn't ASCII starts, in bytes, walking from
/// whichever is nearest of its start, its end and the last place read, and keeping the place
/// found. So reading a string in order takes a step for each character.
fn place_of_char(state: &mut State, address: i64, wanted: usize) -> Option<usize> {
    let at = usize::try_from(address).ok()?;
    let known = state.heap.cursor();
    let block = state.heap.block(u64::try_from(address).ok()?)?;
    let (length, chars) = lengths(block);
    let chars = usize::try_from(chars).ok()?;
    let words = block.get(TEXT_HEADER_WORDS..)?;
    let kept = (known.address == at).then_some((known.chars, known.bytes));
    let place = walk_to_char(words, (length, chars), kept, wanted);
    state.heap.set_cursor(super::heap::Cursor { address: at, chars: wanted, bytes: place });
    Some(place)
}

/// Where a character starts in a string's words, from the nearest of its start, its end and a
/// place already known.
fn walk_to_char(
    words: &[u64],
    (length, chars): (usize, usize),
    kept: Option<(usize, usize)>,
    wanted: usize,
) -> usize {
    let byte =
        |place: usize| words.get(place / 8).and_then(|word| word.to_le_bytes().get(place % 8).copied());
    // Every byte but a continuation byte starts a character.
    let continues = |place: usize| byte(place).is_some_and(|byte| byte & 0xC0 == 0x80);
    let mut here = (0_usize, 0_usize);
    for known in [Some((chars, length)), kept].into_iter().flatten() {
        if known.0.abs_diff(wanted) < here.0.abs_diff(wanted) {
            here = known;
        }
    }
    let (mut char_at, mut place) = here;
    while char_at < wanted {
        place += 1;
        while place < length && continues(place) {
            place += 1;
        }
        char_at += 1;
    }
    while char_at > wanted {
        place = place.saturating_sub(1);
        while place > 0 && continues(place) {
            place -= 1;
        }
        char_at -= 1;
    }
    place
}

/// A new string with the characters `from..to` of one that isn't ASCII, Python's clamping and
/// all, copying only those bytes: what they came from is valid and needs no checking.
fn part_by_chars(state: &mut State, address: i64, from: i64, to: i64) -> Option<i64> {
    let source = u64::try_from(address).ok()?;
    let block = state.heap.block(source)?;
    if !super::heap::is_text(state, block) {
        return None;
    }
    let (length, chars) = lengths(block);
    let count = i64::try_from(chars).ok()?;
    let bound = |at: i64, missing: i64| {
        let at = if at == i64::MIN { missing } else { at };
        let at = if at < 0 { at + count } else { at };
        usize::try_from(at.clamp(0, count)).unwrap_or(0)
    };
    let (from, to) = (bound(from, 0), bound(to, count));
    if from >= to {
        return Some(text_from_bytes(state, &[], 0));
    }
    let first = place_of_char(state, address, from)?;
    let last = if to == usize::try_from(count).ok()? { length } else { place_of_char(state, address, to)? };
    ascii_part_of(state, source, (first, last), u64::try_from(to - from).ok()?)
}

/// A string block's bytes, and its length in characters.
fn bytes(state: &State, address: i64) -> Option<(Vec<u8>, usize)> {
    let block = state.heap.block(u64::try_from(address).ok()?)?;
    if !super::heap::is_text(state, block) {
        return None;
    }
    let length = usize::try_from(*block.get(2)?).ok()?;
    let chars = usize::try_from(*block.get(3)?).ok()?;
    let words = block.get(TEXT_HEADER_WORDS..)?;
    Some((words.iter().flat_map(|word| word.to_le_bytes()).take(length).collect(), chars))
}

/// A new string with the bytes `from..to` of an ASCII string's block, copying only those.
fn ascii_part(state: &mut State, address: i64, from: i64, to: i64) -> Option<i64> {
    let source = u64::try_from(address).ok()?;
    let block = state.heap.block(source)?;
    let (length, chars) = (*block.get(2)?, *block.get(3)?);
    if !super::heap::is_text(state, block) || length != chars {
        return None;
    }
    let length = i64::try_from(length).ok()?;
    // Python's clamping: negative positions count from the end, and both ends stay inside.
    let bound = |at: i64, missing: i64| {
        let at = if at == i64::MIN { missing } else { at };
        let at = if at < 0 { at + length } else { at };
        usize::try_from(at.clamp(0, length)).unwrap_or(0)
    };
    let (from, to) = (bound(from, 0), bound(to, length));
    let size = to.saturating_sub(from);
    if size == 1 {
        let word = block.get(TEXT_HEADER_WORDS + from / 8)?;
        let byte = word.to_le_bytes().get(from % 8).copied()?;
        return state.tables.ascii.get(usize::from(byte)).and_then(|at| i64::try_from(*at).ok());
    }
    let words = size.div_ceil(8).max(1);
    let mut made = state.heap.fresh(TEXT_HEADER_WORDS + words);
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    made.get_mut(..TEXT_HEADER_WORDS)?.copy_from_slice(&[
        1,
        state.tables.str_shape,
        number(size),
        number(size),
        number(words * 8),
        0,
    ]);
    let text = state.heap.block(source)?.get(TEXT_HEADER_WORDS..)?;
    // Each word of the part is made of the end of one word and the start of the next.
    let shift = u32::try_from(from % 8).unwrap_or(0) * 8;
    for (at, to_word) in made.get_mut(TEXT_HEADER_WORDS..)?.iter_mut().enumerate().take(size.div_ceil(8)) {
        let low = text.get(from / 8 + at).copied().unwrap_or(0) >> shift;
        let high =
            if shift == 0 { 0 } else { text.get(from / 8 + at + 1).copied().unwrap_or(0) << (64 - shift) };
        *to_word = low | high;
    }
    // What's past the end of the part reads as zero, as native code expects.
    if size % 8 != 0
        && let Some(last) = made.get_mut(TEXT_HEADER_WORDS + size / 8)
    {
        *last &= (1_u64 << (u32::try_from(size % 8).unwrap_or(0) * 8)) - 1;
    }
    Some(state.heap.insert(made))
}

/// `text[from:to]`, where a missing end is `i64::MIN`.
pub extern "C" fn slice(address: i64, from: i64, to: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        if let Some(part) = ascii_part(state, address, from, to) {
            return Ok(part);
        }
        part_by_chars(state, address, from, to).ok_or_else(|| lost(start, end))
    })
}

/// The part of a string between two places in its bytes, each where a character starts or at
/// its end, for `std.intrinsics`' `text_part`.
pub extern "C" fn part(address: i64, from: i64, to: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| part_by_bytes(state, address, from, to).ok_or_else(|| lost(start, end)))
}

fn part_by_bytes(state: &mut State, address: i64, from: i64, to: i64) -> Option<i64> {
    let source = u64::try_from(address).ok()?;
    let (from, to) = (usize::try_from(from).ok()?, usize::try_from(to).ok()?);
    let block = state.heap.block(source)?;
    let (length, chars) = lengths(block);
    let all_ascii = usize::try_from(chars).ok() == Some(length);
    if !super::heap::is_text(state, block) || from > to || to > length {
        return None;
    }
    let chars = if all_ascii {
        to - from
    } else {
        let words = block.get(TEXT_HEADER_WORDS..)?;
        (from..to)
            .filter(|at| {
                let byte = words.get(at / 8).and_then(|word| word.to_le_bytes().get(at % 8).copied());
                byte.is_some_and(|byte| byte & 0xC0 != 0x80)
            })
            .count()
    };
    ascii_part_of(state, source, (from, to), u64::try_from(chars).ok()?)
}

/// How two strings order: -1, 0 or 1, comparing bytes.
pub extern "C" fn compare(a: i64, b: i64) -> i64 {
    with(|state| match bytes(state, a).unwrap_or_default().0.cmp(&bytes(state, b).unwrap_or_default().0) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

/// A string's bytes where they are: the words holding them, packed eight to a word, and how many
/// there are. Nothing, for an address that isn't a string's block.
pub(super) fn bytes_in_block(state: &State, address: i64) -> (&[u64], usize) {
    u64::try_from(address)
        .ok()
        .and_then(|address| state.heap.block(address))
        .filter(|block| super::heap::is_text(state, block))
        .map_or((&[], 0), |block| {
            let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
            (block.get(TEXT_HEADER_WORDS..).unwrap_or_default(), length)
        })
}

/// The longest `part` looked for in place: it fits one word.
const WORD_BYTES: usize = 8;

/// A one in each byte, and each byte's top bit, for finding a byte a word at a time.
const LOW_BITS: u64 = u64::from_ne_bytes([1; 8]);
const HIGH_BITS: u64 = LOW_BITS << 7;

/// Whether `part`, of at most a word's bytes, is in `whole`, with nothing copied: a word at a
/// time to where `part`'s first byte is, then whether the rest follows. It takes one look per
/// place, so it's linear whatever the strings hold.
fn word_in_place((whole, length): (&[u64], usize), part: u64, size: usize) -> bool {
    let mask = if size >= WORD_BYTES { u64::MAX } else { (1 << (size * 8)) - 1 };
    let (part, first) = (part & mask, part & 0xff);
    for (at, word) in whole.iter().enumerate() {
        let word = u64::from_le(*word);
        let others = word ^ (first * LOW_BITS);
        // Each byte that's `first` has its top bit set here, along with perhaps some byte above
        // one that is, which the look below turns down.
        let mut places = others.wrapping_sub(LOW_BITS) & !others & HIGH_BITS;
        while places != 0 {
            let byte = usize::try_from(places.trailing_zeros() / 8).unwrap_or(0);
            if at * 8 + byte + size > length {
                return false;
            }
            let next = whole.get(at + 1).map_or(0, |next| u64::from_le(*next));
            let shift = byte * 8;
            let found = if shift == 0 { word } else { (word >> shift) | (next << (64 - shift)) };
            if found & mask == part {
                return true;
            }
            places &= places - 1;
        }
    }
    false
}

/// Whether `part`'s bytes are in `whole`'s, each given where it is. Bytes are the same question as
/// characters, in UTF-8, where no character's bytes start partway through another's.
pub(super) fn bytes_contain(whole: (&[u64], usize), (part, size): (&[u64], usize)) -> bool {
    if size == 0 {
        return true;
    }
    if size > whole.1 {
        return false;
    }
    if size <= WORD_BYTES {
        let first = part.first().map_or(0, |word| u64::from_le(*word));
        return word_in_place(whole, first, size);
    }
    // Longer ones are rare, and `str`'s search stays linear however the two repeat themselves.
    let text_of = |(words, length): (&[u64], usize)| {
        let bytes = words.iter().flat_map(|word| word.to_le_bytes()).take(length).collect();
        String::from_utf8(bytes).unwrap_or_default()
    };
    text_of(whole).contains(text_of((part, size)).as_str())
}

/// `part in text`.
pub extern "C" fn contains(whole: i64, part: i64) -> i8 {
    with(|state| i8::from(bytes_contain(bytes_in_block(state, whole), bytes_in_block(state, part))))
}

/// Prints a string as `stdout.print` shows it.
pub extern "C" fn print(address: i64) {
    attempt(|state: &mut State| {
        let printed = text(state, address).unwrap_or_default();
        super::io::write(state, &printed)
    });
}

/// A new string with an int's text.
pub extern "C" fn int_text(n: i64) -> i64 {
    // Written on the stack, so the only thing made is the string's block. Its digits and sign are
    // ASCII, so they're its text as they are, a character each, with nothing to check or count.
    let mut digits = [0_u8; 20];
    let written = super::text::decimal(n, &mut digits);
    super::made_with("fennec_int_text", |state| text_from_bytes(state, written, written.len()))
}

/// `reversed(text)`: a new string of its characters, last first.
pub extern "C" fn reversed(address: i64) -> i64 {
    super::made_with("fennec_str_reversed", |state| {
        let whole = text(state, address).unwrap_or_default();
        let turned: String = whole.chars().rev().collect();
        text_address(state, &turned)
    })
}

/// `text.codes()`: a new list of the code of each character.
pub extern "C" fn codes(address: i64, element: i64) -> i64 {
    super::made_with("fennec_str_codes", |state| {
        let whole = text(state, address).unwrap_or_default();
        let words: Vec<u64> = whole.chars().map(|c| u64::from(u32::from(c))).collect();
        super::lists::list_of_words(state, element, &words)
    })
}

/// `str.from_code(code)`: a new string of the character with a code, or 0 if none has it.
pub extern "C" fn from_code(code: i64) -> i64 {
    match u32::try_from(code).ok().and_then(char::from_u32) {
        Some(c) => with(|state| text_address(state, c.encode_utf8(&mut [0; 4]))),
        None => 0,
    }
}

/// What's wrong with a number that isn't a character's code.
pub extern "C" fn from_code_problem(code: i64) -> i64 {
    super::made_with("fennec_str_from_code_problem", |state| {
        text_address(state, &format!("{code} isn't the code of a character"))
    })
}

/// A new string with a string's text as it shows inside a collection, in quotes.
pub extern "C" fn str_repr(text: i64) -> i64 {
    with(|state| {
        let held =
            u64::try_from(text).ok().and_then(|at| state.heap.block(at)).map_or_else(String::new, block_text);
        text_address(state, &crate::format::repr(&Value::Str(held.into())))
    })
}

/// A new string with a float's text, as it prints.
pub extern "C" fn float_text(x: f64) -> i64 {
    super::made_with("fennec_float_text", |state| {
        text_address(state, &crate::format::display(&Value::Float(x)))
    })
}

/// A new string with the text of the value in a block, read from the block.
pub extern "C" fn shown_text(address: i64) -> i64 {
    let shown = super::heap::shown::shown(u64::try_from(address).unwrap_or(0));
    super::made_with("fennec_block_text", |state| text_address(state, &shown))
}

/// `str(x)` for a `u64`: a new string of its digits, unsigned.
pub extern "C" fn u64_text(n: i64) -> i64 {
    with(|state| text_address(state, &n.cast_unsigned().to_string()))
}

/// `str(x)` for an `f32`: a new string of the shortest text that reads back as it.
pub extern "C" fn f32_text(x: f64) -> i64 {
    with(|state| text_address(state, &crate::format::float32(x)))
}

/// A new string with a `u64`'s text in a format, like `{n:,}`.
pub extern "C" fn u64_spec_text(n: i64, spec: i64, start: i64, end: i64) -> i64 {
    spec_text_with(spec, (start, end), |spec| crate::format::with_spec_u64(n.cast_unsigned(), spec))
}

/// A new string with an `f32`'s text in a format, like `{x:.2f}`.
pub extern "C" fn f32_spec_text(x: f64, spec: i64, start: i64, end: i64) -> i64 {
    spec_text_with(spec, (start, end), |spec| crate::format::with_spec_f32(x, spec))
}

/// A new string with what `format` makes of the spec in the string block `spec`, failing as the
/// format does.
fn spec_text_with(
    spec: i64,
    (start, end): (i64, i64),
    format: impl FnOnce(&str) -> Result<String, String>,
) -> i64 {
    attempt(|state: &mut State| {
        let spec = text(state, spec).unwrap_or_default();
        let piece = format(&spec).map_err(|message| Diagnostic::new(message, span(start, end)))?;
        Ok(text_address(state, &piece))
    })
}

/// A new string with a float's text in a format, like `{x:.9f}`, with no temporary.
pub extern "C" fn float_spec_text(x: f64, spec: i64, start: i64, end: i64) -> i64 {
    number_spec_text(&Value::Float(x), spec, (start, end))
}

/// A new string with an int's text in a format, with no temporary.
pub extern "C" fn int_spec_text(n: i64, spec: i64, start: i64, end: i64) -> i64 {
    number_spec_text(&Value::Int(n), spec, (start, end))
}

fn number_spec_text(value: &Value, spec: i64, (start, end): (i64, i64)) -> i64 {
    attempt(|state: &mut State| {
        let spec = text(state, spec).unwrap_or_default();
        let piece = crate::format::with_spec(value, &spec)
            .map_err(|message| Diagnostic::new(message, span(start, end)))?;
        Ok(text_address(state, &piece))
    })
}

/// `text.code()`. Fails for anything but one character.
pub extern "C" fn code(address: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let whole = text(state, address).unwrap_or_default();
        let mut chars = whole.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Ok(i64::from(u32::from(c)));
        }
        let count = whole.chars().count();
        Err(Diagnostic::new(format!("`code` needs one character, but this has {count}"), span(start, end))
            .with_help("use `codes()` for the code of every character"))
    })
}

/// `strip` (0), `lstrip` (1) or `rstrip` (2). A string with nothing to take off is shared,
/// so most lines cost nothing.
pub extern "C" fn strip(address: i64, which: i64) -> i64 {
    super::made_with("fennec_str_strip", |state| {
        let Some(at) = u64::try_from(address).ok() else { return 0 };
        let ends = state.heap.block(at).and_then(|block| Some((ascii_ends(block, which)?, lengths(block))));
        if let Some(((from, to), (length, chars))) = ends {
            if from == 0 && to == length {
                retain(state, at);
                return address;
            }
            // Only ASCII came off, a character a byte.
            let left = chars.saturating_sub(u64::try_from(length - (to - from)).unwrap_or(0));
            return ascii_part_of(state, at, (from, to), left).unwrap_or(0);
        }
        // Short strings are read without allocating.
        let mut short = [0_u8; 120];
        let long;
        let length = state.heap.block(at).and_then(|block| block.get(2)).copied().unwrap_or(0);
        let whole: &[u8] = if let Some(room) = usize::try_from(length).ok().and_then(|n| short.get_mut(..n)) {
            let words =
                state.heap.block(at).and_then(|block| block.get(TEXT_HEADER_WORDS..)).unwrap_or_default();
            for (chunk, word) in room.chunks_mut(8).zip(words) {
                let bytes = word.to_le_bytes();
                chunk.copy_from_slice(bytes.get(..chunk.len()).unwrap_or_default());
            }
            room
        } else {
            long = bytes(state, address).unwrap_or_default().0;
            &long
        };
        let whole = std::str::from_utf8(whole).unwrap_or_default();
        let part = match which {
            1 => whole.trim_start(),
            2 => whole.trim_end(),
            _ => whole.trim(),
        };
        // Nothing but ASCII whitespace comes off most strings.
        let kept = if whole.is_ascii() { part.len() } else { part.chars().count() };
        text_from_bytes(state, part.as_bytes(), kept)
    })
}

/// Whether a byte is a whitespace character, as `char::is_whitespace` says of ASCII.
fn is_ascii_space(byte: u8) -> bool {
    byte == b' ' || (0x09..=0x0d).contains(&byte)
}

/// Where `strip` (0), `lstrip` (1) or `rstrip` (2) leaves a string's text, in bytes, when what
/// comes off is ASCII whitespace. `None` when a character that isn't ASCII is at an end it
/// looks at, which may be whitespace of another kind.
fn ascii_ends(block: &[u64], which: i64) -> Option<(usize, usize)> {
    let length = usize::try_from(*block.get(2)?).ok()?;
    let words = block.get(TEXT_HEADER_WORDS..)?;
    let byte = |at: usize| words.get(at / 8).and_then(|word| word.to_le_bytes().get(at % 8).copied());
    let (mut start, mut end) = (0, length);
    while which != 2 && start < end {
        let first = byte(start).filter(u8::is_ascii)?;
        if !is_ascii_space(first) {
            break;
        }
        start += 1;
    }
    while which != 1 && end > start {
        let last = byte(end - 1).filter(u8::is_ascii)?;
        if !is_ascii_space(last) {
            break;
        }
        end -= 1;
    }
    Some((start, end))
}

/// A string owned once of the bytes `from..to` of a string, `chars` characters of them,
/// copied a word at a time.
pub(super) fn ascii_part_of(
    state: &mut State,
    address: u64,
    (from, to): (usize, usize),
    chars: u64,
) -> Option<i64> {
    let kept = to - from;
    if kept == 0 {
        return Some(text_from_bytes(state, &[], 0));
    }
    if kept == 1 {
        let byte = state.heap.block(address)?.get(TEXT_HEADER_WORDS + from / 8)?.to_le_bytes();
        let part = byte.get(from % 8..from % 8 + kept)?.to_vec();
        return Some(text_from_bytes(state, &part, kept));
    }
    let words = kept.div_ceil(8);
    let mut part = state.heap.fresh(TEXT_HEADER_WORDS + words);
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    let header = [1, state.tables.str_shape, number(kept), chars, number(words * 8), 0];
    part.get_mut(..TEXT_HEADER_WORDS)?.copy_from_slice(&header);
    let source = state.heap.block(address)?.get(TEXT_HEADER_WORDS + from / 8..)?;
    let shift = u32::try_from(from % 8 * 8).ok()?;
    for (at, word) in part.iter_mut().skip(TEXT_HEADER_WORDS).enumerate() {
        let low = source.get(at).map_or(0, |word| word >> shift);
        let high = if shift == 0 { 0 } else { source.get(at + 1).map_or(0, |word| word << (64 - shift)) };
        *word = low | high;
    }
    let tail = u32::try_from(kept % 8 * 8).ok()?;
    if tail > 0
        && let Some(last) = part.last_mut()
    {
        *last &= u64::MAX >> (64 - tail);
    }
    Some(state.heap.insert(part))
}

/// A string's length in bytes and characters, from its block.
fn lengths(block: &[u64]) -> (usize, u64) {
    let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
    (length, block.get(3).copied().unwrap_or(0))
}

/// Puts a string's text into `joined` from `at` bytes in, giving back its lengths.
fn add_text(joined: &mut [u64], at: usize, part: &[u64]) -> (usize, u64) {
    let (length, chars) = lengths(part);
    let words = part.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + length.div_ceil(8)).unwrap_or_default();
    put_words(joined, at, words, length);
    (length, chars)
}

/// `separator.join(list)` for a list whose items are strings' headers.
fn join_headers(state: &mut State, separator: i64, list: i64) -> Option<i64> {
    let block = state.heap.block(u64::try_from(list).ok()?)?;
    if super::lists::element_in(state, block) != Some(&Element::Text) {
        return None;
    }
    let parts = super::text::texts(state, super::lists::items(block, 3));
    let between = text(state, separator).unwrap_or_default();
    Some(text_address(state, &parts.join(&between)))
}

/// `separator.join(list)`, for a list of strings, written straight into the joined block.
/// Each string's block is looked up once.
pub extern "C" fn join(separator: i64, list: i64) -> i64 {
    if let Some(joined) = with(|state| join_headers(state, separator, list)) {
        return joined;
    }
    super::made_with("fennec_str_join", |state| {
        let block = |address: u64| state.heap.block(address).unwrap_or_default();
        let between = u64::try_from(separator).map_or(&[][..], block);
        let items = super::lists::items(u64::try_from(list).map_or(&[][..], block), 1);
        let parts: Vec<&[u64]> = items.iter().map(|item| block(*item)).collect();
        let bytes = parts.iter().map(|part| lengths(part).0).sum::<usize>()
            + lengths(between).0 * parts.len().saturating_sub(1);
        // Made here rather than taken from the heap, which the parts are borrowed from.
        let mut joined = vec![0; TEXT_HEADER_WORDS + bytes.div_ceil(8).max(1)].into_boxed_slice();
        let (mut at, mut chars) = (0, 0);
        for (n, part) in parts.iter().enumerate() {
            if n > 0 {
                let (length, count) = add_text(&mut joined, at, between);
                (at, chars) = (at + length, chars + count);
            }
            let (length, count) = add_text(&mut joined, at, part);
            (at, chars) = (at + length, chars + count);
        }
        let room = u64::try_from(joined.len() - TEXT_HEADER_WORDS).unwrap_or(0) * 8;
        let header = [1, state.tables.str_shape, 0, 0, room, 0];
        if let Some(start) = joined.get_mut(..TEXT_HEADER_WORDS) {
            start.copy_from_slice(&header);
        }
        set_lengths(&mut joined, at, chars);
        state.heap.insert(joined)
    })
}

/// One of the string methods, run on a string block with string blocks as its arguments.
fn method(state: &State, address: i64, name: &str, args: &[i64], at: Span) -> Result<Value, Diagnostic> {
    let lost = || Diagnostic::new("the native code lost a string", at);
    let whole = text(state, address).ok_or_else(lost)?;
    let args =
        args.iter().map(|arg| text(state, *arg).map(|text| Value::str(&text))).collect::<Option<Vec<_>>>();
    crate::collections::str_method(&whole, name, &args.ok_or_else(lost)?, at)
}

/// A string method that gives a string, as a block owned once.
fn text_method(address: i64, name: &str, args: &[i64]) -> i64 {
    super::made_with(name, |state| match method(state, address, name, args, Span::default()) {
        Ok(Value::Str(text)) => text_address(state, &text),
        _ => 0,
    })
}

/// `text.upper()`.
pub extern "C" fn upper(address: i64) -> i64 {
    text_method(address, "upper", &[])
}

/// `text.lower()`.
pub extern "C" fn lower(address: i64) -> i64 {
    text_method(address, "lower", &[])
}

/// `text.replace(old, new)`.
pub extern "C" fn replace(address: i64, old: i64, new: i64) -> i64 {
    text_method(address, "replace", &[old, new])
}

/// `text.find(part)`: which character `part` first starts at, or -1 when it isn't there.
pub extern "C" fn find(address: i64, part: i64) -> i64 {
    with(|state| match method(state, address, "find", &[part], Span::default()) {
        Ok(Value::Int(at)) => at,
        _ => -1,
    })
}

/// `text.split(separator)`, or `text.split()` when `separator` is 0: a list of str owned
/// once. An empty separator fails.
pub extern "C" fn split(address: i64, separator: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let separators: &[i64] = if separator == 0 { &[] } else { &[separator] };
        let parts = method(state, address, "split", separators, at)?;
        let element = super::text::string_element(state);
        super::lists::write_list(state, &element, &parts).ok_or_else(|| lost(start, end))
    })
}

#[cfg(test)]
mod place_tests {
    use std::rc::Rc;

    use super::super::text::inline_bytes;
    use super::super::{Tables, finish, heap, start, with};
    use super::{index_text, slice};
    use crate::packed::Element;

    fn begin() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
    }

    fn character(text: i64, at: i64) -> String {
        let bytes = inline_bytes(&index_text(text, at, 0, 0).words).unwrap_or_default();
        String::from_utf8(bytes).unwrap_or_default()
    }

    fn kept() -> (usize, usize, usize) {
        with(|state| {
            let cursor = state.heap.cursor();
            (cursor.address, cursor.chars, cursor.bytes)
        })
    }

    #[test]
    fn reading_keeps_the_place_it_got_to_and_finds_each_character_from_it() {
        begin();
        let wide = "a\u{e9}b\u{65e5}c\u{1f98a}d long enough to have a block of its own";
        let text = with(|state| heap::text_address(state, wide));
        let at = usize::try_from(text).unwrap();
        let all: Vec<char> = wide.chars().collect();
        for (number, expected) in all.iter().enumerate().rev() {
            assert_eq!(character(text, i64::try_from(number).unwrap()), expected.to_string());
        }
        for (number, expected) in wide.chars().enumerate() {
            assert_eq!(character(text, i64::try_from(number).unwrap()), expected.to_string());
            let place = wide.char_indices().nth(number).map(|(place, _)| place).unwrap();
            assert_eq!(kept(), (at, number, place));
        }
        assert_eq!(character(text, -1), wide.chars().last().unwrap().to_string());
        finish();
    }

    #[test]
    fn a_place_is_forgotten_only_when_its_own_string_goes() {
        begin();
        let wide = "\u{e9} a string long enough to have a block of its own";
        let text = with(|state| heap::text_address(state, wide));
        let other =
            with(|state| heap::text_address(state, "\u{e9} another one, also with a block of its own"));
        character(text, 5);
        let at = usize::try_from(text).unwrap();
        // Letting go of another string leaves it.
        heap::free_text(other);
        assert_eq!(kept().0, at);
        heap::free_text(text);
        assert_eq!(kept(), (0, 0, 0));
        finish();
    }

    #[test]
    fn a_slice_of_a_string_that_isnt_ascii_is_counted_in_characters() {
        begin();
        let wide = "a\u{e9}b\u{65e5}c\u{1f98a}d long enough to have a block of its own";
        let text = with(|state| heap::text_address(state, wide));
        let part = slice(text, 1, 6, 0, 0);
        with(|state| {
            assert_eq!(super::text(state, part).as_deref(), Some("\u{e9}b\u{65e5}c\u{1f98a}"));
        });
        let tail = slice(text, -3, i64::MIN, 0, 0);
        with(|state| assert_eq!(super::text(state, tail).as_deref(), Some("own")));
        let empty = slice(text, 5, 2, 0, 0);
        with(|state| assert_eq!(super::text(state, empty).as_deref(), Some("")));
        finish();
    }
}

#[cfg(test)]
mod concat_tests {
    use std::rc::Rc;

    use super::super::{Tables, aot, finish, heap, lists, start, with};
    use super::{append, concat, join};
    use crate::packed::Element;

    fn begin() {
        let tables = Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![Rc::new(Element::Str)],
            ..Tables::default()
        };
        start(tables, crate::Inputs::default());
        // Made as native code makes it, once the run has started.
        aot::ascii();
    }

    fn string(text: &str) -> i64 {
        with(|state| heap::text_address(state, text))
    }

    /// A string's text and its lengths in bytes and characters, as its block holds them.
    fn read(address: i64) -> (String, u64, u64) {
        with(|state| {
            let block = state.heap.block(address.cast_unsigned()).unwrap_or_default();
            let text = super::text(state, address).unwrap_or_default();
            (text, block.get(2).copied().unwrap_or(0), block.get(3).copied().unwrap_or(0))
        })
    }

    /// Lets go of one owner, as native code does, which leaves a block shared for the whole run.
    fn let_go(address: i64) {
        with(|state| heap::release(state, address.cast_unsigned()));
    }

    fn allocated() -> u64 {
        with(|state| state.heap.counts.allocated_bytes)
    }

    /// `a + b` puts their bytes one after the other whatever word each ends partway through, and its
    /// lengths are theirs added up, wide characters too.
    #[test]
    fn a_joined_string_holds_both_parts_and_their_lengths_added_up() {
        begin();
        let parts = [
            "",
            "a",
            "abc",
            "abcdefgh",
            "abcdefghi",
            "\u{e9}t\u{e9}",
            "\u{65e5}\u{672c}\u{1f98a} and then some more bytes",
            "seventeen bytes!!",
        ];
        for first in parts {
            for second in parts {
                let (a, b) = (string(first), string(second));
                let joined = concat(a, b);
                let whole = format!("{first}{second}");
                let (text, bytes, chars) = read(joined);
                assert_eq!(text, whole);
                assert_eq!(bytes, u64::try_from(whole.len()).unwrap());
                assert_eq!(chars, u64::try_from(whole.chars().count()).unwrap(), "{whole:?}");
                for address in [a, b, joined] {
                    let_go(address);
                }
            }
        }
        finish();
    }

    /// A string of one ASCII character made by `+` is the block every such string shares, as one
    /// made any other way is.
    #[test]
    fn one_ascii_character_joined_is_the_shared_one() {
        begin();
        let (empty, x) = (string(""), string("x"));
        assert_eq!(concat(empty, x), x);
        assert_eq!(concat(x, empty), x);
        assert_eq!(concat(0, x), x, "what isn't a string adds nothing");
        let_go(empty);
        finish();
    }

    /// Building a string a piece at a time with `+=`, or all at once with `join`, allocates in
    /// proportion to its length, where making each piece's string anew would take its square.
    #[test]
    fn a_string_built_by_appends_or_a_join_costs_in_proportion_to_its_length() {
        begin();
        let piece = string("twelve bytes");
        let appended = |pieces: usize| {
            let before = allocated();
            let mut text = string("");
            for _ in 0..pieces {
                text = append(text, piece);
            }
            assert_eq!(read(text).1, 12 * u64::try_from(pieces).unwrap());
            heap::free_text(text);
            allocated() - before
        };
        let joined = |pieces: usize| {
            let list = lists::new(0, i64::try_from(pieces).unwrap());
            with(|state| {
                let block = state.heap.block_mut(list.cast_unsigned()).unwrap();
                let items = block.get_mut(lists::LIST_HEADER_WORDS..).unwrap();
                items.iter_mut().take(pieces).for_each(|item| *item = piece.cast_unsigned());
            });
            let before = allocated();
            let comma = string(", ");
            let text = join(comma, list);
            assert_eq!(read(text).1, 14 * u64::try_from(pieces).unwrap() - 2);
            heap::free_text(text);
            heap::free_text(comma);
            allocated() - before
        };
        for pieces in [1_000, 3_000, 10_000] {
            // Room doubles as a string grows, so it has at most twice its length, and all it had
            // before at most twice that again.
            let length = 12 * u64::try_from(pieces).unwrap();
            let took = appended(pieces);
            assert!(took <= 8 * length + 1_024, "{pieces} appends took {took} bytes for {length}");
            // A join makes one block, just big enough.
            let length = 14 * u64::try_from(pieces).unwrap();
            let took = joined(pieces);
            assert!(took <= length + 128, "joining {pieces} took {took} bytes for {length}");
        }
        finish();
    }
}

#[cfg(test)]
mod sized_tests {
    use std::rc::Rc;

    use super::super::text::{f32_header, inline_bytes, u64_header};
    use super::super::{Tables, finish, heap, lists, numbers, start, with};
    use super::{f32_spec_text, f32_text, u64_spec_text, u64_text};
    use crate::packed::Element;

    fn begin() {
        let tables = Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![Rc::new(Element::U64), Rc::new(Element::F32)],
            ..Tables::default()
        };
        start(tables, crate::Inputs::default());
    }

    fn text(address: i64) -> String {
        with(|state| super::text(state, address)).unwrap_or_default()
    }

    fn string(text: &str) -> i64 {
        with(|state| heap::text_address(state, text))
    }

    /// The `f64` that's exactly the `f32` nearest `x`, as native code holds an `f32`.
    #[expect(clippy::cast_possible_truncation, reason = "rounding to an f32 is the point")]
    fn f32_of(x: f64) -> f64 {
        f64::from(x as f32)
    }

    /// A `u64` shows unsigned however big, and an `f32` as the shortest text that reads back as it,
    /// in `str`, a header, and an f-string's spec.
    #[test]
    fn a_u64_shows_unsigned_and_an_f32_shortest() {
        begin();
        let most = u64::MAX.cast_signed();
        assert_eq!(text(u64_text(most)), "18446744073709551615");
        assert_eq!(text(u64_text(7)), "7");
        assert_eq!(text(f32_text(f32_of(0.1))), "0.1");
        assert_eq!(text(f32_text(f32_of(1.0))), "1.0");
        assert_eq!(text(f32_text(f32_of(1e20))), "1e+20");
        assert_eq!(text(f32_text(f32_of(-2.5e-7))), "-2.5e-07");
        assert_eq!(inline_bytes(&u64_header(most).words), Some(b"18446744073709551615".to_vec()));
        assert_eq!(inline_bytes(&f32_header(f32_of(0.1)).words), Some(b"0.1".to_vec()));
        assert_eq!(text(u64_spec_text(most, string(","), 0, 0)), "18,446,744,073,709,551,615");
        assert_eq!(text(u64_spec_text(1234, string(",d"), 0, 0)), "1,234");
        assert_eq!(text(f32_spec_text(f32_of(0.1), string(""), 0, 0)), "0.1");
        assert_eq!(text(f32_spec_text(f32_of(0.1), string(".3f"), 0, 0)), "0.100");
        assert_eq!(u64_spec_text(most, string(".2f"), 0, 0), 0);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message.contains("isn't built yet for a `u64`")));
    }

    /// `u64.parse` takes 0 to 2^64 − 1 and nothing else.
    #[test]
    fn a_u64_parses_from_zero_to_its_most() {
        begin();
        let mut bits = 0;
        assert_eq!(numbers::parse_u64(string("18446744073709551615"), &mut bits), 1);
        assert_eq!(bits.cast_unsigned(), u64::MAX);
        assert_eq!(numbers::parse_u64(string(" 42 "), &mut bits), 1);
        assert_eq!(bits, 42);
        for wrong in ["-1", "18446744073709551616", "1.5", ""] {
            assert_eq!(numbers::parse_u64(string(wrong), &mut bits), 0, "{wrong:?}");
        }
        finish();
    }

    /// Inside a list, the runtime shows each kind as it shows one alone.
    #[test]
    fn lists_of_u64_and_f32_show_as_their_items_do() {
        begin();
        let unsigned = lists::new(0, 2);
        let floats = lists::new(1, 2);
        with(|state| {
            let words = state.heap.block_mut(u64::try_from(unsigned).unwrap()).unwrap();
            words[lists::LIST_HEADER_WORDS] = u64::MAX;
            words[lists::LIST_HEADER_WORDS + 1] = 3;
            let words = state.heap.block_mut(u64::try_from(floats).unwrap()).unwrap();
            words[lists::LIST_HEADER_WORDS] = f32_of(0.1).to_bits();
            words[lists::LIST_HEADER_WORDS + 1] = f32_of(2.0).to_bits();
        });
        assert_eq!(heap::shown::shown(u64::try_from(unsigned).unwrap()), "[18446744073709551615, 3]");
        assert_eq!(heap::shown::shown(u64::try_from(floats).unwrap()), "[0.1, 2.0]");
        heap::free(unsigned);
        heap::free(floats);
        finish();
    }
}
