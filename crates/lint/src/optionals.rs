//! `optional-of-optional`: a `T??` written by hand in a field's, a parameter's or a return's type,
//! or in a type alias. It holds two kinds of nothing, `None` and an optional holding `None`, told
//! apart only by how many `?` a reader counts. An enum names each kind instead.
//!
//! Only what's written is looked at, so a `T??` that comes from generics, like `pop()` on a
//! `list[int?]` or a channel's `receive()` of an `int?`, is never flagged, and nor is a local, whose
//! type is worked out. An alias the program declares to an optional, written with a `?` after it,
//! is one too.

use std::collections::BTreeSet;

use fennec_syntax::{Diagnostic, FnDef, Item, Module, OPTIONAL, Sources, Span, TypeExpr};

use crate::Lint;
use crate::sizes::functions;

pub(crate) fn of(module: &Module, sources: &Sources, optional_aliases: &[String]) -> Vec<Lint> {
    let mut found = BTreeSet::new();
    let mut each = |ty: &TypeExpr| written(ty, optional_aliases, &mut found);
    for def in functions(module) {
        signature(def, &mut each);
    }
    for item in &module.items {
        match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                let fields = def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields));
                fields.filter_map(|field| field.ty.as_ref()).for_each(&mut each);
            }
            Item::Interface(interface) => interface.methods.iter().for_each(|def| signature(def, &mut each)),
            Item::TypeAlias { ty, .. } => each(ty),
            _ => {}
        }
    }
    found
        .into_iter()
        .map(|(start, end)| Lint::new("optional-of-optional", said(Span { start, end }, sources)))
        .collect()
}

/// The aliases among `items` that stand for an optional, through other aliases too: `Seen` after
/// `type Seen = int?`.
pub(crate) fn aliases(items: &[Item]) -> Vec<String> {
    let declared: Vec<(&str, &TypeExpr)> = items
        .iter()
        .filter_map(|item| match item {
            Item::TypeAlias { name, ty, .. } => Some((name.as_str(), ty)),
            _ => None,
        })
        .collect();
    let mut optional: BTreeSet<String> = BTreeSet::new();
    loop {
        let more: Vec<&str> = declared
            .iter()
            .filter(|(name, ty)| !optional.contains(*name) && is_optional(ty, &optional))
            .map(|(name, _)| *name)
            .collect();
        if more.is_empty() {
            return optional.into_iter().collect();
        }
        optional.extend(more.into_iter().map(str::to_owned));
    }
}

/// Each type a function's signature writes: its parameters' and its return's.
fn signature(def: &FnDef, each: &mut impl FnMut(&TypeExpr)) {
    def.params.iter().filter_map(|param| param.ty.as_ref()).for_each(&mut *each);
    if let Some(ret) = &def.ret {
        each(ret);
    }
}

/// Whether a written type is an optional: `int?`, `int??`, or an alias of one.
fn is_optional(ty: &TypeExpr, aliases: &BTreeSet<String>) -> bool {
    ty.optional || ty.name == OPTIONAL || (ty.args.is_empty() && aliases.contains(&ty.name))
}

/// Where `ty`, and each type written inside it, is an optional of an optional: `int??`, or
/// `Seen?` for an alias of an optional. `int???` is found once.
fn written(ty: &TypeExpr, aliases: &[String], found: &mut BTreeSet<(u32, u32)>) {
    let optional_alias = ty.optional && ty.args.is_empty() && aliases.contains(&ty.name);
    let mut held = ty;
    if ty.name == OPTIONAL || optional_alias {
        found.insert((ty.span.start, ty.span.end));
        while held.name == OPTIONAL {
            let Some(inner) = held.args.first() else { return };
            held = inner;
        }
    }
    for arg in &held.args {
        written(arg, aliases, found);
    }
}

fn said(at: Span, sources: &Sources) -> Diagnostic {
    let text = sources
        .file_at(at)
        .and_then(|(_, text, base)| {
            let from = usize::try_from(at.start.checked_sub(base)?).ok()?;
            let to = usize::try_from(at.end.checked_sub(base)?).ok()?;
            text.get(from..to)
        })
        .map_or_else(|| "this type".to_owned(), |text| format!("`{text}`"));
    Diagnostic::new(
        format!("{text} is an optional of an optional: its two kinds of nothing are told apart only by how many `?` there are"),
        at,
    )
    .with_help(
        "an enum that names each kind of nothing says which one it is, like `enum Reading:` with the \
         cases `Unread`, `Blank` and `Read(value: int)`, in place of an `int??` whose outer `None` \
         is unread and inner `None` blank",
    )
}
