//! Every place that refers to what a name refers to, for find-references and rename. Each place
//! the name is written as a word, in any of the program's files, is looked up the way go to
//! definition looks a name up, and kept when it reaches the same declaration. Lookup knows
//! scopes and types, so a local of the same name elsewhere, or another type's member, isn't
//! counted.
//!
//! Loading a kit leaves its files' imports of each other out of the module, so the names an
//! import lists are found from each file's own text. A kit's files share their top-level names,
//! so a name an import lists is that one: the standard library's for an import from `std`, and
//! the program's own otherwise.

use std::collections::BTreeSet;

use fennec_syntax::{Item, Sources, Span};

use crate::analysis::Analysis;
use crate::declaration::declarations;
use crate::definition::{Target, imported_names, name_in, target};

/// Each name an import in the program's files lists: the name, where it's written, and whether
/// the import is from the standard library.
pub(crate) fn imported(sources: &Sources) -> Vec<(String, Span, bool)> {
    let mut found = Vec::new();
    for (path, text) in sources.files() {
        let Some((_, base)) = sources.by_path(path) else { continue };
        let Ok(module) = fennec_syntax::parse_at(text, base) else { continue };
        for item in &module.items {
            let Item::Import(import) = item else { continue };
            let from_std = import.module.first().is_some_and(|first| first == "std");
            let listed = imported_names(sources, import.span);
            for name in &import.names {
                found.push((name.clone(), name_in(sources, listed, name), from_std));
            }
        }
    }
    found
}

/// What the name an import lists at `at` refers to: a top-level name.
pub(crate) fn import_at(analysis: &Analysis, at: u32) -> Option<Target> {
    let (name, ..) =
        imported(&analysis.sources).into_iter().find(|(_, span, _)| span.start <= at && at <= span.end)?;
    Some(Target::Global(name))
}

/// Where each declaration a target may refer to has its name, as start and end.
fn declared(analysis: &Analysis, found: &Target) -> BTreeSet<(u32, u32)> {
    let Some(module) = &analysis.module else { return BTreeSet::new() };
    declarations(found, module, &analysis.sources, analysis.checked.as_ref())
        .iter()
        .map(|declaration| (declaration.name_at.start, declaration.name_at.end))
        .collect()
}

/// Where `name` is written as a whole word in `text`, by byte offset.
fn words<'t>(text: &'t str, name: &'t str) -> impl Iterator<Item = usize> + 't {
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    text.match_indices(name).map(|(at, _)| at).filter(move |at| {
        let before = text.get(..*at).and_then(|text| text.chars().next_back());
        let after = text.get(at + name.len()..).and_then(|text| text.chars().next());
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// Every place in the program's own files that refers to what `found` does, its declaration
/// included, in the order the files and places come.
pub(crate) fn references(analysis: &Analysis, found: &Target) -> Vec<Span> {
    let Some(module) = &analysis.module else { return Vec::new() };
    let wanted = declared(analysis, found);
    if wanted.is_empty() {
        return Vec::new();
    }
    let name = found.name();
    let length = u32::try_from(name.len()).unwrap_or(0);
    let mut places = Vec::new();
    if let Target::Global(_) = found {
        let library = wanted.iter().all(|(start, end)| Span { start: *start, end: *end }.in_prelude());
        let listed = imported(&analysis.sources)
            .into_iter()
            .filter(|(listed, _, from_std)| listed == name && *from_std == library);
        places.extend(listed.map(|(_, span, _)| span));
    }
    for (path, text) in analysis.sources.files() {
        for offset in words(text, name) {
            let Some(at) = analysis.span_at(path, offset) else { continue };
            let Some(here) = target(module, &analysis.sources, at) else { continue };
            if declared(analysis, &here).intersection(&wanted).next().is_some() {
                places.push(Span { start: at, end: at + length });
            }
        }
    }
    places.sort_by_key(|span| (span.start, span.end));
    places.dedup();
    places
}

/// Whether `name` can be what something is renamed to: a name, and not a word the language
/// keeps for itself, which can't be assigned to.
pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let starts = chars.next().is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
    starts
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && fennec_syntax::parse(&format!("{name} = 1\n")).is_ok()
}

/// Whether what `found` refers to is the program's own, so renaming it changes only the
/// program: none of its declarations is the standard library's.
pub(crate) fn own(analysis: &Analysis, found: &Target) -> bool {
    let Some(module) = &analysis.module else { return false };
    let all = declarations(found, module, &analysis.sources, analysis.checked.as_ref());
    !all.is_empty() && all.iter().all(|declaration| !declaration.name_at.in_prelude())
}
