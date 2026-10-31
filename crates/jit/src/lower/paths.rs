//! Changes to what's inside a native value: `self.tasks[at].queue = 1`, or
//! `self.items.append(2)`. Each list, dict or boxed value on the way, starting from the name,
//! is taken out of what holds it and held by a hidden name while it changes, so it has just
//! one owner and needn't be copied, and then put back. Everything the change works out comes
//! first, as nothing can look at a value that's been taken out.

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Span, Stmt};

use super::expr::Lowered;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::types::{Ty, unsupported};

/// A name made up for a value on the way to a change, which no program can write.
fn hidden(what: &str, span: Span) -> String {
    format!("\0{what}{}", span.start)
}

fn name_expr(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Name(name.to_owned()), span }
}

/// Whether working something out might call something, which could have an effect.
pub(super) fn has_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Name(_) => {
            false
        }
        ExprKind::Attr { object, .. } => has_call(object),
        ExprKind::Index { object, index } => has_call(object) || has_call(index),
        ExprKind::Binary { lhs, rhs, .. } => has_call(lhs) || has_call(rhs),
        ExprKind::Unary { expr, .. } => has_call(expr),
        _ => true,
    }
}

/// The list methods that change the list.
const CHANGES_LIST: [&str; 7] = ["append", "clear", "insert", "pop", "remove", "remove_where", "sort"];

/// The dict methods that change the dict.
const CHANGES_DICT: [&str; 3] = ["clear", "pop", "remove"];

impl Lowerer<'_, '_> {
    /// What a place inside a changeable name holds natively, if it's one.
    pub(super) fn place_ty(&self, expr: &Expr) -> Option<Ty> {
        match &expr.kind {
            ExprKind::Name(name) => self.local(name).filter(|local| local.is_mut).map(|local| local.ty),
            ExprKind::Attr { object, name, optional: false } => match self.place_ty(object)? {
                Ty::Boxed(id) | Ty::Struct(id) if self.cases(id).ok()?.is_none() => {
                    self.struct_field(id, name).ok()?.map(|field| field.ty)
                }
                _ => None,
            },
            ExprKind::Index { object, .. } => match self.place_ty(object)? {
                Ty::List(item) => Some(item.ty()),
                Ty::Dict(_, value) => Some(super::dicts::word_ty(value)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The outermost list, dict or boxed value a place is inside, other than a name.
    fn holder<'e>(&self, place: &'e Expr) -> Option<&'e Expr> {
        let (ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. }) = &place.kind
        else {
            return None;
        };
        match (&object.kind, self.place_ty(object)?) {
            (ExprKind::Name(_), _) => None,
            (_, ty) if ty.counted() => Some(object),
            _ => self.holder(object),
        }
    }

    /// Whether a `for mut` loop goes over a list inside a native value that its body leaves
    /// alone, other than through the loop's name.
    pub(super) fn loops_inside(&self, iter: &Expr, body: &[Stmt]) -> bool {
        !matches!(iter.kind, ExprKind::Name(_))
            && matches!(self.place_ty(iter), Some(Ty::List(_)))
            && root_name(iter).is_some_and(|root| !Self::body_names(body, root))
    }

    /// `for mut item in place` for a list inside a native value. A `return` can leave from
    /// the middle, so the list stays where it is, with what holds it having one owner.
    pub(super) fn for_mut_inside(
        &mut self,
        names: &[String],
        iter: &Expr,
        body: &[Stmt],
    ) -> Result<(), Diagnostic> {
        self.open_scope();
        let iter = self.with_indexes_worked_out(iter)?;
        let looped = self.reached(&iter, &mut |lowerer, held| {
            if lowerer.for_packed(names, held, body, true)? {
                Ok((None, Ty::Unit))
            } else {
                Err(unsupported("this `for mut` loop", held.span))
            }
        });
        self.close_scope()?;
        looped.map(|_| ())
    }

    /// Whether assigning to a place goes through something that has to be taken out.
    pub(super) fn changes_inside(&self, target: &Expr) -> bool {
        self.place_ty(target).is_some() && self.holder(target).is_some()
    }

    /// `place = value` inside a native value. The value comes first, so a left side that's
    /// read for `+=` has to come out the same whichever is worked out first.
    pub(super) fn assign_inside(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
    ) -> Result<(), Diagnostic> {
        if op.is_some() && has_call(value) {
            return self.interpret_assign(target, op, value, false, target.span);
        }
        self.open_scope();
        let value = self.worked_out("value", value)?;
        let target = self.with_indexes_worked_out(target)?;
        let done = match self.holder(&target) {
            Some(holder) => self.taken(holder, &mut |lowerer, held| {
                let target = swapped(&target, holder, held);
                lowerer.assign(&target, op, &value, false)?;
                Ok((None, Ty::Unit))
            }),
            None => Err(internal("a place with nothing to take out", target.span)),
        };
        self.close_scope()?;
        done.map(|_| ())
    }

    /// `place.method(args)` inside a native value, for a method that changes what it's called on.
    pub(super) fn call_inside(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return Ok(None) };
        if matches!(object.kind, ExprKind::Name(_)) || args.iter().any(|arg| arg.is_mut) {
            return Ok(None);
        }
        // An arrow runs while the value is taken out, so it can't look at what held it.
        let Some(root) = root_name(object) else { return Ok(None) };
        let sees_root = |arg: &Arg| {
            let mut names = Vec::new();
            super::fallback::expr_names(&arg.value, &mut names);
            matches!(arg.value.kind, ExprKind::Lambda { .. }) && names.iter().any(|named| named == root)
        };
        if args.iter().any(sees_root) {
            return Ok(None);
        }
        let changes = match self.place_ty(object) {
            Some(Ty::List(_)) => CHANGES_LIST.contains(&name.as_str()),
            Some(Ty::Dict(..)) => CHANGES_DICT.contains(&name.as_str()),
            Some(Ty::Boxed(id)) => {
                let key = format!("{}.{name}", self.layout(id)?.name);
                let compiled = self.shared.compiled.get(&key);
                compiled.and_then(|compiled| compiled.params.first()).is_some_and(|param| param.2)
            }
            _ => false,
        };
        if !changes {
            return Ok(None);
        }
        self.open_scope();
        let mut worked_out = Vec::with_capacity(args.len());
        for arg in args {
            let value = if matches!(arg.value.kind, ExprKind::Lambda { .. }) {
                arg.value.clone()
            } else {
                self.worked_out("arg", &arg.value)?
            };
            worked_out.push(Arg { name: arg.name.clone(), is_mut: false, value });
        }
        let object = self.with_indexes_worked_out(object)?;
        let result = self.taken(&object, &mut |lowerer, held| {
            let callee = Expr {
                kind: ExprKind::Attr { object: Box::new(held.clone()), name: name.clone(), optional: false },
                span: callee.span,
            };
            let call = Expr {
                kind: ExprKind::Call { callee: Box::new(callee.clone()), args: worked_out.clone() },
                span: expr.span,
            };
            lowerer.call(&call, &callee, &worked_out)
        });
        let result = result.map(|(value, ty)| (value.map(|value| self.own(value, ty)), ty));
        self.close_scope()?;
        result.map(Some)
    }

    /// Works an expression out now and gives back a hidden name holding it, or the expression
    /// itself when it's just a number or a name.
    fn worked_out(&mut self, what: &str, expr: &Expr) -> Result<Expr, Diagnostic> {
        if matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Name(_))
        {
            return Ok(expr.clone());
        }
        let lowered = self.expr(expr)?;
        let name = hidden(what, expr.span);
        self.hold(&name, lowered)?;
        Ok(name_expr(&name, expr.span))
    }

    /// Holds a value under a name in this scope.
    fn hold(&mut self, name: &str, (value, ty): Lowered) -> Result<(), Diagnostic> {
        match (value, ty) {
            (Some(value), Ty::Struct(id)) => self.declare_struct(name, value, id, false),
            (Some(value), Ty::Obj) => self.declare_object(name, value, false),
            (value, ty) => {
                let value = value.map(|value| self.own(value, ty));
                self.bind_native(name, ty, value, false);
                Ok(())
            }
        }
    }

    /// A place with each index in it worked out, from the outside in.
    fn with_indexes_worked_out(&mut self, place: &Expr) -> Result<Expr, Diagnostic> {
        Ok(match &place.kind {
            ExprKind::Attr { object, name, optional } => Expr {
                kind: ExprKind::Attr {
                    object: Box::new(self.with_indexes_worked_out(object)?),
                    name: name.clone(),
                    optional: *optional,
                },
                span: place.span,
            },
            ExprKind::Index { object, index } => {
                let object = self.with_indexes_worked_out(object)?;
                let index = self.worked_out("index", index)?;
                Expr {
                    kind: ExprKind::Index { object: Box::new(object), index: Box::new(index) },
                    span: place.span,
                }
            }
            _ => place.clone(),
        })
    }

    /// Lowers a change to a value with it taken out of the place holding it, under a hidden
    /// name, and puts it back after.
    fn taken(
        &mut self,
        place: &Expr,
        change: &mut dyn FnMut(&mut Self, &Expr) -> Result<Lowered, Diagnostic>,
    ) -> Result<Lowered, Diagnostic> {
        let ty = self.place_ty(place).ok_or_else(|| internal("a lost place", place.span))?;
        let (ExprKind::Attr { object, .. } | ExprKind::Index { object, .. }) = &place.kind else {
            return change(self, place);
        };
        if matches!(object.kind, ExprKind::Name(_)) {
            let slot = self.slot(place, object)?;
            return self.taken_from(slot, ty, place.span, change);
        }
        self.taken(object, &mut |lowerer, held| {
            let inside = swapped(place, object, held);
            let slot = lowerer.slot(&inside, held)?;
            lowerer.taken_from(slot, ty, place.span, change)
        })
    }

    /// Lowers something with a value inside a native value, and each value on the way to it,
    /// held under a hidden name, still in place with one owner.
    fn reached(
        &mut self,
        place: &Expr,
        change: &mut dyn FnMut(&mut Self, &Expr) -> Result<Lowered, Diagnostic>,
    ) -> Result<Lowered, Diagnostic> {
        let ty = self.place_ty(place).ok_or_else(|| internal("a lost place", place.span))?;
        let (ExprKind::Attr { object, .. } | ExprKind::Index { object, .. }) = &place.kind else {
            return change(self, place);
        };
        if matches!(object.kind, ExprKind::Name(_)) {
            let slot = self.slot(place, object)?;
            return self.reached_in(slot, ty, place.span, change);
        }
        self.reached(object, &mut |lowerer, held| {
            let inside = swapped(place, object, held);
            let slot = lowerer.slot(&inside, held)?;
            lowerer.reached_in(slot, ty, place.span, change)
        })
    }

    fn reached_in(
        &mut self,
        slot: Value,
        ty: Ty,
        span: Span,
        change: &mut dyn FnMut(&mut Self, &Expr) -> Result<Lowered, Diagnostic>,
    ) -> Result<Lowered, Diagnostic> {
        let flags = MemFlagsData::trusted();
        let value = self.b.ins().load(types::I64, flags, slot, 0);
        let count = self.b.ins().load(types::I64, flags, value, 0);
        let shared = self.b.ins().icmp_imm_s(IntCC::NotEqual, count, 1);
        let [copy, done] = [(); 2].map(|()| self.b.create_block());
        self.b.set_cold_block(copy);
        self.b.append_block_param(done, types::I64);
        self.b.ins().brif(shared, copy, &[], done, &[value.into()]);
        self.b.switch_to_block(copy);
        self.b.seal_block(copy);
        let copied = self.help(Helper::Unshare, &[value])?;
        self.b.ins().store(flags, copied, slot, 0);
        self.b.ins().jump(done, &[copied.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        let value = self.b.block_params(done).first().copied();
        let value = value.ok_or_else(|| internal("a lost value", span))?;
        let name = hidden("reached", span);
        self.open_scope();
        self.bind_native(&name, ty, Some(value), true);
        if let Some(var) = self.local(&name).and_then(|local| local.var) {
            self.borrowed_names.insert(var.index());
        }
        let changed = change(self, &name_expr(&name, span));
        self.close_scope()?;
        changed
    }

    /// Where a field or item is, in what a name holds, which has only that name for an owner.
    fn slot(&mut self, place: &Expr, object: &Expr) -> Result<Value, Diagnostic> {
        let name = match &object.kind {
            ExprKind::Name(name) => name.clone(),
            _ => return Err(internal("a place inside something without a name", place.span)),
        };
        match (&place.kind, self.local(&name).map(|local| (local.ty, local.var))) {
            (ExprKind::Attr { name: field, .. }, Some((Ty::Boxed(id), Some(var)))) => {
                let fields = self.unshare(var)?;
                let field =
                    self.struct_field(id, field)?.ok_or_else(|| internal("a lost field", place.span))?;
                Ok(self.b.ins().iadd_imm_s(fields, i64::from(field.offset)))
            }
            (ExprKind::Index { index, .. }, Some((Ty::List(item), _))) => {
                self.item_address(&name, item.ty(), index, true, place.span)
            }
            _ => Err(internal("a place inside something that isn't held natively", place.span)),
        }
    }

    fn taken_from(
        &mut self,
        slot: Value,
        ty: Ty,
        span: Span,
        change: &mut dyn FnMut(&mut Self, &Expr) -> Result<Lowered, Diagnostic>,
    ) -> Result<Lowered, Diagnostic> {
        let flags = MemFlagsData::trusted();
        let value = self.b.ins().load(types::I64, flags, slot, 0);
        let none = self.int(0);
        self.b.ins().store(flags, none, slot, 0);
        let name = hidden("taken", span);
        self.open_scope();
        self.bind_native(&name, ty, Some(value), true);
        let changed = change(self, &name_expr(&name, span));
        if !self.is_dead() {
            let var = self.local(&name).and_then(|local| local.var);
            let var = var.ok_or_else(|| internal("a lost value", span))?;
            let value = self.b.use_var(var);
            self.b.ins().store(flags, value, slot, 0);
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.remove(&name);
        }
        self.close_scope()?;
        changed
    }
}

/// The name a place starts from.
fn root_name(place: &Expr) -> Option<&str> {
    match &place.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => root_name(object),
        _ => None,
    }
}

/// A place with what's inside it, `from`, replaced by `to`.
fn swapped(place: &Expr, from: &Expr, to: &Expr) -> Expr {
    if std::ptr::eq(place, from) {
        return to.clone();
    }
    let kind = match &place.kind {
        ExprKind::Attr { object, name, optional } => ExprKind::Attr {
            object: Box::new(swapped(object, from, to)),
            name: name.clone(),
            optional: *optional,
        },
        ExprKind::Index { object, index } => {
            ExprKind::Index { object: Box::new(swapped(object, from, to)), index: index.clone() }
        }
        other => other.clone(),
    };
    Expr { kind, span: place.span }
}
