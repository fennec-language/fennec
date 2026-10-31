//! The methods that change a native list, and `in`. Native code takes the last item off
//! itself; the runtime moves items around for the rest.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Span, UnaryOp};

use super::expr::Lowered;
use super::lists::LIST_ITEMS;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::types::{ListItem, Ty};

impl Lowerer<'_, '_> {
    /// A method that changes a list a name holds, natively, or `None` for anything else.
    pub(super) fn list_method(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return Ok(None) };
        let Some((list, item)) = self.packed(object) else { return Ok(None) };
        if args.iter().any(|arg| arg.is_mut) {
            return Ok(None);
        }
        let plain = args.iter().all(|arg| arg.name.is_none());
        match (name.as_str(), args) {
            ("pop", []) => self.pop_last(&list, item, span).map(Some),
            ("pop", [at]) if plain => self.pop_at(&list, item, &at.value, span).map(Some),
            ("clear", []) => {
                let held = self.own_list_named(&list)?;
                self.call_helper(Helper::ListClear, &[held])?;
                Ok(Some((None, Ty::Unit)))
            }
            ("insert", [at, value]) if plain => {
                self.insert(&list, item, (&at.value, &value.value), span).map(Some)
            }
            ("remove", [value]) if plain && !matches!(item, Ty::Struct(_)) => {
                self.remove(&list, item, &value.value, span).map(Some)
            }
            ("sort", _) => self.sort_list(object, &list, args, span),
            ("remove_where", [test]) if plain => self.remove_where(&list, item, &test.value, span),
            _ => Ok(None),
        }
    }

    /// The list a name holds, with that name as its only owner.
    fn own_list_named(&mut self, name: &str) -> Result<Value, Diagnostic> {
        let items = self.items_at(name, Ty::Int, true)?;
        Ok(self.b.ins().iadd_imm_s(items, -LIST_ITEMS))
    }

    /// `list.pop()`: the last item, which moves out of the list.
    fn pop_last(&mut self, name: &str, item: Ty, span: Span) -> Result<Lowered, Diagnostic> {
        let flags = MemFlagsData::trusted();
        let list = self.own_list_named(name)?;
        let length = self.length_of_list(list);
        let empty = self.b.ins().icmp_imm_s(IntCC::Equal, length, 0);
        let [fail, ok] = [(); 2].map(|()| self.b.create_block());
        self.b.set_cold_block(fail);
        let here = self.b.current_block();
        self.b.ins().brif(empty, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        self.b.seal_block(fail);
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::PopEmpty, &[start, end])?;
        self.b.ins().jump(self.bail, &[]);
        self.carry_on(here, ok);
        self.b.seal_block(ok);
        let shorter = self.b.ins().iadd_imm_s(length, -1);
        let stride = self.stride(item)?;
        let offset = self.b.ins().imul_imm_s(shorter, stride);
        let address = self.b.ins().iadd(list, offset);
        let address = self.b.ins().iadd_imm_s(address, LIST_ITEMS);
        let popped = self.load_item(address, item)?;
        if item.counted() {
            // Whatever comes next finds the room empty, and the item has one owner fewer.
            let value = popped.0.ok_or_else(|| internal("a lost item", span))?;
            let count = self.b.ins().load(types::I64, flags, value, 0);
            let fewer = self.b.ins().iadd_imm_s(count, -1);
            self.b.ins().store(flags, fewer, value, 0);
            let none = self.int(0);
            self.b.ins().store(flags, none, address, 0);
        }
        self.b.ins().store(flags, shorter, list, 16);
        Ok(popped)
    }

    /// `list.pop(at)`.
    fn pop_at(&mut self, name: &str, item: Ty, at: &Expr, span: Span) -> Result<Lowered, Diagnostic> {
        let address = self.item_address(name, item, at, true, span)?;
        let popped = self.load_item(address, item)?;
        let list = self.own_list_named(name)?;
        self.call_helper(Helper::ListRemoveAt, &[list, address])?;
        Ok(popped)
    }

    /// `list.insert(at, value)`, worked out in that order.
    fn insert(
        &mut self,
        name: &str,
        item: Ty,
        (at, value): (&Expr, &Expr),
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let (place, from) = self.expr(at)?;
        let place = self.convert(place, from, Ty::Int, at.span)?;
        let (new, from) = self.expr(value)?;
        let new = self.convert(new, from, item, value.span)?;
        let var = self.local(name).and_then(|local| local.var);
        let var = var.ok_or_else(|| internal("a lost list", span))?;
        let list = self.b.use_var(var);
        let [start, end] = self.span_args(span);
        let list = self.help(Helper::ListInsert, &[list, place, start, end])?;
        self.b.def_var(var, list);
        let stride = self.stride(item)?;
        // The runtime has checked the place, and a place from the end isn't allowed.
        let offset = self.b.ins().imul_imm_s(place, stride);
        let address = self.b.ins().iadd(list, offset);
        let address = self.b.ins().iadd_imm_s(address, LIST_ITEMS);
        self.store_item(address, item, (Some(new), item), span)?;
        Ok((None, Ty::Unit))
    }

    /// `list.remove(value)`, for items of a word each.
    fn remove(&mut self, name: &str, item: Ty, value: &Expr, span: Span) -> Result<Lowered, Diagnostic> {
        let (found, from) = self.expr(value)?;
        let found = self.convert(found, from, item, value.span)?;
        let list = self.own_list_named(name)?;
        let mut words = Vec::new();
        self.push_words(found, item, &mut words)?;
        let word = words.first().copied().ok_or_else(|| internal("a lost value", span))?;
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::ListRemove, &[list, word, start, end])?;
        self.consume(Some(found), item)?;
        Ok((None, Ty::Unit))
    }

    /// `list.sort()` or `list.sort(key=...)`: the list a name holds, now in order.
    fn sort_list(
        &mut self,
        object: &Expr,
        name: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let Some(local) = self.local(name) else { return Ok(None) };
        let Some(made) = self.sorted_as(object, args, local.ty, span)? else { return Ok(None) };
        self.set_local(name, local, made.0, made.1, span)?;
        Ok(Some((None, Ty::Unit)))
    }

    /// `list.remove_where((item) => test)`: the list a name holds, with only the items the
    /// test turns down.
    fn remove_where(
        &mut self,
        name: &str,
        item: Ty,
        test: &Expr,
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ExprKind::Lambda { params, body } = &test.kind else { return Ok(None) };
        let ([param], Some(local)) = (params.as_slice(), self.local(name)) else { return Ok(None) };
        let Some(kept_item) = ListItem::of(item) else { return Ok(None) };
        let keep = Expr { kind: ExprKind::Unary { op: UnaryOp::Not, expr: body.clone() }, span: body.span };
        let kept = self.filtered(name, param, &keep, kept_item, span)?;
        self.set_local(name, local, Some(kept), Ty::List(kept_item), span)?;
        Ok(Some((None, Ty::Unit)))
    }

    /// `item in list` or `item not in list` on a list of items of a word each, natively, or
    /// `None` for anything else.
    pub(super) fn list_binary(
        &mut self,
        op: BinOp,
        (found, found_ty): Lowered,
        (list, list_ty): Lowered,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let (BinOp::In | BinOp::NotIn, Some(found), Some(list), Ty::List(item)) = (op, found, list, list_ty)
        else {
            return Ok(None);
        };
        if matches!(item, ListItem::Struct(_)) || item.ty() != found_ty {
            return Ok(None);
        }
        let mut words = Vec::new();
        self.push_words(found, found_ty, &mut words)?;
        let word = words.first().copied().ok_or_else(|| internal("a lost value", Span::default()))?;
        let there = self.help(Helper::ListContains, &[list, word])?;
        self.consume(Some(found), found_ty)?;
        self.consume(Some(list), list_ty)?;
        let there = if op == BinOp::NotIn { self.b.ins().bxor_imm_u(there, 1) } else { there };
        Ok(Some((Some(there), Ty::Bool)))
    }
}
