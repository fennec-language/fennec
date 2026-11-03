//! `unused-item`: a private function, struct, enum or error nothing uses. The fix removes it,
//! with its doc comment.
//!
//! An item is used when its name is written anywhere in the program's files outside its own
//! definition: called, named as a type, extended, imported, read in a test, or even mentioned
//! in a comment or a string, which only ever makes the lint hold back. An enum is used, too,
//! when any of its cases is written, since `.Case` names a case without its enum. `main`, an
//! item marked `pub`, which another kit may use, and a name starting with `_` are left alone.

use fennec_syntax::{Diagnostic, Item, Module, Sources, Span};

use crate::Lint;
use crate::dead::has_word;

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

/// Whether the item at `first` is written `pub`, so another kit may use it.
fn public(text: &str, first: usize) -> bool {
    text.get(first..).is_some_and(|line| line.trim_start().starts_with("pub "))
}

/// Whether any of `names` is written in the program's files outside the item's own lines.
fn used(sources: &Sources, path: &str, (from, to): (usize, usize), names: &[&str]) -> bool {
    sources.files().any(|(file, text)| {
        let elsewhere = if file == path {
            format!("{}{}", text.get(..from).unwrap_or(""), text.get(to..).unwrap_or(""))
        } else {
            text.to_owned()
        };
        names.iter().any(|name| has_word(&elsewhere, name))
    })
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for item in &module.items {
        let Some((name, kind, names)) = named(item) else { continue };
        if name == "main" || name.starts_with('_') {
            continue;
        }
        let span = match item {
            Item::Function(def) => def.span,
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.span,
            _ => continue,
        };
        let Some((path, text, base)) = sources.file_at(span) else { continue };
        let Ok(start) = usize::try_from(span.start - base) else { continue };
        let Some((from, first, to)) = lines_of(text, start) else { continue };
        if public(text, first) || used(sources, path, (from, to), &names) {
            continue;
        }
        let at = |offset: usize| u32::try_from(offset).ok().map(|offset| base + offset);
        let (Some(from), Some(to)) = (at(from), at(to)) else { continue };
        let diagnostic = Diagnostic::new(format!("nothing uses the {kind} `{name}`"), span)
            .with_fix(Span { start: from, end: to }, "");
        found.push(Lint::new("unused-item", diagnostic));
    }
    found
}
