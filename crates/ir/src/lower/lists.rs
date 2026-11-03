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
pub(super) struct Counting<'a> {
    pub(super) names: &'a [String],
    pub(super) first: Operand,
    pub(super) limit: Operand,
    pub(super) down: bool,
    pub(super) items: Option<(LocalId, Type)>,
    /// Where each item goes back to, for a `for mut` loop.
    pub(super) back: Option<Back>,
}

/// Where a `for mut` loop's items go back to.
pub(super) struct Back {
    place: Place,
    /// It goes through `IdMap` items by their `Id`s, not by position.
    by_id: bool,
}

/// What a loop goes through.
pub(super) struct Over<'a> {
    pub(super) names: &'a [String],
    pub(super) iter: &'a Expr,
    pub(super) back: Option<Back>,
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
        let held = self.type_at(object.span);
        if let (Some(Type::Tuple(items)), ExprKind::Int(at)) = (&held, &index.kind)
            && let Some(at) = usize::try_from(*at).ok().filter(|at| items.get(*at) == Some(&ty))
        {
            let value = self.expr(object)?;
            let dest = self.temporary(ty);
            self.emit(InstKind::GetTupleItem { dest, value, at }, expr.span);
            return Ok(Operand::Local(dest));
        }
        let key = match &held {
            Some(Type::List(_) | Type::Buffer(_) | Type::Str | Type::Bytes | Type::Tuple(_)) => Type::Int,
            Some(Type::Dict(key, _)) => *key.clone(),
            Some(Type::IdMap(item)) => Type::Id(item.clone()),
            Some(Type::Named(name)) if name == "Args" => {
                let object = self.expr(object)?;
                let index = self.expr(index)?;
                let dest = self.temporary(ty);
                self.emit(InstKind::GetIndex { dest, object, index }, expr.span);
                return Ok(Operand::Local(dest));
            }
            Some(Type::Named(owner)) => {
                let found = self.resolve(&format!("{owner}.[]"), std::slice::from_ref(&held), Some(&ty));
                if found.is_none() && self.has_mut_form(owner) {
                    let place = self.place_of(expr)?;
                    return self.read_steps(place.local, &place.steps, expr.span);
                }
                let param = found.as_ref().and_then(|(_, found)| found.params.get(1));
                param.ok_or_else(|| format!("it indexes `{owner}`, which has no `[]`"))?.ty.clone()
            }
            _ => return Err("it uses indexing on something the IR doesn't index".to_owned()),
        };
        let object = self.expr(object)?;
        let lowered = self.expr(index)?;
        let index = self.coerce(lowered, &key, index.span)?;
        Ok(Operand::Local(self.read_item(held.as_ref().unwrap_or(&ty), object, index, &ty, expr.span)))
    }

    /// `item in value` when `value`'s type has its own `def in`: a call to it, with the value
    /// first, and `not in` its opposite. The item is worked out first, as it's written first.
    fn own_in(
        &mut self,
        op: BinOp,
        item: &Expr,
        items: &Expr,
        span: Span,
    ) -> Lowered<Option<(Operand, Operand, Operand)>> {
        let Some(held) = self.type_at(items.span) else { return Ok(None) };
        let Some(key) = owner_name(&held).map(|owner| format!("{owner}.in")) else { return Ok(None) };
        let given = [Some(held.clone()), self.type_at(item.span)];
        let Some((function, signature)) = self.resolve(&key, &given, None) else { return Ok(None) };
        let item = self.expr(item)?;
        let items = self.expr(items)?;
        let own = self.context.functions.contains(self.context.base_key(&key).as_str());
        let item = match signature.params.get(1) {
            Some(param) if own => self.coerce(item, &param.ty, span)?,
            _ => item,
        };
        let dest = self.temporary(Type::Bool);
        let args = vec![items.clone(), item.clone()];
        self.emit(InstKind::Call { dest: Some(dest), function, args }, span);
        if op == BinOp::In {
            return Ok(Some((Operand::Local(dest), item, items)));
        }
        let flipped = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value: Operand::Local(dest) }, span);
        Ok(Some((Operand::Local(flipped), item, items)))
    }

    /// `item in items`, and `not in`, on a collection.
    pub(super) fn contains(&mut self, op: BinOp, item: &Expr, items: &Expr, span: Span) -> Lowered<Operand> {
        self.contains_of(op, item, items, span).map(|(test, _, _)| test)
    }

    /// The same, with both sides.
    pub(super) fn contains_of(
        &mut self,
        op: BinOp,
        item: &Expr,
        items: &Expr,
        span: Span,
    ) -> Lowered<(Operand, Operand, Operand)> {
        if let Some(found) = self.own_in(op, item, items, span)? {
            return Ok(found);
        }
        let owner = match self.type_at(items.span) {
            Some(
                ref ty @ (Type::List(_)
                | Type::Set(_)
                | Type::Dict(..)
                | Type::IdMap(_)
                | Type::Str
                | Type::Bytes),
            ) => owner_name(ty).unwrap_or_default().to_owned(),
            _ => return Err(format!("it uses `{}` on a type of its own", op.symbol())),
        };
        if let Some(Type::List(held)) = self.type_at(items.span)
            && self.context.signatures.own_equality(&held)
        {
            return Err(super::operators::OWN_EQUALITY.to_owned());
        }
        let item = self.expr(item)?;
        let items = self.expr(items)?;
        let dest = self.temporary(Type::Bool);
        let name = format!("{owner}.contains");
        let args = vec![items.clone(), item.clone()];
        self.emit(InstKind::Builtin { dest: Some(dest), name, args, changes: false }, span);
        if op == BinOp::In {
            return Ok((Operand::Local(dest), item, items));
        }
        let flipped = self.temporary(Type::Bool);
        let value = Operand::Local(dest);
        self.emit(InstKind::Unary { dest: flipped, op: UnaryOp::Not, value }, span);
        Ok((Operand::Local(flipped), item, items))
    }

    /// A value as printing shows it: when its type writes its own text with `def str`, that
    /// text, from a `StrWriter` the method fills. Any other value as it is, for printing to
    /// show as it always has.
    pub(super) fn shown(&mut self, expr: &Expr) -> Lowered<Operand> {
        let Some(held) = self.type_at(expr.span) else { return self.expr(expr) };
        let writer = Type::Named("StrWriter".to_owned());
        let Some(function) = self.own_str(&held) else {
            // Native code prints a struct's fields itself, which would miss their own text.
            if self.holds_own_str(&held, &mut Vec::new()) {
                return Err(format!("it prints a {held}, which holds a value with its own `def str`"));
            }
            return self.expr(expr);
        };
        let value = self.expr(expr)?;
        let out = self.make("StrWriter", &[], expr.span)?;
        self.emit(InstKind::Call { dest: None, function, args: vec![value, out.clone()] }, expr.span);
        let (text, _) = self
            .resolve("StrWriter.text", &[Some(writer)], None)
            .ok_or("it prints a value with its own `def str`, but `StrWriter.text` isn't there")?;
        let dest = self.temporary(Type::Str);
        self.emit(InstKind::Call { dest: Some(dest), function: text, args: vec![out] }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// The `def str` a type writes its own text with, if it has one.
    fn own_str(&self, held: &Type) -> Option<String> {
        let key = format!("{}.str", owner_name(held)?);
        let writer = Type::Named("StrWriter".to_owned());
        self.resolve(&key, &[Some(held.clone()), Some(writer)], None).map(|(function, _)| function)
    }

    /// Whether a struct holds, in a field or a field's field, a value whose type has its own
    /// `def str`. `seen` are the structs looked at already.
    fn holds_own_str(&self, held: &Type, seen: &mut Vec<String>) -> bool {
        let name = match held {
            Type::Optional(inner) => return self.holds_own_str(inner, seen),
            Type::Tuple(items) => return items.iter().any(|item| self.holds_own_str(item, seen)),
            Type::Named(name) | Type::Generic(name, _) => name,
            _ => return false,
        };
        if seen.contains(name) {
            return false;
        }
        seen.push(name.clone());
        let Ok(fields) = self.struct_fields(&held.to_string()) else { return false };
        fields.iter().any(|(_, ty)| self.own_str(ty).is_some() || self.holds_own_str(ty, seen))
    }

    pub(super) fn format(&mut self, parts: &[FStrPart], span: Span) -> Lowered<Operand> {
        let mut lowered = Vec::with_capacity(parts.len());
        for part in parts {
            lowered.push(match part {
                FStrPart::Text(text) => FormatPart::Text(text.clone()),
                FStrPart::Expr { expr, spec } => {
                    let value = if spec.is_none() { self.shown(expr)? } else { self.expr(expr)? };
                    FormatPart::Value { value, spec: spec.clone() }
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
        // A `for mut` over `parallel(xs)` changes the items of `xs` where they are, and running
        // its iterations in order is a correct way to run it.
        let iter = if is_mut { super::parallel::parallel_items(iter).unwrap_or(iter) } else { iter };
        if !is_mut && let Some(expansion) = self.expanded(names, iter, body) {
            return self.fused(expansion);
        }
        if let (true, [name], Some(Type::Named(owner))) = (is_mut, names, self.type_at(iter.span))
            && self.has_mut_form(&owner)
        {
            return self.through_positions(name, (iter, &owner), body, span);
        }
        // Only the items of a list or `IdMap` are written back, and only one name holds them.
        let by_id = matches!(self.type_at(iter.span), Some(Type::IdMap(_)));
        let changes_items =
            is_mut && names.len() == 1 && (by_id || matches!(self.type_at(iter.span), Some(Type::List(_))));
        let back =
            if changes_items { Some(Back { place: self.held_place(iter, span)?, by_id }) } else { None };
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
        let ty = self.type_at(iter.span).ok_or("what it loops over has no type")?;
        let value = self.expr(iter)?;
        if let Type::Iterator(item) = &ty {
            let known = known_generator(iter, &|key| self.context.generators.contains_key(key));
            return self.through_generator(names, (value, (**item).clone()), known, span, body);
        }
        if ty == Type::Str && back.is_none() {
            return self.through_text(names, value, span, body);
        }
        let (held, item, conversion) = match &ty {
            // A loop goes through the list as it was, whatever the body does to the name.
            Type::List(item) | Type::Buffer(item) => (ty.clone(), *item.clone(), None),
            Type::Str => (Type::List(Box::new(Type::Str)), Type::Str, Some("str.chars")),
            Type::Set(item) | Type::Dict(item, _) => (Type::List(item.clone()), *item.clone(), Some("list")),
            Type::Bytes => (Type::List(Box::new(Type::Int)), Type::Int, Some("list")),
            Type::IdMap(item) if back.is_some() => {
                let pair = Type::Tuple(vec![Type::Id(item.clone()), *item.clone()]);
                (Type::List(Box::new(pair.clone())), pair, Some("IdMap.items"))
            }
            Type::IdMap(item) => (Type::List(item.clone()), *item.clone(), Some("IdMap.values")),
            Type::Named(name) if name == "Args" => (Type::List(Box::new(Type::Str)), Type::Str, Some("list")),
            Type::Named(name) => {
                let found = self.resolve(&format!("{name}.iter"), &[Some(ty.clone())], None);
                let Some((function, found)) = found else {
                    return Err(format!("it has a `for` loop over {name}"));
                };
                if let Type::Iterator(item) = &found.returns {
                    let generated = self.temporary(found.returns.clone());
                    let kind = InstKind::Call { dest: Some(generated), function, args: vec![value] };
                    self.emit(kind, span);
                    let source = (Operand::Local(generated), (**item).clone());
                    return self.through_generator(names, source, None, span, body);
                }
                let Type::List(item) = found.returns else {
                    return Err(format!("it has a `for` loop over {name}"));
                };
                let items = self.temporary(Type::List(item.clone()));
                self.emit(InstKind::Call { dest: Some(items), function, args: vec![value] }, span);
                return self.count_through(names, (items, *item), back, span, body);
            }
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
        self.count_through(names, (items, item), back, span, body)
    }

    /// Goes through a list held in a local of its own.
    fn count_through<F>(
        &mut self,
        names: &[String],
        (items, item): (LocalId, Type),
        back: Option<Back>,
        span: Span,
        body: F,
    ) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
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
    pub(super) fn counting<F>(&mut self, loop_of: Counting<'_>, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let Counting { names, first, limit, down, items, back } = loop_of;
        let count = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: count, value: first }, span);
        let stop = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: stop, value: limit }, span);
        self.counts.push(crate::Count { count, stop, down });
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
        let by_id = back.as_ref().is_some_and(|back| back.by_id);
        let bound = self.bind_item(names, items, count, by_id, span);
        let item = names.first().and_then(|name| self.lookup(name));
        let leaving = match (back, item, &bound) {
            (Some(back), Some(item), Ok(index)) => {
                Some(Leaving::Item { place: back.place, item, index: index.clone() })
            }
            _ => None,
        };
        let broke = if leaving.is_some() { self.new_block() } else { exit };
        self.leaving.extend(leaving.clone());
        self.loops.push(Loop { next, exit: broke, depth: self.leaving.len() });
        let lowered = bound.and_then(|_| body(self));
        self.loops.pop();
        if leaving.is_some() {
            self.leaving.pop();
        }
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        if let Some(leaving) = &leaving {
            self.undo(leaving, span)?;
        }
        let op = if down { BinOp::Sub } else { BinOp::Add };
        let lhs = Operand::Local(count);
        self.emit(InstKind::Binary { dest: count, op, lhs, rhs: Operand::Int(1) }, span);
        self.finish(Terminator::Jump(head));
        if let Some(leaving) = &leaving {
            self.current = broke;
            self.undo(leaving, span)?;
            self.finish(Terminator::Jump(exit));
        }
        self.current = exit;
        Ok(())
    }

    /// Gives a loop's names their values for this time around. Gives back what its item
    /// is indexed by.
    fn bind_item(
        &mut self,
        names: &[String],
        items: Option<(LocalId, Type)>,
        count: LocalId,
        by_id: bool,
        span: Span,
    ) -> Lowered<Operand> {
        let at = Operand::Local(count);
        let Some((items, ty)) = items else {
            let [name] = names else { return Err("it unpacks a number".to_owned()) };
            let dest = self.declare(name, Type::Int);
            self.emit(InstKind::Copy { dest, value: at.clone() }, span);
            return Ok(at);
        };
        let (object, index) = (Operand::Local(items), at.clone());
        if let ([name], false) = (names, by_id) {
            let dest = self.declare(name, ty);
            self.emit(InstKind::GetIndex { dest, object, index }, span);
            return Ok(at);
        }
        let item = self.temporary(ty.clone());
        self.emit(InstKind::GetIndex { dest: item, object, index }, span);
        let value = Operand::Local(item);
        if let (true, [name], Type::Tuple(parts)) = (by_id, names, &ty) {
            let [id, held] = parts.as_slice() else { return Err("an item has no id".to_owned()) };
            let id_local = self.temporary(id.clone());
            self.emit(InstKind::GetTupleItem { dest: id_local, value: value.clone(), at: 0 }, span);
            let dest = self.declare(name, held.clone());
            self.emit(InstKind::GetTupleItem { dest, value, at: 1 }, span);
            return Ok(Operand::Local(id_local));
        }
        self.unpack(names, &value, span)?;
        Ok(at)
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

/// The function that resumes what a loop goes over, when it can be seen from here.
fn known_generator(iter: &Expr, is_generator: &dyn Fn(&str) -> bool) -> Option<String> {
    let ExprKind::Call { callee, .. } = &iter.kind else { return None };
    let ExprKind::Name(key) = &callee.kind else { return None };
    is_generator(key).then(|| super::generators::resume_name(key))
}

impl Lowering<'_> {
    /// `for c in text`, a character at a time from where the last one ended, so nothing is
    /// made but each character. `str.char_at_byte(text, at)` gives the character starting at
    /// byte `at`, and `str.byte_len(c)` how many bytes it takes.
    fn through_text<F>(&mut self, names: &[String], text: Operand, span: Span, body: F) -> Lowered<()>
    where
        F: FnOnce(&mut Self) -> Lowered<()>,
    {
        let [name] = names else { return Err("it unpacks a string's characters".to_owned()) };
        let builtin = |name: &str, dest, args| InstKind::Builtin {
            dest: Some(dest),
            name: name.to_owned(),
            args,
            changes: false,
        };
        let whole = self.temporary(Type::Str);
        self.emit(InstKind::Copy { dest: whole, value: text }, span);
        let length = self.temporary(Type::Int);
        self.emit(builtin("str.byte_len", length, vec![Operand::Local(whole)]), span);
        let at = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: at, value: Operand::Int(0) }, span);
        let step = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: step, value: Operand::Int(0) }, span);
        let (head, then, next, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(at), Operand::Local(length));
        self.emit(InstKind::Binary { dest: going, op: BinOp::Lt, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then, otherwise: exit });
        self.current = then;
        self.scopes.push(HashMap::new());
        let character = self.declare(name, Type::Str);
        let args = vec![Operand::Local(whole), Operand::Local(at)];
        self.emit(builtin("str.char_at_byte", character, args), span);
        self.emit(builtin("str.byte_len", step, vec![Operand::Local(character)]), span);
        self.loops.push(Loop { next, exit, depth: self.leaving.len() });
        let lowered = body(self);
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        let lhs = Operand::Local(at);
        self.emit(InstKind::Binary { dest: at, op: BinOp::Add, lhs, rhs: Operand::Local(step) }, span);
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }
}
