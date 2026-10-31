//! Lists and strs: writing lists out, reading and changing items, `in`, f-strings, and `for`
//! loops, which count in a local of their own.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, FStrPart, Span, Stmt, UnaryOp};

use super::calls::owner_name;
use super::places::{Leaving, Place};
use super::{Loop, Lowered, Lowering, Tail};
use crate::{FormatPart, InstKind, LocalId, Operand, Terminator};

/// What a `for` loop counts through. Its names hold the count, or the item at the count,
/// unpacked when there are several.
struct Counting<'a> {
    names: &'a [String],
    first: Operand,
    limit: Operand,
    down: bool,
    items: Option<(LocalId, Type)>,
    /// Where each item goes back to, for a `for mut` loop.
    back: Option<Place>,
}

/// What a loop goes through.
pub(super) struct Over<'a> {
    pub(super) names: &'a [String],
    pub(super) iter: &'a Expr,
    pub(super) back: Option<Place>,
}

impl Lowering<'_> {
    pub(super) fn list(&mut self, expr: &Expr, items: &[Expr]) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let Type::List(item) = &ty else { return Err("a list has another type".to_owned()) };
        let mut values = Vec::with_capacity(items.len());
        for written in items {
            let value = self.expr(written)?;
            values.push(self.coerce(value, item, written.span)?);
        }
        let dest = self.temporary(ty);
        self.emit(InstKind::MakeList { dest, items: values }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// `object[index]`. A tuple's item at a position written as a number is known here.
    pub(super) fn get_index(&mut self, expr: &Expr, object: &Expr, index: &Expr) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let held = self.body.expr(object.span).cloned();
        if let (Some(Type::Tuple(items)), ExprKind::Int(at)) = (&held, &index.kind)
            && let Some(at) = usize::try_from(*at).ok().filter(|at| items.get(*at) == Some(&ty))
        {
            let value = self.expr(object)?;
            let dest = self.temporary(ty);
            self.emit(InstKind::GetTupleItem { dest, value, at }, expr.span);
            return Ok(Operand::Local(dest));
        }
        let key = match &held {
            Some(Type::List(item)) if **item == ty => Type::Int,
            Some(Type::Str) if ty == Type::Str => Type::Int,
            Some(Type::Bytes) if ty == Type::Int => Type::Int,
            Some(Type::Tuple(items)) if items.iter().all(|item| *item == ty) => Type::Int,
            Some(Type::Dict(key, value)) if **value == ty => *key.clone(),
            Some(Type::Named(name)) if name == "Args" && ty == Type::Str => {
                let object = self.expr(object)?;
                let index = self.expr(index)?;
                let dest = self.temporary(ty);
                self.emit(InstKind::GetIndex { dest, object, index }, expr.span);
                return Ok(Operand::Local(dest));
            }
            Some(Type::Named(owner)) => {
                let signatures = &self.context.checked.signatures;
                let found = signatures.get(&format!("{owner}.[]"));
                let param = found.and_then(|found| found.params.get(1));
                param.ok_or_else(|| format!("it indexes `{owner}`, which has no `[]`"))?.ty.clone()
            }
            _ => return Err("it uses indexing on something the IR doesn't index".to_owned()),
        };
        let object = self.expr(object)?;
        let lowered = self.expr(index)?;
        let index = self.coerce(lowered, &key, index.span)?;
        Ok(Operand::Local(self.read_item(held.as_ref().unwrap_or(&ty), object, index, &ty, expr.span)))
    }

    /// `item in items`, and `not in`, on a collection.
    pub(super) fn contains(&mut self, op: BinOp, item: &Expr, items: &Expr, span: Span) -> Lowered<Operand> {
        let owner = match self.body.expr(items.span) {
            Some(ty @ (Type::List(_) | Type::Set(_) | Type::Dict(..) | Type::Str | Type::Bytes)) => {
                owner_name(ty).unwrap_or_default().to_owned()
            }
            _ => return Err(format!("it uses `{}` on a type of its own", op.symbol())),
        };
        let item = self.expr(item)?;
        let items = self.expr(items)?;
        let dest = self.temporary(Type::Bool);
        let name = format!("{owner}.contains");
        self.emit(
            InstKind::Builtin { dest: Some(dest), name, args: vec![items, item], changes: false },
            span,
        );
        if op == BinOp::In {
            return Ok(Operand::Local(dest));
        }
        let flipped = self.temporary(Type::Bool);
        let value = Operand::Local(dest);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value }, span);
        Ok(Operand::Local(flipped))
    }

    pub(super) fn format(&mut self, parts: &[FStrPart], span: Span) -> Lowered<Operand> {
        let mut lowered = Vec::with_capacity(parts.len());
        for part in parts {
            lowered.push(match part {
                FStrPart::Text(text) => FormatPart::Text(text.clone()),
                FStrPart::Expr { expr, spec } => {
                    FormatPart::Value { value: self.expr(expr)?, spec: spec.clone() }
                }
            });
        }
        let dest = self.temporary(Type::Str);
        self.emit(InstKind::Format { dest, parts: lowered }, span);
        Ok(Operand::Local(dest))
    }

    /// A `for` loop.
    pub(super) fn for_stmt(
        &mut self,
        (names, is_mut): (&[String], bool),
        iter: &Expr,
        body: &[Stmt],
        span: Span,
    ) -> Lowered<()> {
        // Only a list's items are written back, and only one name holds them.
        let changes_items =
            is_mut && names.len() == 1 && matches!(self.body.expr(iter.span), Some(Type::List(_)));
        let back = if changes_items { Some(self.held_place(iter, span)?) } else { None };
        let over = Over { names, iter, back };
        self.looping(over, span, |lowering| lowering.stmts(body, Tail::Drop))
    }

    /// Runs `body` for each item of what `iter` gives. A `range` going one step at a time is
    /// counted, and anything else but a list is gone through as a list of its items.
    pub(super) fn looping<F>(&mut self, over: Over<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let Over { names, iter, back } = over;
        if self.is_range(iter)
            && let Some((args, down)) = range_args(iter)
        {
            let (first, limit) = match args.as_slice() {
                [limit] => (Operand::Int(0), self.expr(limit)?),
                [first, limit] => (self.expr(first)?, self.expr(limit)?),
                _ => return Err("it calls `range` with a named argument".to_owned()),
            };
            return self.counting(
                Counting { names, first, limit, down, items: None, back: None },
                span,
                body,
            );
        }
        let ty = self.body.expr(iter.span).cloned().ok_or("what it loops over has no type")?;
        let value = self.expr(iter)?;
        let (held, item, conversion) = match &ty {
            // A loop goes through the list as it was, whatever the body does to the name.
            Type::List(item) => (ty.clone(), *item.clone(), None),
            Type::Str => (Type::List(Box::new(Type::Str)), Type::Str, Some("str.chars")),
            Type::Set(item) | Type::Dict(item, _) => (Type::List(item.clone()), *item.clone(), Some("list")),
            Type::Bytes => (Type::List(Box::new(Type::Int)), Type::Int, Some("list")),
            Type::Named(name) if name == "Args" => (Type::List(Box::new(Type::Str)), Type::Str, Some("list")),
            other => return Err(format!("it has a `for` loop over {other}")),
        };
        let items = self.temporary(held);
        self.emit(
            match conversion {
                None => InstKind::Copy { dest: items, value },
                Some(name) => InstKind::Builtin {
                    dest: Some(items),
                    name: name.to_owned(),
                    args: vec![value],
                    changes: false,
                },
            },
            span,
        );
        let limit = self.temporary(Type::Int);
        let args = vec![Operand::Local(items)];
        let kind = InstKind::Builtin { dest: Some(limit), name: "list.len".to_owned(), args, changes: false };
        self.emit(kind, span);
        let counting = Counting {
            names,
            first: Operand::Int(0),
            limit: Operand::Local(limit),
            down: false,
            items: Some((items, item)),
            back,
        };
        self.counting(counting, span, body)
    }

    /// Counts in a local of its own, so the loop's name is a fresh value each time around.
    /// The limit is read once. Going one step at a time, the count can't overflow, since
    /// it's short of the limit before each step.
    fn counting<F>(&mut self, loop_of: Counting<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let Counting { names, first, limit, down, items, back } = loop_of;
        let count = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: count, value: first }, span);
        let stop = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: stop, value: limit }, span);
        let (head, then, next, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(count), Operand::Local(stop));
        let op = if down { BinOp::Gt } else { BinOp::Lt };
        self.emit(InstKind::Binary { dest: going, op, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then, otherwise: exit });
        self.current = then;
        self.scopes.push(HashMap::new());
        let bound = self.bind_item(names, items, count, span);
        let item = names.first().and_then(|name| self.lookup(name));
        let leaving = back.zip(item).map(|(place, item)| Leaving { place, item, count });
        let broke = if leaving.is_some() { self.new_block() } else { exit };
        self.leaving.extend(leaving.clone());
        self.loops.push(Loop { next, exit: broke });
        let lowered = bound.and_then(|()| body(self));
        self.loops.pop();
        if leaving.is_some() {
            self.leaving.pop();
        }
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        if let Some(leaving) = &leaving {
            self.write_item(leaving, span)?;
        }
        let op = if down { BinOp::Sub } else { BinOp::Add };
        let lhs = Operand::Local(count);
        self.emit(InstKind::Binary { dest: count, op, lhs, rhs: Operand::Int(1) }, span);
        self.finish(Terminator::Jump(head));
        if let Some(leaving) = &leaving {
            self.current = broke;
            self.write_item(leaving, span)?;
            self.finish(Terminator::Jump(exit));
        }
        self.current = exit;
        Ok(())
    }

    /// Gives a loop's names their values for this time around.
    fn bind_item(
        &mut self,
        names: &[String],
        items: Option<(LocalId, Type)>,
        count: LocalId,
        span: Span,
    ) -> Lowered<()> {
        let Some((items, ty)) = items else {
            let [name] = names else { return Err("it unpacks a number".to_owned()) };
            let dest = self.declare(name, Type::Int);
            self.emit(InstKind::Copy { dest, value: Operand::Local(count) }, span);
            return Ok(());
        };
        let (object, index) = (Operand::Local(items), Operand::Local(count));
        if let [name] = names {
            let dest = self.declare(name, ty);
            self.emit(InstKind::GetIndex { dest, object, index }, span);
            return Ok(());
        }
        let item = self.temporary(ty);
        self.emit(InstKind::GetIndex { dest: item, object, index }, span);
        self.unpack(names, &Operand::Local(item), span)
    }

    /// Whether an expression is a call to the built-in `range`.
    fn is_range(&self, iter: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &iter.kind else { return false };
        matches!(&callee.kind, ExprKind::Name(name) if name == "range" && self.lookup(name).is_none())
            && !self.context.functions.contains("range")
    }
}

/// The first and limit arguments of a call to `range` with a step of 1 or -1, and
/// whether it counts down.
fn range_args(iter: &Expr) -> Option<(Vec<&Expr>, bool)> {
    let ExprKind::Call { args, .. } = &iter.kind else { return None };
    if args.iter().any(|arg| arg.name.is_some() || arg.is_mut) {
        return None;
    }
    let values: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
    match values.as_slice() {
        [first, limit, step] => {
            let down = match &step.kind {
                ExprKind::Int(1) => false,
                ExprKind::Int(-1) => true,
                ExprKind::Unary { op: UnaryOp::Neg, expr } if matches!(expr.kind, ExprKind::Int(1)) => true,
                _ => return None,
            };
            Some((vec![first, limit], down))
        }
        _ => Some((values, false)),
    }
}
