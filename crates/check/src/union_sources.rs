//! A `with` source every member of a union has, as `with store.transaction()? as tx:` on a
//! `Lite | Postgres`, runs the source of the member the value holds. Which one that is isn't known
//! until the program runs, and a source is written in where it's used, so the `with` is written in
//! once for each member, under an `is` for it:
//!
//! ```fennec
//! if store is Lite:
//!     with store.transaction()? as tx:
//!         ...
//! else:
//!     with store.transaction()? as tx:
//!         ...
//! ```
//!
//! Checked again, `store` is narrowed in each, so each calls its member's own source, which is
//! written in as any other is.

use std::collections::BTreeMap;

use fennec_syntax::{BinOp, Expr, ExprKind, Module, Span, Stmt, StmtKind};

use crate::lend_lowering::{below_written, here, name, reserved, stmt, written_in};

/// What a `with` source called on a union is written in with: the name it's called on, and each
/// member's type, in the union's order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Dispatch {
    pub(crate) path: String,
    pub(crate) members: Vec<String>,
}

/// Writes each `with` over a source called on a union in once for each member, saying whether
/// there were any.
pub(crate) fn lower(module: &mut Module, found: &BTreeMap<(u32, u32), Dispatch>) -> bool {
    if found.is_empty() {
        return false;
    }
    below_written(module);
    let mut wrote = false;
    fennec_syntax::visit::functions_mut(module, &mut |def| wrote |= stmts(&mut def.body, found));
    wrote
}

fn stmts(body: &mut [Stmt], found: &BTreeMap<(u32, u32), Dispatch>) -> bool {
    let mut wrote = false;
    for each in body {
        wrote |= inner(each, found);
        if let StmtKind::With { value, names, lock: false, .. } = &each.kind
            && names.len() <= 1
            && let Some(at) = called(value)
            // One written in already is narrowed: found again, it wasn't, and isn't written in twice.
            && !written_in(Span { start: at.0, end: at.1 })
            && let Some(dispatch) = found.get(&at)
        {
            *each = dispatched(each, dispatch);
            wrote = true;
        }
    }
    wrote
}

/// Writes in those inside a statement's blocks, the `match` cases of one that's a `match` among them.
fn inner(each: &mut Stmt, found: &BTreeMap<(u32, u32), Dispatch>) -> bool {
    match &mut each.kind {
        StmtKind::If { branches, otherwise } => {
            let mut wrote = false;
            for (_, body) in branches {
                wrote |= stmts(body, found);
            }
            if let Some(body) = otherwise {
                wrote |= stmts(body, found);
            }
            wrote
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            stmts(body, found)
        }
        StmtKind::OnFailure { cleanup, body, .. } => stmts(cleanup, found) | stmts(body, found),
        StmtKind::Lend { otherwise, .. } => stmts(otherwise, found),
        StmtKind::Def(def) => stmts(&mut def.body, found),
        StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
            let mut wrote = false;
            for arm in arms {
                wrote |= stmts(&mut arm.body, found);
            }
            wrote
        }
        _ => false,
    }
}

/// The span of the call a `with` takes, through a `?` after it.
fn called(value: &Expr) -> Option<(u32, u32)> {
    let call = match &value.kind {
        ExprKind::Try { value: inner, .. } => inner.as_ref(),
        _ => value,
    };
    matches!(call.kind, ExprKind::Call { .. }).then_some((call.span.start, call.span.end))
}

/// `if path is First: with ... elif path is Second: with ... else: with ...`, each `with` a
/// copy of `with_stmt` on spans of its own.
fn dispatched(with_stmt: &Stmt, dispatch: &Dispatch) -> Stmt {
    let at = with_stmt.span;
    let mut copies: Vec<Vec<Stmt>> = dispatch.members.iter().map(|_| vec![apart(with_stmt)]).collect();
    let last = copies.pop().unwrap_or_default();
    let branches = dispatch
        .members
        .iter()
        .zip(copies)
        .map(|(member, copy)| {
            let test = ExprKind::Binary {
                op: BinOp::Is,
                lhs: Box::new(path_expr(&dispatch.path, at)),
                rhs: Box::new(name(member, at)),
            };
            (Expr { kind: test, span: here(at) }, copy)
        })
        .collect();
    stmt(StmtKind::If { branches, otherwise: Some(last) }, at)
}

/// A name, or a path of fields from one, as an expression.
fn path_expr(path: &str, at: Span) -> Expr {
    let mut parts = path.split('.');
    let mut expr = name(parts.next().unwrap_or_default(), at);
    for field in parts {
        let kind = ExprKind::Attr { object: Box::new(expr), name: field.to_owned(), optional: false };
        expr = Expr { kind, span: here(at) };
    }
    expr
}

/// A copy of a statement on spans of its own, so what's found by a span in one copy, like the
/// source a call is to, is never another's: the program's text keeps where each part is against
/// the rest, and each span written in before gets a new one.
fn apart(original: &Stmt) -> Stmt {
    let mut copy = original.clone();
    let body = std::slice::from_mut(&mut copy);
    let (mut low, mut high) = (u32::MAX, 0);
    fennec_syntax::visit::respan_stmts(body, &mut |span| {
        if own(span) {
            low = low.min(span.start);
            high = high.max(span.end);
        }
        span
    });
    let base = if low > high { 0 } else { reserved(high - low + 1) };
    let mut written: BTreeMap<(u32, u32), Span> = BTreeMap::new();
    fennec_syntax::visit::respan_stmts(body, &mut |span| {
        if own(span) {
            Span { start: span.start - low + base, end: span.end - low + base }
        } else if written_in(span) {
            *written.entry((span.start, span.end)).or_insert_with(|| {
                let at = reserved(0);
                Span { start: at, end: at }
            })
        } else {
            span
        }
    });
    copy
}

/// Whether a span is the program's own text, rather than the standard library's or written in.
fn own(span: Span) -> bool {
    !span.in_prelude() && !written_in(span)
}
