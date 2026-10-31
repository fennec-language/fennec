//! Strings for native code. A string is a block in the heap (see `heap`): its owner count,
//! its shape, its length in bytes and in characters, how many bytes it has room for, then
//! its UTF-8 bytes. Copies share a block. The one change in place is `+=` on a block with
//! one owner, which may have room left after its bytes. Native code reads lengths and ASCII characters itself; everything
//! that makes a new string, or needs to walk UTF-8, happens here.

use fennec_syntax::Diagnostic;

use super::heap::{TEXT_HEADER_WORDS, block_text, release, text_address};
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

/// The character starting `at` bytes into a string, as a string owned once.
pub extern "C" fn char_at(text: i64, at: i64) -> i64 {
    with(|state| {
        let (all, _) = bytes(state, text).unwrap_or_default();
        let rest = all.get(usize::try_from(at).unwrap_or(0)..).unwrap_or_default();
        let one = String::from_utf8_lossy(rest).chars().next().map(String::from).unwrap_or_default();
        text_address(state, &one)
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

/// The bytes `from..to` of an ASCII string's block, copying only those.
fn ascii_part(state: &State, address: i64, from: i64, to: i64) -> Option<String> {
    let block = state.heap.block(u64::try_from(address).ok()?)?;
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
    let words = block.get(TEXT_HEADER_WORDS..)?;
    let part: Vec<u8> = (from..to.max(from))
        .filter_map(|at| words.get(at / 8).and_then(|word| word.to_le_bytes().get(at % 8).copied()))
        .collect();
    String::from_utf8(part).ok()
}

/// `text[from:to]`, where a missing end is `i64::MIN`.
pub extern "C" fn slice(address: i64, from: i64, to: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        if let Some(part) = ascii_part(state, address, from, to) {
            return Ok(text_address(state, &part));
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
