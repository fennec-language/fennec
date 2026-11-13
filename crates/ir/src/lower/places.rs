//! Places a program can change: a local, or what's reached from it by fields and items.
//! Values are copies, so changing an item's insides reads the item, changes that, and writes
//! it back. A backend or the optimizer can do that in place.

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, Span, WithName};

use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, Terminator};

/// One step from a local towards what's changed. An index is worked out once, ahead of time.
#[derive(Clone)]
pub(super) enum Step {
    Field(String),
    Index(Operand),
}

#[derive(Clone)]
pub(super) struct Place {
    pub(super) local: LocalId,
    pub(super) steps: Vec<Step>,
}

/// What has to happen on leaving part of a function, however that happens.
#[derive(Clone)]
pub(super) enum Leaving {
    /// A `for mut` loop writes its item back to where it came from.
    Item { place: Place, item: LocalId, index: Operand },
    /// A `for mut` loop over positions writes its item back to the place the position is.
    Place { place: Place, item: LocalId },
    /// A `with` block gives back the locks of the `shared` handles it holds.
    Unlock { handles: Vec<LocalId> },
    /// A loop lets go of the generator made for it, closing what that holds.
    Generator { generator: Operand, known: Option<String> },
}

impl Lowering<'_> {
    /// The place an expression names. Its indexes run, in the order they're written.
    pub(super) fn place_of(&mut self, target: &Expr) -> Lowered<Place> {
        let other = "it changes something other than a name, field or item";
        match &target.kind {
            ExprKind::Name(name) => {
                let local = self.lookup(name).ok_or(other)?;
                Ok(Place { local, steps: Vec::new() })
            }
            ExprKind::Attr { object, name, optional: false } => {
                let mut place = self.place_of(object)?;
                place.steps.push(Step::Field(name.clone()));
                Ok(place)
            }
            ExprKind::Index { object, index } => {
                let mut place = self.place_of(object)?;
                let ty = self.steps_type(&place)?;
                if let Type::Named(owner) = &ty
                    && self.has_mut_form(owner)
                    && self.type_at(index.span) == Some(Type::Int)
                {
                    let at = self.expr(index)?;
                    return self.position_place(place, owner, at, target.span);
                }
                let key = self.key_type(&ty).ok_or("it changes an item of something that has none")?;
                let lowered = self.expr(index)?;
                place.steps.push(Step::Index(self.coerce(lowered, &key, index.span)?));
                Ok(place)
            }
            _ => Err(other.to_owned()),
        }
    }

    /// A place to come back to after other code runs, so its indexes are held apart from
    /// the names they came from.
    pub(super) fn held_place(&mut self, target: &Expr, span: Span) -> Lowered<Place> {
        let mut place = self.place_of(target)?;
        for step in &mut place.steps {
            if let Step::Index(index @ Operand::Local(_)) = step {
                let ty = crate::type_in(&self.locals, index).ok_or("an index has no type")?;
                let dest = self.temporary(ty);
                self.emit(InstKind::Copy { dest, value: index.clone() }, span);
                *index = Operand::Local(dest);
            }
        }
        Ok(place)
    }

    /// What indexes a value of this type.
    fn key_type(&self, ty: &Type) -> Option<Type> {
        match ty {
            Type::List(_) | Type::Array(..) | Type::Buffer(_) => Some(Type::Int),
            Type::Dict(key, _) => Some(*key.clone()),
            Type::Table(item) => Some(Type::Id(item.clone())),
            Type::Named(owner) => {
                let given = [Some(ty.clone())];
                let found = self
                    .resolve(&format!("{owner}.[]="), &given, None)
                    .or_else(|| self.resolve(&format!("{owner}.[]"), &given, None));
                found?.1.params.get(1).map(|param| param.ty.clone())
            }
            _ => None,
        }
    }

    /// What indexing a value of this type gives.
    fn item_type(&self, ty: &Type) -> Option<Type> {
        match ty {
            Type::List(item)
            | Type::Array(item, _)
            | Type::Buffer(item)
            | Type::Dict(_, item)
            | Type::Table(item) => Some(*item.clone()),
            Type::Named(owner) => {
                let given = [Some(ty.clone())];
                if let Some((_, read)) = self.resolve(&format!("{owner}.[]"), &given, None) {
                    return Some(read.returns);
                }
                let written = self.resolve(&format!("{owner}.[]="), &given, None)?.1;
                written.params.get(2).map(|param| param.ty.clone())
            }
            _ => None,
        }
    }

    pub(super) fn step_type(&self, ty: &Type, step: &Step) -> Lowered<Type> {
        match step {
            Step::Field(field) => {
                let Type::Named(owner) = ty else {
                    return Err(format!("it changes `.{field}` of something else"));
                };
                let fields = self.struct_fields(owner)?;
                let found = fields.iter().find(|(name, _)| name == field);
                found.map(|(_, ty)| ty.clone()).ok_or_else(|| format!("`{owner}` has no `{field}`"))
            }
            Step::Index(_) => {
                self.item_type(ty).ok_or_else(|| "it changes an item of something that has none".to_owned())
            }
        }
    }

    /// The type of what a place holds, as it's declared.
    pub(super) fn steps_type(&self, place: &Place) -> Lowered<Type> {
        let mut ty = self.local_type(place.local);
        for step in &place.steps {
            ty = self.step_type(&ty, step)?;
        }
        Ok(ty)
    }

    /// Reads a place, one temporary for each step.
    pub(super) fn read_steps(&mut self, local: LocalId, steps: &[Step], span: Span) -> Lowered<Operand> {
        let mut ty = self.local_type(local);
        let mut read = Operand::Local(local);
        for step in steps {
            let held = self.step_type(&ty, step)?;
            if !self.context.holds(&held) {
                return Err(format!("it reads a value of type {held}"));
            }
            read = match step {
                Step::Field(field) => {
                    let dest = self.temporary(held.clone());
                    self.emit(InstKind::GetField { dest, object: read, field: field.clone() }, span);
                    Operand::Local(dest)
                }
                Step::Index(index) => Operand::Local(self.read_item(&ty, read, index.clone(), &held, span)),
            };
            ty = held;
        }
        Ok(read)
    }

    /// An item of a value of type `ty`: by what the engines provide, or the type's own `[]`.
    pub(super) fn read_item(
        &mut self,
        ty: &Type,
        object: Operand,
        index: Operand,
        item: &Type,
        span: Span,
    ) -> LocalId {
        let dest = self.temporary(item.clone());
        if let Type::Named(owner) = ty {
            let found = self.resolve(&format!("{owner}.[]"), &[Some(ty.clone())], Some(item));
            let function = found.map_or_else(|| format!("{owner}.[]"), |(name, _)| name);
            let args = vec![object, index];
            self.emit(InstKind::Call { dest: Some(dest), function, args }, span);
        } else {
            self.emit(InstKind::GetIndex { dest, object, index }, span);
        }
        dest
    }

    /// Changes a place to a value of the type it holds, then runs the `where` rule of every
    /// field the write went through. A broken one stops the program.
    pub(super) fn store(
        &mut self,
        local: LocalId,
        steps: &[Step],
        value: Operand,
        span: Span,
    ) -> Lowered<()> {
        self.write_place(local, steps, value, span)?;
        self.check_rules(local, steps, span)
    }

    /// Runs the `where` rule of every field a write to a place went through.
    pub(super) fn check_rules(&mut self, local: LocalId, steps: &[Step], span: Span) -> Lowered<()> {
        let mut ty = self.local_type(local);
        for (at, step) in steps.iter().enumerate() {
            let held = self.step_type(&ty, step)?;
            if let (Step::Field(field), Type::Named(owner)) = (step, &ty) {
                let reached = steps.get(..=at).unwrap_or_default();
                self.check_rule(owner, field, (local, reached), span)?;
            }
            ty = held;
        }
        Ok(())
    }

    /// Runs the `where` rule on a field, if it has one. The rule sees only that field.
    fn check_rule(
        &mut self,
        owner: &str,
        field: &str,
        (local, steps): (LocalId, &[Step]),
        span: Span,
    ) -> Lowered<()> {
        let context = self.context;
        let copy = context.type_instance(owner);
        let base = copy.as_ref().map_or(owner, |copy| copy.base.as_str());
        let Some(rules) = context.rules.get(base) else { return Ok(()) };
        let Some((_, rule)) = rules.iter().find(|(name, _)| *name == field) else { return Ok(()) };
        let value = self.read_steps(local, steps, span)?;
        let ty = crate::type_in(&self.locals, &value).ok_or("a value has no type")?;
        let body = context.signatures.types().body(&format!("fields of {base}"));
        let body = body.ok_or_else(|| format!("the fields of `{owner}` have no types"))?;
        let subst = copy.map(|copy| copy.subst).unwrap_or_default();
        let outer_body = std::mem::replace(&mut self.body, body);
        let outer_scopes = std::mem::replace(&mut self.scopes, vec![std::collections::HashMap::new()]);
        let outer_subst = std::mem::replace(&mut self.subst, subst);
        let held = self.declare(field, ty);
        self.emit(InstKind::Copy { dest: held, value }, span);
        let cond = self.expr(rule);
        self.body = outer_body;
        self.scopes = outer_scopes;
        self.subst = outer_subst;
        let (then, broken) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: cond?, then, otherwise: broken });
        self.current = broken;
        let message = format!("this breaks the `where` rule on `{owner}.{field}`");
        self.finish(Terminator::Stop { message, span });
        self.current = then;
        Ok(())
    }

    /// Changes a place, however deeply it's nested.
    pub(super) fn write_place(
        &mut self,
        local: LocalId,
        steps: &[Step],
        value: Operand,
        span: Span,
    ) -> Lowered<()> {
        let first_index = steps.iter().position(|step| matches!(step, Step::Index(_)));
        let Some(at) = first_index else {
            if steps.is_empty() {
                self.emit(InstKind::Copy { dest: local, value }, span);
            } else {
                let path = field_path(steps);
                self.emit(InstKind::SetField { object: local, path, value }, span);
            }
            return Ok(());
        };
        let (prefix, rest) = steps.split_at(at);
        let Some((Step::Index(index), rest)) = rest.split_first() else {
            return Err("a place lost its index".to_owned());
        };
        let mut ty = self.local_type(local);
        for step in prefix {
            ty = self.step_type(&ty, step)?;
        }
        let value = if rest.is_empty() {
            value
        } else {
            let item = self.item_type(&ty).ok_or("it changes an item of something that has none")?;
            let container = self.read_steps(local, prefix, span)?;
            let held = self.read_item(&ty, container, index.clone(), &item, span);
            self.write_place(held, rest, value, span)?;
            Operand::Local(held)
        };
        if let Type::Named(owner) = &ty {
            let Operand::Local(holder) = self.read_steps(local, prefix, span)? else {
                return Err("a value isn't in a local".to_owned());
            };
            let found = self.resolve(&format!("{owner}.[]="), &[Some(ty.clone())], None);
            let function = found.map_or_else(|| format!("{owner}.[]="), |(name, _)| name);
            let args = vec![Operand::Local(holder), index.clone(), value];
            self.emit(InstKind::Call { dest: None, function, args }, span);
            if !prefix.is_empty() {
                self.write_place(local, prefix, Operand::Local(holder), span)?;
            }
            return Ok(());
        }
        let path = field_path(prefix);
        self.emit(InstKind::SetIndex { object: local, path, index: index.clone(), value }, span);
        Ok(())
    }

    /// `object.field = value`, `items[index] = value`, and `+=` and so on.
    pub(super) fn assign_place(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        span: Span,
    ) -> Lowered<()> {
        let (calls, passes_ref) = calls_in(value);
        // The target comes first, left to right: an index the value changes by `ref` isn't the
        // place that's written.
        let place = if passes_ref { self.held_place(target, span)? } else { self.place_of(target)? };
        let held = self.steps_type(&place)?;
        // What a call could change is read before it, so `+=` adds to what was there. One that
        // can't is read after, so the place is its only owner as it's added to: a text grows
        // where it is rather than being copied every time.
        let read_first = match op {
            Some(_) if calls && self.calls_could_change(target, &place, value, passes_ref)? => {
                Some(self.read_steps(place.local, &place.steps, span)?)
            }
            _ => None,
        };
        let mut value = self.expr(value)?;
        if let Some(op) = op {
            let read = match read_first {
                Some(read) => read,
                None => self.read_steps(place.local, &place.steps, span)?,
            };
            // A text growing in a field is one local read, added to and put back, as a local's
            // `+=` is, so a backend can grow it where it is.
            if let (BinOp::Add, Type::Str, Operand::Local(dest)) = (op, &held, &read) {
                let dest = *dest;
                self.emit(InstKind::Binary { dest, op, lhs: read, rhs: value }, span);
                return self.store(place.local, &place.steps, Operand::Local(dest), span);
            }
            value = self.operate(op, (read, value), &held, span)?;
        }
        let value = self.coerce(value, &held, span)?;
        self.store(place.local, &place.steps, value, span)
    }

    /// Whether the calls in `value` could change what `place` holds. Only a call given something
    /// by `ref` can change a value, or one on what the place is reached from, as a method taking
    /// `ref self` is, which names it. A `shared` handle on the way to the place is the exception:
    /// any call given it, or given what holds it, can lock it and change it.
    fn calls_could_change(
        &self,
        target: &Expr,
        place: &Place,
        value: &Expr,
        passes_ref: bool,
    ) -> Lowered<bool> {
        if passes_ref || super::root_name(target).is_none_or(|root| super::mentions(value, root)) {
            return Ok(true);
        }
        let mut ty = self.local_type(place.local);
        for step in &place.steps {
            if self.context.is_handle(&ty) {
                return Ok(true);
            }
            ty = self.step_type(&ty, step)?;
        }
        Ok(self.context.is_handle(&ty))
    }

    /// Does what leaving a `for mut` loop or `with` block has to.
    pub(super) fn undo(&mut self, leaving: &Leaving, span: Span) -> Lowered<()> {
        match leaving {
            Leaving::Item { place, item, index } => {
                let mut steps = place.steps.clone();
                steps.push(Step::Index(index.clone()));
                self.store(place.local, &steps, Operand::Local(*item), span)
            }
            Leaving::Place { place, item } => {
                self.store(place.local, &place.steps, Operand::Local(*item), span)
            }
            Leaving::Unlock { handles } => {
                // A failure leaving a lock to write puts back what the block changed, as a crash
                // does: the last taken first.
                if self.failing_out {
                    for handle in handles.iter().rev() {
                        let args = vec![Operand::Local(*handle)];
                        let name = "lock.roll_back".to_owned();
                        self.emit(InstKind::Builtin { dest: None, name, args, changes: true }, span);
                    }
                }
                let kind = match handles.as_slice() {
                    [handle] => InstKind::Unlock { handle: Operand::Local(*handle) },
                    _ => {
                        InstKind::UnlockAll { handles: handles.iter().copied().map(Operand::Local).collect() }
                    }
                };
                self.emit(kind, span);
                Ok(())
            }
            Leaving::Generator { generator, known } => {
                // A cleanup's failure leaves from here, through what was entered before the
                // loop, and not through this again.
                let at = self.leaving.iter().position(|entered| {
                    matches!(entered, Leaving::Generator { generator: held, .. } if held == generator)
                });
                let all = self.leaving.clone();
                if let Some(at) = at {
                    self.leaving.truncate(at);
                }
                let let_go = match self.failing_with {
                    Some(failure) => {
                        self.let_go_noting(generator, known.clone(), failure, span);
                        Ok(())
                    }
                    None => self.let_go_now(generator, known.clone(), span),
                };
                self.leaving = all;
                let_go
            }
        }
    }

    /// Does what leaving has to for everything entered since there were `depth` of them.
    pub(super) fn undo_since(&mut self, depth: usize, span: Span) -> Lowered<()> {
        let leaving = self.leaving.get(depth..).unwrap_or_default().to_vec();
        for entered in leaving.iter().rev() {
            self.undo(entered, span)?;
        }
        Ok(())
    }

    /// `with value as name:`, or `with a, b as mut x, mut y:`. What it holds is cleaned up when
    /// the block is left, the last one first. The `shared` handles it holds are locked for the
    /// whole block, to write through a `mut` name and otherwise to read.
    pub(super) fn with_block(
        &mut self,
        value: &Expr,
        names: &[WithName],
        body: &[fennec_syntax::Stmt],
        tail: super::Tail,
        span: Span,
    ) -> Lowered<()> {
        let items: Vec<&Expr> = match &value.kind {
            ExprKind::Tuple(items) if names.len() > 1 => items.iter().collect(),
            _ => vec![value],
        };
        let held = items.into_iter().map(|item| self.expr(item)).collect::<Lowered<Vec<_>>>()?;
        self.scopes.push(std::collections::HashMap::new());
        let mut entries = Vec::new();
        let mut locks = Vec::new();
        for (at, held) in held.into_iter().enumerate() {
            let ty = crate::type_in(&self.locals, &held).ok_or("a value has no type")?;
            let resource = match names.get(at) {
                Some(name) => self.declare(&name.name, ty.clone()),
                None => self.temporary(ty.clone()),
            };
            self.emit(InstKind::Copy { dest: resource, value: held }, span);
            if self.context.is_handle(&ty) {
                locks.push((resource, names.get(at).is_some_and(|name| name.is_mut)));
            }
        }
        let kind = match locks.as_slice() {
            [] => None,
            [(handle, write)] => Some(InstKind::Lock { handle: Operand::Local(*handle), write: *write }),
            _ => Some(InstKind::LockAll {
                handles: locks.iter().map(|(handle, write)| (Operand::Local(*handle), *write)).collect(),
            }),
        };
        if let Some(kind) = kind {
            // A `with` can end the program here, and it says so at what it takes.
            self.emit(kind, value.span);
            entries.push(Leaving::Unlock { handles: locks.iter().map(|(handle, _)| *handle).collect() });
        }
        let locked = self.locked.len();
        self.locked.extend(locks.iter().map(|(handle, _)| *handle));
        let depth = self.leaving.len();
        self.leaving.extend(entries);
        let lowered = self.stmts(body, tail);
        let entries = self.leaving.split_off(depth);
        self.locked.truncate(locked);
        self.scopes.pop();
        lowered?;
        if self.is_open() {
            for entry in entries.iter().rev() {
                self.undo(entry, span)?;
            }
        }
        Ok(())
    }

    /// `assert cond`. One that compares two values says what they were when it fails.
    pub(super) fn assert(&mut self, cond: &Expr) -> Lowered<()> {
        let span = cond.span;
        if let Some((value, pattern)) = cond.asserted_failure() {
            return self.assert_fails(value, pattern, span);
        }
        let (test, sides) = self.asserted(cond)?;
        let (then, failed) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: test, then, otherwise: failed });
        self.current = failed;
        if let Some((left, right)) = sides {
            let name = "assert.sides".to_owned();
            self.emit(InstKind::Builtin { dest: None, name, args: vec![left, right], changes: false }, span);
        }
        self.finish(Terminator::Stop { message: "this assertion failed".to_owned(), span });
        self.current = then;
        Ok(())
    }

    /// `assert load(text) fails Oops(line=3)`: it fails, with what the pattern matches. If not,
    /// the assertion fails saying what happened instead: what it gave, or what it failed with.
    fn assert_fails(&mut self, value: &Expr, pattern: &fennec_syntax::Pattern, span: Span) -> Lowered<()> {
        let held = self.expr(value)?;
        let Some(Type::Result(ok, error)) = crate::type_in(&self.locals, &held) else {
            return Err("it asserts a failure of something that can't fail".to_owned());
        };
        let failed = self.temporary(Type::Bool);
        self.emit(InstKind::IsErr { dest: failed, value: held.clone() }, span);
        let (failing, gave, mismatched, done) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(failed), then: failing, otherwise: gave });
        self.current = gave;
        let shown = if *ok == Type::Unit {
            Operand::Str("it gave nothing, and didn't fail".to_owned())
        } else {
            let got = self.temporary(*ok.clone());
            self.emit(InstKind::GetOk { dest: got, value: held.clone() }, span);
            self.said("it gave ", Operand::Local(got), span)
        };
        self.emit(
            InstKind::Builtin {
                dest: None,
                name: "assert.said".to_owned(),
                args: vec![shown],
                changes: false,
            },
            span,
        );
        self.finish(Terminator::Stop { message: "this assertion failed".to_owned(), span });
        self.current = failing;
        let why = self.temporary(*error.clone());
        self.emit(InstKind::GetErr { dest: why, value: held }, span);
        self.scopes.push(std::collections::HashMap::new());
        let matched = self.pattern(pattern, &Operand::Local(why), &error, mismatched, span);
        self.scopes.pop();
        matched?;
        self.finish(Terminator::Jump(done));
        self.current = mismatched;
        let shown = self.said("it failed with ", Operand::Local(why), span);
        self.emit(
            InstKind::Builtin {
                dest: None,
                name: "assert.said".to_owned(),
                args: vec![shown],
                changes: false,
            },
            span,
        );
        self.finish(Terminator::Stop { message: "this assertion failed".to_owned(), span });
        self.current = done;
        Ok(())
    }

    /// `said` and then a value, as an f-string writes it.
    fn said(&mut self, said: &str, value: Operand, span: Span) -> Operand {
        let dest = self.temporary(Type::Str);
        let parts =
            vec![crate::FormatPart::Text(said.to_owned()), crate::FormatPart::Value { value, spec: None }];
        self.emit(InstKind::Format { dest, parts }, span);
        Operand::Local(dest)
    }

    /// What an `assert` tests, and the two sides of a comparison.
    fn asserted(&mut self, cond: &Expr) -> Lowered<(Operand, Option<(Operand, Operand)>)> {
        let span = cond.span;
        // A failed assert says what each side was, so a `Secret` in one has to stay hidden.
        if let ExprKind::Binary { lhs, rhs, .. } = &cond.kind {
            for side in [lhs, rhs] {
                if let Some(held) = self.type_at(side.span)
                    && self.holds_secret(&held, &mut Vec::new())
                {
                    self.own_texts_within(&held, &mut Vec::new(), side.span)?;
                }
            }
        }
        let ExprKind::Binary { op, lhs, rhs } = &cond.kind else { return Ok((self.expr(cond)?, None)) };
        match op {
            BinOp::Is | BinOp::IsNot => {
                let (test, value) = self.is_none_of(*op, lhs, rhs, span)?;
                Ok((test, Some((value, Operand::None))))
            }
            BinOp::In | BinOp::NotIn => {
                let (test, item, items) = self.contains_of(*op, lhs, rhs, span)?;
                Ok((test, Some((item, items))))
            }
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                let ty = self.ty(cond)?;
                let left = self.expr(lhs)?;
                let (left, right) = self.both(left, |lowering| lowering.expr(rhs), span)?;
                let test = self.operate(*op, (left.clone(), right.clone()), &ty, span)?;
                Ok((test, Some((left, right))))
            }
            _ => Ok((self.expr(cond)?, None)),
        }
    }

    /// Returns from the function. Every `for mut` loop being left writes its item back
    /// first, and the value given back is what it was before they do.
    pub(super) fn leave(&mut self, value: Option<Operand>, span: Span) -> Lowered<()> {
        let leaving = self.leaving.clone();
        let mut value = value;
        if !leaving.is_empty()
            && let Some(held @ Operand::Local(_)) = &value
        {
            let ty = crate::type_in(&self.locals, held).ok_or("a value has no type")?;
            let dest = self.temporary(ty);
            self.emit(InstKind::Copy { dest, value: held.clone() }, span);
            value = Some(Operand::Local(dest));
        }
        self.undo_since(0, span)?;
        self.finish(Terminator::Return(value));
        Ok(())
    }
}

/// Whether an expression makes a call, and whether one passes something by `ref`.
pub(super) fn calls_in(value: &Expr) -> (bool, bool) {
    let (mut calls, mut passes_ref) = (false, false);
    fennec_syntax::visit::in_expr(&mut value.clone(), &mut |seen: &mut Expr| {
        if let ExprKind::Call { args, .. } = &seen.kind {
            calls = true;
            passes_ref |= args.iter().any(|arg| arg.is_mut);
        }
    });
    (calls, passes_ref)
}

fn field_path(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Field(field) => Some(field.clone()),
            Step::Index(_) => None,
        })
        .collect()
}
