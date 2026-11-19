//! Regular expressions for native code, by the addresses of strings' blocks. A pattern is
//! compiled once and kept.
//!
//! What's found comes back as numbers, so this knows nothing of how `Match` is laid out: a list
//! of ints, in characters, with each match's start and end, then the same for each of its
//! groups, -1 for a group that took no part. A pattern with `g` groups has `2 * (g + 1)`
//! numbers for each match.

use std::cell::Cell;
use std::rc::Rc;

use fennec_re::Anchor;
use fennec_syntax::{Diagnostic, Span};

use super::heap::{TEXT_HEADER_WORDS, text_address};
use super::lists::{list_of_strings, list_of_words};
use super::{State, attempt, span, with};
use crate::stdlib::re::{chars, invalid, replaced};

thread_local! {
    /// Where a pattern's text, the text it searches and what it puts in place of a match are
    /// copied to be read as `str`s: kept from call to call, so a search of every line of a file
    /// makes nothing for them after the first.
    static PATTERN: Cell<String> = const { Cell::new(String::new()) };
    static SEARCHED: Cell<String> = const { Cell::new(String::new()) };
    static WITH: Cell<String> = const { Cell::new(String::new()) };
}

/// The most room a buffer keeps from one call to the next. A longer text is copied into room of
/// its own, which goes when the call ends, as every text's did: kept, one search of a big file
/// would hold a copy of it on that thread for the rest of the run.
const KEPT_BYTES: usize = 64 << 10;

/// Runs `work` with the text of the string at `address`, copied into the room `kept` holds,
/// which is given back to it after: an empty text for no string.
fn with_text<R>(
    state: &mut State,
    kept: &'static std::thread::LocalKey<Cell<String>>,
    address: i64,
    work: impl FnOnce(&mut State, &str) -> R,
) -> R {
    let mut bytes = kept.take().into_bytes();
    bytes.clear();
    if let Some(block) = u64::try_from(address).ok().and_then(|at| state.heap.block(at)) {
        let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let words = block.get(TEXT_HEADER_WORDS..).unwrap_or_default().iter().take(length.div_ceil(8));
        // A text too long to keep gets room for all of it at once, as it always did: grown as it's
        // copied, it could end with room for twice the text.
        if words.len() * 8 > KEPT_BYTES {
            bytes.reserve_exact(words.len() * 8);
        }
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.truncate(length);
    }
    // A string's block always holds UTF-8: anything else reads as empty, as it always did.
    let text = String::from_utf8(bytes).unwrap_or_default();
    let result = work(state, &text);
    if text.capacity() <= KEPT_BYTES {
        kept.set(text);
    }
    result
}

/// The pattern at `pattern`, compiled once and kept.
fn compiled(state: &mut State, pattern: i64) -> Result<Rc<fennec_re::Regex>, fennec_re::Error> {
    with_text(state, &PATTERN, pattern, |state, pattern| state.regexes.compiled(pattern))
}

/// `compiled`, its problem said as a failure at `at`.
fn compiled_at(state: &mut State, pattern: i64, at: Span) -> Result<Rc<fennec_re::Regex>, Diagnostic> {
    compiled(state, pattern).map_err(|error| invalid(&error, at))
}

/// `re.is_regex(pattern)`.
pub extern "C" fn is_valid(pattern: i64) -> i8 {
    with(|state| i8::from(compiled(state, pattern).is_ok()))
}

/// A new string saying what's wrong with a pattern, as making a `Regex` of it says.
pub extern "C" fn problem(pattern: i64) -> i64 {
    super::made_with("fennec_regex_problem", |state| {
        let problem = compiled(state, pattern).err();
        let message = problem.map(|error| invalid(&error, Span::default()).message).unwrap_or_default();
        text_address(state, &message)
    })
}

/// How many groups a pattern has, not counting the whole match: what says how many numbers
/// `find_all` and `find` give for each match. -1 for a pattern that isn't valid.
pub extern "C" fn groups(pattern: i64) -> i64 {
    with(|state| {
        compiled(state, pattern).ok().and_then(|regex| i64::try_from(regex.groups()).ok()).unwrap_or(-1)
    })
}

/// `Regex(pattern).replace(text, with)`: a new string.
pub extern "C" fn replace(pattern: i64, whole: i64, with_text_at: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let regex = compiled_at(state, pattern, at)?;
        let made = with_text(state, &SEARCHED, whole, |state, whole| {
            with_text(state, &WITH, with_text_at, |_, with_text| replaced(whole, &regex, with_text, at))
        })?;
        Ok(text_address(state, &made))
    })
}

/// Pushes where a match and its groups are, in characters. `byte` and `char` say how far
/// through the text it's counted, which keeps going through all the matches linear.
fn push_places(
    whole: &str,
    captures: &fennec_re::Captures,
    (byte, char): &mut (usize, usize),
    out: &mut Vec<u64>,
) {
    let number = |n: i64| u64::from_ne_bytes(n.to_ne_bytes());
    let (from, to) = captures.whole();
    *char += chars(whole, *byte, from);
    *byte = from;
    let start = *char;
    let at = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    out.push(number(at(start)));
    out.push(number(at(start + chars(whole, from, to))));
    for group in captures.groups.iter().skip(1) {
        match group {
            Some((group_from, group_to)) => {
                let group_start = start + chars(whole, from, *group_from);
                out.push(number(at(group_start)));
                out.push(number(at(group_start + chars(whole, *group_from, *group_to))));
            }
            None => out.extend([number(-1), number(-1)]),
        }
    }
}

/// `find_all`: where every match is, as a list of ints. `ints` is the number of the kind of
/// item that list has.
pub extern "C" fn find_all(pattern: i64, whole: i64, ints: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let regex = compiled_at(state, pattern, span(start, end))?;
        let places = with_text(state, &SEARCHED, whole, |_, whole| {
            let mut places = Vec::new();
            let mut counted = (0, 0);
            for captures in regex.find_all(whole) {
                push_places(whole, &captures, &mut counted, &mut places);
            }
            places
        });
        Ok(list_of_words(state, ints, &places))
    })
}

/// `search` (0), `match` (1) or `fullmatch` (2): where the match is, or an empty list.
pub extern "C" fn find(pattern: i64, whole: i64, kind: i64, ints: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let regex = compiled_at(state, pattern, span(start, end))?;
        let anchor = match kind {
            1 => Anchor::Start,
            2 => Anchor::Both,
            _ => Anchor::Search,
        };
        let places = with_text(state, &SEARCHED, whole, |_, whole| {
            let mut places = Vec::new();
            if let Some(captures) = regex.find_at(whole, 0, anchor) {
                push_places(whole, &captures, &mut (0, 0), &mut places);
            }
            places
        });
        Ok(list_of_words(state, ints, &places))
    })
}

/// `split`: the text between matches, as a list of strings. `strings` is the number of the
/// kind of item that list has.
pub extern "C" fn split(pattern: i64, whole: i64, strings: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let regex = compiled_at(state, pattern, span(start, end))?;
        Ok(with_text(state, &SEARCHED, whole, |state, whole| {
            let mut pieces = Vec::new();
            let mut from = 0;
            for captures in regex.find_all(whole) {
                let (match_from, match_to) = captures.whole();
                pieces.push(whole.get(from..match_from).unwrap_or(""));
                from = match_to;
            }
            pieces.push(whole.get(from..).unwrap_or(""));
            list_of_strings(state, strings, &pieces)
        }))
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, with};
    use super::{KEPT_BYTES, SEARCHED, with_text};
    use crate::packed::Element;

    /// How much room the searched text's buffer keeps between calls.
    fn kept_room() -> usize {
        let kept = SEARCHED.take();
        let room = kept.capacity();
        SEARCHED.set(kept);
        room
    }

    /// A short text's room is kept for the next call, but a big one's copy goes when its call
    /// ends: kept, one search of a big file held a copy of it on its thread for the rest of the run.
    #[test]
    fn a_kept_buffer_lets_a_big_texts_copy_go() {
        start(Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() }, crate::Inputs::default());
        let line = "a line of a file, long enough to take a block of its own";
        let short = with(|state| heap::text_address(state, line));
        let read = with(|state| with_text(state, &SEARCHED, short, |_, text| text.to_owned()));
        assert_eq!(read, line);
        assert!((line.len()..=KEPT_BYTES).contains(&kept_room()), "{} bytes kept", kept_room());
        let file = "a line of a big file\n".repeat(4 * KEPT_BYTES / 21);
        let big = with(|state| heap::text_address(state, &file));
        let read = with(|state| with_text(state, &SEARCHED, big, |_, text| text.len()));
        assert_eq!(read, file.len());
        assert!(kept_room() <= KEPT_BYTES, "{} bytes kept after a {}-byte text", kept_room(), file.len());
        let (_, failure, _) = finish();
        assert!(failure.is_none());
    }
}
