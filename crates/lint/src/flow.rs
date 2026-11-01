//! Lints about which way code goes: what can't run, what can't differ, and branches that only
//! give back what their condition already says.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Module, Sources, Span, Stmt, StmtKind};

use crate::Lint;
use crate::rewrites::{operand, replaceable, written};
use crate::sizes::{blocks, end_of, functions};

/// Whether code after this statement in its block can never run.
fn leaves(stmt: &Stmt) -> bool {
    matches!(stmt.kind, StmtKind::Return(_) | StmtKind::Fail(_) | StmtKind::Break | StmtKind::Continue)
}

/// Code after a `return`, `fail`, `break` or `continue` in the same block.
fn unreachable(stmts: &[Stmt], found: &mut Vec<Lint>) {
    let dead = stmts.iter().position(leaves).and_then(|at| stmts.get(at + 1..)).unwrap_or_default();
    if let Some(first) = dead.first() {
        let span = Span { start: first.span.start, end: end_of(dead) };
        let diagnostic = Diagnostic::new("nothing can reach this", first.span).with_fix(span, "");
        found.push(Lint::new("unreachable", diagnostic));
    }
    for block in stmts.iter().flat_map(blocks) {
        unreachable(block, found);
    }
}

/// What a line gives back, if it's only a bool.
fn gives_bool(stmt: &Stmt) -> Option<bool> {
    let (StmtKind::Expr(value) | StmtKind::Return(Some(value))) = &stmt.kind else { return None };
    match value.kind {
        ExprKind::Bool(value) => Some(value),
        _ => None,
    }
}

/// A function that ends in an `if` returning `True` or `False`, whichever way it's written, then
/// gives back the other one.
fn return_bool(body: &[Stmt], sources: &Sources) -> Option<Lint> {
    let (last, before) = body.split_last()?;
    let (cond, then, otherwise, start) = match (&last.kind, before.last()) {
        (StmtKind::If { branches, otherwise: Some(otherwise) }, _) => {
            let [(cond, then)] = branches.as_slice() else { return None };
            let [otherwise] = otherwise.as_slice() else { return None };
            (cond, then, otherwise, last.span.start)
        }
        (_, Some(Stmt { kind: StmtKind::If { branches, otherwise: None }, span })) => {
            let [(cond, then)] = branches.as_slice() else { return None };
            (cond, then, last, span.start)
        }
        _ => return None,
    };
    let [then] = then.as_slice() else { return None };
    let (when_true, when_false) = (gives_bool(then)?, gives_bool(otherwise)?);
    if when_true == when_false {
        return None;
    }
    let fixed =
        if when_true { written(sources, cond.span)? } else { format!("not {}", operand(sources, cond)?) };
    // A block's span can run on to the end of its last line, which has to stay.
    let whole = Span { start, end: end_of(std::slice::from_ref(last)) };
    let text = written(sources, whole)?;
    let trailing = u32::try_from(text.len() - text.trim_end().len()).ok()?;
    let span = Span { start, end: whole.end - trailing };
    let diagnostic =
        Diagnostic::new(format!("this `if` only gives back its condition: write `{fixed}`"), span)
            .with_fix(span, fixed);
    Some(Lint::new("return-bool", diagnostic))
}

/// Whether an expression gives the same value every time it's looked at, as far as can be seen.
fn steady(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Attr { object, .. } => steady(object),
        ExprKind::Binary { lhs, rhs, .. } => steady(lhs) && steady(rhs),
        ExprKind::Unary { expr, .. } => steady(expr),
        kind => matches!(
            kind,
            ExprKind::Name(_)
                | ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Str(_)
                | ExprKind::Bool(_)
                | ExprKind::None
        ),
    }
}

/// The same condition twice in an `if` chain, so the second branch can never run, and an `if`
/// whose branches are the same.
fn same_branches(stmts: &[Stmt], sources: &Sources, found: &mut Vec<Lint>) {
    for stmt in stmts {
        if let StmtKind::If { branches, otherwise } = &stmt.kind {
            let mut seen: Vec<String> = Vec::new();
            for (cond, _) in branches {
                let Some(text) = written(sources, cond.span).filter(|_| steady(cond)) else { continue };
                if seen.contains(&text) {
                    let message =
                        format!("`{text}` was already tested earlier in this chain, so this can't run");
                    found.push(Lint::new("duplicate-condition", Diagnostic::new(message, cond.span)));
                }
                seen.push(text);
            }
            if let ([(_, then)], Some(otherwise)) = (branches.as_slice(), otherwise)
                && let (Some(a), Some(b)) = (body_text(then, sources), body_text(otherwise, sources))
                && a == b
            {
                let diagnostic = Diagnostic::new("both branches of this `if` do the same thing", stmt.span);
                found.push(Lint::new("identical-branches", diagnostic));
            }
        }
        for block in blocks(stmt) {
            same_branches(block, sources, found);
        }
    }
}

/// A block's source, to compare it with another at the same depth.
fn body_text(stmts: &[Stmt], sources: &Sources) -> Option<String> {
    let start = stmts.first()?.span.start;
    written(sources, Span { start, end: end_of(stmts) })
}

/// `0 <= x and x <= 10` as `0 <= x <= 10`.
fn chained_compare(expr: &Expr, sources: &Sources) -> Option<Lint> {
    let ExprKind::Binary { op: BinOp::And, lhs, rhs } = &expr.kind else { return None };
    let ExprKind::Binary { op: first, lhs: low, rhs: middle } = &lhs.kind else { return None };
    let ExprKind::Binary { op: second, lhs: again, rhs: high } = &rhs.kind else { return None };
    let upward = |op: &BinOp| matches!(op, BinOp::Lt | BinOp::LtEq);
    let downward = |op: &BinOp| matches!(op, BinOp::Gt | BinOp::GtEq);
    let same_way = (upward(first) && upward(second)) || (downward(first) && downward(second));
    if !same_way || !matches!(middle.kind, ExprKind::Name(_)) || !replaceable(sources, expr) {
        return None;
    }
    let shared = written(sources, middle.span)?;
    if shared != written(sources, again.span)? {
        return None;
    }
    let fixed = format!(
        "{} {} {shared} {} {}",
        operand(sources, low)?,
        first.symbol(),
        second.symbol(),
        operand(sources, high)?
    );
    let diagnostic =
        Diagnostic::new(format!("write this as `{fixed}`"), expr.span).with_fix(expr.span, fixed);
    Some(Lint::new("chained-compare", diagnostic))
}

/// `x if x is not None else d` as `x else d`.
fn manual_default(expr: &Expr, sources: &Sources) -> Option<Lint> {
    let ExprKind::Cond { then, cond, otherwise } = &expr.kind else { return None };
    let ExprKind::Binary { op, lhs: tested, rhs } = &cond.kind else { return None };
    if !matches!(rhs.kind, ExprKind::None) || !steady(tested) {
        return None;
    }
    let (value, default) = match op {
        BinOp::IsNot => (then, otherwise),
        BinOp::Is => (otherwise, then),
        _ => return None,
    };
    let name = written(sources, tested.span)?;
    if written(sources, value.span)? != name || !replaceable(sources, expr) {
        return None;
    }
    let fixed = format!("{name} else {}", operand(sources, default)?);
    let diagnostic =
        Diagnostic::new(format!("write this as `{fixed}`"), expr.span).with_fix(expr.span, fixed);
    Some(Lint::new("manual-default", diagnostic))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for def in functions(module) {
        unreachable(&def.body, &mut found);
        same_branches(&def.body, sources, &mut found);
        found.extend(return_bool(&def.body, sources));
    }
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        found.extend(chained_compare(expr, sources));
        found.extend(manual_default(expr, sources));
    });
    found
}
