//! Regular expressions for native code, by the addresses of strings' blocks. A pattern is
//! compiled once and kept.
//!
//! What's found comes back as numbers, so this knows nothing of how `Match` is laid out: a list
//! of ints, in characters, with each match's start and end, then the same for each of its
//! groups, -1 for a group that took no part. A pattern with `g` groups has `2 * (g + 1)`
//! numbers for each match.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fennec_re::Anchor;
use fennec_syntax::{Diagnostic, Span};

use super::heap::{IMMORTAL, NEVER_FREED_COUNT, TEXT_HEADER_WORDS, mark_never_freed, text_address};
use super::lists::{list_of_strings, list_of_words};
use super::text::{block_of, bytes_of};
use super::{State, attempt, span, with};
use crate::stdlib::re::{LONGEST_KEPT, chars, invalid, replaced};

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
        copy_words(&mut bytes, block.get(TEXT_HEADER_WORDS..).unwrap_or_default(), length);
    }
    read_copy(state, kept, bytes, work)
}

/// [`with_text`] for a string given as its header's words, read where its bytes are: in the
/// header itself when it's inline, or in its block.
fn with_header_text<R>(
    state: &mut State,
    kept: &'static std::thread::LocalKey<Cell<String>>,
    words: [u64; 3],
    work: impl FnOnce(&mut State, &str) -> R,
) -> R {
    let mut bytes = kept.take().into_bytes();
    bytes.clear();
    let (held, length) = bytes_of(state, &words);
    copy_words(&mut bytes, held, length);
    read_copy(state, kept, bytes, work)
}

/// Copies the first `length` bytes of `words` onto `bytes`.
fn copy_words(bytes: &mut Vec<u8>, words: &[u64], length: usize) {
    let words = words.iter().take(length.div_ceil(8));
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

/// Runs `work` with `bytes` as a `str`, then gives their room back to `kept` if it's small enough.
fn read_copy<R>(
    state: &mut State,
    kept: &'static std::thread::LocalKey<Cell<String>>,
    bytes: Vec<u8>,
    work: impl FnOnce(&mut State, &str) -> R,
) -> R {
    // A string always holds UTF-8: anything else reads as empty, as it always did.
    let text = String::from_utf8(bytes).unwrap_or_default();
    let result = work(state, &text);
    if text.capacity() <= KEPT_BYTES {
        kept.set(text);
    }
    result
}

/// How many patterns `RECENT` holds.
const MOST_RECENT: usize = 8;

/// A pattern used lately: the block its text was last read from, or the inline header that held it,
/// that text, and what it compiled to.
struct Recent {
    address: u64,
    inline: Option<[u64; 3]>,
    text: String,
    regex: Rc<fennec_re::Regex>,
}

thread_local! {
    /// The patterns used most lately, newest last. A loop searching with the same few patterns,
    /// as one over a file's lines does, finds each here by its block and a comparison of the
    /// block's words with the text, with no copy, UTF-8 check or hash of it. The text is compared,
    /// not only the address, so a block freed and made again holding another text isn't taken for
    /// the pattern that was there.
    static RECENT: RefCell<Vec<Recent>> = const { RefCell::new(Vec::new()) };
}

/// Whether the string block `block` holds exactly `text`.
fn holds_text(block: &[u64], text: &str) -> bool {
    let words = block.get(TEXT_HEADER_WORDS..).unwrap_or_default();
    block.get(2).and_then(|n| usize::try_from(*n).ok()) == Some(text.len())
        && words.len() >= text.len().div_ceil(8)
        && words
            .iter()
            .zip(text.as_bytes().chunks(8))
            .all(|(word, part)| word.to_le_bytes().get(..part.len()) == Some(part))
}

/// The pattern at `pattern`, compiled once and kept.
fn compiled(state: &mut State, pattern: i64) -> Result<Rc<fennec_re::Regex>, fennec_re::Error> {
    let address = u64::try_from(pattern).ok();
    if let Some((address, block)) = address.and_then(|at| Some((at, state.heap.block(at)?))) {
        let found = RECENT.with_borrow(|recent| {
            let same =
                recent.iter().rev().find(|kept| kept.address == address && holds_text(block, &kept.text));
            same.map(|kept| Rc::clone(&kept.regex))
        });
        if let Some(regex) = found {
            return Ok(regex);
        }
    }
    with_text(state, &PATTERN, pattern, |state, text| {
        // The same text in another block, as a pattern made again for each call is: this block now.
        let moved = RECENT.with_borrow_mut(|recent| {
            let same = recent.iter_mut().rev().find(|kept| kept.text == text)?;
            same.address = address.unwrap_or(0);
            Some(Rc::clone(&same.regex))
        });
        if let Some(regex) = moved {
            return Ok(regex);
        }
        let regex = state.regexes.compiled(text)?;
        if let Some(address) = address
            && text.len() <= LONGEST_KEPT
        {
            RECENT.with_borrow_mut(|recent| {
                if recent.len() >= MOST_RECENT {
                    recent.remove(0);
                }
                recent.push(Recent {
                    address,
                    inline: None,
                    text: text.to_owned(),
                    regex: Rc::clone(&regex),
                });
            });
        }
        Ok(regex)
    })
}

/// The pattern a header holds, compiled once and kept. One in a block is found as [`compiled`]
/// finds it. An inline one, as every pattern of 23 bytes or fewer is, is found by its three words,
/// which hold all of its text, so nothing is read, made or compared beyond them.
fn compiled_header(state: &mut State, words: [u64; 3]) -> Result<Rc<fennec_re::Regex>, fennec_re::Error> {
    if let Some(address) = block_of(&words) {
        return compiled(state, address.cast_signed());
    }
    let found = RECENT.with_borrow(|recent| {
        recent.iter().rev().find(|kept| kept.inline == Some(words)).map(|kept| Rc::clone(&kept.regex))
    });
    if let Some(regex) = found {
        return Ok(regex);
    }
    with_header_text(state, &PATTERN, words, |state, text| {
        let regex = state.regexes.compiled(text)?;
        RECENT.with_borrow_mut(|recent| {
            if recent.len() >= MOST_RECENT {
                recent.remove(0);
            }
            let kept =
                Recent { address: 0, inline: Some(words), text: text.to_owned(), regex: Rc::clone(&regex) };
            recent.push(kept);
        });
        Ok(regex)
    })
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

/// How many groups a pattern has, not counting the whole match, the pattern given as its header's
/// words: what says how many numbers `find_all` and `find` give for each match. -1 for a pattern
/// that isn't valid.
pub extern "C" fn groups(p0: u64, p1: u64, p2: u64) -> i64 {
    with(|state| {
        let regex = compiled_header(state, [p0, p1, p2]).ok();
        regex.and_then(|regex| i64::try_from(regex.groups()).ok()).unwrap_or(-1)
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

thread_local! {
    /// For each kind of list of ints, the empty one a search that finds nothing gives back, by its
    /// address and its kind's word: made once and never freed.
    static NO_PLACES: RefCell<Vec<(i64, u64, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Whether `block` is an empty list that's never freed, with the kind word `kind`, which says it's
/// never freed too: one that can stand for no places, whichever run made it.
fn kept_empty(block: &[u64], kind: u64) -> bool {
    block.first().is_some_and(|count| *count >= IMMORTAL)
        && block.get(1) == Some(&kind)
        && block.get(2) == Some(&0)
}

/// A list of ints holding `places`, with the number of its kind of list. Where nothing was found,
/// the list is one made once and never freed, so the searches that find nothing, most of those a
/// grep makes, make and free no list. Native code only reads it. It's checked in the heap each time,
/// so one kept from a run that's ended, or a block freed and made again since, is never given out.
fn places_list(state: &mut State, ints: i64, places: &[u64]) -> i64 {
    if !places.is_empty() {
        return list_of_words(state, ints, places);
    }
    let kept = NO_PLACES.with_borrow(|kept| {
        kept.iter().find(|(element, ..)| *element == ints).map(|(_, at, kind)| (*at, *kind))
    });
    if let Some((at, kind)) = kept
        && state.heap.block(at).is_some_and(|block| kept_empty(block, kind))
    {
        return i64::try_from(at).unwrap_or(0);
    }
    let made = list_of_words(state, ints, &[]);
    if let Some(at) = u64::try_from(made).ok()
        && let Some(block) = state.heap.block_mut(at)
        && let Some(count) = block.first_mut()
    {
        // Its owners stay far above the mark, as a constant's do, so letting one go never frees it.
        *count = count.saturating_add(NEVER_FREED_COUNT);
        mark_never_freed(block);
        if let Some(kind) = block.get(1).copied() {
            NO_PLACES.with_borrow_mut(|kept| {
                kept.retain(|(element, ..)| *element != ints);
                kept.push((ints, at, kind));
            });
        }
    }
    made
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
        Ok(places_list(state, ints, &places))
    })
}

/// `search` (0), `match` (1) or `fullmatch` (2): where the match is, or an empty list. The pattern
/// and the text come as their headers' words and are read where they are, so a search makes no block
/// for either.
pub extern "C" fn find(
    p0: u64,
    p1: u64,
    p2: u64,
    t0: u64,
    t1: u64,
    t2: u64,
    kind: i64,
    ints: i64,
    start: i64,
    end: i64,
) -> i64 {
    attempt(|state: &mut State| {
        let regex =
            compiled_header(state, [p0, p1, p2]).map_err(|error| invalid(&error, span(start, end)))?;
        let anchor = match kind {
            1 => Anchor::Start,
            2 => Anchor::Both,
            _ => Anchor::Search,
        };
        let places = with_header_text(state, &SEARCHED, [t0, t1, t2], |_, whole| {
            let mut places = Vec::new();
            if let Some(captures) = regex.find_at(whole, 0, anchor) {
                push_places(whole, &captures, &mut (0, 0), &mut places);
            }
            places
        });
        Ok(places_list(state, ints, &places))
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

    use super::super::text;
    use super::super::{Tables, finish, heap, start, with};
    use super::{IMMORTAL, KEPT_BYTES, SEARCHED, compiled, compiled_header, find, holds_text, with_text};
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

    /// A pattern searched with again is found by its block and text. The same text in another block is
    /// the same compiled pattern, and a block is taken for a pattern only when it holds that text.
    #[test]
    fn a_pattern_used_again_is_found_by_its_block_and_text() {
        start(Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() }, crate::Inputs::default());
        let pattern = r"TODO\((\w+)\): (get|put) the";
        let (first, again, other) = with(|state| {
            let first = heap::text_address(state, pattern);
            (first, heap::text_address(state, pattern), heap::text_address(state, "(a)b"))
        });
        let found = with(|state| [first, first, again, other, first].map(|at| compiled(state, at).ok()));
        let found: Vec<_> = found.into_iter().flatten().collect();
        assert_eq!(found.len(), 5);
        let [one, two, three, four, five] = found.as_slice() else { return };
        assert!(Rc::ptr_eq(one, two) && Rc::ptr_eq(one, three) && Rc::ptr_eq(one, five));
        assert_eq!((one.groups(), four.groups()), (2, 1));
        let held = with(|state| {
            let block = u64::try_from(first).ok().and_then(|at| state.heap.block(at)).unwrap_or_default();
            [pattern, r"TODO\((\w+)\): (get|put) thE", r"TODO\((\w+)\): (get|put) th", ""]
                .map(|text| holds_text(block, text))
        });
        assert_eq!(held, [true, false, false, false]);
        let (_, failure, _) = finish();
        assert!(failure.is_none());
    }

    /// A short pattern, held inline, is found again by its header's words: the same compiled pattern
    /// each time, and another pattern's words never taken for it.
    #[test]
    fn an_inline_pattern_is_found_again_by_its_words() {
        start(Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() }, crate::Inputs::default());
        let found = with(|state| {
            let (todo, other) = (text::header(state, r"TODO\((\w+)\)"), text::header(state, "(a)(b)"));
            [todo, todo, other].map(|words| compiled_header(state, words).ok())
        });
        let found: Vec<_> = found.into_iter().flatten().collect();
        let [one, two, three] = found.as_slice() else { return assert_eq!(found.len(), 3) };
        assert!(Rc::ptr_eq(one, two) && !Rc::ptr_eq(one, three));
        assert_eq!((one.groups(), three.groups()), (1, 2));
        let (_, failure, _) = finish();
        assert!(failure.is_none());
    }

    /// Searches that find nothing give back one kept empty list, the same each time, which letting go
    /// doesn't free; a run started afresh gets one in its own heap, never one kept from a run that's
    /// ended.
    #[test]
    fn searches_that_find_nothing_share_one_kept_empty_list() {
        let tables = || Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![Rc::new(Element::Int)],
            ..Tables::default()
        };
        let kept_empty = |at: i64| {
            with(|state| {
                let block = u64::try_from(at).ok().and_then(|address| state.heap.block(address));
                block.is_some_and(|block| {
                    block.first().is_some_and(|count| *count >= IMMORTAL) && block.get(2) == Some(&0)
                })
            })
        };
        let mut made = Vec::new();
        for _ in 0..2 {
            start(tables(), crate::Inputs::default());
            // A short pattern, held inline, and a line long enough for a block of its own.
            let ([p0, p1, p2], [t0, t1, t2]) = with(|state| {
                (text::header(state, r"TODO\((\w+)\)"), text::header(state, "nothing to do on this line"))
            });
            let search = || find(p0, p1, p2, t0, t1, t2, 0, 0, 0, 0);
            let (first, again) = (search(), search());
            assert_eq!(first, again);
            assert!(kept_empty(first), "{first} isn't a kept empty list");
            made.push(first);
            let (_, failure, _) = finish();
            assert!(failure.is_none());
        }
        assert_eq!(made.len(), 2);
    }
}
