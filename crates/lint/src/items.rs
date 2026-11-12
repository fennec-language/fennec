//! `unused-item`: a private function, struct, enum or error nothing uses. The fix removes it,
//! with its doc comment.
//!
//! An item is used when its name is written anywhere in the program's files outside its own
//! definition: called, named as a type, extended, imported, read in a test, or even mentioned
//! in a comment or a string, which only ever makes the lint hold back. An enum is used, too,
//! when any of its cases is written, since `.Case` names a case without its enum. `main`, an
//! item marked `export`, which another kit may use, and a name starting with `_` are left alone.

use std::collections::{HashMap, HashSet};

use fennec_syntax::{Diagnostic, Item, Module, Sources, Span};

use crate::Lint;

/// The item's name, what kind of item it is, and the names whose use keeps it.
fn named(item: &Item) -> Option<(&str, &str, Vec<&str>)> {
    Some(match item {
        Item::Function(def) => (def.name.as_str(), "function", vec![def.name.as_str()]),
        Item::Struct(def) => (def.name.as_str(), "struct", vec![def.name.as_str()]),
        Item::Error(def) => (def.name.as_str(), "error", vec![def.name.as_str()]),
        Item::Enum(def) => {
            let mut names = vec![def.name.as_str()];
            names.extend(def.cases.iter().map(|case| case.name.as_str()));
            (def.name.as_str(), "enum", names)
        }
        _ => return None,
    })
}

/// The lines an item takes in its file, from the start of its doc comment to the start of the
/// next thing at the left margin, and its first line, as bytes of `text`.
fn lines_of(text: &str, start: usize) -> Option<(usize, usize, usize)> {
    let first = text.get(..start)?.rfind('\n').map_or(0, |at| at + 1);
    // Its doc comment is the `##` lines just above it.
    let mut from = first;
    while from > 0 {
        let above = text.get(..from - 1)?.rfind('\n').map_or(0, |at| at + 1);
        if !text.get(above..from)?.trim_start().starts_with("##") {
            break;
        }
        from = above;
    }
    // It ends where something else starts at the left margin.
    let mut to = text.get(first..)?.find('\n').map_or(text.len(), |at| first + at + 1);
    while to < text.len() {
        let line_end = text.get(to..)?.find('\n').map_or(text.len(), |at| to + at + 1);
        let line = text.get(to..line_end)?;
        if !line.trim().is_empty() && !line.starts_with([' ', '\t']) {
            break;
        }
        to = line_end;
    }
    Some((from, first, to))
}

/// Whether the item at `first` is written `export`, so another kit may use it.
fn public(text: &str, first: usize) -> bool {
    text.get(first..).is_some_and(|line| line.trim_start().starts_with("export "))
}

/// Where each of `wanted` is written in the program's files as a word: a run of letters, digits
/// and `_`, as a name is. Each place is its file and where in it the word starts. Made once, so
/// finding whether an item is used costs the same however many items there are, and only for
/// the names asked about, so a word that names no candidate is never hashed.
fn words<'s>(sources: &'s Sources, wanted: &HashSet<&str>) -> HashMap<&'s str, Vec<(&'s str, usize)>> {
    // The lengths of the names wanted, so most words are passed over without hashing them. A name
    // longer than 63 bytes shares the last bit.
    let lengths = wanted.iter().fold(0u64, |lengths, name| lengths | 1 << name.len().min(63));
    let mut found: HashMap<&str, Vec<(&str, usize)>> = HashMap::new();
    for (path, text) in sources.files() {
        let mut add = |word: &'s str, from: usize| {
            if lengths & 1 << word.len().min(63) != 0 && wanted.contains(word) {
                found.entry(word).or_default().push((path, from));
            }
        };
        if text.is_ascii() {
            ascii_words(text, &mut add);
        } else {
            any_words(text, &mut add);
        }
    }
    found
}

/// Each word of an ASCII text, with where it starts. Every byte is a character, so the text
/// splits at each byte no word has.
fn ascii_words<'s>(text: &'s str, add: &mut impl FnMut(&'s str, usize)) {
    let mut from = 0;
    for piece in text.as_bytes().split(|&byte| !(byte.is_ascii_alphanumeric() || byte == b'_')) {
        if !piece.is_empty()
            && let Some(word) = text.get(from..from + piece.len())
        {
            add(word, from);
        }
        // The piece, and the one byte that ended it.
        from += piece.len() + 1;
    }
}

/// Each word of any text, with where it starts: a byte at a time where the text is ASCII, and a
/// whole character where it isn't, since a letter of another alphabet is part of a word too.
fn any_words<'s>(text: &'s str, add: &mut impl FnMut(&'s str, usize)) {
    let bytes = text.as_bytes();
    let mut start = None;
    let mut at = 0;
    while at <= bytes.len() {
        let (in_word, width) = match bytes.get(at) {
            None => (false, 1),
            Some(&byte) if byte.is_ascii() => (byte.is_ascii_alphanumeric() || byte == b'_', 1),
            Some(_) => text
                .get(at..)
                .and_then(|rest| rest.chars().next())
                .map_or((false, 1), |ch| (ch.is_alphanumeric(), ch.len_utf8())),
        };
        match (in_word, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                if let Some(word) = text.get(from..at) {
                    add(word, from);
                }
                start = None;
            }
            _ => {}
        }
        at += width;
    }
}

/// Whether any of `names` is written in the program's files outside the item's own lines, which
/// are `from..to` of the file at `path`. They start and end at the start of a line, so no word
/// runs across either end.
fn used(
    words: &HashMap<&str, Vec<(&str, usize)>>,
    path: &str,
    (from, to): (usize, usize),
    names: &[&str],
) -> bool {
    names
        .iter()
        .filter_map(|name| words.get(name))
        .flatten()
        .any(|(file, at)| *file != path || !(from..to).contains(at))
}

/// An item that's a lint unless something uses it: its name, what kind of item it is, the names
/// whose use keeps it, its file and that file's text and base, where its lines start and end, and
/// where it's written.
struct Candidate<'m> {
    name: &'m str,
    kind: &'m str,
    names: Vec<&'m str>,
    path: &'m str,
    base: u32,
    lines: (usize, usize),
    span: Span,
}

/// The item, if nothing but its own lines could keep it: not `main`, a name starting with `_`, or
/// one marked `export`.
fn candidate<'m>(item: &'m Item, sources: &'m Sources) -> Option<Candidate<'m>> {
    let (name, kind, names) = named(item)?;
    if name == "main" || name.starts_with('_') {
        return None;
    }
    let span = match item {
        Item::Function(def) => def.span,
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.span,
        _ => return None,
    };
    let (path, text, base) = sources.file_at(span)?;
    let start = usize::try_from(span.start - base).ok()?;
    let first = text.get(..start)?.rfind('\n').map_or(0, |at| at + 1);
    if public(text, first) {
        return None;
    }
    let (from, _, to) = lines_of(text, start)?;
    Some(Candidate { name, kind, names, path, base, lines: (from, to), span })
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let candidates: Vec<Candidate<'_>> =
        module.items.iter().filter_map(|item| candidate(item, sources)).collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<&str> =
        candidates.iter().flat_map(|candidate| candidate.names.iter().copied()).collect();
    let words = words(sources, &wanted);
    let mut found = Vec::new();
    for Candidate { name, kind, names, path, base, lines, span } in candidates {
        if used(&words, path, lines, &names) {
            continue;
        }
        let at = |offset: usize| u32::try_from(offset).ok().map(|offset| base + offset);
        let (Some(from), Some(to)) = (at(lines.0), at(lines.1)) else { continue };
        let diagnostic = Diagnostic::new(format!("nothing uses the {kind} `{name}`"), span)
            .with_fix(Span { start: from, end: to }, "");
        found.push(Lint::new("unused-item", diagnostic));
    }
    found
}

#[cfg(test)]
mod tests {
    /// Splitting ASCII text at the bytes no word has finds the same words, at the same places, as
    /// reading it a character at a time: at either end, between separators of every kind, and
    /// with digits and `_` in words.
    #[test]
    fn ascii_text_has_the_words_reading_it_a_character_at_a_time_finds() {
        let texts = [
            "",
            "x",
            "_",
            "step_1(number)",
            "  a  b\n\nc_d9 ",
            "def f(x: int) -> int:\n    return g(x) + h_2(x)  # f again\n",
            "\"f\" 'g' f\"{h}\" ## doc f\n",
            "a.b.c,d;e[f]{g}",
            "9lives __init__ x_\t",
        ];
        for text in texts {
            let mut fast = Vec::new();
            super::ascii_words(text, &mut |word, at| fast.push((word, at)));
            let mut slow = Vec::new();
            super::any_words(text, &mut |word, at| slow.push((word, at)));
            assert_eq!(fast, slow, "in {text:?}");
        }
    }
}
