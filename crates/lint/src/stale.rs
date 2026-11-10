//! `stale-copy`: a struct copied from one place into another, like
//! `world.selected = world.units[i]`, while what it came from can still change. Every copy is a
//! snapshot, so a later change to one won't reach the other. A program that meant a link wants a
//! `ref`, or the item's position or `Id`. It's a hint, since a snapshot may be what's meant, and
//! there's nothing else to write to say so.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Module, Sources, Span, Stmt, StmtKind};

use crate::Lint;
use crate::rewrites::written;
use crate::sizes::{blocks, functions};

/// A field or an item, reached from a name: `world.units[i]`, but not `world`.
fn stepped(expr: &Expr) -> Option<&str> {
    let mut at = expr;
    let mut steps = 0;
    loop {
        match &at.kind {
            ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. } => {
                steps += 1;
                at = object;
            }
            ExprKind::Name(name) if steps > 0 => return Some(name),
            _ => return None,
        }
    }
}

/// What holds an item: `world.units` for `world.units[i]`, or nothing for a field.
fn holder(expr: &Expr) -> Option<&Expr> {
    match &expr.kind {
        ExprKind::Index { object, .. } => Some(object),
        _ => None,
    }
}

/// The names in a function that can change what they hold: `ref` parameters, `ref self`, and
/// names declared `mut` or `ref`, or given by `for ref`.
fn changeable(def: &FnDef) -> HashSet<&str> {
    fn block<'a>(stmts: &'a [Stmt], found: &mut HashSet<&'a str>) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Assign { target, declare_mut: true, .. } => {
                    if let ExprKind::Name(name) = &target.kind {
                        found.insert(name);
                    }
                }
                StmtKind::For { names, is_mut: true, .. } => found.extend(names.iter().map(String::as_str)),
                kind => found.extend(kind.local_ref().map(|(name, _)| name)),
            }
            for inner in blocks(stmt) {
                block(inner, found);
            }
        }
    }
    let mut found: HashSet<&str> =
        def.params.iter().filter(|param| param.is_mut).map(|param| param.name.as_str()).collect();
    block(&def.body, &mut found);
    found
}

/// Each `target = value` in some statements that copies one place into another.
fn copies<'a>(stmts: &'a [Stmt], found: &mut Vec<(&'a Expr, &'a Expr)>) {
    for stmt in stmts {
        if let StmtKind::Assign { target, op: None, value, declare_mut: false, ty: None } = &stmt.kind
            && stepped(target).is_some()
            && stepped(value).is_some()
        {
            found.push((target, value));
        }
        for inner in blocks(stmt) {
            copies(inner, found);
        }
    }
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut candidates = Vec::new();
    for def in functions(module).into_iter().filter(|def| !def.span.in_prelude()) {
        let changes = changeable(def);
        let mut found = Vec::new();
        copies(&def.body, &mut found);
        let text = |expr: &Expr| written(sources, expr.span);
        for (target, value) in found {
            let from_changing = stepped(value).is_some_and(|root| changes.contains(root));
            // An item copied to another item of the same list, as a shift does, is the list's own.
            let same_holder = holder(target)
                .zip(holder(value))
                .is_some_and(|(one, other)| text(one).is_some() && text(one) == text(other));
            if from_changing && !same_holder {
                candidates.push((target.clone(), value.clone()));
            }
        }
    }
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
    let mut lints = Vec::new();
    for (target, value) in candidates {
        let Some(Type::Named(_)) = types.get(&(value.span.start, value.span.end)) else { continue };
        let (Some(to), Some(from)) = (written(sources, target.span), written(sources, value.span)) else {
            continue;
        };
        lints.push(Lint::hint("stale-copy", stale(&to, &from, value.span)));
    }
    lints
}

fn stale(to: &str, from: &str, at: Span) -> Diagnostic {
    let named = to.rsplit(['.', '[']).next().unwrap_or(to).trim_end_matches(']');
    Diagnostic::new(format!("`{to}` gets a copy of `{from}`, and a change to one won't reach the other"), at)
        .with_help(format!(
            "to keep track of it, hold its position or `Id`; to change it where it is, use `ref {named} = {from}`"
        ))
}
