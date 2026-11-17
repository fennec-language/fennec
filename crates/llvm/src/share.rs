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
/// or, for a place that doesn't move with that function's text (in the standard library, made up
/// by lowering, or in a function with no place in the program's files), as it is.
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
        .map(|inst| inst.span)
        .filter(|span| moves(*span))
        .map(|span| span.start)
        .min()
}

/// Whether a place is in the program's own files, so it moves with the text around it.
fn moves(span: Span) -> bool {
    span != Span::default() && !span.in_prelude()
}

/// `at` as a unit keeps it, in a function whose text starts at `anchor`.
pub(crate) fn place(at: u32, anchor: Option<u32>) -> Place {
    match anchor {
        Some(anchor) if moves(Span { start: at, end: at }) => Place::From(i64::from(at) - i64::from(anchor)),
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

    /// Adds to `shared` what was kept here, in order, with every type resolved in `layouts`
    /// and every place from where its function starts now. `None` if something doesn't resolve,
    /// or if this share couldn't be kept.
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
                    let item = ListItem::of(layouts.resolved(item)?)?;
                    shared.elements.insert(symbol.clone(), item);
                }
                Added::Shape(symbol, name) => {
                    shared.shapes.insert(symbol.clone(), layouts.id(name)?);
                }
                Added::Dict(symbol, key, value) => {
                    let words = (layouts.resolved_word(key)?, layouts.resolved_word(value)?);
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
        not(test),
        expect(dead_code, reason = "a build using units again reads these, in the next step")
    )]
    pub(crate) reads: Vec<(crate::reads::What, String)>,
    pub(crate) share: Share,
    /// `None` if it asked for a number for something with no name.
    pub(crate) asked: Option<Vec<Asked>>,
}

/// Checks that what each unit kept is all it added, as a build using the units again would need:
/// what they kept, added again to an empty `Shared` in order, is what writing them added to
/// `shared`; and what they asked the layouts to number, asked again in order of layouts made
/// afresh, numbers every kind as writing them did. A dev build of `fennec` checks every program
/// it builds this way.
pub(crate) fn audit(
    program: &Program,
    shared: &Shared,
    layouts: &Layouts,
    kept: &[Kept],
) -> Result<(), String> {
    // A unit that asked for, or shared, a kind with no name to keep it by is never used again, so
    // a build that has one writes it every time, and there's nothing kept to check.
    if kept.iter().any(|kept| kept.share.added.is_none() || kept.asked.is_none()) {
        return Ok(());
    }
    let functions: HashMap<&str, &Function> = program
        .functions
        .iter()
        .chain(&program.constants)
        .map(|function| (function.name.as_str(), function))
        .collect();
    let anchor_of = |name: &str| functions.get(name).map(|function| anchor(function));
    let mut again = Shared::default();
    for Kept { function, share, .. } in kept {
        share
            .add_to(&mut again, layouts, &anchor_of)
            .ok_or_else(|| format!("`{function}` didn't keep all it shares"))?;
    }
    if written(&again) != written(shared) {
        return Err("what the units kept isn't what they added to what they share".to_owned());
    }
    // Made as the build made them, with what it worked out from them before writing any unit.
    let afresh = crate::layouts_of(program)?;
    let _ = crate::native::native_types(program, &afresh);
    for Kept { function, asked, .. } in kept {
        let asked = asked.as_deref().unwrap_or_default();
        afresh.ask_again(asked).ok_or_else(|| format!("`{function}` asked for what has no layout"))?;
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
