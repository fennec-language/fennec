//! `double-lookup`: `counts[key] if key in counts else 0` looks `key` up twice to say what
//! `counts.get(key) else 0` says once, without an index that could stop the program. Only a
//! `dict` is sure to mean the same by `in`, `[]` and `get`, so this looks at a program that
//! checks, and at names and fields, which give the same thing each time they're read. A dict of
//! optional values is left alone: there a key holding `None` gives `None` written the first way,
//! and the default written the second. Each fix is tried, as `needless-conversion`'s is. Where a
//! place is named it's left alone too, as `needless-conversion` leaves it.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Module, Sources, Span};

use crate::Lint;
use crate::conversions::still_fine;
use crate::copies::places;
use crate::rewrites::{replaceable, written};

/// Whether reading an expression twice gives the same thing and does nothing else.
pub(crate) fn pure(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) | ExprKind::Int(_) | ExprKind::Str(_) => true,
        ExprKind::Attr { object, optional: false, .. } => pure(object),
        _ => false,
    }
}

/// A conditional that looks a key up after asking whether it's there, as the table, the key,
/// and what it gives when it isn't.
fn lookup<'e>(expr: &'e Expr, sources: &Sources) -> Option<(&'e Expr, &'e Expr, &'e Expr)> {
    let ExprKind::Cond { then, cond, otherwise } = &expr.kind else { return None };
    let ExprKind::Binary { op, lhs: asked, rhs: table } = &cond.kind else { return None };
    let (read, default) = match op {
        BinOp::In => (then, otherwise),
        BinOp::NotIn => (otherwise, then),
        _ => return None,
    };
    let ExprKind::Index { object, index } = &read.kind else { return None };
    let same = |one: &Expr, other: &Expr| {
        pure(one) && written(sources, one.span).is_some_and(|text| Some(text) == written(sources, other.span))
    };
    (same(asked, index) && same(table, object)).then_some((table, asked, default))
}

/// `table.get(key) else default`, or just `table.get(key)` when the default is `None`.
fn get(sources: &Sources, (table, key, default): (&Expr, &Expr, &Expr), span: Span) -> Expr {
    let callee = ExprKind::Attr { object: Box::new(table.clone()), name: "get".to_owned(), optional: false };
    let arg = Arg { name: None, is_mut: false, value: key.clone() };
    let call = Expr {
        kind: ExprKind::Call { callee: Box::new(Expr { kind: callee, span: table.span }), args: vec![arg] },
        span,
    };
    if matches!(default.kind, ExprKind::None) {
        return call;
    }
    let default = match lookup(default, sources) {
        Some(parts) => get(sources, parts, default.span),
        None => default.clone(),
    };
    Expr { kind: ExprKind::Else { value: Box::new(call), default: Box::new(default), except: false }, span }
}

/// The module with the conditional at `span` written with `get`.
fn with_get(module: &Module, sources: &Sources, span: Span) -> Module {
    let mut trial = module.clone();
    fennec_syntax::visit::exprs_mut(&mut trial, &mut |expr| {
        if expr.span == span
            && let Some(parts) = lookup(expr, sources)
        {
            *expr = get(sources, parts, span);
        }
    });
    trial
}

/// The conditional as `get` writes it, unless a bracket or a comment would be lost.
fn rewritten(sources: &Sources, whole: &Expr, table: &Expr, key: &Expr, default: &Expr) -> Option<String> {
    if !replaceable(sources, whole)
        || !replaceable(sources, default)
        || written(sources, whole.span)?.contains('#')
    {
        return None;
    }
    let call = format!("{}.get({})", written(sources, table.span)?, written(sources, key.span)?);
    if matches!(default.kind, ExprKind::None) {
        return Some(call);
    }
    // A lookup after it is written the same way, so a chain becomes one, and any other one-line
    // `if` goes in parentheses, since it can't share a line with a fallback `else` without them.
    let rest = match lookup(default, sources) {
        Some((table, key, inner)) => rewritten(sources, default, table, key, inner)?,
        None if matches!(default.kind, ExprKind::Cond { .. }) => {
            format!("({})", written(sources, default.span)?)
        }
        None => written(sources, default.span)?,
    };
    Some(format!("{call} else {rest}"))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut candidates: Vec<(Expr, Span, String, String)> = Vec::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        if !expr.span.in_prelude()
            && let Some((table, key, default)) = lookup(expr, sources)
            && let Some(fixed) = rewritten(sources, expr, table, key, default)
            && let Some(key) = written(sources, key.span)
        {
            candidates.push((table.clone(), expr.span, key, fixed));
        }
    });
    if candidates.is_empty() {
        return Vec::new();
    }
    let Ok(checked) = fennec_check::check_typed(module) else { return Vec::new() };
    let types: HashMap<(u32, u32), &Type> = checked
        .types
        .iter()
        .flat_map(|(_, body)| body.exprs())
        .map(|(span, ty)| ((span.start, span.end), ty))
        .collect();
    let places = places(module);
    let mut found = Vec::new();
    for (table, span, key, fixed) in candidates {
        let dict = types.get(&(table.span.start, table.span.end)).is_some_and(|ty| {
            matches!(ty, Type::Dict(_, value) if !matches!(**value, Type::Optional(_) | Type::None | Type::Unknown))
        });
        if dict && !places.contains(&(span.start, span.end)) && still_fine(with_get(module, sources, span)) {
            let message = format!("this looks `{key}` up twice: write `{fixed}`");
            found.push(Lint::new("double-lookup", Diagnostic::new(message, span).with_fix(span, fixed)));
        }
    }
    found
}
