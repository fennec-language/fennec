//! Assignment, and places: a name, or a path into one, like `world.player.health` or
//! `scores[2]`. Changing a place needs its name to be `mut`.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span};

use crate::function::FnCx;
use crate::types::Type;

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
                if let Some((_, false)) = self.lookup(name) {
                    self.error(self.cannot_change(name, expr.span));
                }
            }
        }
    }

    pub(crate) fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, declare_mut: bool) {
        match &target.kind {
            ExprKind::Name(name) => {
                let ty = self.expr(value);
                self.assign_name(name, target.span, op, ty, declare_mut);
            }
            ExprKind::Tuple(names) => {
                let ty = self.expr(value);
                let parts = self.unpack(&ty, names.len(), value.span);
                for (name, ty) in names.iter().zip(parts) {
                    if let ExprKind::Name(name) = &name.kind {
                        self.assign_name(name, target.span, None, ty, false);
                    }
                }
            }
            ExprKind::Index { object, index } if self.has_set_index(object) => {
                self.set_index(object, index, value);
            }
            _ => {
                self.mut_place(target, "this can't be assigned to");
                let current = self.expr(target);
                let ty = self.expr(value);
                if let Some(op) = op {
                    self.binary(op, &current, &ty, target.span, (target.span, value.span));
                }
            }
        }
    }

    fn assign_name(&mut self, name: &str, span: Span, op: Option<BinOp>, ty: Type, declare_mut: bool) {
        self.forget_fields(name);
        let existing = self.lookup(name);
        match (existing, op) {
            (Some(_), _) if declare_mut => self.error(
                Diagnostic::new(format!("`{name}` already exists"), span)
                    .with_help(format!("to change it, write `{name} = ...` without `mut`")),
            ),
            (None, Some(_)) => self.error(Diagnostic::new(format!("`{name}` doesn't exist yet"), span)),
            (None, None) => self.declare(name, ty, declare_mut),
            (Some((_, false)), _) => self.error(self.cannot_change(name, span)),
            (Some((current, true)), Some(op)) => {
                let ty = self.binary(op, &current, &ty, span, (span, span));
                self.set_type(name, ty);
            }
            (Some(_), None) => self.set_type(name, ty),
        }
    }

    /// Whether `object[...] = value` calls the object's own `def []=`.
    fn has_set_index(&mut self, object: &Expr) -> bool {
        let ty = self.peek_type(object);
        match ty {
            Type::Named(name) => self.checker.program.method(&name, "[]=").is_some(),
            _ => false,
        }
    }

    /// `grid[at] = value` on a type with its own `def []=`.
    fn set_index(&mut self, object: &Expr, index: &Expr, value: &Expr) {
        let ty = self.expr(object);
        let Type::Named(name) = ty else { return };
        let Some((key, _)) = self.checker.program.method(&name, "[]=") else { return };
        let Some(signature) = self.checker.signature(&key) else { return };
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
    }
}
