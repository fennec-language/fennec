//! Strings for native code. A string is a block in the heap (see `heap`): its owner count,
//! its shape, its length in bytes and in characters, how many bytes it has room for, then
//! its UTF-8 bytes. Copies share a block. The one change in place is `+=` on a block with
//! one owner, which may have room left after its bytes. Native code reads lengths and ASCII characters itself; everything
//! that makes a new string, or needs to walk UTF-8, happens here.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use super::heap::{TEXT_HEADER_WORDS, block_text, release, retain, text_address, text_from_bytes};
use super::{State, attempt, span, with};
use crate::packed::Element;
use crate::value::Value;

/// A string's text, when the address is a string's block.
fn text(state: &State, address: i64) -> Option<String> {
    let block = state.heap.block(u64::try_from(address).ok()?)?;
    super::heap::is_text(state, block).then(|| block_text(block))
}

fn lost(start: i64, end: i64) -> Diagnostic {
    Diagnostic::new("the JIT lost a string", span(start, end))
        .with_help("this is a bug in Fennec; run with `--interp` and please report it")
}

/// A string block owned once with a temporary's text, failing like the interpreter when
/// it isn't a string.
pub extern "C" fn from_value(value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| match state.temp(value) {
        Value::Str(text) => Ok(text_address(state, &text)),
        other => Err(Diagnostic::new(
            format!("expected a str here, but this is a {}", other.type_name()),
            span(start, end),
        )),
    })
}

/// `a + b`.
pub extern "C" fn concat(a: i64, b: i64) -> i64 {
    with(|state| {
        let joined = format!("{}{}", text(state, a).unwrap_or_default(), text(state, b).unwrap_or_default());
        text_address(state, &joined)
    })
}

/// An empty string owned once, with room for at least `bytes` bytes, for code generators
/// that know a string's length before making it, like an f-string's. They copy its bytes in
/// themselves and then set its lengths in bytes and characters.
pub extern "C" fn with_room(bytes: i64) -> i64 {
    with(|state| {
        let words = usize::try_from(bytes).unwrap_or(0).div_ceil(8).max(1);
        let mut block = state.heap.fresh(TEXT_HEADER_WORDS + words);
        if let Some([count, shape, _, _, room]) = block.get_mut(..TEXT_HEADER_WORDS) {
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
    with(|state| grow(state, text, more).unwrap_or(text))
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
    let fits = block.first() == Some(&1) && block.len() >= needed;
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
    put_words(&mut grown, length, added, extra);
    set_lengths(&mut grown, total, chars + extra_chars);
    if let Some([count, .., bytes]) = grown.get_mut(..TEXT_HEADER_WORDS) {
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
    with(|state| {
        // A character is at most four bytes, so only the words holding those are read.
        let at = usize::try_from(at).unwrap_or(0);
        let block = u64::try_from(text).ok().and_then(|text| state.heap.block(text)).unwrap_or_default();
        let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let words = block.get(TEXT_HEADER_WORDS + at / 8..).unwrap_or_default();
        let near: Vec<u8> = words.iter().take(2).flat_map(|word| word.to_le_bytes()).collect();
        let wanted = length.saturating_sub(at).min(4);
        let start = near.get(at % 8..at % 8 + wanted).unwrap_or_default();
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

/// `text[at]`, a string of one character, counting characters as the interpreter does.
pub extern "C" fn index(address: i64, at: i64, start: i64, end: i64) -> i64 {
    let byte = attempt(|state: &mut State| byte_of_char(state, address, at, (start, end)).map(Some));
    byte.map_or(0, |byte| char_at(address, byte))
}

/// Where the `at`th character starts, in bytes, reading only the text before it, and none of
/// it for ASCII. An index past either end fails as the interpreter's does.
fn byte_of_char(state: &State, address: i64, at: i64, (start, end): (i64, i64)) -> Result<i64, Diagnostic> {
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
    let words = block.get(TEXT_HEADER_WORDS..).unwrap_or_default();
    let mut seen = -1;
    let bytes = words.iter().flat_map(|word| word.to_le_bytes()).take(usize::try_from(length).unwrap_or(0));
    for (byte_at, byte) in bytes.enumerate() {
        // Every byte but a continuation byte starts a character.
        if byte & 0xC0 != 0x80 {
            seen += 1;
            if seen == wanted {
                return i64::try_from(byte_at).map_err(|_| lost(start, end));
            }
        }
    }
    Err(lost(start, end))
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
        let (all, _) = bytes(state, address).ok_or_else(|| lost(start, end))?;
        let whole = Value::str(&String::from_utf8(all).unwrap_or_default());
        let bound = |at: i64| (at != i64::MIN).then_some(Value::Int(at));
        let (from, to) = (bound(from), bound(to));
        match crate::ops::slice(&whole, from.as_ref(), to.as_ref(), span(start, end))? {
            Value::Str(part) => Ok(text_address(state, &part)),
            _ => Err(lost(start, end)),
        }
    })
}

/// How two strings order: -1, 0 or 1, comparing bytes as the interpreter does.
pub extern "C" fn compare(a: i64, b: i64) -> i64 {
    with(|state| match bytes(state, a).unwrap_or_default().0.cmp(&bytes(state, b).unwrap_or_default().0) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

/// `part in text`.
pub extern "C" fn contains(whole: i64, part: i64) -> i8 {
    with(|state| {
        let whole = text(state, whole).unwrap_or_default();
        i8::from(whole.contains(text(state, part).unwrap_or_default().as_str()))
    })
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
    with(|state| text_address(state, &n.to_string()))
}

/// A new string with a float's text, as it prints.
pub extern "C" fn float_text(x: f64) -> i64 {
    with(|state| text_address(state, &crate::format::display(&Value::Float(x))))
}

/// A new string with a temporary's text, as `str` gives it.
pub extern "C" fn value_text(value: i64) -> i64 {
    with(|state| {
        let value = state.temp(value);
        text_address(state, &crate::format::display(&value))
    })
}

/// A new string with a temporary's text, formatted by a spec, failing like the interpreter
/// for a spec that doesn't fit.
pub extern "C" fn spec_text(value: i64, spec: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let value = state.temp(value);
        let spec = text(state, spec).unwrap_or_default();
        let piece = crate::format::with_spec(&value, &spec)
            .map_err(|message| Diagnostic::new(message, span(start, end)))?;
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

/// `text.code()`, failing like the interpreter for anything but one character.
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
    with(|state| {
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
    let header = [1, state.tables.str_shape, number(kept), chars, number(words * 8)];
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

/// `separator.join(list)`, for a list of strings, written straight into the joined block.
/// Each string's block is looked up once.
pub extern "C" fn join(separator: i64, list: i64) -> i64 {
    with(|state| {
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
        let header = [1, state.tables.str_shape, 0, 0, room];
        if let Some(start) = joined.get_mut(..TEXT_HEADER_WORDS) {
            start.copy_from_slice(&header);
        }
        set_lengths(&mut joined, at, chars);
        state.heap.insert(joined)
    })
}

/// One of the interpreter's string methods, run on a string block with string blocks as its
/// arguments, as the interpreter runs it.
fn method(state: &State, address: i64, name: &str, args: &[i64], at: Span) -> Result<Value, Diagnostic> {
    let lost = || Diagnostic::new("the native code lost a string", at);
    let whole = text(state, address).ok_or_else(lost)?;
    let args =
        args.iter().map(|arg| text(state, *arg).map(|text| Value::str(&text))).collect::<Option<Vec<_>>>();
    crate::collections::str_method(&whole, name, &args.ok_or_else(lost)?, at)
}

/// A string method that gives a string, as a block owned once.
fn text_method(address: i64, name: &str, args: &[i64]) -> i64 {
    with(|state| match method(state, address, name, args, Span::default()) {
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
/// once. An empty separator fails, as in the interpreter.
pub extern "C" fn split(address: i64, separator: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let separators: &[i64] = if separator == 0 { &[] } else { &[separator] };
        let parts = method(state, address, "split", separators, at)?;
        super::lists::write_list(state, &Rc::new(Element::Str), &parts).ok_or_else(|| lost(start, end))
    })
}
