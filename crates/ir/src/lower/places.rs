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

/// A `for mut` loop being lowered: leaving it, however that happens, writes its item back
/// to the list.
#[derive(Clone)]
pub(super) struct Leaving {
    pub(super) place: Place,
    pub(super) item: LocalId,
    pub(super) count: LocalId,
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
            Type::Named(owner) => {
                let signatures = &self.context.checked.signatures;
                let found = signatures
                    .get(&format!("{owner}.[]="))
                    .or_else(|| signatures.get(&format!("{owner}.[]")));
                found?.params.get(1).map(|param| param.ty.clone())
            }
            _ => None,
        }
    }

    /// What indexing a value of this type gives.
    fn item_type(&self, ty: &Type) -> Option<Type> {
        match ty {
            Type::List(item) | Type::Dict(_, item) => Some(*item.clone()),
            Type::Named(owner) => {
                let signatures = &self.context.checked.signatures;
                if let Some(read) = signatures.get(&format!("{owner}.[]")) {
                    return Some(read.returns.clone());
                }
                signatures.get(&format!("{owner}.[]="))?.params.get(2).map(|param| param.ty.clone())
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
                let found = self.struct_fields(owner)?.iter().find(|(name, _)| name == field);
                found.map(|(_, ty)| ty.clone()).ok_or_else(|| format!("`{owner}` has no `{field}`"))
            }
            Step::Index(_) => {
                self.item_type(ty).ok_or_else(|| "it changes an item of something that has none".to_owned())
            }
        }
    }

    /// The type of what a place holds, as it's declared.
    fn steps_type(&self, place: &Place) -> Lowered<Type> {
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
                Step::Index(index) => self.read_item(&ty, read, index.clone(), &held, span)?,
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
    ) -> Lowered<Operand> {
        let dest = self.temporary(item.clone());
        if let Type::Named(owner) = ty {
            let args = vec![object, index];
            let function = format!("{owner}.[]");
            self.emit(InstKind::Call { dest: Some(dest), function, args }, span);
        } else {
            self.emit(InstKind::GetIndex { dest, object, index }, span);
        }
        Ok(Operand::Local(dest))
    }

    /// Changes a place to a value of the type it holds.
    pub(super) fn store(
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
            let Operand::Local(held) = self.read_item(&ty, container, index.clone(), &item, span)? else {
                return Err("an item isn't in a local".to_owned());
            };
            self.store(held, rest, value, span)?;
            Operand::Local(held)
        };
        if let Type::Named(owner) = &ty {
            let Operand::Local(holder) = self.read_steps(local, prefix, span)? else {
                return Err("a value isn't in a local".to_owned());
            };
            let args = vec![Operand::Local(holder), index.clone(), value];
            let function = format!("{owner}.[]=");
            self.emit(InstKind::Call { dest: None, function, args }, span);
            if !prefix.is_empty() {
                self.store(local, prefix, Operand::Local(holder), span)?;
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

    /// Writes a `for mut` loop's item back to its list.
    pub(super) fn write_item(&mut self, leaving: &Leaving, span: Span) -> Lowered<()> {
        let mut steps = leaving.place.steps.clone();
        steps.push(Step::Index(Operand::Local(leaving.count)));
        self.store(leaving.place.local, &steps, Operand::Local(leaving.item), span)
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
        for loop_of in leaving.iter().rev() {
            self.write_item(loop_of, span)?;
        }
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
