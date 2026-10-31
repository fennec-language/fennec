//! Places a program can change: a local, or what's reached from it by fields and items.
//! Values are copies, so changing an item's insides reads the item, changes that, and writes
//! it back. A backend or the optimizer can do that in place.

use fennec_check::Type;
use fennec_syntax::{BinOp, Expr, ExprKind, Span};

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
    /// A `with` block calls the `del` of what it holds.
    With { resource: LocalId, cleanup: String },
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
            Type::List(_) => Some(Type::Int),
            Type::Dict(key, _) => Some(*key.clone()),
            Type::IdMap(item) => Some(Type::Id(item.clone())),
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
            Type::List(item) | Type::Dict(_, item) | Type::IdMap(item) => Some(*item.clone()),
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

    fn step_type(&self, ty: &Type, step: &Step) -> Lowered<Type> {
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
    fn write_place(&mut self, local: LocalId, steps: &[Step], value: Operand, span: Span) -> Lowered<()> {
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
        let place = self.place_of(target)?;
        let held = self.steps_type(&place)?;
        let mut value = self.expr(value)?;
        if let Some(op) = op {
            let read = self.read_steps(place.local, &place.steps, span)?;
            value = self.operate(op, (read, value), &held, span)?;
        }
        let value = self.coerce(value, &held, span)?;
        self.store(place.local, &place.steps, value, span)
    }

    /// Does what leaving a `for mut` loop or `with` block has to.
    pub(super) fn undo(&mut self, leaving: &Leaving, span: Span) -> Lowered<()> {
        match leaving {
            Leaving::Item { place, item, index } => {
                let mut steps = place.steps.clone();
                steps.push(Step::Index(index.clone()));
                self.store(place.local, &steps, Operand::Local(*item), span)
            }
            Leaving::With { resource, cleanup } => {
                let args = vec![Operand::Local(*resource)];
                self.emit(InstKind::Call { dest: None, function: cleanup.clone(), args }, span);
                Ok(())
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

    /// `with value as name:`. What it holds is cleaned up when the block is left.
    pub(super) fn with_block(
        &mut self,
        value: &Expr,
        name: Option<&str>,
        body: &[fennec_syntax::Stmt],
        tail: super::Tail,
        span: Span,
    ) -> Lowered<()> {
        let held = self.expr(value)?;
        let ty = crate::type_in(&self.locals, &held).ok_or("a value has no type")?;
        self.scopes.push(std::collections::HashMap::new());
        let resource = match name {
            Some(name) => self.declare(name, ty.clone()),
            None => self.temporary(ty.clone()),
        };
        self.emit(InstKind::Copy { dest: resource, value: held }, span);
        let cleanup = crate::lower::calls::owner_name(&ty).map(|owner| format!("{owner}.del"));
        let cleanup =
            cleanup.and_then(|key| self.resolve(&key, &[Some(ty.clone())], None)).map(|(name, _)| name);
        let entry = cleanup.map(|cleanup| Leaving::With { resource, cleanup });
        self.leaving.extend(entry.clone());
        let lowered = self.stmts(body, tail);
        if entry.is_some() {
            self.leaving.pop();
        }
        self.scopes.pop();
        lowered?;
        if let Some(entry) = &entry
            && self.is_open()
        {
            self.undo(entry, span)?;
        }
        Ok(())
    }

    /// `assert cond`.
    pub(super) fn assert(&mut self, cond: &Expr, span: Span) -> Lowered<()> {
        let cond = self.expr(cond)?;
        let (then, failed) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond, then, otherwise: failed });
        self.current = failed;
        self.finish(Terminator::Stop { message: "assertion failed".to_owned(), span });
        self.current = then;
        Ok(())
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

fn field_path(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Field(field) => Some(field.clone()),
            Step::Index(_) => None,
        })
        .collect()
}
