//! What a function body does, found without checking it: whether it can fail (it contains
//! `fail` or `?`) and whether it's a generator (it contains `yield`). This is part of a
//! function's signature, so it's known before its body is checked, even when it calls itself.
//! Nested functions and arrow functions are their own bodies, so they aren't searched.

use fennec_syntax::{Expr, ExprKind, FStrPart, Stmt, StmtKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Effects {
    pub(crate) fails: bool,
    pub(crate) yields: bool,
}

pub(crate) fn of_body(body: &[Stmt]) -> Effects {
    let mut effects = Effects::default();
    block(body, &mut effects);
    effects
}

fn block(body: &[Stmt], effects: &mut Effects) {
    for stmt in body {
        statement(stmt, effects);
    }
}

fn statement(stmt: &Stmt, effects: &mut Effects) {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Assert(expr) | StmtKind::Return(Some(expr)) => {
            expression(expr, effects);
        }
        StmtKind::Fail(expr) => {
            effects.fails = true;
            expression(expr, effects);
        }
        StmtKind::Yield(expr) => {
            effects.yields = true;
            expression(expr, effects);
        }
        StmtKind::Assign { target, value, .. } => {
            expression(target, effects);
            expression(value, effects);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                expression(cond, effects);
                block(body, effects);
            }
            block(otherwise.as_deref().unwrap_or_default(), effects);
        }
        StmtKind::While { cond: expr, body }
        | StmtKind::For { iter: expr, body, .. }
        | StmtKind::With { value: expr, body, .. } => {
            expression(expr, effects);
            block(body, effects);
        }
        StmtKind::Return(None) | StmtKind::Def(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {
        }
    }
}

fn expression(expr: &Expr, effects: &mut Effects) {
    for inner in children(expr) {
        expression(inner, effects);
    }
    match &expr.kind {
        ExprKind::Try { .. } => effects.fails = true,
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    expression(guard, effects);
                }
                block(&arm.body, effects);
            }
        }
        _ => {}
    }
}

/// The expressions directly inside an expression, except an arrow function's body and a
/// match's arms (which are statements).
pub(crate) fn children(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::FStr(parts) => parts
            .iter()
            .filter_map(|part| match part {
                FStrPart::Expr { expr, .. } => Some(expr),
                FStrPart::Text(_) => None,
            })
            .collect(),
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => items.iter().collect(),
        ExprKind::Dict(pairs) => pairs.iter().flat_map(|(key, value)| [key, value]).collect(),
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            [Some(elem.as_ref()), value.as_deref(), Some(iter.as_ref()), cond.as_deref()]
                .into_iter()
                .flatten()
                .collect()
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Attr { object: inner, .. } => vec![inner],
        ExprKind::Binary { lhs: a, rhs: b, .. }
        | ExprKind::Else { value: a, default: b }
        | ExprKind::Index { object: a, index: b } => vec![a, b],
        ExprKind::Cond { then, cond, otherwise } => vec![then, cond, otherwise],
        ExprKind::Try { value, context } => {
            [Some(value.as_ref()), context.as_deref()].into_iter().flatten().collect()
        }
        ExprKind::Call { callee, args } => {
            std::iter::once(callee.as_ref()).chain(args.iter().map(|arg| &arg.value)).collect()
        }
        ExprKind::Slice { object, start, end } => {
            [Some(object.as_ref()), start.as_deref(), end.as_deref()].into_iter().flatten().collect()
        }
        ExprKind::Match { subject, .. } => vec![subject],
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Name(_)
        | ExprKind::Variant(_)
        | ExprKind::Lambda { .. } => Vec::new(),
    }
}

/// Whether a loop body contains a `break` for this loop (not one inside a nested loop).
pub(crate) fn breaks(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Break => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| breaks(body)) || otherwise.as_deref().is_some_and(breaks)
        }
        StmtKind::With { body, .. } => breaks(body),
        StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
            arms.iter().any(|arm| breaks(&arm.body))
        }
        _ => false,
    })
}
