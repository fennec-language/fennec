//! Lists made natively for a name that holds a native list: `[a, b, c]`, and comprehensions
//! `[item for x in range(...)]`. The runtime makes the list with every item zero, and
//! native code fills it.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, types};
use fennec_syntax::{CompKind, Diagnostic, Expr, ExprKind};

use super::Lowerer;
use super::expr::Lowered;
use super::fallback::expr_names;
use super::lists::LIST_ITEMS;
use crate::helpers::Helper;
use crate::types::{ListItem, Ty};

impl Lowerer<'_, '_> {
    /// `[a, b, c]` made natively, when the name it's for holds a native list, or `None`.
    /// Items are worked out in order, as the interpreter does.
    pub(super) fn list_literal_for(
        &mut self,
        name: &str,
        value: &Expr,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let (ExprKind::List(items), Some(ty @ Ty::List(item))) = (&value.kind, self.checked_ty(name)) else {
            return Ok(None);
        };
        let mut lowered = Vec::with_capacity(items.len());
        for each in items {
            let (value, from) = self.expr(each)?;
            lowered.push(self.convert(value, from, item.ty(), each.span)?);
        }
        let element = self.element(item.ty())?;
        let count = self.int(i64::try_from(items.len()).unwrap_or(0));
        let list = self.help(Helper::ListNew, &[element, count])?;
        let stride = self.stride(item.ty())?;
        for (at, value) in lowered.into_iter().enumerate() {
            let offset = i64::try_from(at).unwrap_or(0) * stride + LIST_ITEMS;
            let address = self.b.ins().iadd_imm_s(list, offset);
            self.store_item(address, item.ty(), (Some(value), item.ty()), value_span(items, at))?;
        }
        Ok(Some((Some(list), ty)))
    }

    /// Declares `name` as `[item for x in range(...)]` natively, when it holds a packed list
    /// of ints, floats or bools. Gives back `false`, having done nothing, when it can't.
    pub(super) fn packed_comprehension(
        &mut self,
        name: &str,
        value: &Expr,
        is_mut: bool,
    ) -> Result<bool, Diagnostic> {
        let ExprKind::Comprehension { kind: CompKind::List, elem, value: None, names, iter, cond: None } =
            &value.kind
        else {
            return Ok(false);
        };
        let ([var], ExprKind::Call { callee, args }) = (names.as_slice(), &iter.kind) else {
            return Ok(false);
        };
        let is_range = matches!(&callee.kind, ExprKind::Name(callee) if callee == "range" && self.local(callee).is_none());
        if !is_range
            || !(1..=2).contains(&args.len())
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
        {
            return Ok(false);
        }
        let Some(Ty::List(list_item @ (ListItem::Int | ListItem::Float | ListItem::Bool))) =
            self.checked_ty(name)
        else {
            return Ok(false);
        };
        let item = list_item.ty();
        // An item that refers to the name sees what it held before, so leave that to the
        // interpreter.
        let mut mentioned = Vec::new();
        expr_names(elem, &mut mentioned);
        if mentioned.iter().any(|mentioned| mentioned == name) {
            return Ok(false);
        }
        let mut bounds = Vec::with_capacity(args.len());
        for arg in args {
            let (bound, ty) = self.expr(&arg.value)?;
            bounds.push(self.convert(bound, ty, Ty::Int, arg.value.span)?);
        }
        let zero = self.int(0);
        let (start, end) = match bounds.as_slice() {
            [end] => (zero, *end),
            [start, end] => (*start, *end),
            _ => return Ok(false),
        };
        let difference = self.b.ins().isub(end, start);
        let count = self.b.ins().smax(difference, zero);
        let element = self.element(item)?;
        let made = self.help(Helper::ListNew, &[element, count])?;
        self.declare(name, Some(made), Ty::List(list_item), is_mut, value)?;
        if !is_zero(elem) {
            self.fill(name, item, (var, start, count), elem)?;
        }
        Ok(true)
    }

    /// Works `elem` out for each `var` from `start` on, storing it into each of the `count`
    /// items of a packed list nothing else refers to yet.
    fn fill(
        &mut self,
        name: &str,
        item: Ty,
        (var, start, count): (&str, cranelift_codegen::ir::Value, cranelift_codegen::ir::Value),
        elem: &Expr,
    ) -> Result<(), Diagnostic> {
        let items = self.items_at(name, item, true)?;
        self.drop_temps()?;
        let counter = self.b.declare_var(types::I64);
        let zero = self.int(0);
        self.b.def_var(counter, zero);
        let [header, inside, exit] = [(); 3].map(|()| self.b.create_block());
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let at = self.b.use_var(counter);
        let more = self.b.ins().icmp(IntCC::SignedLessThan, at, count);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        self.open_scope();
        let x = self.b.ins().iadd(start, at);
        self.bind_native(var, Ty::Int, Some(x), false);
        let lowered = self.expr(elem)?;
        let offset = self.b.ins().ishl_imm_s(at, 3);
        let address = self.b.ins().iadd(items, offset);
        self.store_item(address, item, lowered, elem.span)?;
        self.drop_temps()?;
        self.close_scope()?;
        let next = self.b.ins().iadd_imm_s(at, 1);
        self.b.def_var(counter, next);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        Ok(())
    }
}

fn value_span(items: &[Expr], at: usize) -> fennec_syntax::Span {
    items.get(at).map(|item| item.span).unwrap_or_default()
}

/// Whether an item is a zero the runtime's new list already holds.
fn is_zero(elem: &Expr) -> bool {
    match &elem.kind {
        ExprKind::Int(n) => *n == 0,
        ExprKind::Float(x) => x.to_bits() == 0,
        ExprKind::Bool(b) => !b,
        _ => false,
    }
}
