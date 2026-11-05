//! `parallel(xs)` for a backend that runs it on threads. The work for a range of the list's
//! positions is lifted into a function of its own, as an arrow function is: its first
//! parameters take what it captures, then the list, then where the range starts and ends. The
//! function goes through its range one position at a time, as an ordinary counted loop, so the
//! backend compiles it like any other. Then a builtin, given the list and a closure of that
//! function, runs it over ranges covering the list, on as many threads as the backend likes:
//! - `for x in parallel(xs): body` becomes `parallel.for(xs, range)`. The checker has made sure
//!   the body writes only its own item and names, so its iterations are independent. A
//!   `for mut x` puts each changed item back where it came from, with `parallel.put_back`, and
//!   `parallel.for` then changes the list, as `list.append` does. The list is made the loop's
//!   own first (`parallel.own`), so the change reaches only it.
//! - `[f(x) for x in parallel(xs)]` becomes `parallel.map(xs, range)`. The range takes the new
//!   list too, and puts each item's result at its position, with `parallel.put`.
//! - `sum(f(x) for x in parallel(xs))` becomes `parallel.sum(xs, range, start)`. The range gives
//!   back its partial, folded from its first item, and the backend adds them in the runtime's
//!   fixed tree, so it's the same bit for bit however many threads run, and the same as the
//!   interpreter.
//!
//! `put` and `put_back` write the list in place, never copying it, yet say `changes: false`.
//! That's sound only because the list is the call's alone before any range runs (the runtime
//! makes `put`'s list new, and `parallel.own` makes `put_back`'s the loop's own) and the
//! runtime's ranges never overlap, so no position is written twice and nothing else holds the
//! list to see the write. A backend has to treat them as these stores, not as changes it may
//! make a copy for.
//!
//! A sum's range starts its partial from its first item, reading `xs[from]` before anything
//! else, so it must never be given an empty range. The runtime never does:
//! `fennec_parallel_reduce` gives back the start at once for an empty list, and splits only
//! ranges longer than a leaf, at their middle, so each half holds at least one position.
//!
//! This is done only when the backend asks (`Options::outline_parallel`), and only where the
//! lifted function can hold what's written: a body that leaves the loop early or the function,
//! yields, or can fail, and a comprehension or sum with an `if`, are lowered as before, which
//! runs the same way, in order.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, Span, Stmt, StmtKind, visit};

use super::generics::FunctionInstance;
use super::lists::Counting;
use super::{Lowered, Lowering, Tail};
use crate::{InstKind, LocalId, Operand};

/// What a loop goes over when it's `parallel(xs)` with one plain argument: `xs`.
pub(super) fn parallel_items(iter: &Expr) -> Option<&Expr> {
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

/// Whether a server's task can be lifted from a body: it can end by failing, which ends only the
/// task, but not by leaving the loop or the function.
fn leaves_only_by_failing(body: &[Stmt]) -> bool {
    body.iter().all(|stmt| match &stmt.kind {
        StmtKind::Break | StmtKind::Continue | StmtKind::Return(_) | StmtKind::Yield(_) => false,
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(_, block)| leaves_only_by_failing(block))
                && otherwise.as_deref().is_none_or(leaves_only_by_failing)
        }
        StmtKind::With { body, .. } => leaves_only_by_failing(body),
        // A loop's own `break` and `continue` stay in it.
        StmtKind::While { body, .. } | StmtKind::For { body, .. } => no_returning(body),
        _ => true,
    })
}

/// Whether nothing in a body leaves the function it's in, other than by failing.
fn no_returning(body: &[Stmt]) -> bool {
    body.iter().all(|stmt| match &stmt.kind {
        StmtKind::Return(_) | StmtKind::Yield(_) => false,
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(_, block)| no_returning(block))
                && otherwise.as_deref().is_none_or(no_returning)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            no_returning(body)
        }
        _ => true,
    })
}

/// Whether a body uses `?` outside a nested function's own body.
fn tries(body: &[Stmt]) -> bool {
    let mut found = false;
    let mut written = body.to_vec();
    visit::in_stmts(&mut written, &mut |seen| found |= matches!(seen.kind, ExprKind::Try { .. }));
    found
}

/// The names an assignment writes through: `a` for `a`, `a.x` and `a[i]`, each of a tuple's.
fn assigned_roots<'e>(target: &'e Expr, roots: &mut Vec<&'e str>) {
    match &target.kind {
        ExprKind::Name(name) => roots.push(name),
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => assigned_roots(object, roots),
        ExprKind::Tuple(items) => items.iter().for_each(|item| assigned_roots(item, roots)),
        _ => {}
    }
}

/// What a lifted value makes: a list of each item's, or their sum from a start, `0` or `0.0`
/// when none is written.
#[derive(Clone, Copy)]
pub(super) enum Gives<'e> {
    List,
    Sum(Option<&'e Expr>),
}

/// What a range runs for each item.
enum Runs<'e> {
    /// A loop's body, which changes its item when it's a `for mut`'s.
    Body(&'e [Stmt], bool),
    /// An expression whose value goes into a new list of this type of item.
    Each(&'e Expr, Type),
    /// An expression whose values are added up, of this type.
    Sum(&'e Expr, Type),
}

/// The hidden names a range function has, which nothing written can name.
const LIST: &str = "\0parallel.list";
const FROM: &str = "\0parallel.from";
const TO: &str = "\0parallel.to";
const OUT: &str = "\0parallel.out";

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
        // The checker keeps a written body to its own item and names, but a loop the lowering
        // makes up, like a filtered sum's, may add into a name from outside it, which a range
        // would only change a copy of.
        if self.writes_outside(body, name) {
            return None;
        }
        let (items, item) = self.parallel_list(iter)?;
        // A `for mut` body changes its item, and the loop changes the list, which has to be a
        // local here.
        if is_mut && !matches!(&items.kind, ExprKind::Name(list) if self.lookup(list).is_some()) {
            return None;
        }
        let lifted = self.lift(name, (items, item), &Runs::Body(body, is_mut), span);
        Some(lifted.map(|(list, range)| {
            let args = vec![list, range];
            let call =
                InstKind::Builtin { dest: None, name: "parallel.for".to_owned(), args, changes: is_mut };
            self.emit(call, span);
        }))
    }

    /// `for conn in parallel(server)`, each connection a task the runtime runs, when the backend
    /// asks for it and the body can be lifted. `?` in the body ends only its task. `None` when it's
    /// lowered as an ordinary loop.
    pub(super) fn parallel_serve(
        &mut self,
        names: &[String],
        iter: &Expr,
        body: &[Stmt],
        span: Span,
    ) -> Option<Lowered<()>> {
        let (true, [name], Some(server)) =
            (self.context.options.outline_parallel, names, parallel_items(iter))
        else {
            return None;
        };
        if self.type_at(server.span)? != Type::Named("Server".to_owned()) || !leaves_only_by_failing(body) {
            return None;
        }
        if self.writes_outside(body, name) {
            return None;
        }
        Some(self.lift_task(name, server, body, span))
    }

    /// Lifts a server's loop body into a function of a connection, and that into the closure the
    /// runtime runs for each, which takes the connection's handle, makes its `Connection`, and
    /// stops its task with any failure the body passes up.
    fn lift_task(&mut self, name: &str, server: &Expr, body: &[Stmt], span: Span) -> Lowered<()> {
        let server = self.expr(server)?;
        let mut names = Vec::new();
        visit::in_stmts(&mut body.to_vec(), &mut |seen: &mut Expr| {
            if let ExprKind::Name(found) = &seen.kind {
                names.push(found.clone());
            }
        });
        let captures = self.captures(names, &[name]);
        let connection = Type::Named("Connection".to_owned());
        // A body with `?` makes its enclosing function fail with what it can fail with.
        let returns = match &self.returns {
            Type::Result(_, error) => Type::Result(Box::new(Type::Unit), error.clone()),
            _ => Type::Unit,
        };
        let inner = format!("{}.task@{}", self.name, span.start);
        let instance =
            FunctionInstance { base: self.base.clone(), name: inner.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, returns.clone());
        for (captured, local) in &captures {
            lifted.declare(captured, self.local_type(*local));
        }
        lifted.declare(name, connection.clone());
        lifted.scopes.push(HashMap::new());
        lifted.stmts(body, Tail::Drop)?;
        lifted.end_function(span)?;
        self.lifted.extend(lifted.finished(captures.len() + 1, Vec::new()));

        let key = format!("{}.serve@{}", self.name, span.start);
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut task = Lowering::new(&instance, self.context, self.body, Type::Unit);
        let mut args = Vec::with_capacity(captures.len() + 1);
        for (captured, local) in &captures {
            args.push(Operand::Local(task.declare(captured, self.local_type(*local))));
        }
        let handle = task.declare("\0parallel.conn", Type::Int);
        task.scopes.push(HashMap::new());
        let peer = task.temporary(Type::Str);
        let asked = vec![Operand::Local(handle)];
        task.emit(
            InstKind::Builtin {
                dest: Some(peer),
                name: "parallel.peer".to_owned(),
                args: asked,
                changes: false,
            },
            span,
        );
        let made = task.temporary(connection);
        let fields = vec![Operand::Local(handle), Operand::Local(peer)];
        task.emit(InstKind::Make { dest: made, type_name: "Connection".to_owned(), fields }, span);
        args.push(Operand::Local(made));
        let passes_up = returns != Type::Unit;
        let dest = passes_up.then(|| task.temporary(returns.clone()));
        task.emit(InstKind::Call { dest, function: inner, args }, span);
        if let (Some(result), Type::Result(_, error)) = (dest, &returns) {
            // A failure passed up ends only this task, saying what it was.
            let (failed, cause) = (task.temporary(Type::Bool), task.temporary((**error).clone()));
            task.emit(InstKind::IsErr { dest: failed, value: Operand::Local(result) }, span);
            let (stops, goes_on) = (task.new_block(), task.new_block());
            task.finish(crate::Terminator::Branch { cond: Operand::Local(failed), then: stops, otherwise: goes_on });
            task.current = stops;
            task.emit(InstKind::GetErr { dest: cause, value: Operand::Local(result) }, span);
            let args = vec![Operand::Local(cause)];
            task.emit(InstKind::Builtin { dest: None, name: "task.failed".to_owned(), args, changes: false }, span);
            let message = "this connection's task failed".to_owned();
            task.finish(crate::Terminator::Stop { message, span });
            task.current = goes_on;
        }
        task.end_function(span)?;
        self.lifted.extend(task.finished(captures.len() + 1, Vec::new()));

        let arrow = Type::Arrow(vec![Type::Int], Box::new(Type::Unit));
        let closure = self.make_closure(key, &captures, arrow, span);
        let args = vec![server, Operand::Local(closure)];
        let call = InstKind::Builtin { dest: None, name: "parallel.serve".to_owned(), args, changes: false };
        self.emit(call, span);
        Ok(())
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
        let runs = match (&ty, gives) {
            (Type::List(each), Gives::List) => Runs::Each(elem, (**each).clone()),
            (Type::Int | Type::Float, Gives::Sum(_)) => Runs::Sum(elem, ty.clone()),
            _ => return None,
        };
        let lifted = self.lift(name, (items, item), &runs, expr.span);
        Some(lifted.and_then(|(list, range)| {
            let dest = self.temporary(ty.clone());
            let (name, args) = match gives {
                Gives::List => ("parallel.map", vec![list, range]),
                Gives::Sum(start) => {
                    let start = match start {
                        Some(start) => self.expr(start)?,
                        None if ty == Type::Float => Operand::Float(0.0),
                        None => Operand::Int(0),
                    };
                    ("parallel.sum", vec![list, range, start])
                }
            };
            let call = InstKind::Builtin { dest: Some(dest), name: name.to_owned(), args, changes: false };
            self.emit(call, expr.span);
            Ok(Operand::Local(dest))
        }))
    }

    /// Whether `body` assigns to a name from outside it other than `item`, at any depth.
    fn writes_outside(&self, body: &[Stmt], item: &str) -> bool {
        body.iter().any(|stmt| match &stmt.kind {
            StmtKind::Assign { target, .. } => {
                let mut roots = Vec::new();
                assigned_roots(target, &mut roots);
                roots.iter().any(|root| *root != item && self.lookup(root).is_some())
            }
            StmtKind::If { branches, otherwise } => {
                branches.iter().any(|(_, block)| self.writes_outside(block, item))
                    || otherwise.as_deref().is_some_and(|block| self.writes_outside(block, item))
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                self.writes_outside(body, item)
            }
            _ => false,
        })
    }

    /// The list `parallel(xs)` goes over and the type of its items, when `iter` is that.
    fn parallel_list<'e>(&self, iter: &'e Expr) -> Option<(&'e Expr, Type)> {
        let items = parallel_items(iter)?;
        match self.type_at(items.span)? {
            Type::List(item) => Some((items, *item)),
            _ => None,
        }
    }

    /// Lifts the work for a range of positions into a function of its own, taking what the
    /// item's code captures, the list, and the range's start and end, and for a comprehension
    /// the new list: the list, and a closure of the function.
    fn lift(
        &mut self,
        name: &str,
        (items, item): (&Expr, Type),
        runs: &Runs<'_>,
        span: Span,
    ) -> Lowered<(Operand, Operand)> {
        // The list first, as it's written before the body, then what the body captures.
        let list = self.expr(items)?;
        if let (Runs::Body(_, true), Operand::Local(_)) = (runs, &list) {
            // Made the loop's own before the range holds it too, so changing it in place
            // reaches only this list.
            let own = InstKind::Builtin {
                dest: None,
                name: "parallel.own".to_owned(),
                args: vec![list.clone()],
                changes: true,
            };
            self.emit(own, span);
        }
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
            Runs::Each(elem, _) => {
                visit::in_expr(&mut (*elem).clone(), &mut record);
                Type::Unit
            }
            Runs::Sum(elem, ty) => {
                visit::in_expr(&mut (*elem).clone(), &mut record);
                ty.clone()
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
        let list_ty = Type::List(Box::new(item.clone()));
        let held = lifted.declare(LIST, list_ty.clone());
        let (from, to) = (lifted.declare(FROM, Type::Int), lifted.declare(TO, Type::Int));
        let mut params = vec![Type::Int, Type::Int];
        let out = match runs {
            Runs::Each(_, each) => {
                let out_ty = Type::List(Box::new(each.clone()));
                params.push(out_ty.clone());
                Some(lifted.declare(OUT, out_ty))
            }
            _ => None,
        };
        lifted.scopes.push(HashMap::new());
        lifted.range_body(name, (held, item, from, to, out), runs, span)?;
        lifted.end_function(span)?;
        let count = captures.len() + 1 + params.len();
        self.lifted.extend(lifted.finished(count, Vec::new()));
        let mut held_values: Vec<(String, LocalId)> = captures;
        let list_local = match &list {
            Operand::Local(local) => *local,
            other => {
                let local = self.temporary(list_ty);
                self.emit(InstKind::Copy { dest: local, value: other.clone() }, span);
                local
            }
        };
        held_values.push((LIST.to_owned(), list_local));
        let range = self.make_closure(key, &held_values, Type::Arrow(params, Box::new(returns)), span);
        Ok((Operand::Local(list_local), Operand::Local(range)))
    }

    /// The body of a range function: what `runs` says, for each position from `from` up to
    /// `to` of the list `held`, whose items are `item`s, named `name`.
    fn range_body(
        &mut self,
        name: &str,
        (held, item, from, to, out): (LocalId, Type, LocalId, LocalId, Option<LocalId>),
        runs: &Runs<'_>,
        span: Span,
    ) -> Lowered<()> {
        let names = [name.to_owned()];
        let each = |first: Operand| Counting {
            names: &names,
            first,
            limit: Operand::Local(to),
            down: false,
            items: Some((held, item.clone())),
            back: None,
        };
        match runs {
            Runs::Body(body, changes) => {
                let counting = each(Operand::Local(from));
                self.counting(counting, span, |lowering| {
                    // The range's own count, taken before the body, which may count loops of its own.
                    let at = lowering.at()?;
                    lowering.stmts(body, Tail::Drop)?;
                    if *changes {
                        lowering.put("parallel.put_back", (held, at), name, None, span)?;
                    }
                    Ok(())
                })
            }
            Runs::Each(elem, _) => {
                let out = out.ok_or("a comprehension's range has nowhere to put its items")?;
                let counting = each(Operand::Local(from));
                self.counting(counting, span, |lowering| {
                    let at = lowering.at()?;
                    lowering.put("parallel.put", (out, at), name, Some(elem), span)
                })
            }
            Runs::Sum(elem, ty) => {
                // The first item starts the partial, so a range adds only what's in it, as the
                // runtime's tree does. One loop, not the first item and then a loop over the
                // rest: what's in `elem` is lowered once, so what it lifts is lifted once. A
                // range is never empty, so the partial is always set.
                let total = self.temporary(ty.clone());
                self.counting(each(Operand::Local(from)), span, |lowering| {
                    let value = lowering.expr(elem)?;
                    let at = lowering.at()?;
                    let first = lowering.temporary(Type::Bool);
                    let (lhs, rhs) = (Operand::Local(at), Operand::Local(from));
                    lowering.emit(InstKind::Binary { dest: first, op: BinOp::Eq, lhs, rhs }, span);
                    let (starts, adds, joined) =
                        (lowering.new_block(), lowering.new_block(), lowering.new_block());
                    let cond = Operand::Local(first);
                    lowering.finish(crate::Terminator::Branch { cond, then: starts, otherwise: adds });
                    lowering.current = starts;
                    lowering.emit(InstKind::Copy { dest: total, value: value.clone() }, span);
                    lowering.finish(crate::Terminator::Jump(joined));
                    lowering.current = adds;
                    let lhs = Operand::Local(total);
                    lowering.emit(InstKind::Binary { dest: total, op: BinOp::Add, lhs, rhs: value }, span);
                    lowering.finish(crate::Terminator::Jump(joined));
                    lowering.current = joined;
                    Ok(())
                })?;
                self.finish(crate::Terminator::Return(Some(Operand::Local(total))));
                Ok(())
            }
        }
    }

    /// The position the innermost counted loop is at.
    fn at(&self) -> Lowered<LocalId> {
        self.counts.last().map(|count| count.count).ok_or_else(|| "a range has no count".to_owned())
    }

    /// Puts an item at position `at` of `list`: `elem`'s value into a new list, or the item
    /// named `name`, changed, back into its list.
    fn put(
        &mut self,
        builtin: &str,
        (list, at): (LocalId, LocalId),
        name: &str,
        elem: Option<&Expr>,
        span: Span,
    ) -> Lowered<()> {
        let value = match elem {
            Some(elem) => self.expr(elem)?,
            None => Operand::Local(self.lookup(name).ok_or("a range's item has no name")?),
        };
        let args = vec![Operand::Local(list), Operand::Local(at), value];
        // Not marked as changing the list: it writes in place, on purpose, where a change would
        // copy a list something else also holds, which the runtime's list and the loop's are.
        self.emit(InstKind::Builtin { dest: None, name: builtin.to_owned(), args, changes: false }, span);
        Ok(())
    }
}
