//! A `for` loop over a range whose count only reads a list it can't change, as
//! `for at in range(starts[near], starts[near + 1]): other = order[at]`. Every read is checked once,
//! before the loop: a range that starts at 0 or more and ends at the list's length or less can't
//! read out of it. The loop is then made twice: one copy for a range that passed, which a backend
//! can read the list in with no check (`fennec_llvm`'s `inside` proves it from the test before
//! it), and today's loop for any other range, which fails at the first read that's out, with the
//! same message, after the rounds before it ran, as it always did.
//!
//! Only this shape is made twice, kept small enough to see it can't change the list: a body of
//! plain statements, with no loop, `with`, nested function, arrow function, comprehension or
//! `match` in it, that names the list only to read an item or its length, and the count only to
//! read the list. A body made twice is code made twice, so a loop inside a loop isn't.

use std::collections::BTreeSet;

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, Span, Stmt, StmtKind, fuse, sight, visit};

use super::lists::{Counting, range_args};
use super::{Lowered, Lowering, Tail};
use crate::{InstKind, Operand, Terminator};

impl Lowering<'_> {
    /// The list a `for` loop over a range counting up reads by its count and can't change, if
    /// it's the only thing the count is used for: see the module's doc.
    pub(super) fn checked_once(&self, name: &str, iter: &Expr, body: &[Stmt]) -> Option<String> {
        if !self.is_range(iter) || self.lookup("len").is_some() || self.own_function("len") {
            return None;
        }
        let (args, false) = range_args(iter)? else { return None };
        // A loop to the list's length is known to stay in it already.
        if args.last().is_some_and(|limit| length_of(limit).is_some()) || !body.iter().all(plain) {
            return None;
        }
        let bound = sight::bound_in(body, &[]);
        let mut read = body.to_vec();
        let mut lists = BTreeSet::new();
        let mut written = false;
        visit::in_stmts(&mut read, &mut |expr| match &expr.kind {
            ExprKind::Lambda { .. } | ExprKind::Comprehension { .. } | ExprKind::Match { .. } => {
                written = true;
            }
            ExprKind::Index { object, index } => {
                if let (ExprKind::Name(list), ExprKind::Name(at)) = (&object.kind, &index.kind)
                    && at == name
                {
                    lists.insert(list.clone());
                    expr.kind = ExprKind::None;
                }
            }
            _ => {}
        });
        let [list] = std::mem::take(&mut lists).into_iter().collect::<Vec<_>>().try_into().ok()?;
        let Some(Type::List(_)) = self.lookup(&list).map(|local| self.local_type(local)) else { return None };
        if written || bound.contains(name) || bound.contains(&list) || targets_name(body, &[name, &list]) {
            return None;
        }
        // What's left may read the list's other items and its length, and nothing else of it.
        visit::in_stmts(&mut read, &mut |expr| {
            let reads = match &expr.kind {
                ExprKind::Index { object, .. } => {
                    matches!(&object.kind, ExprKind::Name(held) if *held == list)
                }
                _ => length_of(expr) == Some(list.as_str()),
            };
            if reads {
                if let ExprKind::Index { object, .. } = &mut expr.kind {
                    object.kind = ExprKind::None;
                } else {
                    expr.kind = ExprKind::None;
                }
            }
        });
        let left = fuse::names_in(&read);
        (!left.iter().any(|named| named == name || *named == list)).then_some(list)
    }

    /// The loop `checked_once` found a list for, made twice: see the module's doc.
    pub(super) fn versioned(
        &mut self,
        name: &str,
        (iter, list): (&Expr, &str),
        body: &[Stmt],
        span: Span,
    ) -> Lowered<()> {
        let (args, _) = range_args(iter).ok_or("it isn't a range")?;
        let (first, limit) = match args.as_slice() {
            [limit] => (Operand::Int(0), self.expr(limit)?),
            [first, limit] => (self.expr(first)?, self.expr(limit)?),
            _ => return Err("it calls `range` with a named argument".to_owned()),
        };
        // Each is read once, so the test and whichever loop runs see the same numbers.
        let first = self.held_int(first, span);
        let limit = self.held_int(limit, span);
        let held = self.lookup(list).ok_or("a loop's list has no name")?;
        let length = self.temporary(Type::Int);
        let args = vec![Operand::Local(held)];
        self.emit(
            InstKind::Builtin { dest: Some(length), name: "list.len".to_owned(), args, changes: false },
            span,
        );
        let (to_end, inside, outside, after) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        // A range with nothing in it reads nothing, and is often most of them, as a counting sort's
        // empty cells are: it's let go before the test, which costs more than the loop it skips.
        let tested = self.new_block();
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (first.clone(), limit.clone());
        self.emit(InstKind::Binary { dest: going, op: BinOp::Lt, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then: tested, otherwise: after });
        self.current = tested;
        if matches!(first, Operand::Int(0..)) {
            self.finish(Terminator::Jump(to_end));
        } else {
            let from_start = self.temporary(Type::Bool);
            let (lhs, rhs) = (first.clone(), Operand::Int(0));
            self.emit(InstKind::Binary { dest: from_start, op: BinOp::GtEq, lhs, rhs }, span);
            self.finish(Terminator::Branch {
                cond: Operand::Local(from_start),
                then: to_end,
                otherwise: outside,
            });
        }
        self.current = to_end;
        let fits = self.temporary(Type::Bool);
        let (lhs, rhs) = (limit.clone(), Operand::Local(length));
        self.emit(InstKind::Binary { dest: fits, op: BinOp::LtEq, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(fits), then: inside, otherwise: outside });
        let names = [name.to_owned()];
        for start in [inside, outside] {
            self.current = start;
            let (first, limit) = (first.clone(), limit.clone());
            let counting = Counting { names: &names, first, limit, down: false, items: None, back: None };
            self.counting(counting, span, |lowering| lowering.stmts(body, Tail::Drop))?;
            self.finish(Terminator::Jump(after));
        }
        self.current = after;
        Ok(())
    }

    /// An int as it is now, in a local of its own unless it's a number.
    fn held_int(&mut self, value: Operand, span: Span) -> Operand {
        if matches!(value, Operand::Int(_)) {
            return value;
        }
        let dest = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest, value }, span);
        Operand::Local(dest)
    }
}

/// The list `len(list)` measures, if that's what this is.
fn length_of(value: &Expr) -> Option<&str> {
    let ExprKind::Call { callee, args } = &value.kind else { return None };
    let (ExprKind::Name(called), [arg]) = (&callee.kind, args.as_slice()) else { return None };
    match &arg.value.kind {
        ExprKind::Name(list) if called == "len" && arg.name.is_none() && !arg.is_mut => Some(list),
        _ => None,
    }
}

/// Whether a statement is one a loop made twice can hold: no loop, `with`, nested function,
/// `yield`, cleanup or lending, anywhere in it.
fn plain(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Expr(_)
        | StmtKind::Assign { .. }
        | StmtKind::Return(_)
        | StmtKind::Fail(_)
        | StmtKind::Assert(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Pass => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(_, body)| body.iter().all(plain)) && otherwise.iter().flatten().all(plain)
        }
        _ => false,
    }
}

/// Whether anything in `body` is assigned to through one of `names`: `name = ...`,
/// `name[i] = ...`, `name.field += ...`.
fn targets_name(body: &[Stmt], names: &[&str]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Assign { target, .. } => {
            let mut target = target.clone();
            let mut found = false;
            visit::in_expr(&mut target, &mut |expr| {
                if let ExprKind::Name(named) = &expr.kind {
                    found |= names.contains(&named.as_str());
                }
            });
            found
        }
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| targets_name(body, names))
                || otherwise.as_deref().is_some_and(|body| targets_name(body, names))
        }
        _ => false,
    })
}
