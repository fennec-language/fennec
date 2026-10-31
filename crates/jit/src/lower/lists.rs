//! Packed lists natively. A list of numbers, bools or plain structs is the address of a
//! block the runtime owns (see the runtime's `lists`): a count of items, the room it has,
//! then the items one after another. Native code reads, changes and appends items itself,
//! checking every index. A change to a list something else shares copies it first, and an
//! append to a full one grows it, both in the runtime. Anything else done with the list
//! goes through the interpreter, which sees an ordinary list.
//!
//! A loop whose body only indexes a list works out where its items are once, before the
//! loop, as nothing in the body can copy or grow it.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, InstBuilder, MemFlagsData, Value, types};
use cranelift_frontend::Variable;
use fennec_interp::embed::layout::{LIST_LENGTH, LIST_ROOM};
use fennec_interp::packed::Element;
use fennec_syntax::{Diagnostic, Expr, ExprKind, Span, Stmt};

use super::expr::Lowered;
use super::structs::Place;
use super::{Lowerer, abi, internal};
use crate::helpers::Helper;
use crate::layout::WORD;
use crate::types::{ListItem, Ty};

pub(super) const LIST_ITEMS: i64 = fennec_interp::embed::layout::LIST_ITEMS as i64;

/// An index already checked against a list's length, which straight-line code after it
/// needn't check again: in `block`, `at` resolved to `resolved` for a list `length` long.
pub(super) struct Checked {
    block: Block,
    length: Value,
    at: Value,
    resolved: Value,
}

/// The most facts about items kept at once. Lookups go through them all.
const MOST_KEPT: usize = 32;

fn trim<T>(facts: &mut Vec<T>) {
    if facts.len() > MOST_KEPT {
        facts.drain(..facts.len() - MOST_KEPT);
    }
}

/// Where an item is, which straight-line code after it can use again: `at` items into the
/// list whose items start at `items`.
pub(super) struct KnownItem {
    block: Block,
    items: Value,
    at: Value,
    address: Value,
}

impl Lowerer<'_, '_> {
    /// A runtime object as an owned native list, failing like the interpreter when it isn't
    /// a list of the kind of item expected.
    pub(super) fn handle_to_list(
        &mut self,
        handle: Value,
        item: ListItem,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let element = self.element(item.ty())?;
        let [start, end] = self.span_args(span);
        self.help(Helper::ListFromValue, &[handle, element, start, end])
    }

    /// The runtime's number for a kind of item, registering it the first time.
    pub(super) fn element(&mut self, item: Ty) -> Result<Value, Diagnostic> {
        if let Some(&id) = self.elements.get(&item) {
            return Ok(self.handed_number("element", id));
        }
        let described = std::rc::Rc::new(self.describe(item)?);
        let id = i64::try_from(self.shared.tables.elements.len()).unwrap_or(0);
        self.shared.tables.elements.push(described);
        self.elements.insert(item, id);
        Ok(self.handed_number("element", id))
    }

    fn describe(&self, item: Ty) -> Result<Element, Diagnostic> {
        Ok(match item {
            Ty::Int => Element::Int,
            Ty::Float => Element::Float,
            Ty::Bool => Element::Bool,
            Ty::Str => Element::Str,
            Ty::Boxed(id) => Element::Boxed(usize::try_from(id).unwrap_or(0)),
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
            ExprKind::Name(name) => match self.local(name)?.ty {
                Ty::List(item) => Some((name.clone(), item.ty())),
                _ => None,
            },
            _ => None,
        }
    }

    /// How many bytes one item takes.
    pub(super) fn stride(&self, item: Ty) -> Result<i64, Diagnostic> {
        Ok(match item {
            Ty::Struct(id) => i64::from(self.layout(id)?.size),
            _ => i64::from(WORD),
        })
    }

    /// The variable holding the list a name holds.
    fn list_var(&self, name: &str) -> Result<Variable, Diagnostic> {
        self.local(name)
            .and_then(|local| local.var)
            .ok_or_else(|| internal("a list without a variable", Span::default()))
    }

    /// Where a packed list's items start, for reading or for changing them. Changing them
    /// first gives the name a list of its own, if it shares one.
    pub(super) fn items_at(&mut self, name: &str, _item: Ty, write: bool) -> Result<Value, Diagnostic> {
        if let Some(hoisted) = self.hoisted(name).filter(|hoisted| hoisted.writable || !write) {
            return Ok(hoisted.items);
        }
        let var = self.list_var(name)?;
        let list = self.b.use_var(var);
        let list = if write { self.own_list(var, list)? } else { list };
        Ok(self.b.ins().iadd_imm_s(list, LIST_ITEMS))
    }

    /// A list only `var` holds: its own, or a copy the runtime makes when it's shared.
    fn own_list(&mut self, var: Variable, list: Value) -> Result<Value, Diagnostic> {
        let count = self.b.ins().load(types::I64, MemFlagsData::trusted(), list, 0);
        let alone = self.b.ins().icmp_imm_s(IntCC::Equal, count, 1);
        let [copy, done] = [(); 2].map(|()| self.b.create_block());
        self.b.set_cold_block(copy);
        self.b.append_block_param(done, types::I64);
        self.b.ins().brif(alone, done, &[list.into()], copy, &[]);
        self.b.switch_to_block(copy);
        let copied = self.help(Helper::ListUnshare, &[list])?;
        self.b.ins().jump(done, &[copied.into()]);
        self.b.switch_to_block(done);
        let list = self
            .b
            .block_params(done)
            .first()
            .copied()
            .ok_or_else(|| internal("lost a list", Span::default()))?;
        self.b.def_var(var, list);
        Ok(list)
    }

    pub(super) fn list_length(&mut self, name: &str, _item: Ty) -> Result<Value, Diagnostic> {
        if let Some(hoisted) = self.hoisted(name) {
            return Ok(hoisted.length);
        }
        let var = self.list_var(name)?;
        if let Some(length) = self.fixed_length(var) {
            return Ok(length);
        }
        let list = self.b.use_var(var);
        Ok(self.length_of_list(list))
    }

    /// How many items a list has.
    pub(super) fn length_of_list(&mut self, list: Value) -> Value {
        self.load_length(list, LIST_LENGTH)
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
        let (at, at_ty) = self.expr(index)?;
        let at = self.convert(at, at_ty, Ty::Int, index.span)?;
        let len = self.list_length(name, item)?;
        if self.known_inside(at, len) {
            return self.item_at(name, item, write, at);
        }
        let here = self.b.current_block();
        // The SSA builder can give a variable's value back under another name.
        let (len, at) = (self.b.func.dfg.resolve_aliases(len), self.b.func.dfg.resolve_aliases(at));
        let checked =
            self.checked.iter().find(|done| Some(done.block) == here && done.length == len && done.at == at);
        if let Some(resolved) = checked.map(|done| done.resolved) {
            return self.item_at(name, item, write, resolved);
        }
        // Unsigned, a negative position is huge, so one comparison checks a position from
        // the start. One from the end is the rare case, checked apart.
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, at, len);
        let [from_end, fail, ok] = [(); 3].map(|()| self.b.create_block());
        self.b.set_cold_block(from_end);
        self.b.set_cold_block(fail);
        self.b.append_block_param(ok, types::I64);
        self.b.ins().brif(inside, ok, &[at.into()], from_end, &[]);
        self.b.switch_to_block(from_end);
        self.b.seal_block(from_end);
        let back = self.b.ins().iadd(at, len);
        let inside = self.b.ins().icmp(IntCC::UnsignedLessThan, back, len);
        self.b.ins().brif(inside, ok, &[back.into()], fail, &[]);
        self.b.switch_to_block(fail);
        self.b.seal_block(fail);
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::OutOfRange, &[at, len, start, end])?;
        self.b.ins().jump(self.bail, &[]);
        self.carry_on(here, ok);
        self.b.seal_block(ok);
        let resolved =
            self.b.block_params(ok).first().copied().ok_or_else(|| internal("lost a position", span))?;
        self.checked.push(Checked { block: ok, length: len, at, resolved });
        self.item_at(name, item, write, resolved)
    }

    fn item_at(&mut self, name: &str, item: Ty, write: bool, resolved: Value) -> Result<Value, Diagnostic> {
        let items = self.items_at(name, item, write)?;
        let (here, items, at) = (
            self.b.current_block(),
            self.b.func.dfg.resolve_aliases(items),
            self.b.func.dfg.resolve_aliases(resolved),
        );
        let known = self
            .addresses
            .iter()
            .find(|done| Some(done.block) == here && done.items == items && done.at == at);
        if let Some(address) = known.map(|done| done.address) {
            return Ok(address);
        }
        let stride = self.stride(item)?;
        let offset = self.b.ins().imul_imm_s(resolved, stride);
        let address = self.b.ins().iadd(items, offset);
        if let Some(block) = here {
            self.addresses.push(KnownItem { block, items, at, address });
        }
        Ok(address)
    }

    /// Goes on in `next`, a block only `from` leads to other than to a failure, so every
    /// index checked in `from` stays checked there. What's known in other blocks stays with
    /// them: a failure's cold path goes through here too, and comes back to none of it.
    pub(super) fn carry_on(&mut self, from: Option<Block>, next: Block) {
        self.carry_stack(from, next);
        // Few lists are indexed at once, so an old entry is a lost chance at most.
        trim(&mut self.addresses);
        trim(&mut self.checked);
        for done in &mut self.addresses {
            if Some(done.block) == from {
                done.block = next;
            }
        }
        for done in &mut self.checked {
            if Some(done.block) == from {
                done.block = next;
            }
        }
        self.b.switch_to_block(next);
    }

    /// The item at an address, as a value of its own: a later change to the list doesn't
    /// change it.
    pub(super) fn load_item(&mut self, address: Value, item: Ty) -> Result<Lowered, Diagnostic> {
        if let Ty::Struct(id) = item {
            return Ok((Some(self.clone_struct(address, id)?), item));
        }
        let abi_ty = abi(item)
            .ok_or_else(|| internal("a list item without a machine type", self.function.def.span))?;
        let value = self.b.ins().load(abi_ty, MemFlagsData::trusted(), address, 0);
        // The list could let go of a counted item while this is still using it.
        if item.counted() {
            if self.now.lending {
                self.borrow(value);
            } else {
                self.retain(value);
            }
        }
        Ok((Some(value), item))
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
        if item.counted() {
            let value = self.own(value, item);
            let old = self.b.ins().load(types::I64, MemFlagsData::trusted(), address, 0);
            self.b.ins().store(MemFlagsData::trusted(), value, address, 0);
            return self.release(old);
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
        let value = self.expr(value)?;
        let converted = self.convert(value.0, value.1, item, span)?;
        let flags = MemFlagsData::trusted();
        let var = self.list_var(name)?;
        let list = self.b.use_var(var);
        // Room on the end of a list nothing else holds takes one more item in place.
        let count = self.b.ins().load(types::I64, flags, list, 0);
        let length = self.b.ins().load(types::I64, flags, list, LIST_LENGTH);
        let room = self.b.ins().load(types::I64, flags, list, LIST_ROOM);
        let alone = self.b.ins().icmp_imm_s(IntCC::Equal, count, 1);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThan, length, room);
        let ready = self.b.ins().band(alone, fits);
        let [quick, grow, done] = [(); 3].map(|()| self.b.create_block());
        self.b.set_cold_block(grow);
        self.b.append_block_param(done, types::I64);
        self.b.ins().brif(ready, quick, &[], grow, &[]);
        self.b.switch_to_block(quick);
        let longer = self.b.ins().iadd_imm_s(length, 1);
        self.b.ins().store(flags, longer, list, LIST_LENGTH);
        self.b.ins().jump(done, &[list.into()]);
        self.b.switch_to_block(grow);
        let grown = self.help(Helper::ListGrow, &[list])?;
        self.b.ins().jump(done, &[grown.into()]);
        self.b.switch_to_block(done);
        let list = self.b.block_params(done).first().copied().ok_or_else(|| internal("lost a list", span))?;
        self.b.def_var(var, list);
        let stride = self.stride(item)?;
        let offset = self.b.ins().imul_imm_s(length, stride);
        let address = self.b.ins().iadd(list, offset);
        let address = self.b.ins().iadd_imm_s(address, LIST_ITEMS);
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
        // A list a call gives is the loop's own, and one in a field needn't be copied. One held
        // as an object is left for the runtime, which needn't copy it, and `range` is counted
        // without making a list.
        let held = match &list.kind {
            ExprKind::Name(_) => false,
            ExprKind::Attr { .. } | ExprKind::Index { .. } => {
                matches!(self.static_ty(list), Some(Ty::List(_)))
            }
            ExprKind::Call { callee, .. } => !matches!(&callee.kind, ExprKind::Name(name) if name == "range"),
            _ => true,
        };
        if held
            && !is_mut
            && let [_] = names
            && let Some(ty @ Ty::List(_)) = self.checked_expr(list)
        {
            let (value, from) = self.expr(list)?;
            let value = self.convert(value, from, ty, list.span)?;
            return self.holding_list((value, ty), false, list.span, |lowerer, held| {
                lowerer.for_packed(names, held, body, false)
            });
        }
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
        let [inside, advance, exit] = [(); 3].map(|()| self.b.create_block());
        let hoisted = self.hoist_lists(&[name.clone(), list_name.clone()], None, body)?;
        // The test comes first, then again at the bottom, so going round takes one jump.
        let more = self.b.ins().icmp(IntCC::SignedLessThan, zero, len);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        let at = self.b.use_var(counter);
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
        let lowered = self.statements(body);
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
        let more = self.b.ins().icmp(IntCC::SignedLessThan, after, len);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(exit);
        Ok(true)
    }
}
