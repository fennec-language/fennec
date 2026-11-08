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

use fennec_syntax::{Arg, BinOp, Expr, ExprKind, FnDef, Module, Stmt, StmtKind};

use crate::lend_lowering::{assign, int, name, stmt};

/// Writes in each `for at, ref item in enumerate(items)`, and says whether there was one.
pub(crate) fn lower(module: &mut Module) -> bool {
    let mut made = 0;
    fennec_syntax::visit::functions_mut(module, &mut |def| function(def, &mut made));
    for item in &mut module.items {
        if let fennec_syntax::Item::Test(test) = item {
            stmts(&mut test.body, &mut made);
        }
    }
    made > 0
}

fn function(def: &mut FnDef, made: &mut usize) {
    stmts(&mut def.body, made);
}

fn stmts(body: &mut Vec<Stmt>, made: &mut usize) {
    let mut at = 0;
    while at < body.len() {
        let Some(each) = body.get_mut(at) else { return };
        inner(each, made);
        match counted(each, made) {
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
fn inner(each: &mut Stmt, made: &mut usize) {
    match &mut each.kind {
        StmtKind::If { branches, otherwise } => {
            for (_, body) in branches {
                stmts(body, made);
            }
            if let Some(body) = otherwise {
                stmts(body, made);
            }
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => {
            stmts(body, made);
        }
        StmtKind::Lend { otherwise, .. } => stmts(otherwise, made),
        StmtKind::Def(def) => function(def, made),
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
                stmts(&mut arm.body, made);
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
fn counted(each: &Stmt, made: &mut usize) -> Option<Vec<Stmt>> {
    let StmtKind::For { names, is_mut: true, iter, body } = &each.kind else { return None };
    let [at, item] = names.as_slice() else { return None };
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let (ExprKind::Name(called), [Arg { value: items, .. }]) = (&callee.kind, args.as_slice()) else {
        return None;
    };
    if called != "enumerate" {
        return None;
    }
    *made += 1;
    let span = each.span;
    let count = format!("{at}__at{made}");
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
