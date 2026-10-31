//! Inlining: lowering a small function's body right where it's called, so `a + b` on two
//! `Vec3`s is three additions instead of a call. Only small bodies of numbers, bools and
//! plain structs are inlined. A function goes into itself only one level deep, which saves
//! the calls that return straight away in something like `fib`.

use std::rc::Rc;

use cranelift_codegen::ir::Value;
use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use super::Lowerer;
use super::expr::Lowered;
use super::structs::Place;
use crate::types::{Function, Ty};

/// How many syntax nodes a body may have and still be inlined.
const MAX_NODES: usize = 48;

/// How deeply inlined calls may nest inside each other.
const MAX_DEPTH: usize = 4;

/// The body to inline at calls to a function, if it's small and simple enough.
pub(super) fn inline_body(function: &Function<'_>) -> Option<Rc<FnDef>> {
    // Only `self` may be `mut`: an inlined method then changes the caller's struct directly.
    let params_fit = function.params.iter().enumerate().all(|(at, param)| match param.ty {
        Ty::Struct(_) => {
            !param.is_mut || (at == 0 && function.def.params.first().is_some_and(|first| first.is_self))
        }
        Ty::Int | Ty::Float | Ty::Bool => !param.is_mut,
        Ty::Cap(_) => true,
        Ty::Unit | Ty::Obj | Ty::Boxed(_) | Ty::Str | Ty::Dict(..) | Ty::List(_) => false,
    });
    // A struct's body keeps its statements: what it gives back is checked by the last one.
    let body = if matches!(function.ret, Ty::Struct(_)) {
        function.def.body.clone()
    } else {
        without_returns(&function.def.body)
    };
    let mut nodes = 0;
    let simple = body.iter().all(|stmt| simple_stmt(stmt, &mut nodes));
    (params_fit
        && !function.fallible
        && !matches!(function.ret, Ty::Obj | Ty::Boxed(_) | Ty::Str | Ty::Dict(..) | Ty::List(_))
        && simple
        && nodes <= MAX_NODES)
        .then(|| Rc::new(FnDef { body, ..function.def.clone() }))
}

/// A body with `return`s written as the values of its branches, where that says the same:
/// `if c: return x`, then the rest, is `if c: x else: the rest`. Any `return` it can't
/// rewrite is left in, and stops the body being inlined.
fn without_returns(stmts: &[Stmt]) -> Vec<Stmt> {
    let mut written = Vec::with_capacity(stmts.len());
    for (at, stmt) in stmts.iter().enumerate() {
        let rest = stmts.get(at + 1..).unwrap_or_default();
        match &stmt.kind {
            StmtKind::Return(value) if rest.is_empty() => written.extend(
                value.as_ref().map(|value| Stmt { kind: StmtKind::Expr(value.clone()), span: stmt.span }),
            ),
            StmtKind::If { branches, otherwise: None } if branches.iter().all(|(_, body)| leaves(body)) => {
                let kind = StmtKind::If {
                    branches: branches
                        .iter()
                        .map(|(cond, body)| (cond.clone(), without_returns(body)))
                        .collect(),
                    otherwise: Some(without_returns(rest)),
                };
                written.push(Stmt { kind, span: stmt.span });
                return written;
            }
            StmtKind::If { branches, otherwise: Some(otherwise) } if rest.is_empty() => {
                let kind = StmtKind::If {
                    branches: branches
                        .iter()
                        .map(|(cond, body)| (cond.clone(), without_returns(body)))
                        .collect(),
                    otherwise: Some(without_returns(otherwise)),
                };
                written.push(Stmt { kind, span: stmt.span });
            }
            _ => written.push(stmt.clone()),
        }
    }
    written
}

/// Whether a block ends by returning, on every path through it.
fn leaves(stmts: &[Stmt]) -> bool {
    match stmts.last().map(|stmt| &stmt.kind) {
        Some(StmtKind::Return(_)) => true,
        Some(StmtKind::If { branches, otherwise: Some(otherwise) }) => {
            branches.iter().all(|(_, body)| leaves(body)) && leaves(otherwise)
        }
        _ => false,
    }
}

fn simple_stmt(stmt: &Stmt, nodes: &mut usize) -> bool {
    *nodes += 1;
    match &stmt.kind {
        StmtKind::Expr(expr) => simple_expr(expr, nodes),
        StmtKind::Assign { target, value, .. } => {
            matches!(target.kind, ExprKind::Name(_) | ExprKind::Attr { .. })
                && simple_expr(target, nodes)
                && simple_expr(value, nodes)
        }
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(cond, body)| {
                simple_expr(cond, nodes) && body.iter().all(|stmt| simple_stmt(stmt, nodes))
            }) && otherwise.iter().flatten().all(|stmt| simple_stmt(stmt, nodes))
        }
        StmtKind::Pass => true,
        _ => false,
    }
}

/// Numbers, names, operators, fields and calls: what native code does without the runtime.
fn simple_expr(expr: &Expr, nodes: &mut usize) -> bool {
    *nodes += 1;
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Name(_)
        | ExprKind::Variant(_) => true,
        ExprKind::Unary { expr, .. } | ExprKind::Attr { object: expr, optional: false, .. } => {
            simple_expr(expr, nodes)
        }
        ExprKind::Binary { lhs, rhs, .. } => simple_expr(lhs, nodes) && simple_expr(rhs, nodes),
        ExprKind::Cond { then, cond, otherwise } => {
            simple_expr(then, nodes) && simple_expr(cond, nodes) && simple_expr(otherwise, nodes)
        }
        ExprKind::Call { callee, args } => {
            matches!(
                callee.kind,
                ExprKind::Name(_) | ExprKind::Attr { optional: false, .. } | ExprKind::Variant(_)
            ) && simple_expr(callee, nodes)
                && args.iter().all(|arg| !arg.is_mut && simple_expr(&arg.value, nodes))
        }
        ExprKind::Match { subject, arms } => {
            simple_expr(subject, nodes)
                && arms.iter().all(|arm| {
                    *nodes += 1;
                    arm.body.iter().all(|stmt| simple_stmt(stmt, nodes))
                })
        }
        _ => false,
    }
}

impl Lowerer<'_, '_> {
    /// The body to inline for a call to `key` from here, if any.
    pub(super) fn inlinable(&self, key: &str) -> Option<Rc<FnDef>> {
        if self.inlining.len() >= MAX_DEPTH
            || (self.function.key == key && !self.inlining.is_empty())
            || self.inlining.iter().any(|(open, _)| open == key)
        {
            return None;
        }
        self.shared.compiled.get(key)?.inline.clone()
    }

    /// Where a failure at `span` is reported: inside an inlined standard library function,
    /// at the program's call to it.
    pub(super) fn site(&self, span: Span) -> Span {
        match self.call_site {
            Some(site) if span.in_prelude() => site,
            _ => span,
        }
    }

    /// Lowers a call to `key` by lowering its body here, with its parameters bound to
    /// `values`, which are in the order a real call would pass them. For a method that
    /// changes `self`, `self_place` is where the caller holds the struct.
    pub(super) fn inline_call(
        &mut self,
        key: &str,
        def: &FnDef,
        values: Vec<Value>,
        self_place: Option<Place>,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let compiled = self.shared.compiled.get(key).map(|compiled| (compiled.params.clone(), compiled.ret));
        let (params, ret) = compiled.ok_or_else(|| super::internal("unknown function", span))?;
        self.inlining.push((key.to_owned(), self.scopes.len()));
        let outer_site = self.call_site;
        let outer_origin = self.enter_origin(key, def);
        if self.call_site.is_none() && !span.in_prelude() {
            self.call_site = Some(span);
        }
        self.open_scope();
        let mut values = values.into_iter();
        let mut self_place = self_place;
        let mut bound = Ok(());
        for (name, ty, is_mut) in &params {
            let value = if matches!(ty, Ty::Cap(_)) { None } else { values.next() };
            bound = bound.and_then(|()| match (ty, value, self_place.take()) {
                (Ty::Struct(id), _, Some(place)) => {
                    self.bind_place(name, place, *id, true);
                    Ok(())
                }
                (Ty::Struct(id), Some(value), None) => self.declare_struct(name, value, *id, *is_mut),
                _ => {
                    self.bind_native(name, *ty, value, *is_mut);
                    Ok(())
                }
            });
        }
        let lowered = bound.and_then(|()| self.tail(&def.body));
        let closed = self.close_scope();
        self.inlining.pop();
        self.call_site = outer_site;
        self.inlined_origin = outer_origin;
        let (value, ty) = lowered?;
        closed?;
        match ret {
            Ty::Unit | Ty::Cap(_) => Ok((None, Ty::Unit)),
            Ty::Struct(id) => {
                let value = self.convert(value, ty, ret, span)?;
                // A body that ends by naming a struct, like `self`, gives back a copy of it.
                let names_one = def.body.last().is_some_and(|stmt| {
                    matches!(&stmt.kind, StmtKind::Expr(expr) if matches!(expr.kind, ExprKind::Name(_) | ExprKind::Attr { .. }))
                });
                let value = if names_one { self.clone_struct(value, id)? } else { value };
                Ok((Some(value), ret))
            }
            _ => Ok((Some(self.convert(value, ty, ret, span)?), ret)),
        }
    }
}
