//! A generic function that calls a `with` source an interface gives its type parameter, as
//! `with db.transaction()? as tx:` for `db: D` with `D: Database`, can't have the source written
//! in where it's used: which type's source it is depends on the call. So it's copied for each
//! type it's called with, `D` written in as that type, and each call made to its own copy. Checked
//! again, each copy calls that type's own source, which is written in as any other is.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use fennec_syntax::{Expr, ExprKind, FnDef, Item, Module, Span, TypeExpr};

use crate::lend_lowering::{below_written, reserved, retype, retype_one, written_in, written_type};
use crate::types::Type;

/// A call to a generic function: the function it's made in, the one called, and the types the
/// called one's type parameters are there, in order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Call {
    pub(crate) caller: String,
    pub(crate) callee: String,
    pub(crate) types: Vec<Type>,
}

/// Copies each generic function in `sourcing` for each type it's called with, and makes each such
/// call to its copy, saying whether any was. A generic function that calls one of them with its
/// own type parameter is copied too, so its copies call the first's.
pub(crate) fn lower(
    module: &mut Module,
    sourcing: &BTreeSet<String>,
    calls: &BTreeMap<(u32, u32), Call>,
) -> bool {
    if sourcing.is_empty() {
        return false;
    }
    let mut needing = sourcing.clone();
    loop {
        let before = needing.len();
        let callers: Vec<String> = calls
            .values()
            .filter(|call| needing.contains(&call.callee) && call.types.iter().any(Type::has_params))
            .map(|call| call.caller.clone())
            .collect();
        needing.extend(callers);
        if needing.len() == before {
            break;
        }
    }
    let generic: BTreeMap<String, FnDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(def) if needing.contains(&def.name) && !def.generics.is_empty() => {
                Some((def.name.clone(), def.clone()))
            }
            _ => None,
        })
        .collect();
    if generic.is_empty() {
        return false;
    }
    let mut have: BTreeSet<String> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(def) => Some(def.name.clone()),
            _ => None,
        })
        .collect();
    below_written(module);
    let mut copies = Vec::new();
    let mut wrote = false;
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        let Some(call) = calls.get(&(expr.span.start, expr.span.end)) else { return };
        let Some(def) = generic.get(&call.callee) else { return };
        let ExprKind::Call { callee, .. } = &mut expr.kind else { return };
        if !names(callee, &call.callee) || call.types.iter().any(|ty| ty.has_params() || !ty.is_known()) {
            return;
        }
        let Some(name) = copy_name(&call.callee, &call.types) else { return };
        if !have.contains(&name) {
            let Some(copy) = copy_for(def, &call.types, &name) else { return };
            copies.push(Item::Function(copy));
            have.insert(name.clone());
        }
        callee.kind = ExprKind::Name(name);
        wrote = true;
    });
    module.items.extend(copies);
    wrote
}

/// Whether a call's callee is the function by its name, as `saved` or `saved[Lite]`.
fn names(callee: &Expr, function: &str) -> bool {
    match &callee.kind {
        ExprKind::Name(name) => name == function,
        ExprKind::Index { object, .. } => matches!(&object.kind, ExprKind::Name(name) if name == function),
        _ => false,
    }
}

/// The name of a function's copy for some types: its own, then each type's text with every
/// character but a letter or digit written as its number, so no two types' copies share one.
fn copy_name(function: &str, types: &[Type]) -> Option<String> {
    let mut name = format!("{function}__for");
    for ty in types {
        name.push('_');
        for c in ty.source()?.chars() {
            if c.is_ascii_alphanumeric() {
                name.push(c);
            } else {
                let _ = write!(name, "_{:x}_", u32::from(c));
            }
        }
    }
    Some(name)
}

/// A generic function's copy for some types, named `name`: each type parameter written in as its
/// type, in what it takes, gives and fails with, and in its body, on spans of its own.
fn copy_for(def: &FnDef, types: &[Type], name: &str) -> Option<FnDef> {
    if def.generics.len() != types.len() {
        return None;
    }
    let mut given = BTreeMap::new();
    for (generic, ty) in def.generics.iter().zip(types) {
        given.insert(generic.name.clone(), written_type(ty)?);
    }
    let mut copy = def.clone();
    name.clone_into(&mut copy.name);
    copy.generics.clear();
    let written = copy.params.iter_mut().filter_map(|param| param.ty.as_mut());
    for ty in written.chain(copy.ret.as_mut()).chain(copy.fails.as_mut()) {
        retype_one(ty, &given);
    }
    retype(&mut copy.body, &given);
    apart(&mut copy);
    Some(copy)
}

/// Moves a copy's text to spans of its own, so what's found by a span in one copy, like the
/// source a call is to, is never another's: the program's text keeps where each part is against
/// the rest, and each span written in before gets a new one.
fn apart(copy: &mut FnDef) {
    let (mut low, mut high) = (u32::MAX, 0);
    fennec_syntax::visit::in_function(copy, &mut |expr| {
        if own(expr.span) {
            low = low.min(expr.span.start);
            high = high.max(expr.span.end);
        }
    });
    let base = if low > high { 0 } else { reserved(high - low + 1) };
    let mut written: BTreeMap<(u32, u32), Span> = BTreeMap::new();
    fennec_syntax::visit::in_function(copy, &mut |expr| {
        let span = expr.span;
        if own(span) {
            expr.span = Span { start: span.start - low + base, end: span.end - low + base };
        } else if written_in(span) {
            let start = *written.entry((span.start, span.end)).or_insert_with(|| {
                let at = reserved(0);
                Span { start: at, end: at }
            });
            expr.span = start;
        }
    });
    let moved = |span: Span| {
        if own(span) { Span { start: span.start - low + base, end: span.end - low + base } } else { span }
    };
    for param in &mut copy.params {
        if let Some(ty) = param.ty.as_mut() {
            respan(ty, &moved);
        }
    }
}

/// Whether a span is the program's own text, rather than the standard library's or written in.
fn own(span: Span) -> bool {
    !span.in_prelude() && !written_in(span)
}

fn respan(written: &mut TypeExpr, moved: &dyn Fn(Span) -> Span) {
    written.span = moved(written.span);
    for arg in &mut written.args {
        respan(arg, moved);
    }
}
