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
//!   fixed tree, so it's the same bit for bit however many threads run.
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
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => liftable(body),
        _ => true,
    }) && !tries(body)
}

/// Whether nothing in a body leaves the function it's in, other than by failing.
fn no_returning(body: &[Stmt]) -> bool {
    body.iter().all(|stmt| match &stmt.kind {
        StmtKind::Return(_) | StmtKind::Yield(_) => false,
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(_, block)| no_returning(block))
                && otherwise.as_deref().is_none_or(no_returning)
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => no_returning(body),
        _ => true,
    })
}

/// Whether an expression uses `?` outside a nested function's own body.
/// The names what a range runs mentions, and what its function gives back.
fn named_in(runs: &Runs<'_>) -> (Vec<String>, Type) {
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
    (names, returns)
}

fn expr_tries(expr: &Expr) -> bool {
    let mut found = false;
    visit::in_expr(&mut expr.clone(), &mut |seen| found |= matches!(seen.kind, ExprKind::Try { .. }));
    found
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

/// What a server's round gives back, for the runtime: it ended normally or by `continue`, so the
/// server goes on.
const ROUND_ON: i64 = 0;
/// It failed, handing its failure's value over first: the server stops, the other rounds are cut
/// short, and the loop fails with it.
const ROUND_FAILED: i64 = 2;
/// It ended by `break`: the server takes no more connections, and lets the running rounds finish.
const ROUND_BREAK: i64 = 3;

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
        let lifted = self.lift(name, (items, item), (&Runs::Body(body, is_mut), None), span);
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
        let (true, false, [name], Some(server)) = (
            self.context.options.outline_parallel,
            self.context.options.serve_in_order,
            names,
            parallel_items(iter),
        ) else {
            return None;
        };
        // A round's `continue` and `break` give back their codes; only `return` can't leave it.
        if self.type_at(server.span)? != Type::Named("Server".to_owned()) || !no_returning(body) {
            return None;
        }
        if self.writes_outside(body, name) {
            return None;
        }
        Some(self.lift_task(name, server, body, span))
    }

    /// Lifts a server's loop body into a function of a connection, and that into the closure the
    /// runtime runs for each, which takes the connection's handle, makes its `Connection`, and
    /// hands over any failure the body passes up, which the loop then fails with.
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
        // A round gives back how it ended, as the runtime reads it: `ROUND_ON` when it ended
        // normally or by `continue`, and `ROUND_BREAK` by `break`. A body with `?` makes its
        // enclosing function fail with what it can fail with.
        let returns = match &self.returns {
            Type::Result(_, error) => Type::Result(Box::new(Type::Int), error.clone()),
            _ => Type::Int,
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
        // `continue` and `break` leave the round, each with its code.
        let (goes_on, breaks) = (lifted.new_block(), lifted.new_block());
        lifted.loops.push(super::Loop { next: goes_on, exit: breaks, depth: lifted.leaving.len() });
        lifted.stmts(body, Tail::Drop)?;
        if lifted.is_open() {
            lifted.finish(crate::Terminator::Jump(goes_on));
        }
        lifted.loops.pop();
        for (block, code) in [(goes_on, ROUND_ON), (breaks, ROUND_BREAK)] {
            lifted.current = block;
            lifted.give_back_code(code, span)?;
        }
        self.lifted.extend(lifted.finished(captures.len() + 1, Vec::new()));

        let key = format!("{}.serve@{}", self.name, span.start);
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut task = Lowering::new(&instance, self.context, self.body, Type::Int);
        let mut args = Vec::with_capacity(captures.len() + 1);
        for (captured, local) in &captures {
            args.push(Operand::Local(task.declare(captured, self.local_type(*local))));
        }
        let handle = task.declare("\0parallel.conn", Type::Int);
        task.scopes.push(HashMap::new());
        let made = served_connection(&mut task, handle, connection, span);
        args.push(Operand::Local(made));
        let result = task.temporary(returns.clone());
        task.emit(InstKind::Call { dest: Some(result), function: inner, args }, span);
        let code = if let Type::Result(_, error) = &returns {
            task.hand_over_failure(result, error, span)?;
            let code = task.temporary(Type::Int);
            task.emit(InstKind::GetOk { dest: code, value: Operand::Local(result) }, span);
            code
        } else {
            result
        };
        task.leave(Some(Operand::Local(code)), span)?;
        self.lifted.extend(task.finished(captures.len() + 1, Vec::new()));

        let arrow = Type::Arrow(vec![Type::Int], Box::new(Type::Int));
        let closure = self.make_closure(key, &captures, arrow, span);
        let args = vec![server, Operand::Local(closure)];
        let ended = self.temporary(Type::Int);
        let call =
            InstKind::Builtin { dest: Some(ended), name: "parallel.serve".to_owned(), args, changes: false };
        self.emit(call, span);
        self.pass_up_round_failure(ended, span)
    }

    /// `intrinsics.launch(task)`, the task a closure taking nothing: started as a closure that
    /// calls it and gives back how it ended, as the runtime reads it, `0` normally, or `2` when it
    /// failed, its failure handed over first.
    pub(super) fn launch_task(
        &mut self,
        dest: Option<LocalId>,
        values: &[Operand],
        span: Span,
    ) -> Lowered<LocalId> {
        let [Operand::Local(closure)] = values else {
            return Err("`launch` given something other than a task".to_owned());
        };
        let arrow = self.local_type(*closure);
        let Type::Arrow(params, returns) = &arrow else {
            return Err("`launch` given something other than a closure".to_owned());
        };
        if !params.is_empty() {
            return Err("`launch` given a task that takes something".to_owned());
        }
        // Each `with launch` is written in from the same source, so its span is the same as the
        // others': what this function has lifted so far tells them apart.
        let key = format!("{}.launch@{}.{}", self.name, span.start, self.lifted.len());
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut task = Lowering::new(&instance, self.context, self.body, Type::Int);
        let held = task.declare("\0launch.task", arrow.clone());
        task.scopes.push(HashMap::new());
        let result = (**returns != Type::Unit).then(|| task.temporary((**returns).clone()));
        let call = InstKind::CallClosure { dest: result, closure: Operand::Local(held), args: Vec::new() };
        task.emit(call, span);
        if let (Some(result), Type::Result(_, error)) = (result, &**returns) {
            task.hand_over_failure(result, error, span)?;
        }
        task.leave(Some(Operand::Int(0)), span)?;
        self.lifted.extend(task.finished(1, Vec::new()));
        let captures = vec![("\0launch.task".to_owned(), *closure)];
        let started = self.make_closure(key, &captures, Type::Arrow(Vec::new(), Box::new(Type::Int)), span);
        let dest = dest.unwrap_or_else(|| self.temporary(Type::Int));
        let args = vec![Operand::Local(started)];
        self.emit(
            InstKind::Builtin {
                dest: Some(dest),
                name: "intrinsics.launch".to_owned(),
                args,
                changes: false,
            },
            span,
        );
        Ok(dest)
    }

    /// After serving: a round's failure passes up out of the loop, as a `?` would, when serving
    /// gave back that one failed.
    fn pass_up_round_failure(&mut self, ended: LocalId, span: Span) -> Lowered<()> {
        let Type::Result(_, error) = self.returns.clone() else { return Ok(()) };
        let failed = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(ended), Operand::Int(ROUND_FAILED));
        self.emit(InstKind::Binary { dest: failed, op: BinOp::Eq, lhs, rhs }, span);
        let (up, on) = (self.new_block(), self.new_block());
        self.finish(crate::Terminator::Branch { cond: Operand::Local(failed), then: up, otherwise: on });
        self.current = up;
        let cause = self.temporary(*error);
        let name = "parallel.serve_failure".to_owned();
        self.emit(InstKind::Builtin { dest: Some(cause), name, args: Vec::new(), changes: false }, span);
        self.pass_up_error(Operand::Local(cause), span)?;
        self.current = on;
        Ok(())
    }

    /// Leaves a round's body with `code`, as success when the body can fail.
    fn give_back_code(&mut self, code: i64, span: Span) -> Lowered<()> {
        if matches!(self.returns, Type::Result(..)) {
            let dest = self.temporary(self.returns.clone());
            self.emit(InstKind::MakeOk { dest, value: Some(Operand::Int(code)) }, span);
            return self.leave(Some(Operand::Local(dest)), span);
        }
        self.leave(Some(Operand::Int(code)), span)
    }

    /// What a server's task does with a failure its body passed up: hands its value over, and
    /// gives back that the round failed.
    fn hand_over_failure(&mut self, result: LocalId, error: &Type, span: Span) -> Lowered<()> {
        let (failed, cause) = (self.temporary(Type::Bool), self.temporary(error.clone()));
        self.emit(InstKind::IsErr { dest: failed, value: Operand::Local(result) }, span);
        let (stops, goes_on) = (self.new_block(), self.new_block());
        self.finish(crate::Terminator::Branch {
            cond: Operand::Local(failed),
            then: stops,
            otherwise: goes_on,
        });
        self.current = stops;
        self.emit(InstKind::GetErr { dest: cause, value: Operand::Local(result) }, span);
        let args = vec![Operand::Local(cause)];
        self.emit(
            InstKind::Builtin { dest: None, name: "task.failed".to_owned(), args, changes: false },
            span,
        );
        self.leave(Some(Operand::Int(ROUND_FAILED)), span)?;
        self.current = goes_on;
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
        // An item that can fail, like `save(fs.read(path)?)?`, makes a list whose failure passes
        // up, as `?` does. A sum of them isn't lifted yet, so it's made in order here instead.
        let fails = match (expr_tries(elem), &self.returns, gives) {
            (false, ..) => None,
            (true, Type::Result(_, error), Gives::List) => Some((**error).clone()),
            (true, ..) => return None,
        };
        let (items, item) = self.parallel_list(iter)?;
        let ty = self.type_at(expr.span)?;
        let runs = match (&ty, gives) {
            (Type::List(each), Gives::List) => Runs::Each(elem, (**each).clone()),
            (Type::Int | Type::Float, Gives::Sum(_)) => Runs::Sum(elem, ty.clone()),
            _ => return None,
        };
        let lifted = self.lift(name, (items, item), (&runs, fails.as_ref()), expr.span);
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
            if let Some(error) = &fails {
                self.pass_up_range_failure(dest, error, expr.span)?;
            }
            Ok(Operand::Local(dest))
        }))
    }

    /// After a `parallel` comprehension whose items can fail: the failure a range handed over
    /// passes up, as a `?` would, when the call made no list.
    fn pass_up_range_failure(&mut self, made: LocalId, error: &Type, span: Span) -> Lowered<()> {
        let failed = self.temporary(Type::Bool);
        let args = vec![Operand::Local(made)];
        let name = "parallel.map_failed".to_owned();
        self.emit(InstKind::Builtin { dest: Some(failed), name, args, changes: false }, span);
        let (up, on) = (self.new_block(), self.new_block());
        self.finish(crate::Terminator::Branch { cond: Operand::Local(failed), then: up, otherwise: on });
        self.current = up;
        let cause = self.temporary(error.clone());
        let name = "parallel.failure".to_owned();
        self.emit(InstKind::Builtin { dest: Some(cause), name, args: Vec::new(), changes: false }, span);
        self.pass_up_error(Operand::Local(cause), span)?;
        self.current = on;
        Ok(())
    }

    /// The function the runtime runs for a range whose items can fail: it calls `inner` with
    /// everything it's given, hands over a failure that passes up, and gives back how it ended.
    fn handing_over(
        &mut self,
        key: &str,
        (inner, inner_returns): (&str, Type),
        (params, error): (Vec<(&str, Type)>, &Type),
        span: Span,
    ) -> Lowered<()> {
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.to_owned(), subst: self.subst.clone() };
        let mut range = Lowering::new(&instance, self.context, self.body, Type::Int);
        let count = params.len();
        let args = params.into_iter().map(|(name, ty)| Operand::Local(range.declare(name, ty))).collect();
        range.scopes.push(HashMap::new());
        let result = range.temporary(inner_returns);
        range.emit(InstKind::Call { dest: Some(result), function: inner.to_owned(), args }, span);
        range.hand_over_failure(result, error, span)?;
        range.leave(Some(Operand::Int(ROUND_ON)), span)?;
        self.lifted.extend(range.finished(count, Vec::new()));
        Ok(())
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
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => self.writes_outside(body, item),
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
        (runs, fails): (&Runs<'_>, Option<&Type>),
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
        let (names, returns) = named_in(runs);
        let captures = self.captures(names, &[name]);
        let key = format!("{}.parallel@{}", self.name, span.start);
        // A range whose items can fail is a function that can, called by the one the runtime runs,
        // which hands its failure over and gives back that it failed.
        let (inner, inner_returns) = match fails {
            Some(error) => {
                (format!("{key}.inner"), Type::Result(Box::new(Type::Unit), Box::new(error.clone())))
            }
            None => (key.clone(), returns.clone()),
        };
        let instance =
            FunctionInstance { base: self.base.clone(), name: inner.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, inner_returns.clone());
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
        let returns = match fails {
            Some(error) => {
                let types: Vec<Type> = captures.iter().map(|(_, local)| self.local_type(*local)).collect();
                let names =
                    captures.iter().map(|(captured, _)| captured.as_str()).chain([LIST, FROM, TO, OUT]);
                let types = types.into_iter().chain([list_ty.clone()]).chain(params.iter().cloned());
                self.handing_over(&key, (&inner, inner_returns), (names.zip(types).collect(), error), span)?;
                Type::Int
            }
            None => returns,
        };
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

/// The `Connection` a task serving a connection makes from its handle: with its peer, and its
/// label, which it prints as, counted by the runtime as it's made.
fn served_connection(task: &mut Lowering<'_>, handle: LocalId, connection: Type, span: Span) -> LocalId {
    let peer = task.temporary(Type::Str);
    let asked = vec![Operand::Local(handle)];
    task.emit(
        InstKind::Builtin { dest: Some(peer), name: "parallel.peer".to_owned(), args: asked, changes: false },
        span,
    );
    // Its label, which it prints as, counted by the runtime as it's made.
    let label = task.temporary(Type::Int);
    task.emit(
        InstKind::Builtin {
            dest: Some(label),
            name: "parallel.label".to_owned(),
            args: vec![Operand::Local(handle)],
            changes: false,
        },
        span,
    );
    let made = task.temporary(connection);
    let fields = vec![Operand::Local(handle), Operand::Local(label), Operand::Local(peer)];
    task.emit(InstKind::Make { dest: made, type_name: "Connection".to_owned(), fields }, span);
    made
}
