//! `parallel(xs)` for a backend that runs it on threads. What runs for each item is lifted into
//! a function of its own, as an arrow function is: its first parameters take what it captures,
//! and its last the item. Then a builtin, given the list and a closure of that function, runs it
//! over the list's positions on as many threads as the backend likes:
//! - `for x in parallel(xs): body` becomes `parallel.for(xs, body)`. The checker has made sure
//!   the body writes only its own item and names, so its iterations are independent. A
//!   `for mut x` body changes its item, and `parallel.for` then changes the list, as
//!   `list.append` does.
//! - `[f(x) for x in parallel(xs)]` becomes `parallel.map(xs, f)`, a list of what `f` gives.
//! - `sum(f(x) for x in parallel(xs))` becomes `parallel.sum(xs, f, start)`, which adds what `f`
//!   gives in the runtime's fixed tree, so it's the same bit for bit however many threads run.
//!
//! This is done only when the backend asks (`Options::outline_parallel`), and only where the
//! lifted function can hold what's written: a body that leaves the loop early or the function,
//! yields, or can fail, and a comprehension or sum with an `if`, are lowered as before, which
//! runs the same way, in order.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind, Span, Stmt, StmtKind, visit};

use super::generics::FunctionInstance;
use super::{Lowered, Lowering, Tail};
use crate::{InstKind, Operand};

/// What a loop goes over when it's `parallel(xs)` with one plain argument: `xs`.
fn parallel_items(iter: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let ExprKind::Name(name) = &callee.kind else { return None };
    let [first] = args.as_slice() else { return None };
    (name == "parallel" && first.name.is_none() && !first.is_mut).then_some(&first.value)
}

/// Whether a body can be lifted into a function: it doesn't leave the loop early or the
/// function, yield, or fail. A nested function's or arrow function's own body doesn't count.
fn liftable(body: &[Stmt]) -> bool {
    body.iter().all(|stmt| match &stmt.kind {
        StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Return(_)
        | StmtKind::Yield(_)
        | StmtKind::Fail(_) => false,
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(_, block)| liftable(block)) && otherwise.as_deref().is_none_or(liftable)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            liftable(body)
        }
        _ => true,
    }) && !tries(body)
}

/// Whether a body uses `?` outside a nested function's own body.
fn tries(body: &[Stmt]) -> bool {
    let mut found = false;
    let mut written = body.to_vec();
    visit::in_stmts(&mut written, &mut |seen| found |= matches!(seen.kind, ExprKind::Try { .. }));
    found
}

/// What a lifted value makes: a list of each item's, or their sum from a start, `0` or `0.0`
/// when none is written.
#[derive(Clone, Copy)]
pub(super) enum Gives<'e> {
    List,
    Sum(Option<&'e Expr>),
}

/// What a lifted function runs for its item: a loop's body, which changes its item when it's a
/// `for mut`'s, or an expression whose value it gives back.
enum Runs<'e> {
    Body(&'e [Stmt], bool),
    Value(&'e Expr, Type),
}

impl Lowering<'_> {
    /// `for x in parallel(xs)` over a list, lifted, when the backend asks for it and the body
    /// can be. `None` when it's lowered as an ordinary loop.
    pub(super) fn parallel_for(
        &mut self,
        (names, is_mut): (&[String], bool),
        iter: &Expr,
        body: &[Stmt],
        span: Span,
    ) -> Option<Lowered<()>> {
        let (true, [name], true) = (self.context.options.outline_parallel, names, liftable(body)) else {
            return None;
        };
        // A `for mut` loop's `parallel(xs)` is just `xs` once checked, which the checker notes.
        // Its body changes its item, and the loop changes the list, which has to be a local.
        let (items, item) = if is_mut {
            let ExprKind::Name(list) = &iter.kind else { return None };
            self.lookup(list)?;
            match self.type_at(iter.span) {
                Some(Type::List(item)) if self.context.signatures.parallel_items(iter.span) => (iter, *item),
                _ => return None,
            }
        } else {
            self.parallel_list(iter)?
        };
        let lifted = self.lift(name, (items, item), &Runs::Body(body, is_mut), span);
        Some(lifted.map(|(list, closure)| {
            let args = vec![list, closure];
            let call =
                InstKind::Builtin { dest: None, name: "parallel.for".to_owned(), args, changes: is_mut };
            self.emit(call, span);
        }))
    }

    /// `[f(x) for x in parallel(xs)]` or `sum(f(x) for x in parallel(xs))`, lifted, when the
    /// backend asks for it: `parallel.map` or `parallel.sum`, as `gives` says. `None` when it's
    /// lowered as before.
    pub(super) fn parallel_value(
        &mut self,
        expr: &Expr,
        (elem, names, iter, cond): (&Expr, &[String], &Expr, Option<&Expr>),
        gives: Gives<'_>,
    ) -> Option<Lowered<Operand>> {
        let (true, [name], None) = (self.context.options.outline_parallel, names, cond) else { return None };
        let (items, item) = self.parallel_list(iter)?;
        let ty = self.type_at(expr.span)?;
        let each = match (&ty, gives) {
            (Type::List(each), Gives::List) => (**each).clone(),
            (Type::Int | Type::Float, Gives::Sum(_)) => ty.clone(),
            _ => return None,
        };
        let lifted = self.lift(name, (items, item), &Runs::Value(elem, each), expr.span);
        Some(lifted.and_then(|(list, closure)| {
            let dest = self.temporary(ty.clone());
            let (name, args) = match gives {
                Gives::List => ("parallel.map", vec![list, closure]),
                Gives::Sum(start) => {
                    let start = match start {
                        Some(start) => self.expr(start)?,
                        None if ty == Type::Float => Operand::Float(0.0),
                        None => Operand::Int(0),
                    };
                    ("parallel.sum", vec![list, closure, start])
                }
            };
            let call = InstKind::Builtin { dest: Some(dest), name: name.to_owned(), args, changes: false };
            self.emit(call, expr.span);
            Ok(Operand::Local(dest))
        }))
    }

    /// The list `parallel(xs)` goes over and the type of its items, when `iter` is that.
    fn parallel_list<'e>(&self, iter: &'e Expr) -> Option<(&'e Expr, Type)> {
        let items = parallel_items(iter)?;
        match self.type_at(items.span)? {
            Type::List(item) => Some((items, *item)),
            _ => None,
        }
    }

    /// Lifts what runs for each item into a function of its own, taking what it captures and
    /// then the item named `name`: the list, and a closure of the function.
    fn lift(
        &mut self,
        name: &str,
        (items, item): (&Expr, Type),
        runs: &Runs<'_>,
        span: Span,
    ) -> Lowered<(Operand, Operand)> {
        let mut names = Vec::new();
        let mut record = |seen: &mut Expr| {
            if let ExprKind::Name(found) = &seen.kind {
                names.push(found.clone());
            }
        };
        let returns = match runs {
            Runs::Body(body, _) => {
                visit::in_stmts(&mut body.to_vec(), &mut record);
                Type::Unit
            }
            Runs::Value(elem, each) => {
                visit::in_expr(&mut (*elem).clone(), &mut record);
                each.clone()
            }
        };
        let captures = self.captures(names, &[name]);
        let key = format!("{}.parallel@{}", self.name, span.start);
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, returns.clone());
        for (captured, local) in &captures {
            lifted.declare(captured, self.local_type(*local));
        }
        lifted.declare(name, item.clone());
        lifted.scopes.push(HashMap::new());
        let changes = match runs {
            Runs::Body(body, changes) => {
                lifted.stmts(body, Tail::Drop)?;
                *changes
            }
            Runs::Value(elem, _) => {
                lifted.tail_expr(elem, Tail::Return)?;
                false
            }
        };
        lifted.end_function(span)?;
        // A body that changes its item gives it back changed, as an arrow function that changes
        // its first parameter does.
        let mut_params = if changes { vec![captures.len()] } else { Vec::new() };
        self.lifted.extend(lifted.finished(captures.len() + 1, mut_params));
        let closure = self.make_closure(key, &captures, Type::Arrow(vec![item], Box::new(returns)), span);
        Ok((self.expr(items)?, Operand::Local(closure)))
    }
}
