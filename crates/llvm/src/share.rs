//! What a unit adds to what units share, kept as it's added: every type by its layout's name, so
//! it means the same in any build of the program, and every place in the program's own files
//! from the start of the function it's in, so text added above that function moves it with the
//! function. A build that uses the unit again without writing it adds the same (edit builds,
//! stage 1), and the entry then defines every symbol the unit's object uses.

use std::collections::HashMap;

use fennec_ir::{Function, Program};
use fennec_layout::{Asked, Layouts, ListItem, Named, NamedWord, Word};
use fennec_syntax::Span;

use crate::{CheckKind, CheckLeft, Shared};

/// A place in the source as a unit keeps it: this far from the start of the function it's in,
/// or, for a place that isn't kept as moving with that function's text (in the standard library,
/// at the very start of the program's text, or in a function with no other place in the
/// program's files), as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Place {
    From(i64),
    At(u32),
}

/// Where a function's text starts, for its places to be kept from: the first place in the
/// program's own files its instructions have, or `None` if it has none.
pub(crate) fn anchor(function: &Function) -> Option<u32> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .map(|inst| inst.span.start)
        .filter(|start| moves(*start))
        .min()
}

/// Whether a place starting at `at` is kept as moving with the text around it: one in the
/// program's own files, past its very first byte. A place at byte 0, as a default span is and as
/// `main`'s whole definition is when it's the file's first line, is kept as it is, by its start
/// alone, so the anchor, each place and a function's key always agree on it: such a function is
/// written again when text above it moves, rather than used again with a place left behind.
fn moves(at: u32) -> bool {
    at != 0 && at < Span::PRELUDE
}

/// `at` as a unit keeps it, in a function whose text starts at `anchor`.
pub(crate) fn place(at: u32, anchor: Option<u32>) -> Place {
    match anchor {
        Some(anchor) if moves(at) => Place::From(i64::from(at) - i64::from(anchor)),
        _ => Place::At(at),
    }
}

/// Where a kept place is now, in a function whose text starts at `anchor` now.
fn placed(place: Place, anchor: Option<u32>) -> Option<u32> {
    match place {
        Place::At(at) => Some(at),
        Place::From(offset) => u32::try_from(i64::from(anchor?).checked_add(offset)?).ok(),
    }
}

/// One thing a unit added to what units share, in the order it added it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Added {
    String(String, String),
    Literal(String, String),
    /// A kind of list item's symbol, and the kind.
    Element(String, Named),
    /// A boxed type's shape's symbol, and the type.
    Shape(String, String),
    Dict(String, NamedWord, NamedWord),
    /// A function's symbol for where it starts, the function, and that start.
    Start(String, String, Place),
    Ran(String),
    /// A function's table of places: its symbol, the function, and the places.
    Places(String, String, Vec<Place>),
    /// A check written: the function it's in, where it starts, how long it is, and what it's for.
    Check(String, Place, u32, CheckKind),
    /// What's at a place that says so, in a build that traces costs. Such a build never uses a
    /// unit again, so its places are kept as they are.
    Site(u32, String),
}

/// What a unit added, or `None` once it added something that has no name to keep it by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Share {
    pub(crate) added: Option<Vec<Added>>,
}

impl Share {
    /// A share that keeps what's added to it, or, in a build that doesn't keep units, nothing.
    pub(crate) fn new(keeping: bool) -> Self {
        Share { added: keeping.then(Vec::new) }
    }

    /// Whether this keeps what's added to it.
    pub(crate) fn keeping(&self) -> bool {
        self.added.is_some()
    }

    pub(crate) fn add(&mut self, added: Option<Added>) {
        match added {
            Some(added) => {
                if let Some(all) = &mut self.added {
                    all.push(added);
                }
            }
            None => self.added = None,
        }
    }

    /// Adds to `shared` what was kept here, in order, with every type found in `layouts`, which
    /// gives out no new number (the unit's asking, asked again first, numbered every kind it
    /// shares), and every place from where its function starts now. `None` if something isn't
    /// found, or if this share couldn't be kept.
    pub(crate) fn add_to(
        &self,
        shared: &mut Shared,
        layouts: &Layouts,
        anchor_of: &dyn Fn(&str) -> Option<Option<u32>>,
    ) -> Option<()> {
        for added in self.added.as_ref()? {
            match added {
                Added::String(symbol, text) => {
                    shared.strings.insert(symbol.clone(), text.clone());
                }
                Added::Literal(symbol, text) => {
                    shared.literals.insert(symbol.clone(), text.clone());
                }
                Added::Element(symbol, item) => {
                    let item = ListItem::of(layouts.found(item)?)?;
                    shared.elements.insert(symbol.clone(), item);
                }
                Added::Shape(symbol, name) => {
                    shared.shapes.insert(symbol.clone(), layouts.id(name)?);
                }
                Added::Dict(symbol, key, value) => {
                    let words = (layouts.found_word(key)?, layouts.found_word(value)?);
                    shared.dicts.insert(symbol.clone(), words);
                }
                Added::Start(symbol, function, start) => {
                    let start = placed(*start, anchor_of(function)?)?;
                    shared.starts.insert(symbol.clone(), start);
                }
                Added::Ran(function) => {
                    shared.ran.insert(function.clone());
                }
                Added::Places(symbol, function, places) => {
                    let anchor = anchor_of(function)?;
                    let places = places.iter().map(|place| placed(*place, anchor)).collect::<Option<_>>()?;
                    shared.places.insert(symbol.clone(), places);
                }
                Added::Check(function, start, length, what) => {
                    let start = placed(*start, anchor_of(function)?)?;
                    let span = Span { start, end: start.checked_add(*length)? };
                    shared.checks.push(CheckLeft { function: function.clone(), span, what: what.clone() });
                }
                Added::Site(at, what) => {
                    shared.sites.entry(*at).or_insert_with(|| what.clone());
                }
            }
        }
        Some(())
    }
}

/// A kind of list item, by names.
pub(crate) fn element(layouts: &Layouts, symbol: &str, item: ListItem) -> Option<Added> {
    Some(Added::Element(symbol.to_owned(), layouts.named(item.ty())?))
}

/// A dict's words, by names.
pub(crate) fn dict(layouts: &Layouts, symbol: &str, (key, value): (Word, Word)) -> Option<Added> {
    Some(Added::Dict(symbol.to_owned(), layouts.named_word(key)?, layouts.named_word(value)?))
}

/// A boxed type's shape, by the type's name.
pub(crate) fn shape(layouts: &Layouts, symbol: &str, id: u32) -> Option<Added> {
    Some(Added::Shape(symbol.to_owned(), layouts.get(id)?.name.clone()))
}

/// What writing one unit read of the whole program, what it added to what units share, and what
/// it asked the layouts to number: everything a build would need to use it again.
#[derive(Debug)]
pub(crate) struct Kept {
    pub(crate) function: String,
    #[cfg_attr(
        not(all(test, not(windows))),
        expect(
            dead_code,
            reason = "a build using units again reads these, in the next step; tests read them where emit's run"
        )
    )]
    pub(crate) reads: Vec<(crate::reads::What, String)>,
    pub(crate) share: Share,
    /// `None` if it asked for a number for something with no name.
    pub(crate) asked: Option<Vec<Asked>>,
    /// What writing it added to what units share, on its own: what the audit checks its share
    /// against.
    pub(crate) wrote: Shared,
}

/// An empty `Shared` with `shared`'s settings, for one unit to be written into on its own.
pub(crate) fn settings_of(shared: &Shared) -> Shared {
    let Shared { traced, tracing_costs, noting_checks, proofs, facts, native_blocks, .. } = shared;
    Shared {
        traced: *traced,
        tracing_costs: *tracing_costs,
        noting_checks: *noting_checks,
        proofs: *proofs,
        facts: *facts,
        native_blocks: *native_blocks,
        ..Shared::default()
    }
}

/// Adds what one unit wrote, on its own, to what units share, as writing it into `shared` would:
/// every map's entry put in, the last one written standing, checks after the others', and a
/// place's site kept as first said.
pub(crate) fn merge(shared: &mut Shared, wrote: &Shared) {
    let Shared { strings, literals, elements, shapes, dicts, starts, ran, places, checks, sites, .. } = wrote;
    shared.strings.extend(strings.iter().map(|(key, value)| (key.clone(), value.clone())));
    shared.literals.extend(literals.iter().map(|(key, value)| (key.clone(), value.clone())));
    shared.elements.extend(elements.iter().map(|(key, value)| (key.clone(), *value)));
    shared.shapes.extend(shapes.iter().map(|(key, value)| (key.clone(), *value)));
    shared.dicts.extend(dicts.iter().map(|(key, value)| (key.clone(), *value)));
    shared.starts.extend(starts.iter().map(|(key, value)| (key.clone(), *value)));
    shared.ran.extend(ran.iter().cloned());
    shared.places.extend(places.iter().map(|(key, value)| (key.clone(), value.clone())));
    shared.checks.extend(checks.iter().cloned());
    for (at, what) in sites {
        shared.sites.entry(*at).or_insert_with(|| what.clone());
    }
}

/// Checks, unit by unit and in order, that what each one kept is all it added, as a build using
/// it again would need. Layouts are made afresh, as the build made them; each unit's asking is
/// asked again of them, then what it kept is added to an empty `Shared` with every kind found and
/// no number given out, which has to be exactly what writing that unit added. At the end the
/// layouts asked again number every kind as writing numbered it. A dev build of `fennec` checks
/// every program it builds this way, and an error names the unit.
pub(crate) fn audit(program: &Program, layouts: &Layouts, kept: &[Kept]) -> Result<(), String> {
    let functions: HashMap<&str, &Function> = program
        .functions
        .iter()
        .chain(&program.constants)
        .map(|function| (function.name.as_str(), function))
        .collect();
    let anchor_of = |name: &str| functions.get(name).map(|function| anchor(function));
    // Made as the build made them, with what it worked out from them before writing any unit.
    let afresh = crate::layouts_of(program)?;
    let _ = crate::native::native_types(program, &afresh);
    for Kept { function, share, asked, wrote, .. } in kept {
        // A kind with no name is one these layouts never numbered: a bug, never a unit to skip.
        let asked = asked
            .as_deref()
            .ok_or_else(|| format!("`{function}` asked for a number of a kind with no name"))?;
        if share.added.is_none() {
            return Err(format!("`{function}` shared a kind with no name"));
        }
        afresh.ask_again(asked).ok_or_else(|| format!("`{function}` asked for what has no layout"))?;
        let mut again = Shared::default();
        share
            .add_to(&mut again, &afresh, &anchor_of)
            .ok_or_else(|| format!("`{function}` kept what it shares in a way that doesn't add again"))?;
        if written(&again) != written(wrote) {
            return Err(format!("what `{function}` kept isn't what it added to what units share"));
        }
    }
    if afresh.fingerprint() != layouts.fingerprint() {
        return Err("asking again for what the units asked numbers the layouts another way".to_owned());
    }
    Ok(())
}

/// What units add to what they share.
fn written(shared: &Shared) -> impl PartialEq + '_ {
    (
        (&shared.strings, &shared.literals, &shared.elements, &shared.shapes, &shared.dicts),
        (&shared.starts, &shared.ran, &shared.places, &shared.checks, &shared.sites),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit written on its own is written with every setting of the build it's in: a setting
    /// left out would write it as a build without it, untraced, watching native blocks a release
    /// build leaves alone, or handing LLVM facts it was asked to strip, and nothing would say so.
    /// Only the settings come along; what units add starts empty.
    #[test]
    fn a_unit_of_its_own_is_written_with_every_setting_of_the_build() {
        let mut build = Shared {
            traced: true,
            tracing_costs: true,
            noting_checks: true,
            proofs: crate::Proofs::Checked,
            facts: crate::Facts::Stripped,
            native_blocks: crate::NativeBlocks::Trusted,
            ..Shared::default()
        };
        build.strings.insert("s_kept".to_owned(), "kept".to_owned());
        build.ran.insert("main".to_owned());
        let own = settings_of(&build);
        assert!(own.traced && own.tracing_costs && own.noting_checks, "{own:?}");
        assert_eq!(
            (own.proofs, own.facts, own.native_blocks),
            (crate::Proofs::Checked, crate::Facts::Stripped, crate::NativeBlocks::Trusted)
        );
        assert!(own.strings.is_empty() && own.ran.is_empty(), "{own:?}");
    }
}
