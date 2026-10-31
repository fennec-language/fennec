//! Optionals and results: wrapping a value going where one goes, unwrapping a name known not
//! to be `None`, and `is None`, `else`, `fail` and `?`.

use fennec_check::{Checked, Type};
use fennec_syntax::{BinOp, Expr, ExprKind, Pattern, Span, UnaryOp};

use super::{Lowered, Lowering};
use crate::validate::fits;
use crate::{BlockId, InstKind, Operand, StructDef, Terminator, type_in};

/// Every error type the IR can hold: not ones with `where` rules, which are checked
/// whenever a value is made.
pub(super) fn definitions(checked: &Checked) -> Vec<StructDef> {
    let signatures = &checked.signatures;
    signatures
        .error_types()
        .filter_map(|name| {
            let found = signatures.error_type(name).filter(|found| !found.rules)?;
            Some(StructDef { name: name.to_owned(), fields: found.fields.clone() })
        })
        .collect()
}

impl Lowering<'_> {
    /// Makes a value fit where it's going: wrapped as an optional or a result if it has to
    /// be. The checker has shown it fits, so anything else isn't lowered yet.
    pub(super) fn coerce(&mut self, value: Operand, to: &Type, span: Span) -> Lowered<Operand> {
        let from = type_in(&self.locals, &value).ok_or("a value has no type")?;
        if fits(to, &from) {
            return Ok(value);
        }
        match to {
            Type::Optional(inner) => {
                let value = self.coerce(value, inner, span)?;
                let dest = self.temporary(to.clone());
                self.emit(InstKind::MakeSome { dest, value }, span);
                Ok(Operand::Local(dest))
            }
            Type::Result(ok, _) => {
                let value = self.coerce(value, ok, span)?;
                let dest = self.temporary(to.clone());
                self.emit(InstKind::MakeOk { dest, value: Some(value) }, span);
                Ok(Operand::Local(dest))
            }
            _ => Err(format!("it gives {from} where {to} goes")),
        }
    }

    /// A value read as the type the typed tree has for `expr`, which is narrower than it's
    /// declared with once a check has shown it isn't `None`.
    pub(super) fn narrowed(&mut self, value: Operand, expr: &Expr) -> Lowered<Operand> {
        let from = type_in(&self.locals, &value).ok_or("a value has no type")?;
        let Some(read) = self.body.expr(expr.span).filter(|read| **read != from) else {
            return Ok(value);
        };
        match (&from, read) {
            // A local given a function by name holds it as a closure.
            (Type::Arrow(..), Type::Function(_)) => Ok(value),
            (Type::Optional(_), Type::None) => Ok(Operand::None),
            (Type::Optional(inner), read) if **inner == *read => {
                let dest = self.temporary(read.clone());
                self.emit(InstKind::GetSome { dest, value }, expr.span);
                Ok(Operand::Local(dest))
            }
            (from, read) => Err(format!("it reads {from} as {read}")),
        }
    }

    /// `value is None` and `value is not None`.
    pub(super) fn is_none(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Lowered<Operand> {
        if !matches!(rhs.kind, ExprKind::None) {
            return Err("it uses `is` with something other than `None`".to_owned());
        }
        let value = self.expr(lhs)?;
        let test = match type_in(&self.locals, &value) {
            Some(Type::None) => Operand::Bool(true),
            Some(Type::Optional(_)) => {
                let dest = self.temporary(Type::Bool);
                self.emit(InstKind::IsNone { dest, value }, span);
                Operand::Local(dest)
            }
            _ => Operand::Bool(false),
        };
        if op == BinOp::Is {
            return Ok(test);
        }
        let dest = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest, op: UnaryOp::Not, value: test }, span);
        Ok(Operand::Local(dest))
    }

    /// `value else default`: the default only runs when the value is `None` or an error.
    pub(super) fn else_value(&mut self, expr: &Expr, value: &Expr, default: &Expr) -> Lowered<Operand> {
        let span = expr.span;
        let ty = self.ty(expr)?;
        let value = self.expr(value)?;
        let held = type_in(&self.locals, &value).ok_or("a value has no type")?;
        let missing = self.temporary(Type::Bool);
        match &held {
            Type::Optional(_) => self.emit(InstKind::IsNone { dest: missing, value: value.clone() }, span),
            Type::Result(..) => self.emit(InstKind::IsErr { dest: missing, value: value.clone() }, span),
            Type::None => self.emit(InstKind::Copy { dest: missing, value: Operand::Bool(true) }, span),
            _ => return Ok(value),
        }
        let dest = self.temporary(ty.clone());
        let (fallback, present, join) = (self.new_block(), self.new_block(), self.new_block());
        let cond = Operand::Local(missing);
        self.finish(Terminator::Branch { cond, then: fallback, otherwise: present });
        self.current = fallback;
        let lowered = self.expr(default)?;
        let lowered = self.coerce(lowered, &ty, default.span)?;
        self.emit(InstKind::Copy { dest, value: lowered }, span);
        self.finish(Terminator::Jump(join));
        self.current = present;
        let inside = match held {
            Type::Optional(inner) => {
                let inside = self.temporary(*inner);
                self.emit(InstKind::GetSome { dest: inside, value }, span);
                Operand::Local(inside)
            }
            Type::Result(ok, _) if *ok != Type::Unit => {
                let inside = self.temporary(*ok);
                self.emit(InstKind::GetOk { dest: inside, value }, span);
                Operand::Local(inside)
            }
            Type::None => Operand::None,
            _ => return Err("it uses `else` on a result of nothing".to_owned()),
        };
        let inside = self.coerce(inside, &ty, span)?;
        self.emit(InstKind::Copy { dest, value: inside }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// Leaves the function with an error.
    fn return_error(&mut self, error: Operand, span: Span) {
        let dest = self.temporary(self.returns.clone());
        self.emit(InstKind::MakeErr { dest, error }, span);
        self.finish(Terminator::Return(Some(Operand::Local(dest))));
    }

    /// `fail error`.
    pub(super) fn fail(&mut self, error: &Expr, span: Span) -> Lowered<()> {
        let error = self.expr(error)?;
        self.return_error(error, span);
        Ok(())
    }

    /// `value?`, and `value? from wrapper`: leaves the function if it's an error, and gives
    /// the value inside if there's anything there.
    pub(super) fn try_value(
        &mut self,
        value: &Expr,
        context: Option<&Expr>,
        span: Span,
    ) -> Lowered<Option<Operand>> {
        let value = self.expr(value)?;
        let Some(Type::Result(ok, error)) = type_in(&self.locals, &value) else {
            return Err("it uses `?` on something other than a result".to_owned());
        };
        let failed = self.temporary(Type::Bool);
        self.emit(InstKind::IsErr { dest: failed, value: value.clone() }, span);
        let (up, present) = (self.new_block(), self.new_block());
        let cond = Operand::Local(failed);
        self.finish(Terminator::Branch { cond, then: up, otherwise: present });
        self.current = up;
        let cause = self.temporary(*error);
        self.emit(InstKind::GetErr { dest: cause, value: value.clone() }, span);
        let mut passed = Operand::Local(cause);
        if let Some(context) = context {
            let wrapper = self.expr(context)?;
            let ty = type_in(&self.locals, &wrapper).ok_or("a value has no type")?;
            let dest = self.temporary(ty);
            let name = "error.with_cause".to_owned();
            let args = vec![wrapper, passed];
            self.emit(InstKind::Builtin { dest: Some(dest), name, args, changes: false }, span);
            passed = Operand::Local(dest);
        }
        self.return_error(passed, span);
        self.current = present;
        if *ok == Type::Unit {
            return Ok(None);
        }
        let dest = self.temporary(*ok);
        self.emit(InstKind::GetOk { dest, value }, span);
        Ok(Some(Operand::Local(dest)))
    }

    /// `return value`, or a function's last value. A last line that gives nothing runs, and
    /// the function gives back nothing.
    pub(super) fn return_value(&mut self, expr: &Expr) -> Lowered<()> {
        if matches!(self.body.expr(expr.span), Some(Type::Unit)) {
            self.effect(expr)?;
            return self.return_nothing(expr.span);
        }
        let value = self.expr(expr)?;
        let returns = self.returns.clone();
        let value = self.coerce(value, &returns, expr.span)?;
        self.leave(Some(value), expr.span)
    }

    /// Leaves a function that gives back nothing, which is success if it can fail.
    pub(super) fn return_nothing(&mut self, span: Span) -> Lowered<()> {
        if matches!(self.returns, Type::Result(..)) {
            let dest = self.temporary(self.returns.clone());
            self.emit(InstKind::MakeOk { dest, value: None }, span);
            self.leave(Some(Operand::Local(dest)), span)
        } else {
            self.leave(None, span)
        }
    }

    /// Ends a function's last block. A function with a value to give back has already
    /// returned, or the checker would object.
    pub(super) fn end_function(&mut self, span: Span) -> Lowered<()> {
        if !self.is_open() {
            return Ok(());
        }
        match &self.returns {
            Type::Unit => self.finish(Terminator::Return(None)),
            Type::Result(..) => self.return_nothing(span)?,
            _ => self.finish(Terminator::Unreachable),
        }
        Ok(())
    }

    /// Carries on in a new block when `cond` is false, and goes to `fail` when it's true.
    fn branch_unless(&mut self, cond: Operand, fail: BlockId) {
        let then = self.new_block();
        self.finish(Terminator::Branch { cond, then: fail, otherwise: then });
        self.current = then;
    }

    /// A pattern on a result. `Err` and an error type's name look at the error, and
    /// everything else looks through `Ok`, so a forgotten error can't match a plain name.
    pub(super) fn result_pattern(
        &mut self,
        pattern: &Pattern,
        value: &Operand,
        (ok, error): (&Type, &Type),
        fail: BlockId,
        span: Span,
    ) -> Lowered<()> {
        let failed = self.temporary(Type::Bool);
        self.emit(InstKind::IsErr { dest: failed, value: value.clone() }, span);
        let failed = Operand::Local(failed);
        let (sees_error, inner) = match pattern {
            Pattern::Variant { name, dotted: false, args } if name == "Err" => (true, args.first()),
            Pattern::Variant { name, dotted: false, args } if name == "Ok" => (false, args.first()),
            Pattern::Variant { name, dotted: false, .. } if self.context.errors.contains(name.as_str()) => {
                (true, Some(pattern))
            }
            other => (false, Some(other)),
        };
        if sees_error {
            self.branch_or(failed, fail);
            let dest = self.temporary(error.clone());
            self.emit(InstKind::GetErr { dest, value: value.clone() }, span);
            return match inner {
                Some(inner) => self.pattern(inner, &Operand::Local(dest), error, fail, span),
                None => Ok(()),
            };
        }
        self.branch_unless(failed, fail);
        let Some(inner) = inner else { return Ok(()) };
        if *ok == Type::Unit {
            return Err("it matches what a result of nothing holds".to_owned());
        }
        let dest = self.temporary(ok.clone());
        self.emit(InstKind::GetOk { dest, value: value.clone() }, span);
        self.pattern(inner, &Operand::Local(dest), ok, fail, span)
    }

    /// A pattern on an optional: `None`, or a pattern for the value inside.
    pub(super) fn optional_pattern(
        &mut self,
        pattern: &Pattern,
        value: &Operand,
        (_, inner): (&Type, &Type),
        fail: BlockId,
        span: Span,
    ) -> Lowered<()> {
        let missing = self.temporary(Type::Bool);
        self.emit(InstKind::IsNone { dest: missing, value: value.clone() }, span);
        let missing = Operand::Local(missing);
        if matches!(pattern, Pattern::Literal(Expr { kind: ExprKind::None, .. })) {
            self.branch_or(missing, fail);
            return Ok(());
        }
        self.branch_unless(missing, fail);
        let dest = self.temporary(inner.clone());
        self.emit(InstKind::GetSome { dest, value: value.clone() }, span);
        self.pattern(pattern, &Operand::Local(dest), inner, fail, span)
    }
}
