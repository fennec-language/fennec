//! `for at, ref item in enumerate(items):` changes each item where it is, with its position.
//! Once checked, it's written in as a `for ref` that counts, which every engine already runs:
//!
//! ```fennec
//! mut at__at1 = -1
//! for ref item in items:
//!     at__at1 += 1
//!     at = at__at1
//!     ...
//! ```
//!
//! The count goes up first, so a `continue` in the body still counts the item it skips.

use std::collections::BTreeMap;

use fennec_syntax::{Arg, BinOp, Expr, ExprKind, FnDef, Module, REF, Stmt, StmtKind, TypeExpr};

use crate::lend_lowering::{assign, here, int, name, stmt};

/// Writes in each `for at, ref item in enumerate(items)`, and says whether there was one.
pub(crate) fn lower(module: &mut Module, items: &BTreeMap<(u32, u32), bool>) -> bool {
    let mut cx = Cx { made: 0, items };
    fennec_syntax::visit::functions_mut(module, &mut |def| function(def, &mut cx));
    for item in &mut module.items {
        if let fennec_syntax::Item::Test(test) = item {
            stmts(&mut test.body, &mut cx);
        }
    }
    cx.made > 0
}

/// How many loops are written in so far, which numbers the names each makes, and each
/// `for key, ref value in x.items()` the checker found, by its span: whether `x` is a `Table`.
struct Cx<'i> {
    made: usize,
    items: &'i BTreeMap<(u32, u32), bool>,
}

fn function(def: &mut FnDef, cx: &mut Cx<'_>) {
    stmts(&mut def.body, cx);
}

fn stmts(body: &mut Vec<Stmt>, cx: &mut Cx<'_>) {
    let mut at = 0;
    while at < body.len() {
        let Some(each) = body.get_mut(at) else { return };
        inner(each, cx);
        match counted(each, cx) {
            Some(written) => {
                let count = written.len();
                body.splice(at..=at, written);
                at += count;
            }
            None => at += 1,
        }
    }
}

/// The blocks inside a statement, written in first.
fn inner(each: &mut Stmt, cx: &mut Cx<'_>) {
    match &mut each.kind {
        StmtKind::If { branches, otherwise } => {
            for (_, body) in branches {
                stmts(body, cx);
            }
            if let Some(body) = otherwise {
                stmts(body, cx);
            }
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => {
            stmts(body, cx);
        }
        StmtKind::Lend { otherwise, .. } => stmts(otherwise, cx),
        StmtKind::Def(def) => function(def, cx),
        _ => {}
    }
    let mut arms = Vec::new();
    for value in crate::loops::exprs(each) {
        arms.extend(matches_in(value));
    }
    if arms.is_empty() {
        return;
    }
    // The `match`es are found by span, then walked again to change them.
    fennec_syntax::visit::in_stmts(std::slice::from_mut(each), &mut |expr| {
        if arms.contains(&(expr.span.start, expr.span.end))
            && let ExprKind::Match { arms, .. } = &mut expr.kind
        {
            for arm in arms {
                stmts(&mut arm.body, cx);
            }
        }
    });
}

/// The spans of the `match`es in an expression, whose cases hold statements.
fn matches_in(value: &Expr) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    if matches!(value.kind, ExprKind::Match { .. }) {
        found.push((value.span.start, value.span.end));
    }
    for part in crate::effects::children(value) {
        found.extend(matches_in(part));
    }
    found
}

/// `for at, ref item in enumerate(items)` written in as a counter and a `for ref`, or nothing.
fn counted(each: &Stmt, cx: &mut Cx<'_>) -> Option<Vec<Stmt>> {
    if let Some(table) = cx.items.get(&(each.span.start, each.span.end)).copied() {
        return items_by_place(each, table, cx);
    }
    let StmtKind::For { names, is_mut: true, iter, body } = &each.kind else { return None };
    let [at, item] = names.as_slice() else { return None };
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let (ExprKind::Name(called), [Arg { value: items, .. }]) = (&callee.kind, args.as_slice()) else {
        return None;
    };
    if called != "enumerate" {
        return None;
    }
    cx.made += 1;
    let span = each.span;
    let count = format!("{at}__at{}", cx.made);
    let start = assign(&count, int(-1, span), true, None);
    let up = StmtKind::Assign {
        target: name(&count, span),
        op: Some(BinOp::Add),
        value: int(1, span),
        declare_mut: false,
        ty: None,
    };
    let mut looped = vec![stmt(up, span), assign(at, name(&count, span), false, None)];
    looped.extend(body.iter().cloned());
    let kind = StmtKind::For { names: vec![item.clone()], is_mut: true, iter: items.clone(), body: looped };
    Some(vec![start, Stmt { kind, span }])
}

/// `for key, ref value in x.items()` written in as a loop over `x`'s ids, for a `Table`, or over
/// its keys taken first, for a dict, with `ref value = x[key]` the place itself:
///
/// ```fennec
/// for id in units.ids():
///     ref unit = units[id]
///     ...
/// ```
///
/// A dict's keys are taken into a name of their own first, so the loop isn't over the dict the
/// body changes a value of.
fn items_by_place(each: &Stmt, table: bool, cx: &mut Cx<'_>) -> Option<Vec<Stmt>> {
    let StmtKind::For { names, is_mut: true, iter, body } = &each.kind else { return None };
    let [key, item] = names.as_slice() else { return None };
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let ExprKind::Attr { object: held, name: called, optional: false } = &callee.kind else { return None };
    if called != "items" || !args.is_empty() {
        return None;
    }
    cx.made += 1;
    let span = each.span;
    let method = |name: &str| {
        let attr = Expr {
            kind: ExprKind::Attr { object: held.clone(), name: name.to_owned(), optional: false },
            span: here(span),
        };
        Expr { kind: ExprKind::Call { callee: Box::new(attr), args: Vec::new() }, span: here(span) }
    };
    let mut written = Vec::new();
    let over = if table {
        method("ids")
    } else {
        let keys = format!("{key}__keys{}", cx.made);
        written.push(assign(&keys, method("keys"), false, None));
        name(&keys, span)
    };
    let place = Expr {
        kind: ExprKind::Index { object: held.clone(), index: Box::new(name(key, span)) },
        span: here(span),
    };
    let ref_type = TypeExpr { name: REF.to_owned(), args: Vec::new(), optional: false, span: here(span) };
    let mut looped = vec![assign(item, place, false, Some(ref_type))];
    looped.extend(body.iter().cloned());
    let kind = StmtKind::For { names: vec![key.clone()], is_mut: false, iter: over, body: looped };
    written.push(Stmt { kind, span });
    Some(written)
}
