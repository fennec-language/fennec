//! Packed lists natively. A name the checker knows holds a list of numbers, bools or plain
//! structs is packed by the runtime into flat memory, and native code reads, changes and
//! appends to its items directly, checking every index. Anything else done with the list
//! goes through the runtime, which sees an ordinary list.
//!
//! An item's address stays good until the list grows or something else gets hold of it.
//! Native code asks for it again for each access, except in a loop whose body never names
//! the list: there nothing else can touch it, so the loop asks once.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use fennec_check::Type;
use fennec_interp::packed::Element;
use fennec_syntax::{Diagnostic, Expr, ExprKind, Span, Stmt};

use super::expr::Lowered;
use super::structs::Place;
use super::{Lowerer, abi, internal};
use crate::helpers::Helper;
use crate::layout::WORD;
use crate::types::Ty;

impl Lowerer<'_, '_> {
    /// The kind of item in the list a name holds, when the checker knows it's a list that
    /// can be packed.
    pub(super) fn checked_list(&self, name: &str) -> Option<Ty> {
        let signatures = self.signatures;
        let Type::List(item) = signatures.local(self.current_key(), name)? else { return None };
        match self.layouts.ty(item) {
            ty @ (Ty::Int | Ty::Float | Ty::Bool | Ty::Struct(_)) => Some(ty),
            _ => None,
        }
    }

    /// Packs the list a new name holds, and remembers the name as packed.
    pub(super) fn pack_name(&mut self, name: &str, item: Ty) -> Result<(), Diagnostic> {
        let id = self.name_id(name);
        let element = self.element(item)?;
        self.call_helper(Helper::Pack, &[id, element])?;
        if let Some(local) = self.scopes.last_mut().and_then(|scope| scope.locals.get_mut(name)) {
            local.list = Some(item);
        }
        Ok(())
    }

    /// The runtime's number for a kind of item, registering it the first time.
    fn element(&mut self, item: Ty) -> Result<Value, Diagnostic> {
        if let Some(id) = self.elements.get(&item) {
            return Ok(self.int(*id));
        }
        let described = std::rc::Rc::new(self.describe(item)?);
        let id = i64::try_from(self.shared.tables.elements.len()).unwrap_or(0);
        self.shared.tables.elements.push(described);
        self.elements.insert(item, id);
        Ok(self.int(id))
    }

    fn describe(&self, item: Ty) -> Result<Element, Diagnostic> {
        Ok(match item {
            Ty::Int => Element::Int,
            Ty::Float => Element::Float,
            Ty::Bool => Element::Bool,
            Ty::Struct(id) if self.cases(id)?.is_some() => {
                let layout = self.layout(id)?;
                let mut cases = Vec::new();
                for case in self.cases(id)?.unwrap_or_default() {
                    let mut fields = Vec::with_capacity(case.fields.len());
                    for field in &case.fields {
                        fields.push((std::rc::Rc::from(field.name.as_str()), self.describe(field.ty)?));
                    }
                    cases.push((std::rc::Rc::from(case.name.as_str()), fields));
                }
                let words = usize::try_from(layout.size / WORD).unwrap_or(0);
                Element::Enum { name: std::rc::Rc::from(layout.name.as_str()), cases, words }
            }
            Ty::Struct(id) => {
                let layout = self.layout(id)?;
                let mut fields = Vec::with_capacity(layout.fields.len());
                for field in &layout.fields {
                    fields.push((std::rc::Rc::from(field.name.as_str()), self.describe(field.ty)?));
                }
                Element::Struct { name: std::rc::Rc::from(layout.name.as_str()), fields }
            }
            _ => return Err(internal("a list item that can't be packed", self.function.def.span)),
        })
    }

    /// A packed list name, and the kind of its items.
    pub(super) fn packed(&self, expr: &Expr) -> Option<(String, Ty)> {
        match &expr.kind {
            ExprKind::Name(name) => {
                self.local(name).and_then(|local| local.list).map(|item| (name.clone(), item))
            }
            _ => None,
        }
    }

    /// How many bytes one item takes.
    fn stride(&self, item: Ty) -> Result<i64, Diagnostic> {
        Ok(match item {
            Ty::Struct(id) => i64::from(self.layout(id)?.size),
            _ => i64::from(WORD),
        })
    }

    /// Where a packed list's items start, for reading or for changing them.
    pub(super) fn items_at(&mut self, name: &str, item: Ty, write: bool) -> Result<Value, Diagnostic> {
        if let Some(hoisted) = self.hoisted(name).filter(|hoisted| hoisted.writable || !write) {
            return Ok(hoisted.items);
        }
        let id = self.name_id(name);
        let element = self.element(item)?;
        let write = self.b.ins().iconst(types::I8, i64::from(write));
        self.help(Helper::Address, &[id, element, write])
    }

    pub(super) fn list_length(&mut self, name: &str, item: Ty) -> Result<Value, Diagnostic> {
        if let Some(hoisted) = self.hoisted(name) {
            return Ok(hoisted.length);
        }
        let id = self.name_id(name);
        let element = self.element(item)?;
        self.help(Helper::Length, &[id, element])
    }

    /// The address of `list[index]`, failing like the interpreter for an index past either
    /// end. Negative indexes count from the end.
    pub(super) fn item_address(
        &mut self,
        name: &str,
        item: Ty,
        index: &Expr,
        write: bool,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if self.hoisted(name).is_none() {
            self.note(
                span,
                format!("the runtime looks up the packed list `{name}` by name, twice, for each access"),
            );
        }
        let (at, at_ty) = self.expr(index)?;
        let at = self.convert(at, at_ty, Ty::Int, index.span)?;
        let len = self.list_length(name, item)?;
        let negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, at, 0);
        let from_end = self.b.ins().iadd(at, len);
        let resolved = self.b.ins().select(negative, from_end, at);
        // Unsigned, a negative position is huge, so one comparison checks both ends.
        let outside = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, resolved, len);
        let fail = self.b.create_block();
        let ok = self.b.create_block();
        self.b.set_cold_block(fail);
        self.b.ins().brif(outside, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        self.b.seal_block(fail);
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::OutOfRange, &[at, len, start, end])?;
        self.b.ins().jump(self.bail, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
        let items = self.items_at(name, item, write)?;
        let stride = self.stride(item)?;
        let offset = self.b.ins().imul_imm_s(resolved, stride);
        Ok(self.b.ins().iadd(items, offset))
    }

    /// The item at an address, as a value of its own: a later change to the list doesn't
    /// change it.
    pub(super) fn load_item(&mut self, address: Value, item: Ty) -> Result<Lowered, Diagnostic> {
        if let Ty::Struct(id) = item {
            return Ok((Some(self.clone_struct(address, id)?), item));
        }
        let abi_ty = abi(item)
            .ok_or_else(|| internal("a list item without a machine type", self.function.def.span))?;
        Ok((Some(self.b.ins().load(abi_ty, MemFlagsData::trusted(), address, 0)), item))
    }

    /// Writes a value into an item's memory.
    pub(super) fn store_item(
        &mut self,
        address: Value,
        item: Ty,
        (value, ty): Lowered,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let value = self.convert(value, ty, item, span)?;
        if let Ty::Struct(id) = item {
            return self.write_place(&Place::Memory(address), value, id);
        }
        self.b.ins().store(MemFlagsData::trusted(), value, address, 0);
        Ok(())
    }

    /// `list[index]` on a packed list.
    pub(super) fn read_packed(
        &mut self,
        name: &str,
        item: Ty,
        index: &Expr,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let address = self.item_address(name, item, index, false, span)?;
        self.load_item(address, item)
    }

    /// `list.append(value)` on a packed list.
    pub(super) fn append_packed(
        &mut self,
        name: &str,
        item: Ty,
        value: &Expr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        self.note(span, format!("the runtime looks up the packed list `{name}` by name to append"));
        let value = self.expr(value)?;
        let converted = self.convert(value.0, value.1, item, span)?;
        let id = self.name_id(name);
        let element = self.element(item)?;
        let address = self.help(Helper::Push, &[id, element])?;
        self.store_item(address, item, (Some(converted), item), span)
    }

    /// Whether a loop body names the list it loops over: then something in it might change
    /// or share the list, and the loop goes through the runtime.
    pub(super) fn body_names(body: &[Stmt], name: &str) -> bool {
        let mut names = Vec::new();
        for stmt in body {
            super::fallback::stmt_names(stmt, &mut names);
        }
        names.iter().any(|named| named == name)
    }
}

impl Lowerer<'_, '_> {
    /// `for name in list` or `for mut name in list` over a packed list whose items the loop
    /// reads or changes in place. `false` when this loop needs the runtime's way instead.
    pub(super) fn for_packed(
        &mut self,
        names: &[String],
        list: &Expr,
        body: &[Stmt],
        is_mut: bool,
    ) -> Result<bool, Diagnostic> {
        let (Some((list_name, item)), [name]) = (self.packed(list), names) else { return Ok(false) };
        if Self::body_names(body, &list_name) || (is_mut && !matches!(item, Ty::Struct(_))) {
            return Ok(false);
        }
        // Nothing in the body can change or share the list, so its items stay put.
        let len = self.list_length(&list_name, item)?;
        let items = self.items_at(&list_name, item, is_mut)?;
        let stride = self.stride(item)?;
        let counter = self.b.declare_var(types::I64);
        let zero = self.int(0);
        self.b.def_var(counter, zero);
        let [header, inside, advance, exit] = [(); 4].map(|()| self.b.create_block());
        let hoisted = self.hoist_lists(&[name.clone(), list_name.clone()], None, body)?;
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let at = self.b.use_var(counter);
        let more = self.b.ins().icmp(IntCC::SignedLessThan, at, len);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        let depth = self.scopes.len();
        self.open_scope();
        let offset = self.b.ins().imul_imm_s(at, stride);
        let address = self.b.ins().iadd(items, offset);
        match (item, is_mut) {
            // The name stands for the item itself, so changing it changes the list.
            (Ty::Struct(id), true) => self.bind_place(name, Place::Memory(address), id, true),
            (Ty::Struct(id), false) => {
                let value = self.clone_struct(address, id)?;
                self.declare_struct(name, value, id, false)?;
            }
            _ => {
                let (value, ty) = self.load_item(address, item)?;
                self.bind_native(name, ty, value, false);
            }
        }
        self.loops.push(super::Loop { next: advance, done: exit, depth, put_back: None });
        let lowered = self.tail(body);
        self.loops.pop();
        self.unhoist(hoisted);
        lowered?;
        self.close_scope()?;
        if !self.is_dead() {
            self.b.ins().jump(advance, &[]);
        }
        self.b.switch_to_block(advance);
        let at = self.b.use_var(counter);
        let after = self.b.ins().iadd_imm_s(at, 1);
        self.b.def_var(counter, after);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        Ok(true)
    }
}
