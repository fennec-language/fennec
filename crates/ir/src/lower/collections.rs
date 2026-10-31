//! Sets, dicts and tuples written out, unpacking, slices, and comprehensions, which are loops
//! that fill a collection.

use fennec_check::Type;
use fennec_syntax::{CompKind, Expr, ExprKind, Span};

use super::lists::Over;
use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, Terminator};

/// The expressions of a comprehension.
pub(super) struct Parts<'a> {
    pub(super) elem: &'a Expr,
    pub(super) value: Option<&'a Expr>,
    pub(super) cond: Option<&'a Expr>,
}

impl Lowering<'_> {
    /// Values for what's written out, each made to fit where it goes.
    fn items_as(&mut self, items: &[Expr], ty: &Type) -> Lowered<Vec<Operand>> {
        let mut values = Vec::with_capacity(items.len());
        for written in items {
            let value = self.expr(written)?;
            values.push(self.coerce(value, ty, written.span)?);
        }
        Ok(values)
    }

    pub(super) fn set(&mut self, expr: &Expr, items: &[Expr]) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let Type::Set(item) = &ty else { return Err("a set has another type".to_owned()) };
        let items = self.items_as(items, item)?;
        let dest = self.temporary(ty);
        self.emit(InstKind::MakeSet { dest, items }, expr.span);
        Ok(Operand::Local(dest))
    }

    pub(super) fn dict(&mut self, expr: &Expr, written: &[(Expr, Expr)]) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let Type::Dict(key, value) = &ty else { return Err("a dict has another type".to_owned()) };
        let mut pairs = Vec::with_capacity(written.len());
        for (at, item) in written {
            let lowered = self.expr(at)?;
            let at_key = self.coerce(lowered, key, at.span)?;
            let lowered = self.expr(item)?;
            pairs.push((at_key, self.coerce(lowered, value, item.span)?));
        }
        let dest = self.temporary(ty);
        self.emit(InstKind::MakeDict { dest, pairs }, expr.span);
        Ok(Operand::Local(dest))
    }

    pub(super) fn tuple(&mut self, expr: &Expr, written: &[Expr]) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let Type::Tuple(types) = &ty else { return Err("a tuple has another type".to_owned()) };
        let mut items = Vec::with_capacity(written.len());
        for (item, wanted) in written.iter().zip(types) {
            let value = self.expr(item)?;
            items.push(self.coerce(value, wanted, item.span)?);
        }
        let dest = self.temporary(ty);
        self.emit(InstKind::MakeTuple { dest, items }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// Declares each name with its part of a tuple.
    pub(super) fn unpack(&mut self, names: &[String], value: &Operand, span: Span) -> Lowered<()> {
        let Some(Type::Tuple(types)) = crate::type_in(&self.locals, value) else {
            return Err("it unpacks something other than a tuple".to_owned());
        };
        for (at, (name, ty)) in names.iter().zip(types).enumerate() {
            let dest = self.declare(name, ty);
            self.emit(InstKind::GetTupleItem { dest, value: value.clone(), at }, span);
        }
        Ok(())
    }

    /// `first, second = pair`. The whole value is worked out before any name changes.
    pub(super) fn assign_unpacking(&mut self, targets: &[Expr], value: &Expr, span: Span) -> Lowered<()> {
        let value = self.expr(value)?;
        let Some(Type::Tuple(types)) = crate::type_in(&self.locals, &value) else {
            return Err("it unpacks something other than a tuple".to_owned());
        };
        for (at, (target, ty)) in targets.iter().zip(types).enumerate() {
            let ExprKind::Name(name) = &target.kind else {
                return Err("it unpacks into something other than names".to_owned());
            };
            let dest = self.named_target(target, name)?;
            let part = self.temporary(ty);
            self.emit(InstKind::GetTupleItem { dest: part, value: value.clone(), at }, span);
            let wanted = self.local_type(dest);
            let part = self.coerce(Operand::Local(part), &wanted, span)?;
            self.emit(InstKind::Copy { dest, value: part }, span);
        }
        Ok(())
    }

    /// `object[start:end]`.
    pub(super) fn slice(
        &mut self,
        expr: &Expr,
        object: &Expr,
        (start, end): (Option<&Expr>, Option<&Expr>),
    ) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let object = self.expr(object)?;
        let start = start.map(|bound| self.expr(bound)).transpose()?;
        let end = end.map(|bound| self.expr(bound)).transpose()?;
        let dest = self.temporary(ty);
        self.emit(InstKind::Slice { dest, object, start, end }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// A comprehension: a new collection, filled by a loop. `cond` going false moves on to
    /// the next item.
    pub(super) fn comprehension(
        &mut self,
        expr: &Expr,
        kind: CompKind,
        parts: &Parts<'_>,
        (names, iter): (&[String], &Expr),
    ) -> Lowered<Operand> {
        let span = expr.span;
        let dest = self.temporary(self.ty(expr)?);
        self.emit(
            match kind {
                CompKind::List | CompKind::Generator => InstKind::MakeList { dest, items: Vec::new() },
                CompKind::Set => InstKind::MakeSet { dest, items: Vec::new() },
                CompKind::Dict => InstKind::MakeDict { dest, pairs: Vec::new() },
            },
            span,
        );
        let over = Over { names, iter, back: None };
        self.looping(over, span, |lowering| lowering.fill(dest, kind, parts, span))?;
        Ok(Operand::Local(dest))
    }

    /// One time around a comprehension's loop.
    fn fill(&mut self, dest: LocalId, kind: CompKind, parts: &Parts<'_>, span: Span) -> Lowered<()> {
        if let Some(cond) = parts.cond {
            let cond = self.expr(cond)?;
            let next = self.loops.last().ok_or("a comprehension outside its loop")?.next;
            let then = self.new_block();
            self.finish(Terminator::Branch { cond, then, otherwise: next });
            self.current = then;
        }
        let elem = self.expr(parts.elem)?;
        if let (CompKind::Dict, Some(value)) = (kind, parts.value) {
            let value = self.expr(value)?;
            self.emit(InstKind::SetIndex { object: dest, path: Vec::new(), index: elem, value }, span);
            return Ok(());
        }
        let name = if matches!(kind, CompKind::Set) { "set.add" } else { "list.append" };
        let args = vec![Operand::Local(dest), elem];
        self.emit(InstKind::Builtin { dest: None, name: name.to_owned(), args, changes: true }, span);
        Ok(())
    }
}
