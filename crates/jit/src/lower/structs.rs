//! Plain structs in native memory: making them, reading and changing their fields, copying
//! them, and turning them into runtime objects and back where they meet the runtime.
//!
//! A plain struct value is the address of memory in some function's stack frame. Each place
//! that makes one gets its own stack slot, and naming a value copies it into the name's own
//! slot, so changing one name never changes another. A `mut` parameter is the address of
//! the caller's memory, so a method that changes `self` changes the caller's value in place.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, Value, types};
use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Span};

use super::expr::Lowered;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::layout::{Field, Layout, WORD};
use crate::types::Ty;

impl<'a> Lowerer<'a, '_> {
    pub(super) fn layout(&self, id: u32) -> Result<&'a Layout, Diagnostic> {
        let layouts = self.layouts;
        layouts.get(id).ok_or_else(|| internal("unknown struct layout", self.function.def.span))
    }

    /// Fresh memory in this function's frame for a struct.
    pub(super) fn new_struct(&mut self, id: u32) -> Result<Value, Diagnostic> {
        let size = self.layout(id)?.size;
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 3));
        Ok(self.b.ins().stack_addr(self.pointer, slot, 0))
    }

    /// Copies a struct's memory, a word at a time.
    pub(super) fn copy_struct(&mut self, to: Value, from: Value, id: u32) -> Result<(), Diagnostic> {
        let size = self.layout(id)?.size;
        for offset in (0..size).step_by(usize::try_from(WORD).unwrap_or(8)) {
            let offset = i32::try_from(offset).unwrap_or(0);
            let word = self.b.ins().load(types::I64, MemFlagsData::trusted(), from, offset);
            self.b.ins().store(MemFlagsData::trusted(), word, to, offset);
        }
        Ok(())
    }

    /// A copy of a struct in fresh memory.
    pub(super) fn clone_struct(&mut self, from: Value, id: u32) -> Result<Value, Diagnostic> {
        let to = self.new_struct(id)?;
        self.copy_struct(to, from, id)?;
        Ok(to)
    }

    pub(super) fn struct_field(&self, id: u32, name: &str) -> Result<Option<&'a Field>, Diagnostic> {
        Ok(self.layout(id)?.fields.iter().find(|field| field.name == name))
    }

    /// A field's value: a machine value, or for a nested struct, its address inside this one.
    pub(super) fn load_field(&mut self, address: Value, field: &Field) -> Lowered {
        let offset = i32::try_from(field.offset).unwrap_or(0);
        let value = match field.ty {
            Ty::Struct(_) => self.b.ins().iadd_imm_s(address, i64::from(offset)),
            Ty::Float => self.b.ins().load(types::F64, MemFlagsData::trusted(), address, offset),
            Ty::Bool => self.b.ins().load(types::I8, MemFlagsData::trusted(), address, offset),
            _ => self.b.ins().load(types::I64, MemFlagsData::trusted(), address, offset),
        };
        (Some(value), field.ty)
    }

    /// Changes a field, converting the value to the field's type first.
    pub(super) fn store_field(
        &mut self,
        address: Value,
        field: &Field,
        (value, ty): Lowered,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let value = self.convert(value, ty, field.ty, span)?;
        let offset = i32::try_from(field.offset).unwrap_or(0);
        if let Ty::Struct(id) = field.ty {
            let inside = self.b.ins().iadd_imm_s(address, i64::from(offset));
            return self.copy_struct(inside, value, id);
        }
        self.b.ins().store(MemFlagsData::trusted(), value, address, offset);
        Ok(())
    }

    /// A runtime object with a struct's values, for the parts of the program that run there.
    pub(super) fn box_struct(&mut self, address: Value, id: u32, span: Span) -> Result<Value, Diagnostic> {
        let layout = self.layout(id)?;
        for field in &layout.fields {
            let (value, ty) = self.load_field(address, field);
            let handle = self.handle_of(value, ty, span)?;
            self.call_helper(Helper::Input, &[handle])?;
        }
        let name = self.name_id(&layout.name);
        let count = self.int(i64::try_from(layout.fields.len()).unwrap_or(0));
        let [start, end] = self.span_args(span);
        self.help(Helper::MakeInstance, &[name, count, start, end])
    }

    /// A runtime object's values in fresh native memory, failing like the interpreter when
    /// it isn't the struct expected.
    pub(super) fn unbox_struct(&mut self, handle: Value, id: u32, span: Span) -> Result<Value, Diagnostic> {
        let address = self.new_struct(id)?;
        self.unbox_into(handle, id, address, span)?;
        Ok(address)
    }

    fn unbox_into(&mut self, handle: Value, id: u32, address: Value, span: Span) -> Result<(), Diagnostic> {
        let layout = self.layout(id)?;
        for field in &layout.fields {
            let copy = self.help(Helper::Copy, &[handle])?;
            let name = self.name_id(&field.name);
            let optional = self.b.ins().iconst(types::I8, 0);
            let [start, end] = self.span_args(span);
            let value = self.help(Helper::Field, &[copy, name, optional, start, end])?;
            if let Ty::Struct(inner) = field.ty {
                let inside = self.b.ins().iadd_imm_s(address, i64::from(field.offset));
                self.unbox_into(value, inner, inside, span)?;
            } else {
                self.store_field(address, field, (Some(value), Ty::Obj), span)?;
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
        let mut supplied: Vec<Option<&Arg>> = vec![None; layout.fields.len()];
        let mut next = 0;
        for arg in args {
            if arg.is_mut {
                return Ok(None);
            }
            let at = if let Some(name) = &arg.name {
                layout.fields.iter().position(|field| field.name == *name)
            } else {
                next += 1;
                Some(next - 1)
            };
            match at.and_then(|at| supplied.get_mut(at)) {
                Some(slot @ None) => *slot = Some(arg),
                _ => return Ok(None),
            }
        }
        if layout.fields.iter().zip(&supplied).any(|(field, arg)| arg.is_none() && field.default.is_none()) {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push((arg, self.expr(&arg.value)?));
        }
        let address = self.new_struct(id)?;
        for (field, arg) in layout.fields.iter().zip(&supplied) {
            let value = match arg {
                Some(arg) => {
                    values.iter().find(|(given, _)| std::ptr::eq(*given, *arg)).map(|(_, value)| *value)
                }
                None => None,
            };
            let value = match (value, &field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.expr(default)?,
                (None, None) => return Err(internal("a field with no value", span)),
            };
            self.store_field(address, field, value, span)?;
        }
        Ok(Some((Some(address), Ty::Struct(id))))
    }

    /// The address of a field path inside a struct name, like `ash.position` in
    /// `ash.position.x = 1.0`, with the field at its end.
    pub(super) fn struct_target(&mut self, target: &Expr) -> Result<Option<(Value, &'a Field)>, Diagnostic> {
        let ExprKind::Attr { object, name, optional: false } = &target.kind else { return Ok(None) };
        let Some((address, id)) = self.struct_place(object)? else { return Ok(None) };
        Ok(self.struct_field(id, name)?.map(|field| (address, field)))
    }

    /// The address of a struct name, or of a struct field inside one.
    pub(super) fn struct_place(&mut self, expr: &Expr) -> Result<Option<(Value, u32)>, Diagnostic> {
        match &expr.kind {
            ExprKind::Name(name) => Ok(match self.local(name) {
                Some(super::Local { var: Some(var), ty: Ty::Struct(id), .. }) => {
                    Some((self.b.use_var(var), id))
                }
                _ => None,
            }),
            ExprKind::Attr { object, name, optional: false } => {
                let Some((address, id)) = self.struct_place(object)? else { return Ok(None) };
                Ok(match self.struct_field(id, name)? {
                    Some(field @ Field { ty: Ty::Struct(inner), .. }) => {
                        let inside = self.b.ins().iadd_imm_s(address, i64::from(field.offset));
                        Some((inside, *inner))
                    }
                    _ => None,
                })
            }
            _ => Ok(None),
        }
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
            _ => None,
        }
    }
}
