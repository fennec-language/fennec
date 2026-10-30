//! Objects natively: reading fields and items, operators, calling compiled methods on the
//! receiver's type, assigning into fields, and `for mut` loops. Each operation is one call
//! into the runtime, or a native call to a compiled method.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value, types};
use fennec_interp::embed::objects::binary_code;
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Span, Stmt, UnaryOp};

use super::expr::Lowered;
use super::{Loop, Lowerer, internal};
use crate::helpers::Helper;
use crate::types::Ty;

/// What a `for mut` loop puts back after each turn: the item's name, at its key, along the
/// path it loops over.
#[derive(Clone)]
pub(super) struct PutBack {
    root: String,
    fields: Vec<String>,
    key: String,
    item: String,
    span: Span,
}

impl Lowerer<'_, '_> {
    /// A name holding an object and the fields after it, like `ash.position`, when the
    /// expression is only that.
    pub(super) fn field_path(&self, expr: &Expr) -> Option<(String, Vec<String>)> {
        match &expr.kind {
            ExprKind::Name(name) if self.local(name).is_some_and(|local| local.ty == Ty::Obj) => {
                Some((name.clone(), Vec::new()))
            }
            ExprKind::Attr { object, name, optional: false } => {
                let (root, mut fields) = self.field_path(object)?;
                fields.push(name.clone());
                Some((root, fields))
            }
            _ => None,
        }
    }

    fn start_path(&mut self, root: &str, fields: &[String]) -> Result<(), Diagnostic> {
        let root = self.name_id(root);
        self.call_helper(Helper::Path, &[root])?;
        for field in fields {
            let field = self.name_id(field);
            self.call_helper(Helper::PathField, &[field])?;
        }
        Ok(())
    }

    /// `object.name`. A type's name before the dot, as in `Vec3.zero`, goes to the
    /// interpreter.
    pub(super) fn read_field(
        &mut self,
        expr: &Expr,
        object: &Expr,
        name: &str,
        optional: bool,
    ) -> Result<Lowered, Diagnostic> {
        if let ExprKind::Name(object) = &object.kind
            && self.local(object).is_none()
        {
            return self.fallback(expr);
        }
        if !optional && let Some((root, fields)) = self.field_path(expr) {
            self.start_path(&root, &fields)?;
            let [start, end] = self.span_args(expr.span);
            return Ok((Some(self.help(Helper::PathGet, &[start, end])?), Ty::Obj));
        }
        let (value, ty) = self.expr(object)?;
        if let (Ty::Struct(id), Some(address)) = (ty, value)
            && let Some(field) = self.struct_field(id, name)?
        {
            return self.load_field(address, id, field);
        }
        let handle = self.handle_of(value, ty, object.span)?;
        let name = self.name_id(name);
        let optional = self.b.ins().iconst(types::I8, i64::from(optional));
        let [start, end] = self.span_args(expr.span);
        Ok((Some(self.help(Helper::Field, &[handle, name, optional, start, end])?), Ty::Obj))
    }

    /// `object[index]`.
    pub(super) fn read_index_or_packed(
        &mut self,
        expr: &Expr,
        object: &Expr,
        index: &Expr,
    ) -> Result<Lowered, Diagnostic> {
        if let Some((name, item)) = self.packed(object) {
            return self.read_packed(&name, item, index, expr.span);
        }
        self.read_index(expr, object, index)
    }

    pub(super) fn read_index(
        &mut self,
        expr: &Expr,
        object: &Expr,
        index: &Expr,
    ) -> Result<Lowered, Diagnostic> {
        let (value, ty) = self.expr(object)?;
        let object = self.handle_of(value, ty, expr.span)?;
        let (value, ty) = self.expr(index)?;
        let index = self.handle_of(value, ty, expr.span)?;
        let [start, end] = self.span_args(expr.span);
        Ok((Some(self.help(Helper::Index, &[object, index, start, end])?), Ty::Obj))
    }

    pub(super) fn object_unary(
        &mut self,
        op: UnaryOp,
        (value, ty): Lowered,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let handle = self.handle_of(value, ty, span)?;
        let negate = self.int(i64::from(op == UnaryOp::Neg));
        let [start, end] = self.span_args(span);
        Ok((Some(self.help(Helper::Unary, &[negate, handle, start, end])?), Ty::Obj))
    }

    /// An operator on objects: a compiled `def +` (say) of the left side's type, natively,
    /// or else the runtime's.
    pub(super) fn object_binary(
        &mut self,
        op: BinOp,
        left: Lowered,
        right: Lowered,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let code = binary_code(op).ok_or_else(|| internal("no operator code", span))?;
        if let (Ty::Struct(id), Some(method)) = (left.1, op.method_name()) {
            let key = format!("{}.{method}", self.layout(id)?.name);
            let other = self
                .shared
                .compiled
                .get(&key)
                .filter(|compiled| compiled.takes_self)
                .and_then(|compiled| compiled.params.get(1))
                .map(|param| param.1);
            if let (Some(address), Some(other)) = (left.0, other) {
                let right = self.convert(right.0, right.1, other, span)?;
                return self.call_key(&key, vec![address, right], span);
            }
        }
        let a = self.handle_of(left.0, left.1, span)?;
        let b = self.handle_of(right.0, right.1, span)?;
        let candidates = op.method_name().map(|method| self.methods_named(method, 1)).unwrap_or_default();
        let type_id = if candidates.is_empty() { None } else { Some(self.help(Helper::TypeOf, &[a])?) };
        let mut arms = Vec::new();
        if let Some(type_id) = type_id {
            for (owner, key) in candidates {
                self.when_type(type_id, &owner);
                let other = self
                    .shared
                    .compiled
                    .get(&key)
                    .and_then(|compiled| compiled.params.get(1))
                    .map(|param| param.1);
                let b_param = self.convert(Some(b), Ty::Obj, other.unwrap_or(Ty::Obj), span)?;
                let self_ty = self
                    .shared
                    .compiled
                    .get(&key)
                    .and_then(|compiled| compiled.params.first())
                    .map_or(Ty::Obj, |param| param.1);
                let a_param = self.convert(Some(a), Ty::Obj, self_ty, span)?;
                let (value, ty) = self.call_key(&key, vec![a_param, b_param], span)?;
                arms.extend(self.arm_end(value, ty));
                self.otherwise_type();
            }
        }
        let code = self.int(code);
        let [start, end] = self.span_args(span);
        let value = self.help(Helper::Binary, &[code, a, b, start, end])?;
        arms.extend(self.arm_end(Some(value), Ty::Obj));
        self.join(arms, span)
    }

    /// The compiled methods with this name that take `self` and `count` other arguments,
    /// with the type each belongs to.
    fn methods_named(&self, method: &str, count: usize) -> Vec<(String, String)> {
        let candidates = self.shared.methods.get(method).map_or(&[][..], Vec::as_slice);
        let mut found: Vec<(String, String)> = candidates
            .iter()
            .filter(|(_, key)| {
                self.shared.compiled.get(key).is_some_and(|compiled| {
                    compiled.params.len() == count + 1
                        && compiled.params.iter().skip(1).all(|(_, _, is_mut)| !is_mut)
                })
            })
            .cloned()
            .collect();
        found.sort();
        found
    }

    /// Branches to a new block when `type_id` names `owner`, leaving the other way pending
    /// for `otherwise_type`.
    fn when_type(&mut self, type_id: Value, owner: &str) {
        let owner = self.name_id(owner);
        let same = self.b.ins().icmp(IntCC::Equal, type_id, owner);
        let then = self.b.create_block();
        let next = self.b.create_block();
        self.b.ins().brif(same, then, &[], next, &[]);
        self.pending.push(next);
        self.b.switch_to_block(then);
    }

    fn otherwise_type(&mut self) {
        if let Some(next) = self.pending.pop() {
            self.b.switch_to_block(next);
        }
    }

    /// `receiver.method(args)` on an object, calling the compiled method of the receiver's
    /// type natively. `None` when no compiled method could be the one.
    pub(super) fn call_method(
        &mut self,
        expr: &Expr,
        object: &Expr,
        method: &str,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let candidates = self.methods_named(method, args.len());
        if candidates.is_empty() || args.iter().any(|arg| arg.name.is_some() || arg.is_mut) {
            return Ok(None);
        }
        let struct_mut_self = candidates.iter().any(|(_, key)| {
            self.shared
                .compiled
                .get(key)
                .and_then(|compiled| compiled.params.first())
                .is_some_and(|param| param.2 && matches!(param.1, Ty::Struct(_)))
        });
        if struct_mut_self {
            return Ok(None);
        }
        let changes_self = candidates.iter().any(|(_, key)| {
            self.shared
                .compiled
                .get(key)
                .is_some_and(|compiled| compiled.params.first().is_some_and(|param| param.2))
        });
        let receiver_name = match &object.kind {
            ExprKind::Name(name)
                if self.local(name).is_some_and(|local| local.ty == Ty::Obj && local.is_mut) =>
            {
                Some(name.clone())
            }
            _ => None,
        };
        if changes_self && receiver_name.is_none() {
            return Ok(None);
        }
        let mut lowered = Vec::with_capacity(args.len());
        for arg in args {
            lowered.push(self.expr(&arg.value)?);
        }
        let (value, ty) = self.expr(object)?;
        let receiver = self.handle_of(value, ty, object.span)?;
        let type_id = self.help(Helper::TypeOf, &[receiver])?;
        let mut arms = Vec::new();
        for (owner, key) in candidates {
            self.when_type(type_id, &owner);
            let params =
                self.shared.compiled.get(&key).map(|compiled| compiled.params.clone()).unwrap_or_default();
            let mut values = Vec::new();
            match (params.first(), &receiver_name) {
                (Some((_, _, true)), Some(name)) => {
                    let id = self.name_id(name);
                    values.push(self.help(Helper::Take, &[id])?);
                    values.push(id);
                }
                (Some((_, self_ty, _)), _) => {
                    values.push(self.convert(Some(receiver), Ty::Obj, *self_ty, expr.span)?);
                }
                (None, _) => values.push(receiver),
            }
            for ((value, from), (_, to, _)) in lowered.iter().zip(params.iter().skip(1)) {
                if !matches!(to, Ty::Stdout | Ty::Fs) {
                    values.push(self.convert(*value, *from, *to, expr.span)?);
                }
            }
            let (value, ty) = self.call_key(&key, values, expr.span)?;
            arms.extend(self.arm_end(value, ty));
            self.otherwise_type();
        }
        let (value, ty) = self.interpret_method(expr, object, receiver, method, &lowered)?;
        arms.extend(self.arm_end(value, ty));
        self.join(arms, expr.span).map(Some)
    }

    /// `target = value` or `target += value` where the target is a field path, like
    /// `self.position.x`.
    pub(super) fn assign_path(
        &mut self,
        root: &str,
        fields: &[String],
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
    ) -> Result<(), Diagnostic> {
        let span = target.span.to(value.span);
        let (new, ty) = match op {
            Some(op) => {
                let combined = Expr {
                    kind: ExprKind::Binary {
                        op,
                        lhs: Box::new(target.clone()),
                        rhs: Box::new(value.clone()),
                    },
                    span,
                };
                self.expr(&combined)?
            }
            None => self.expr(value)?,
        };
        let handle = self.handle_of(new, ty, value.span)?;
        self.start_path(root, fields)?;
        let [start, end] = self.span_args(target.span);
        self.call_helper(Helper::Put, &[handle, start, end]).map(|_| ())
    }

    /// `for mut name in path:`, putting each item back after its turn, also when the loop is
    /// left with `break` or `return`.
    pub(super) fn for_mut(
        &mut self,
        name: &str,
        (root, fields): (String, Vec<String>),
        iter: &Expr,
        body: &[Stmt],
    ) -> Result<(), Diagnostic> {
        let (value, ty) = self.expr(iter)?;
        let source = self.handle_of(value, ty, iter.span)?;
        let [start, end] = self.span_args(iter.span);
        let entries = self.help(Helper::Entries, &[source, start, end])?;
        self.open_scope();
        let hidden = format!("\0for{}", iter.span.start);
        let key = format!("\0key{}", iter.span.start);
        self.declare_object(&hidden, entries, false)?;
        let none = self.help(Helper::None, &[])?;
        self.declare_object(&key, none, true)?;
        // An item the checker knows is a plain struct lives natively for its turn.
        if let Some(Ty::Struct(id)) = self.checked_ty(name) {
            let zeros = self.leaf_types(id)?.into_iter().map(|(_, ty)| self.zero_leaf(ty)).collect();
            let empty = self.struct_of_leaves(zeros);
            self.declare_struct(name, empty, id, true)?;
        } else {
            let none = self.help(Helper::None, &[])?;
            self.declare_object(name, none, true)?;
        }
        let hidden_id = self.name_id(&hidden);
        let list = self.help(Helper::Get, &[hidden_id])?;
        let count = self.help(Helper::Count, &[list])?;
        self.drop_temps()?;
        let counter = self.b.declare_var(types::I64);
        let zero = self.int(0);
        self.b.def_var(counter, zero);
        let [header, inside, next, done, advance, exit] = [(); 6].map(|()| self.b.create_block());
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let at = self.b.use_var(counter);
        let more = self.b.ins().icmp(IntCC::SignedLessThan, at, count);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        self.next_item(hidden_id, at, &[&key, name], iter.span)?;
        let put_back = PutBack { root, fields, key, item: name.to_owned(), span: iter.span };
        self.loops.push(Loop { next, done, depth: self.scopes.len(), put_back: Some(put_back.clone()) });
        self.open_scope();
        let lowered = self.tail(body);
        let closed = self.close_scope();
        self.loops.pop();
        lowered?;
        closed?;
        if !self.is_dead() {
            self.b.ins().jump(next, &[]);
        }
        for (block, then) in [(next, advance), (done, exit)] {
            self.b.switch_to_block(block);
            self.put_back(&put_back)?;
            self.b.ins().jump(then, &[]);
        }
        self.b.switch_to_block(advance);
        let at = self.b.use_var(counter);
        let after = self.b.ins().iadd_imm_s(at, 1);
        self.b.def_var(counter, after);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        self.close_scope()
    }

    /// Sets a `for mut` loop's key and item names to the `at`th entry.
    fn next_item(&mut self, hidden: Value, at: Value, names: &[&str], span: Span) -> Result<(), Diagnostic> {
        let list = self.help(Helper::Get, &[hidden])?;
        let pair = self.help(Helper::Item, &[list, at])?;
        let count = self.int(2);
        let for_loop = self.b.ins().iconst(types::I8, 1);
        for (index, name) in names.iter().enumerate() {
            let copy = self.help(Helper::Copy, &[pair])?;
            let index = self.int(i64::try_from(index).unwrap_or(0));
            let [start, end] = self.span_args(span);
            let part = self.help(Helper::Unpack, &[copy, index, count, for_loop, start, end])?;
            if let Some(local @ super::Local { ty: Ty::Struct(_), .. }) = self.local(name) {
                self.set_local(name, local, Some(part), Ty::Obj, span)?;
            } else {
                let id = self.name_id(name);
                self.call_helper(Helper::Assign, &[id, part])?;
            }
        }
        self.drop_temps()
    }

    /// Puts a `for mut` loop's item back at its key.
    pub(super) fn put_back(&mut self, put_back: &PutBack) -> Result<(), Diagnostic> {
        self.start_path(&put_back.root, &put_back.fields)?;
        let key = self.name_id(&put_back.key);
        let key = self.help(Helper::Get, &[key])?;
        let item = if let Some(local @ super::Local { ty: ty @ Ty::Struct(_), .. }) =
            self.local(&put_back.item)
            && let Some(place) = self.local_place(local)
        {
            let value = self.read_place(&place);
            self.handle_of(Some(value), ty, put_back.span)?
        } else {
            let item = self.name_id(&put_back.item);
            self.help(Helper::Get, &[item])?
        };
        let [start, end] = self.span_args(put_back.span);
        self.call_helper(Helper::PutItem, &[key, item, start, end])?;
        self.drop_temps()
    }

    /// Before leaving the function from inside `for mut` loops, puts their items back.
    pub(super) fn put_back_all(&mut self) -> Result<(), Diagnostic> {
        let pending: Vec<PutBack> = self.loops.iter().rev().filter_map(|lp| lp.put_back.clone()).collect();
        for put_back in &pending {
            self.put_back(put_back)?;
        }
        Ok(())
    }
}
