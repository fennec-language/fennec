//! Finds what a function body does without running it: whether it can fail (it contains
//! `fail` or `?`) and whether it's a generator (it contains `yield`). Nested functions and
//! arrow functions are their own bodies, so they aren't searched.

use fennec_syntax::{Expr, ExprKind, FStrPart, Stmt, StmtKind};

#[derive(Default)]
struct Effects {
    fails: bool,
    yields: bool,
}

/// Returns `(fallible, generator)`.
pub(crate) fn body_effects(body: &[Stmt]) -> (bool, bool) {
    let mut effects = Effects::default();
    for stmt in body {
        stmt_effects(stmt, &mut effects);
    }
    (effects.fails, effects.yields)
}

fn stmt_effects(stmt: &Stmt, effects: &mut Effects) {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Assert(expr) | StmtKind::Return(Some(expr)) => {
            expr_effects(expr, effects);
        }
        StmtKind::Fail(expr) => {
            effects.fails = true;
            expr_effects(expr, effects);
        }
        StmtKind::Yield(expr) => {
            effects.yields = true;
            expr_effects(expr, effects);
        }
        StmtKind::Assign { target, value, .. } => {
            expr_effects(target, effects);
            expr_effects(value, effects);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                expr_effects(cond, effects);
                block_effects(body, effects);
            }
            if let Some(body) = otherwise {
                block_effects(body, effects);
            }
        }
        StmtKind::While { cond: expr, body }
        | StmtKind::For { iter: expr, body, .. }
        | StmtKind::With { value: expr, body, .. } => {
            expr_effects(expr, effects);
            block_effects(body, effects);
        }
        StmtKind::Return(None) | StmtKind::Def(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {
        }
    }
}

fn block_effects(body: &[Stmt], effects: &mut Effects) {
    for stmt in body {
        stmt_effects(stmt, effects);
    }
}

fn expr_effects(expr: &Expr, effects: &mut Effects) {
    let mut visit = |inner: &Expr| expr_effects(inner, effects);
    match &expr.kind {
        ExprKind::Try { value, context } => {
            effects.fails = true;
            expr_effects(value, effects);
            if let Some(context) = context {
                expr_effects(context, effects);
            }
        }
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr, .. } = part {
                    visit(expr);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => items.iter().for_each(visit),
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                visit(key);
                visit(value);
            }
        }
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            visit(elem);
            value.as_deref().into_iter().chain([iter.as_ref()]).chain(cond.as_deref()).for_each(visit);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Attr { object: inner, .. } => visit(inner),
        ExprKind::Binary { lhs: a, rhs: b, .. }
        | ExprKind::Else { value: a, default: b }
        | ExprKind::Index { object: a, index: b } => {
            visit(a);
            visit(b);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for part in [then, cond, otherwise] {
                visit(part);
            }
        }
        ExprKind::Call { callee, args } => {
            visit(callee);
            for arg in args {
                visit(&arg.value);
            }
        }
        ExprKind::Slice { object, start, end } => {
            visit(object);
            start.as_deref().into_iter().chain(end.as_deref()).for_each(visit);
        }
        ExprKind::Match { subject, arms } => {
            visit(subject);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    expr_effects(guard, effects);
                }
                block_effects(&arm.body, effects);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Name(_)
        | ExprKind::Variant(_)
        | ExprKind::Lambda { .. } => {}
    }
}
