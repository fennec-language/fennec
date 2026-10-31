//! Assignment, and places: a name, or a path into one, like `world.player.health` or
//! `scores[2]`. Changing a place needs its name to be `mut`.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span, TypeExpr};

use crate::function::{Binding, FnCx, Origin};
use crate::messages::mismatch;
use crate::types::{Type, a};

/// The name a place starts at: `world` in `world.player.health`.
pub(crate) fn root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. } => root(object),
        _ => None,
    }
}

impl FnCx<'_> {
    /// Checks an expression names a place that can change. `what` explains what's wrong
    /// when it isn't a place at all.
    pub(crate) fn mut_place(&mut self, expr: &Expr, what: &str) {
        match root(expr) {
            None => self.error(Diagnostic::new(what, expr.span)),
            Some(name) => {
                self.forget_fields(name);
                if self.changes_constant(name, expr.span) {
                    return;
                }
                if let Some((_, false)) = self.lookup(name) {
                    self.error(self.cannot_change(name, expr.span));
                }
            }
        }
    }

    pub(crate) fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
        written: Option<&TypeExpr>,
    ) {
        match &target.kind {
            ExprKind::Name(name) => {
                let expected = match written {
                    Some(written) => self.written_type(written),
                    None if op.is_none() => self.declared_type(name),
                    None => None,
                };
                let ty = match &expected {
                    Some(expected) => self.expr_as(value, expected),
                    None => self.expr(value),
                };
                match written {
                    Some(written) => {
                        self.declare_written(name, written, &ty, (target.span, value.span), declare_mut);
                    }
                    None => self.assign_name(name, target.span, op, ty, declare_mut),
                }
            }
            ExprKind::Tuple(names) => {
                let ty = self.expr(value);
                let parts = self.unpack(&ty, names.len(), value.span);
                for (name, ty) in names.iter().zip(parts) {
                    if let ExprKind::Name(text) = &name.kind {
                        self.assign_name(text, name.span, None, ty, false);
                    }
                }
            }
            ExprKind::Index { object, index } if op.is_none() && self.has_set_index(object) => {
                let held = self.set_index(object, index, value);
                self.record(target.span, &held);
            }
            _ => {
                self.mut_place(target, "this can't be assigned to");
                self.own_indexes_can_write(target);
                let current = self.place_type(target);
                self.record(target.span, &current);
                let mut ty = if op.is_none() { self.expr_as(value, &current) } else { self.expr(value) };
                if let Some(op) = op {
                    ty = self.binary(op, &current, &ty, target.span, (target.span, value.span));
                }
                if !self.checker.fits(&current, &ty) {
                    let current = self.checker.settled(&current);
                    self.error(mismatch("this", &current, &ty, value.span));
                }
            }
        }
    }

    /// The type a field or item holds, as declared: not narrowed by a check for None.
    fn place_type(&mut self, target: &Expr) -> Type {
        match &target.kind {
            ExprKind::Attr { object, name, optional: false } => {
                let ty = self.expr(object);
                self.field_of(&ty, name, false, target.span)
            }
            _ => self.expr(target),
        }
    }

    /// `name: type = value`, which declares a name with its type written.
    fn declare_written(
        &mut self,
        name: &str,
        written: &TypeExpr,
        ty: &Type,
        spans: (Span, Span),
        is_mut: bool,
    ) {
        let (at, value) = spans;
        if self.changes_constant(name, at) {
            return;
        }
        if self.lookup(name).is_some() {
            self.error(
                Diagnostic::new(format!("`{name}` already exists"), at)
                    .with_help(format!("to change it, write `{name} = ...` without its type")),
            );
            return;
        }
        let declared = self.resolve(written);
        if !self.checker.fits(&declared, ty) {
            self.error(mismatch(&format!("`{name}`"), &declared, ty, value));
        }
        let binding =
            Binding { name: name.to_owned(), ty: ty.clone(), declared, is_mut, origin: Origin::Local };
        self.declare_with(binding, at);
        self.record(at, &self.checker.settled(ty));
    }

    fn assign_name(&mut self, name: &str, span: Span, op: Option<BinOp>, ty: Type, declare_mut: bool) {
        if self.changes_constant(name, span) {
            return;
        }
        let existing = self.lookup(name);
        match (existing, op) {
            (Some(_), _) if declare_mut => self.error(
                Diagnostic::new(format!("`{name}` already exists"), span)
                    .with_help(format!("to change it, write `{name} = ...` without `mut`")),
            ),
            (None, Some(_)) => self.error(Diagnostic::new(format!("`{name}` doesn't exist yet"), span)),
            (None, None) => self.declare_assigned(name, ty.clone(), declare_mut, span),
            (Some((_, false)), _) => self.error(self.cannot_change(name, span)),
            (Some((current, true)), Some(op)) => {
                let ty = self.binary(op, &current, &ty, span, (span, span));
                self.give(name, ty, span);
            }
            (Some(_), None) => self.give(name, ty.clone(), span),
        }
        let held = self.lookup(name).map_or(ty, |(held, _)| held);
        self.record(span, &held);
        self.forget_fields(name);
    }

    /// Gives an existing name a new value, which has to fit the type it was declared with.
    fn give(&mut self, name: &str, ty: Type, span: Span) {
        let declared = self.declared_type(name).unwrap_or(Type::Unknown);
        if !self.checker.fits(&declared, &ty) {
            let declared = self.checker.settled(&declared);
            self.error(
                Diagnostic::new(
                    format!("`{name}` holds {}, so it can't be given {}", a(&declared), a(&ty)),
                    span,
                )
                .with_help(format!("a name keeps one type: use a new name for {}", a(&ty))),
            );
            return;
        }
        self.set_type(name, ty);
    }

    /// Changing a place through a type's own `def []` writes the item back with its `def []=`,
    /// so the type needs one.
    fn own_indexes_can_write(&mut self, place: &Expr) {
        let (ExprKind::Attr { object, .. } | ExprKind::Index { object, .. }) = &place.kind else { return };
        self.own_indexes_can_write(object);
        if !matches!(place.kind, ExprKind::Index { .. }) {
            return;
        }
        let ty = self.peek_type(object);
        if let Some(name) = ty.type_name().filter(|name| self.checker.program.method(name, "[]").is_some())
            && self.checker.program.method(name, "[]=").is_none()
        {
            let message = format!("`{name}` has a `def []` but no `def []=`, so its items can't be changed");
            let help = format!("define `def []=(mut self, index: ..., value: ...)` for `{name}`");
            self.error(Diagnostic::new(message, place.span).with_help(help));
        }
    }

    /// Whether `object[...] = value` calls the object's own `def []=`.
    fn has_set_index(&mut self, object: &Expr) -> bool {
        let ty = self.peek_type(object);
        ty.type_name().is_some_and(|name| self.checker.program.method(name, "[]=").is_some())
    }

    /// `grid[at] = value` on a type with its own `def []=`. Returns the type of the value it
    /// takes.
    fn set_index(&mut self, object: &Expr, index: &Expr, value: &Expr) -> Type {
        let ty = self.expr(object);
        let Some(name) = ty.type_name() else { return Type::Unknown };
        let Some((key, _)) = self.checker.program.method(name, "[]=") else { return Type::Unknown };
        let Some(signature) = self.method_signature(&ty, &key) else { return Type::Unknown };
        if signature.params.first().is_some_and(|param| param.is_mut) {
            self.mut_place(object, "`[]=` changes its value, so call it on something held in a mut name");
        }
        let args = [(index, signature.params.get(1)), (value, signature.params.get(2))];
        for (arg, param) in args {
            let actual = self.expr(arg);
            if let Some(param) = param {
                self.check_arg(param, &actual, arg.span);
            }
        }
        signature.params.get(2).map_or(Type::Unknown, |param| param.ty.clone())
    }
}
