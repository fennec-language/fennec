//! Regular expressions for native code, by the addresses of strings' blocks. A pattern is
//! compiled once and kept, as the interpreter does.
//!
//! What's found comes back as numbers, so this knows nothing of how `Match` is laid out: a list
//! of ints, in characters, with each match's start and end, then the same for each of its
//! groups, -1 for a group that took no part. A pattern with `g` groups has `2 * (g + 1)`
//! numbers for each match.

use std::rc::Rc;

use fennec_re::Anchor;
use fennec_syntax::{Diagnostic, Span};

use super::heap::{block_text, text_address};
use super::lists::{list_of_strings, list_of_words};
use super::{State, attempt, span, with};
use crate::stdlib::re::{chars, invalid, replaced};

fn text(state: &State, address: i64) -> String {
    u64::try_from(address).ok().and_then(|at| state.heap.block(at)).map_or_else(String::new, block_text)
}

fn compiled(state: &mut State, pattern: i64, at: Span) -> Result<Rc<fennec_re::Regex>, Diagnostic> {
    let pattern = text(state, pattern);
    state.interp.regex(&pattern).map_err(|error| invalid(&error, at))
}

/// `re.is_regex(pattern)`.
pub extern "C" fn is_valid(pattern: i64) -> i8 {
    with(|state| {
        let pattern = text(state, pattern);
        i8::from(state.interp.regex(&pattern).is_ok())
    })
}

/// How many groups a pattern has, not counting the whole match: what says how many numbers
/// `find_all` and `find` give for each match. -1 for a pattern that isn't valid.
pub extern "C" fn groups(pattern: i64) -> i64 {
    with(|state| {
        let pattern = text(state, pattern);
        state.interp.regex(&pattern).map_or(-1, |regex| i64::try_from(regex.groups()).unwrap_or(-1))
    })
}

/// `Regex(pattern).replace(text, with)`: a new string.
pub extern "C" fn replace(pattern: i64, whole: i64, with_text: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let regex = compiled(state, pattern, at)?;
        let (whole, with_text) = (text(state, whole), text(state, with_text));
        let made = replaced(&whole, &regex, &with_text, at)?;
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
        let regex = compiled(state, pattern, span(start, end))?;
        let whole = text(state, whole);
        let mut places = Vec::new();
        let mut counted = (0, 0);
        for captures in regex.find_all(&whole) {
            push_places(&whole, &captures, &mut counted, &mut places);
        }
        Ok(list_of_words(state, ints, &places))
    })
}

/// `search` (0), `match` (1) or `fullmatch` (2): where the match is, or an empty list.
pub extern "C" fn find(pattern: i64, whole: i64, kind: i64, ints: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let regex = compiled(state, pattern, span(start, end))?;
        let whole = text(state, whole);
        let anchor = match kind {
            1 => Anchor::Start,
            2 => Anchor::Both,
            _ => Anchor::Search,
        };
        let mut places = Vec::new();
        if let Some(captures) = regex.find_at(&whole, 0, anchor) {
            push_places(&whole, &captures, &mut (0, 0), &mut places);
        }
        Ok(list_of_words(state, ints, &places))
    })
}

/// `split`: the text between matches, as a list of strings. `strings` is the number of the
/// kind of item that list has.
pub extern "C" fn split(pattern: i64, whole: i64, strings: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let regex = compiled(state, pattern, span(start, end))?;
        let whole = text(state, whole);
        let mut pieces = Vec::new();
        let mut from = 0;
        for captures in regex.find_all(&whole) {
            let (match_from, match_to) = captures.whole();
            pieces.push(whole.get(from..match_from).unwrap_or(""));
            from = match_to;
        }
        pieces.push(whole.get(from..).unwrap_or(""));
        Ok(list_of_strings(state, strings, &pieces))
    })
}
