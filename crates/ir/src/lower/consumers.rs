//! What uses a generator written right inside it, as a loop with nothing collected: `sum`,
//! `any` and `all`. Over a stage, the loop fuses like any other.

use fennec_check::Type;
use fennec_syntax::{Arg, BinOp, CompKind, Expr, ExprKind, Span, Stmt, StmtKind};

use super::{Lowered, Lowering, Tail};
use crate::Operand;

/// A generator written in place, in its parts.
struct Written<'e> {
    elem: &'e Expr,
    names: &'e [String],
    iter: &'e Expr,
    cond: Option<&'e Expr>,
}

fn written(value: &Expr) -> Option<Written<'_>> {
    let ExprKind::Comprehension { kind: CompKind::Generator, elem, names, iter, cond, .. } = &value.kind
    else {
        return None;
    };
    Some(Written { elem, names, iter, cond: cond.as_deref() })
}

fn stmt(kind: StmtKind, span: Span) -> Stmt {
    Stmt { kind, span }
}

fn name_at(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Name(name.to_owned()), span }
}

/// `name = value`, for a name made here, whose span says so by matching its value's.
fn binding(name: &str, value: Expr, span: Span) -> Stmt {
    let target = name_at(name, value.span);
    stmt(StmtKind::Assign { target, op: None, value, declare_mut: true, ty: None }, span)
}

fn setting(name: &str, to: bool, span: Span) -> Stmt {
    let value = Expr { kind: ExprKind::Bool(to), span };
    let target = name_at(name, span);
    let kind = StmtKind::Assign { target, op: None, value, declare_mut: false, ty: None };
    stmt(kind, span)
}

impl Lowering<'_> {
    /// `sum`, `any` or `all` of a generator written in place, if this is one.
    pub(super) fn consumer(&mut self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Lowered<Option<Operand>> {
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        if self.lookup(name).is_some() || self.context.functions.contains(name.as_str()) {
            return Ok(None);
        }
        let Some(Arg { name: None, is_mut: false, value }) = args.first() else { return Ok(None) };
        let Some(parts) = written(value) else { return Ok(None) };
        match (name.as_str(), args) {
            ("sum", [_] | [_, Arg { name: None, is_mut: false, .. }]) => {
                self.summing(expr, &parts, args.get(1))
            }
            ("any" | "all", [_]) => self.deciding(name == "any", &parts, expr.span).map(Some),
            _ => Ok(None),
        }
    }

    fn summing(&mut self, expr: &Expr, parts: &Written<'_>, start: Option<&Arg>) -> Lowered<Option<Operand>> {
        let ty = self.ty(expr)?;
        if !matches!(ty, Type::Int | Type::Float) {
            return Ok(None);
        }
        let span = Span { start: expr.span.start, end: expr.span.start };
        let total = format!("\0sum{}", expr.span.start);
        let first = start.map_or_else(
            || Expr { kind: if ty == Type::Float { ExprKind::Float(0.0) } else { ExprKind::Int(0) }, span },
            |start| start.value.clone(),
        );
        let target = name_at(&total, expr.span);
        let add = StmtKind::Assign {
            target,
            op: Some(BinOp::Add),
            value: parts.elem.clone(),
            declare_mut: false,
            ty: None,
        };
        let body = with_condition(parts.cond, vec![stmt(add, expr.span)], expr.span);
        self.looping_for_value(&total, first, parts, body, expr.span).map(Some)
    }

    fn deciding(&mut self, any: bool, parts: &Written<'_>, at: Span) -> Lowered<Operand> {
        let span = Span { start: at.start, end: at.start };
        let found = format!("\0decided{}", at.start);
        let first = Expr { kind: ExprKind::Bool(!any), span };
        let leave = vec![setting(&found, any, span), stmt(StmtKind::Break, span)];
        let decide = if any {
            StmtKind::If { branches: vec![(parts.elem.clone(), leave)], otherwise: None }
        } else {
            let pass = vec![stmt(StmtKind::Pass, span)];
            StmtKind::If { branches: vec![(parts.elem.clone(), pass)], otherwise: Some(leave) }
        };
        let body = with_condition(parts.cond, vec![stmt(decide, span)], span);
        self.looping_for_value(&found, first, parts, body, span)
    }

    /// Gives a hidden name its first value, runs the loop, and gives back what it ends up holding.
    fn looping_for_value(
        &mut self,
        name: &str,
        first: Expr,
        parts: &Written<'_>,
        body: Vec<Stmt>,
        span: Span,
    ) -> Lowered<Operand> {
        let start = binding(name, first, span);
        let looped =
            StmtKind::For { names: parts.names.to_vec(), is_mut: false, iter: parts.iter.clone(), body };
        self.stmts(&[start, stmt(looped, span)], Tail::Drop)?;
        let local = self.lookup(name).ok_or("a consumer lost its value")?;
        Ok(Operand::Local(local))
    }
}

fn with_condition(cond: Option<&Expr>, body: Vec<Stmt>, span: Span) -> Vec<Stmt> {
    match cond {
        Some(cond) => {
            let kind = StmtKind::If { branches: vec![(cond.clone(), body)], otherwise: None };
            vec![stmt(kind, span)]
        }
        None => body,
    }
}
