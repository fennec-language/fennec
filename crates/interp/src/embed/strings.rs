//! Strings for native code. A string is a block in the heap (see `heap`): its owner count,
//! its shape, its length in bytes and in characters, how many bytes it has room for, then
//! its UTF-8 bytes. Copies share a block. The one change in place is `+=` on a block with
//! one owner, which may have room left after its bytes. Native code reads lengths and ASCII characters itself; everything
//! that makes a new string, or needs to walk UTF-8, happens here.

use fennec_syntax::Diagnostic;

use super::heap::{TEXT_HEADER_WORDS, block_text, release, retain, text_address, text_from_bytes};
use super::{State, attempt, span, with};
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

/// `text += more`: takes over the owner `text` had and gives back the joined string's.
/// When `text` has no other owner and its block has room, the bytes go on the end in
/// place. Otherwise they're joined in a new block with room to double, so building a
/// string a piece at a time takes time in proportion to its length.
pub extern "C" fn append(text: i64, more: i64) -> i64 {
    with(|state| grow(state, text, more).unwrap_or(text))
}

fn grow(state: &mut State, text: i64, more: i64) -> Option<i64> {
    // Copied first: `text += text` reads the block it writes.
    let (extra, extra_chars) = bytes(state, more)?;
    let at = u64::try_from(text).ok()?;
    let block = state.heap.block(at)?;
    let (length, chars) = (usize::try_from(*block.get(2)?).ok()?, *block.get(3)?);
    let total = length + extra.len();
    let needed = TEXT_HEADER_WORDS + total.div_ceil(8).max(1);
    let fits = block.first() == Some(&1) && block.len() >= needed;
    if fits {
        let block = state.heap.block_mut(at)?;
        put_bytes(block, length, &extra);
        set_lengths(block, total, chars + u64::try_from(extra_chars).ok()?);
        return Some(text);
    }
    let room = 2 * total.div_ceil(8).max(1);
    let mut grown = state.heap.fresh(TEXT_HEADER_WORDS + room);
    let used = TEXT_HEADER_WORDS + length.div_ceil(8);
    for (to, from) in grown.iter_mut().zip(state.heap.block(at)?.get(..used)?) {
        *to = *from;
    }
    put_bytes(&mut grown, length, &extra);
    set_lengths(&mut grown, total, chars + u64::try_from(extra_chars).ok()?);
    if let Some([count, .., bytes]) = grown.get_mut(..TEXT_HEADER_WORDS) {
        *count = 1;
        *bytes = u64::try_from(room * 8).unwrap_or(0);
    }
    let joined = state.heap.insert(grown);
    release(state, at);
    Some(joined)
}

/// Writes `bytes` into a string's block, starting `from` bytes into its text.
fn put_bytes(block: &mut [u64], from: usize, bytes: &[u8]) {
    let Some(words) = block.get_mut(TEXT_HEADER_WORDS..) else { return };
    for (at, byte) in (from..).zip(bytes) {
        if let Some(word) = words.get_mut(at / 8) {
            let mut each = word.to_le_bytes();
            if let Some(slot) = each.get_mut(at % 8) {
                *slot = *byte;
            }
            *word = u64::from_le_bytes(each);
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
        let (all, _) = bytes(state, text).unwrap_or_default();
        let rest = all.get(usize::try_from(at).unwrap_or(0)..).unwrap_or_default();
        // A character is at most four bytes.
        let start = rest.get(..rest.len().min(4)).unwrap_or_default();
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
    attempt(|state: &mut State| {
        let whole = text(state, address).ok_or_else(|| lost(start, end))?;
        let at = crate::ops::index(&Value::str(&whole), &Value::Int(at), span(start, end))?;
        match at {
            Value::Str(one) => Ok(text_address(state, &one)),
            _ => Err(lost(start, end)),
        }
    })
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
        let settled =
            |byte: u8| byte.is_ascii() && !byte.is_ascii_whitespace() && byte != 0x0b && byte >= 0x20;
        let ends = state.heap.block(at).and_then(|block| {
            let length = usize::try_from(*block.get(2)?).ok().filter(|length| *length > 0)?;
            let word = |at: usize| block.get(TEXT_HEADER_WORDS + at / 8).map(|word| word.to_le_bytes());
            let first = word(0)?.first().copied()?;
            let last = word(length - 1)?.get((length - 1) % 8).copied()?;
            Some((first, last))
        });
        if let Some((first, last)) = ends
            && (which == 2 || settled(first))
            && (which == 1 || settled(last))
        {
            retain(state, at);
            return address;
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

/// Adds a string block's bytes to `bytes`, giving back how many characters it has.
fn add_text(block: &[u64], bytes: &mut Vec<u8>) -> usize {
    let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
    let words = block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + length.div_ceil(8)).unwrap_or_default();
    let wanted = bytes.len() + length;
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.truncate(wanted);
    block.get(3).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0)
}

/// `separator.join(list)`, for a list of strings.
pub extern "C" fn join(separator: i64, list: i64) -> i64 {
    with(|state| {
        let between = u64::try_from(separator).ok().and_then(|at| state.heap.block(at)).unwrap_or_default();
        let block = u64::try_from(list).ok().and_then(|list| state.heap.block(list)).unwrap_or_default();
        let room = |block: &[u64]| block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let words = super::lists::items(block, 1);
        let total = words.iter().filter_map(|word| state.heap.block(*word)).map(room).sum::<usize>()
            + room(between) * words.len();
        let mut bytes = Vec::with_capacity(total + 8);
        let mut chars = 0;
        for (at, word) in words.iter().enumerate() {
            if at > 0 {
                chars += add_text(between, &mut bytes);
            }
            chars += state.heap.block(*word).map_or(0, |part| add_text(part, &mut bytes));
        }
        text_from_bytes(state, &bytes, chars)
    })
}
