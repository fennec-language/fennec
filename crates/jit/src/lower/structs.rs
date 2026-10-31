//! Plain structs natively: making them, reading and changing their fields, and turning them
//! into runtime objects and back where they meet the runtime.
//!
//! A struct value is either its fields' values, one machine value per number or bool
//! ("leaves", nested structs flattened), or the address of memory holding them. Values
//! stay as leaves wherever they can, so `a + b` on two `Vec3`s is three additions in
//! registers. They go to memory only to cross into a real call, a branch merge or the
//! runtime.
//!
//! A struct name keeps each leaf in a variable of its own, so changing a field is changing
//! one variable. A `mut` parameter is the address of the caller's memory instead, and a
//! method that changes `self` changes the caller's value in place.

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, Value, types};
use cranelift_frontend::Variable;
use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Span};

use super::expr::Lowered;
use super::{Local, Lowerer, abi, internal};
use crate::helpers::Helper;
use crate::layout::{Field, Layout};
use crate::types::Ty;

/// Where a struct lives that can be changed: a name's variables, or memory.
#[derive(Clone)]
pub(super) enum Place {
    Leaves(Vec<Variable>),
    Memory(Value),
}

impl<'a> Lowerer<'a, '_> {
    pub(super) fn layout(&self, id: u32) -> Result<&'a Layout, Diagnostic> {
        let layouts = self.layouts;
        layouts.get(id).ok_or_else(|| internal("unknown struct layout", self.function.def.span))
    }

    pub(super) fn struct_field(&self, id: u32, name: &str) -> Result<Option<&'a Field>, Diagnostic> {
        Ok(self.layout(id)?.fields.iter().find(|field| field.name == name))
    }

    /// Each leaf's offset and kind, nested structs flattened.
    pub(super) fn leaf_types(&self, id: u32) -> Result<Vec<(i32, Ty)>, Diagnostic> {
        let mut leaves = Vec::new();
        for field in &self.layout(id)?.fields {
            let offset = i32::try_from(field.offset).unwrap_or(0);
            match field.ty {
                Ty::Struct(inner) => {
                    leaves.extend(
                        self.leaf_types(inner)?.into_iter().map(|(inside, ty)| (offset + inside, ty)),
                    );
                }
                ty => leaves.push((offset, ty)),
            }
        }
        Ok(leaves)
    }

    /// Which of a struct's leaves belong to one of its fields.
    fn field_leaves(&self, id: u32, field: &Field) -> Result<std::ops::Range<usize>, Diagnostic> {
        let size = match field.ty {
            Ty::Struct(inner) => self.layout(inner)?.size,
            _ => crate::layout::WORD,
        };
        let (start, end) = (i64::from(field.offset), i64::from(field.offset) + i64::from(size));
        let leaves = self.leaf_types(id)?;
        let first = leaves.iter().position(|(offset, _)| i64::from(*offset) >= start).unwrap_or(leaves.len());
        let count = leaves.iter().filter(|(offset, _)| (start..end).contains(&i64::from(*offset))).count();
        Ok(first..first + count)
    }

    /// Fresh memory in this function's frame for a struct.
    pub(super) fn new_struct(&mut self, id: u32) -> Result<Value, Diagnostic> {
        let size = self.layout(id)?.size;
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 3));
        Ok(self.b.ins().stack_addr(self.pointer, slot, 0))
    }

    /// A zero for a leaf of this kind.
    pub(super) fn zero_leaf(&mut self, ty: Ty) -> Value {
        match ty {
            Ty::Float => self.b.ins().f64const(0.0),
            Ty::Bool => self.b.ins().iconst(types::I8, 0),
            _ => self.b.ins().iconst(types::I64, 0),
        }
    }

    /// A struct value made of leaves.
    pub(super) fn struct_of_leaves(&mut self, leaves: Vec<Value>) -> Value {
        let key = self.b.ins().iconst(types::I64, 0);
        self.leaves_of.insert(key, leaves);
        key
    }

    /// A struct value's leaves, loading them when it's in memory.
    pub(super) fn leaves(&mut self, value: Value, id: u32) -> Result<Vec<Value>, Diagnostic> {
        if let Some(leaves) = self.leaves_of.get(&value) {
            return Ok(leaves.clone());
        }
        let mut leaves = Vec::new();
        for (offset, ty) in self.leaf_types(id)? {
            let abi_ty = abi(ty)
                .ok_or_else(|| internal("a struct leaf without a machine type", self.function.def.span))?;
            leaves.push(self.b.ins().load(abi_ty, MemFlagsData::trusted(), value, offset));
        }
        Ok(leaves)
    }

    /// A struct value in memory, for a real call or a branch merge.
    pub(super) fn address_of(&mut self, value: Value, id: u32) -> Result<Value, Diagnostic> {
        if !self.leaves_of.contains_key(&value) {
            return Ok(value);
        }
        let address = self.new_struct(id)?;
        self.copy_struct(address, value, id)?;
        Ok(address)
    }

    /// Writes a struct value into memory.
    pub(super) fn copy_struct(&mut self, to: Value, from: Value, id: u32) -> Result<(), Diagnostic> {
        let leaves = self.leaves(from, id)?;
        for ((offset, _), leaf) in self.leaf_types(id)?.into_iter().zip(leaves) {
            self.b.ins().store(MemFlagsData::trusted(), leaf, to, offset);
        }
        Ok(())
    }

    /// A copy of a struct value, as it is now: changing what it came from won't change it.
    pub(super) fn clone_struct(&mut self, from: Value, id: u32) -> Result<Value, Diagnostic> {
        let leaves = self.leaves(from, id)?;
        Ok(self.struct_of_leaves(leaves))
    }

    /// A field's value: a machine value, or a nested struct.
    pub(super) fn load_field(&mut self, value: Value, id: u32, field: &Field) -> Result<Lowered, Diagnostic> {
        if let Some(leaves) = self.leaves_of.get(&value).cloned() {
            let range = self.field_leaves(id, field)?;
            let mine = leaves.get(range).map(<[Value]>::to_vec).unwrap_or_default();
            return Ok(match field.ty {
                Ty::Struct(_) => (Some(self.struct_of_leaves(mine)), field.ty),
                _ => (mine.first().copied(), field.ty),
            });
        }
        let offset = i32::try_from(field.offset).unwrap_or(0);
        let loaded = match field.ty {
            Ty::Struct(_) => self.b.ins().iadd_imm_s(value, i64::from(offset)),
            Ty::Float => self.b.ins().load(types::F64, MemFlagsData::trusted(), value, offset),
            Ty::Bool => self.b.ins().load(types::I8, MemFlagsData::trusted(), value, offset),
            _ => self.b.ins().load(types::I64, MemFlagsData::trusted(), value, offset),
        };
        Ok((Some(loaded), field.ty))
    }

    /// A runtime object with a struct's values, for the parts of the program that run there.
    pub(super) fn box_struct(&mut self, value: Value, id: u32, span: Span) -> Result<Value, Diagnostic> {
        let layout = self.layout(id)?;
        if let Some(cases) = &layout.cases {
            return self.box_enum(value, id, cases, span);
        }
        for field in &layout.fields {
            let (field_value, ty) = self.load_field(value, id, field)?;
            let handle = self.handle_of(field_value, ty, span)?;
            self.call_helper(Helper::Input, &[handle])?;
        }
        let name = self.name_id(&layout.name);
        let count = self.int(i64::try_from(layout.fields.len()).unwrap_or(0));
        let [start, end] = self.span_args(span);
        self.help(Helper::MakeInstance, &[name, count, start, end])
    }

    /// A runtime object's values as a native struct, failing like the interpreter when it
    /// isn't the struct expected.
    pub(super) fn unbox_struct(&mut self, handle: Value, id: u32, span: Span) -> Result<Value, Diagnostic> {
        if let Some(cases) = &self.layout(id)?.cases {
            return self.unbox_enum(handle, id, cases, span);
        }
        let mut leaves = Vec::new();
        self.unbox_leaves(handle, id, span, &mut leaves)?;
        Ok(self.struct_of_leaves(leaves))
    }

    fn unbox_leaves(
        &mut self,
        handle: Value,
        id: u32,
        span: Span,
        leaves: &mut Vec<Value>,
    ) -> Result<(), Diagnostic> {
        for field in &self.layout(id)?.fields {
            let copy = self.help(Helper::Copy, &[handle])?;
            let name = self.name_id(&field.name);
            let optional = self.b.ins().iconst(types::I8, 0);
            let [start, end] = self.span_args(span);
            let value = self.help(Helper::Field, &[copy, name, optional, start, end])?;
            match field.ty {
                Ty::Struct(inner) if self.layout(inner)?.cases.is_some() => {
                    let unboxed = self.unbox_struct(value, inner, span)?;
                    leaves.extend(self.leaves(unboxed, inner)?);
                }
                Ty::Struct(inner) => self.unbox_leaves(value, inner, span, leaves)?,
                ty => leaves.push(self.convert(Some(value), Ty::Obj, ty, span)?),
            }
        }
        Ok(())
    }

    /// `Point(x=1.0, y=2.0)`, made natively: each field from its argument, or else its
    /// default. `None` when the arguments don't line up with the fields, which the
    /// interpreter then reports.
    pub(super) fn construct(
        &mut self,
        id: u32,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let layout = self.layout(id)?;
        if layout.cases.is_some() {
            return Ok(None);
        }
        if layout.boxed {
            return self.construct_boxed(id, args, span);
        }
        let Some(values) = self.field_values(&layout.fields, args, span)? else { return Ok(None) };
        let mut leaves = Vec::new();
        for (field, value) in layout.fields.iter().zip(values) {
            match field.ty {
                Ty::Struct(inner) => leaves.extend(self.leaves(value, inner)?),
                _ => leaves.push(value),
            }
        }
        Ok(Some((Some(self.struct_of_leaves(leaves)), Ty::Struct(id))))
    }

    /// The value of each field from call arguments, in the order they're written, then
    /// defaults for the rest, each as its field holds it. `None` when the arguments don't
    /// line up with the fields.
    pub(super) fn field_values(
        &mut self,
        fields: &[Field],
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Vec<Value>>, Diagnostic> {
        let mut supplied: Vec<Option<&Arg>> = vec![None; fields.len()];
        let mut next = 0;
        for arg in args {
            if arg.is_mut {
                return Ok(None);
            }
            let at = if let Some(name) = &arg.name {
                fields.iter().position(|field| field.name == *name)
            } else {
                next += 1;
                Some(next - 1)
            };
            match at.and_then(|at| supplied.get_mut(at)) {
                Some(slot @ None) => *slot = Some(arg),
                _ => return Ok(None),
            }
        }
        if fields.iter().zip(&supplied).any(|(field, arg)| arg.is_none() && field.default.is_none()) {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push((arg, self.expr(&arg.value)?));
        }
        let mut converted = Vec::with_capacity(fields.len());
        for (field, arg) in fields.iter().zip(&supplied) {
            let given = arg.and_then(|arg| {
                values.iter().find(|(given, _)| std::ptr::eq(*given, arg)).map(|(_, value)| *value)
            });
            let (value, ty) = match (given, &field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.expr(default)?,
                (None, None) => return Err(internal("a field with no value", span)),
            };
            converted.push(self.convert(value, ty, field.ty, span)?);
        }
        Ok(Some(converted))
    }

    /// Creates a struct name: a variable for each leaf.
    pub(super) fn declare_struct(
        &mut self,
        name: &str,
        value: Value,
        id: u32,
        is_mut: bool,
    ) -> Result<(), Diagnostic> {
        let leaves = self.leaves(value, id)?;
        let mut vars = Vec::with_capacity(leaves.len());
        for ((_, ty), leaf) in self.leaf_types(id)?.into_iter().zip(leaves) {
            let abi_ty = abi(ty)
                .ok_or_else(|| internal("a struct leaf without a machine type", self.function.def.span))?;
            let var = self.b.declare_var(abi_ty);
            self.b.def_var(var, leaf);
            vars.push(var);
        }
        self.bind_place(name, Place::Leaves(vars), id, is_mut);
        Ok(())
    }

    /// Makes a name refer to a struct place: its own variables, the variables of a struct
    /// it stands for (an inlined method's `mut self`), or memory.
    pub(super) fn bind_place(&mut self, name: &str, place: Place, id: u32, is_mut: bool) {
        let identity = self.b.declare_var(types::I64);
        match place {
            Place::Memory(address) => self.b.def_var(identity, address),
            Place::Leaves(vars) => {
                self.leaf_vars.insert(identity.index(), vars);
            }
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_owned(), Local { var: Some(identity), ty: Ty::Struct(id), is_mut });
        }
    }

    /// Where a struct name lives.
    pub(super) fn local_place(&mut self, local: Local) -> Option<Place> {
        let var = local.var?;
        Some(match self.leaf_vars.get(&var.index()) {
            Some(vars) => Place::Leaves(vars.clone()),
            None => Place::Memory(self.b.use_var(var)),
        })
    }

    /// The struct a place holds now.
    pub(super) fn read_place(&mut self, place: &Place) -> Value {
        match place {
            Place::Leaves(vars) => {
                let leaves = vars.iter().map(|var| self.b.use_var(*var)).collect();
                self.struct_of_leaves(leaves)
            }
            Place::Memory(address) => *address,
        }
    }

    /// Changes the whole struct a place holds.
    pub(super) fn write_place(&mut self, place: &Place, value: Value, id: u32) -> Result<(), Diagnostic> {
        match place {
            Place::Leaves(vars) => {
                for (var, leaf) in vars.iter().zip(self.leaves(value, id)?) {
                    self.b.def_var(*var, leaf);
                }
                Ok(())
            }
            Place::Memory(address) => self.copy_struct(*address, value, id),
        }
    }

    /// The place of a nested struct field inside a place.
    fn field_place(&mut self, place: &Place, id: u32, field: &Field) -> Result<Place, Diagnostic> {
        Ok(match place {
            Place::Leaves(vars) => {
                let range = self.field_leaves(id, field)?;
                Place::Leaves(vars.get(range).map(<[Variable]>::to_vec).unwrap_or_default())
            }
            Place::Memory(address) => {
                Place::Memory(self.b.ins().iadd_imm_s(*address, i64::from(field.offset)))
            }
        })
    }

    /// Changes one field of the struct a place holds.
    pub(super) fn write_field(
        &mut self,
        place: &Place,
        id: u32,
        field: &Field,
        (value, ty): Lowered,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let value = self.convert(value, ty, field.ty, span)?;
        if let Ty::Struct(inner) = field.ty {
            let inside = self.field_place(place, id, field)?;
            return self.write_place(&inside, value, inner);
        }
        match place {
            Place::Leaves(vars) => {
                let range = self.field_leaves(id, field)?;
                if let Some(var) = vars.get(range.start) {
                    self.b.def_var(*var, value);
                }
            }
            Place::Memory(address) => {
                let offset = i32::try_from(field.offset).unwrap_or(0);
                self.b.ins().store(MemFlagsData::trusted(), value, *address, offset);
            }
        }
        Ok(())
    }

    /// The place of a struct name, or of a struct field inside one, like `ash.position`.
    pub(super) fn struct_place(&mut self, expr: &Expr) -> Result<Option<(Place, u32)>, Diagnostic> {
        match &expr.kind {
            ExprKind::Name(name) => Ok(match self.local(name) {
                Some(local @ Local { ty: Ty::Struct(id), .. }) => {
                    self.local_place(local).map(|place| (place, id))
                }
                _ => None,
            }),
            ExprKind::Index { object, index } => match self.packed(object) {
                Some((name, Ty::Struct(id))) => {
                    let address = self.item_address(&name, Ty::Struct(id), index, true, expr.span)?;
                    Ok(Some((Place::Memory(address), id)))
                }
                _ => Ok(None),
            },
            ExprKind::Attr { object, name, optional: false } => {
                let Some((place, id)) = self.struct_place(object)? else { return Ok(None) };
                match self.struct_field(id, name)? {
                    Some(field @ Field { ty: Ty::Struct(inner), .. }) => {
                        Ok(Some((self.field_place(&place, id, field)?, *inner)))
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    /// The struct place and field an assignment like `ash.position.x = 1.0` changes.
    pub(super) fn struct_target(
        &mut self,
        target: &Expr,
    ) -> Result<Option<(Place, u32, &'a Field)>, Diagnostic> {
        let ExprKind::Attr { object, name, optional: false } = &target.kind else { return Ok(None) };
        let Some((place, id)) = self.struct_place(object)? else { return Ok(None) };
        Ok(self.struct_field(id, name)?.map(|field| (place, id, field)))
    }

    /// A place's address, for a real call that may change it, and the variables to read it
    /// back into after the call.
    pub(super) fn place_address(
        &mut self,
        place: &Place,
        id: u32,
    ) -> Result<(Value, Option<Vec<Variable>>), Diagnostic> {
        match place {
            Place::Memory(address) => Ok((*address, None)),
            Place::Leaves(vars) => {
                let value = self.read_place(place);
                let address = self.new_struct(id)?;
                self.copy_struct(address, value, id)?;
                Ok((address, Some(vars.clone())))
            }
        }
    }

    /// After a call that may have changed a struct through its address, reads it back.
    pub(super) fn reload_place(
        &mut self,
        address: Value,
        vars: Option<Vec<Variable>>,
        id: u32,
    ) -> Result<(), Diagnostic> {
        if let Some(vars) = vars {
            let value = self.clone_struct(address, id)?;
            self.write_place(&Place::Leaves(vars), value, id)?;
        }
        Ok(())
    }

    /// The kind of value an expression gives, when it's known without lowering it: names,
    /// and fields of struct names.
    pub(super) fn static_ty(&self, expr: &Expr) -> Option<Ty> {
        match &expr.kind {
            ExprKind::Name(name) => self.local(name).map(|local| local.ty),
            ExprKind::Attr { object, name, optional: false } => match self.static_ty(object)? {
                Ty::Struct(id) => self.struct_field(id, name).ok()?.map(|field| field.ty),
                _ => None,
            },
            // An item of a list of dicts stays an object: reading it mustn't copy the dict.
            ExprKind::Index { .. } if matches!(self.checked_expr(expr), Some(Ty::Dict(..))) => None,
            // Anything else, like the result of a call: what the checker says it gives.
            _ => {
                let signatures = self.signatures;
                let ty = signatures.expr_type(self.current_key(), expr.span)?;
                Some(self.layouts.ty(ty))
                    .filter(|ty| matches!(ty, Ty::Struct(_) | Ty::Boxed(_) | Ty::Dict(..)))
            }
        }
    }
}
