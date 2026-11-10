//! Optionals and results: wrapping a value going where one goes, unwrapping a name known not
//! to be `None`, and `is None`, `else`, `fail` and `?`.

use fennec_check::{Signatures, Type};
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, FnDef, Pattern, Span, UnaryOp};

use super::places::Leaving;
use super::{Lowered, Lowering, Options, Tail};
use crate::validate::fits;
use crate::{
    Block, BlockId, Function, Inst, InstKind, Local, LocalId, Operand, Program, StructDef, Terminator,
    type_in,
};

/// Every error type the IR can hold: not ones with `where` rules, which are checked
/// whenever a value is made.
pub(super) fn definitions(signatures: &Signatures) -> Vec<StructDef> {
    signatures
        .error_types()
        // An error with cases is held like an enum, with the other enums.
        .filter(|name| signatures.enumeration(name).is_none())
        .filter_map(|name| {
            let found = signatures.error_type(name).filter(|found| !found.rules)?;
            Some(StructDef { name: name.to_owned(), fields: found.fields.clone(), shared: false })
        })
        .collect()
}

/// A `with` block's cleanup, run with the failure in `name` when one leaves from inside what the
/// block calls.
pub(super) struct OnFailure {
    pub(super) name: String,
    pub(super) cleanup: Vec<fennec_syntax::Stmt>,
}

impl Lowering<'_> {
    /// `Ok(value)` or `Err(error)` written out, as a function that says `fails E` can return.
    pub(super) fn written_result(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Lowered<Option<Operand>> {
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        if !matches!(name.as_str(), "Ok" | "Err") || self.lookup(name).is_some() {
            return Ok(None);
        }
        let ty = self.ty(expr)?;
        let value = match args {
            [] => None,
            [arg] if arg.name.is_none() && !arg.is_mut => Some(self.expr(&arg.value)?),
            _ => return Err(format!("it calls `{name}` with more than one value")),
        };
        let dest = self.temporary(ty);
        let kind = match (name.as_str(), value) {
            ("Err", Some(error)) => InstKind::MakeErr { dest, error },
            ("Ok", value) => InstKind::MakeOk { dest, value },
            _ => return Err("it calls `Err` with nothing".to_owned()),
        };
        self.emit(kind, expr.span);
        Ok(Some(Operand::Local(dest)))
    }

    /// Makes a value fit where it's going: wrapped as an optional or a result if it has to
    /// be. The checker has shown it fits, so anything else isn't lowered yet.
    pub(super) fn coerce(&mut self, value: Operand, to: &Type, span: Span) -> Lowered<Operand> {
        let from = type_in(&self.locals, &value).ok_or("a value has no type")?;
        if fits(to, &from) {
            return Ok(value);
        }
        // A number written out where a sized one goes, which the checker found fits.
        if matches!(value, Operand::Int(_)) && matches!(to, Type::Sized(_)) {
            return Ok(value);
        }
        // A float written out where an `f32` goes is the nearest `f32`.
        if let (Operand::Float(written), Type::Sized(fennec_check::Num::F32)) = (&value, to) {
            return Ok(Operand::Float(nearest_f32(*written)));
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
            // A member where its union goes, tagged with which member it is.
            Type::Union(members) if let Some(member) = members.iter().position(|member| *member == from) => {
                let dest = self.temporary(to.clone());
                self.emit(InstKind::MakeMember { dest, value, member }, span);
                Ok(Operand::Local(dest))
            }
            // One union where another goes, as what's left after an `is` given where `Pg | Mem`
            // goes, or that given where `Three` goes: remade as the other, member by member.
            Type::Union(members) if let Type::Union(held) = &from => {
                Ok(self.union_as_union(value, held, members, span))
            }
            _ => Err(format!("it gives {from} where {to} goes")),
        }
    }

    /// A union's value remade as another union's: a test for each member they share, that
    /// member read out and tagged as the other's. The checker has shown the value holds one of
    /// them; one that doesn't ends the program, as a `match` it gets past does.
    fn union_as_union(&mut self, value: Operand, held: &[Type], to: &[Type], span: Span) -> Operand {
        let union = Type::Union(to.to_vec());
        let dest = self.temporary(union.clone());
        let join = self.new_block();
        for (at, member) in held.iter().enumerate() {
            let Some(into) = to.iter().position(|each| each == member) else { continue };
            let next = self.new_block();
            let test = self.temporary(Type::Bool);
            self.emit(InstKind::IsMember { dest: test, value: value.clone(), member: at }, span);
            self.branch_or(Operand::Local(test), next);
            let one = self.temporary(member.clone());
            self.emit(InstKind::GetMember { dest: one, value: value.clone(), member: at }, span);
            self.emit(InstKind::MakeMember { dest, value: Operand::Local(one), member: into }, span);
            self.finish(Terminator::Jump(join));
            self.current = next;
        }
        self.finish(Terminator::NoMatch { value, span });
        self.current = join;
        Operand::Local(dest)
    }

    /// A value read as the type the typed tree has for `expr`, which is narrower than it's
    /// declared with once a check has shown it isn't `None`.
    pub(super) fn narrowed(&mut self, value: Operand, expr: &Expr) -> Lowered<Operand> {
        let from = type_in(&self.locals, &value).ok_or("a value has no type")?;
        // A function named inside what's read, as in `(add, 4)`, is held as its closure.
        let read = self.type_at(expr.span).map(|read| match read {
            Type::Function(_) => read,
            other => self.closures_for_functions(&other).unwrap_or(other),
        });
        let Some(read) = read.filter(|read| *read != from) else {
            return Ok(value);
        };
        match (&from, read) {
            // A local given a function by name holds it as a closure.
            (Type::Arrow(..), Type::Function(_)) => Ok(value),
            (Type::Optional(_), Type::None) => Ok(Operand::None),
            (Type::Optional(inner), read) if **inner == read => {
                let dest = self.temporary(read);
                self.emit(InstKind::GetSome { dest, value }, expr.span);
                Ok(Operand::Local(dest))
            }
            // A union the checker narrowed to one member, by `is` or a `match`.
            (Type::Union(members), read)
                if let Some(member) = members.iter().position(|member| *member == read) =>
            {
                let dest = self.temporary(read);
                self.emit(InstKind::GetMember { dest, value, member }, expr.span);
                Ok(Operand::Local(dest))
            }
            // A union the checker narrowed to some of its members, as what's left after
            // `if s is Circle: return`, is held as it was: which member it holds, and each
            // member, are read by the union it is.
            (Type::Union(members), Type::Union(left))
                if left.iter().all(|member| members.contains(member)) =>
            {
                Ok(value)
            }
            // An error from a set the checker narrowed to one, as a `match` whose earlier cases
            // took the others.
            (Type::Errors(members), Type::Named(read)) if members.contains(&read) => {
                let dest = self.temporary(Type::Named(read));
                self.emit(InstKind::AsError { dest, value }, expr.span);
                Ok(Operand::Local(dest))
            }
            (from, read) => Err(format!("it reads {from} as {read}")),
        }
    }

    /// `s is Circle`: whether a union holds that member. A value the checker already narrowed to
    /// one member is known to, or known not to.
    fn is_member(&mut self, op: BinOp, lhs: &Expr, member: &str, span: Span) -> Lowered<(Operand, Operand)> {
        let value = self.expr(lhs)?;
        let held = type_in(&self.locals, &value).ok_or("a value has no type")?;
        let test = match &held {
            Type::Union(members) => {
                let at = members.iter().position(|each| each.to_string() == member);
                let at = at.ok_or_else(|| format!("`{member}` isn't one of the types in {held}"))?;
                let dest = self.temporary(Type::Bool);
                self.emit(InstKind::IsMember { dest, value: value.clone(), member: at }, span);
                Operand::Local(dest)
            }
            other => Operand::Bool(other.to_string() == member),
        };
        if op == BinOp::Is {
            return Ok((test, value));
        }
        let dest = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest, op: UnaryOp::Not, value: test }, span);
        Ok((Operand::Local(dest), value))
    }

    /// `value is None` and `value is not None`.
    pub(super) fn is_none(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Lowered<Operand> {
        self.is_none_of(op, lhs, rhs, span).map(|(test, _)| test)
    }

    /// The same, with the value it looked at.
    pub(super) fn is_none_of(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
    ) -> Lowered<(Operand, Operand)> {
        if let ExprKind::Name(member) = &rhs.kind {
            return self.is_member(op, lhs, member, span);
        }
        if !matches!(rhs.kind, ExprKind::None) {
            return Err("it uses `is` with something other than `None`".to_owned());
        }
        let value = self.expr(lhs)?;
        let test = match type_in(&self.locals, &value) {
            Some(Type::None) => Operand::Bool(true),
            Some(Type::Optional(_)) => {
                let dest = self.temporary(Type::Bool);
                self.emit(InstKind::IsNone { dest, value: value.clone() }, span);
                Operand::Local(dest)
            }
            _ => Operand::Bool(false),
        };
        if op == BinOp::Is {
            return Ok((test, value));
        }
        let dest = self.temporary(Type::Bool);
        self.emit(InstKind::Unary { dest, op: UnaryOp::Not, value: test }, span);
        Ok((Operand::Local(dest), value))
    }

    /// `value else default` for what it does, where `value` gives nothing back when it doesn't
    /// fail: the default runs only when it fails.
    pub(super) fn else_effect(&mut self, value: &Expr, default: &Expr, span: Span) -> Lowered<()> {
        let value = self.expr(value)?;
        let failed = self.temporary(Type::Bool);
        self.emit(InstKind::IsErr { dest: failed, value: value.clone() }, span);
        let (fallback, join) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond: Operand::Local(failed), then: fallback, otherwise: join });
        self.current = fallback;
        if default.else_block().is_some() {
            self.else_block(default, &value, Tail::Drop, span)?;
        } else {
            self.effect(default)?;
        }
        if self.is_open() {
            self.finish(Terminator::Jump(join));
        }
        self.current = join;
        Ok(())
    }

    /// The block of an `else`, which `else as failure:` runs with what failed. It leaves, or its
    /// last line goes where `tail` says.
    fn else_block(&mut self, default: &Expr, value: &Operand, tail: Tail, span: Span) -> Lowered<()> {
        let Some((failure, block)) = default.else_block() else {
            return self.tail_expr(default, tail);
        };
        self.scopes.push(std::collections::HashMap::new());
        if let Some(failure) = failure {
            let Some(Type::Result(_, error)) = type_in(&self.locals, value) else {
                return Err("it names the failure of something that can't fail".to_owned());
            };
            let held = self.declare(failure, *error);
            self.emit(InstKind::GetErr { dest: held, value: value.clone() }, span);
        }
        let lowered = self.stmts(block, tail);
        self.scopes.pop();
        lowered
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
        if default.leaves() {
            self.else_block(default, &value, Tail::Drop, span)?;
        } else if default.else_block().is_some() {
            self.else_block(default, &value, Tail::Into(dest), span)?;
        } else {
            let lowered = self.expr(default)?;
            let lowered = self.coerce(lowered, &ty, default.span)?;
            self.emit(InstKind::Copy { dest, value: lowered }, span);
        }
        if self.is_open() {
            self.finish(Terminator::Jump(join));
        }
        self.current = present;
        // `None else default`, as a `with` source's parameter is when its default is written in,
        // never gets here: it's always the default.
        if held == Type::None {
            self.finish(Terminator::Jump(join));
            self.current = join;
            return Ok(Operand::Local(dest));
        }
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
            _ => return Err("it uses `else` on a result of nothing".to_owned()),
        };
        let inside = self.coerce(inside, &ty, span)?;
        self.emit(InstKind::Copy { dest, value: inside }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// Leaves the function with an error, as any way of leaving does: `for mut` loops write their
    /// items back, and `with` blocks clean up.
    pub(super) fn return_error(&mut self, error: Operand, span: Span) -> Lowered<()> {
        // A generator a loop lets go of on the way out may fail closing: that goes on this
        // failure as a note, so it's kept where a note can be added before it leaves.
        let ty = type_in(&self.locals, &error).ok_or("a value has no type")?;
        if self.leaving.iter().any(|entered| matches!(entered, Leaving::Generator { .. }))
            && self.takes_notes(&ty)
        {
            let held = self.temporary(ty);
            self.emit(InstKind::Copy { dest: held, value: error }, span);
            let (out, with) = (self.failing_out, self.failing_with);
            self.failing_out = true;
            self.failing_with = Some(held);
            let undone = self.undo_since(0, span);
            self.failing_out = out;
            self.failing_with = with;
            undone?;
            let dest = self.temporary(self.returns.clone());
            self.emit(InstKind::MakeErr { dest, error: Operand::Local(held) }, span);
            self.finish(Terminator::Return(Some(Operand::Local(dest))));
            return Ok(());
        }
        let dest = self.temporary(self.returns.clone());
        self.emit(InstKind::MakeErr { dest, error }, span);
        self.failing_out = true;
        let left = self.leave(Some(Operand::Local(dest)), span);
        self.failing_out = false;
        left
    }

    /// Whether a failure of this type carries notes: an error, with cases or without, or a union
    /// of those.
    fn takes_notes(&self, ty: &Type) -> bool {
        let signatures = &self.context.signatures;
        let error = |name: &str| signatures.error_type(name).is_some();
        match ty {
            Type::Named(name) => error(name),
            Type::Errors(members) => members.iter().all(|member| error(member)),
            _ => false,
        }
    }

    /// Leaves with a failure that came from inside what's being called: the `with` block
    /// around it runs its cleanup first, which ends by failing.
    pub(super) fn pass_up_error(&mut self, error: Operand, span: Span) -> Lowered<()> {
        let Some(around) = self.on_failure.pop() else { return self.return_error(error, span) };
        let rounds = std::mem::take(&mut self.plain_rounds);
        self.scopes.push(std::collections::HashMap::new());
        let ty = match &self.returns {
            Type::Result(_, failure) => (**failure).clone(),
            other => other.clone(),
        };
        let held = self.declare(&around.name, ty);
        self.emit(InstKind::Copy { dest: held, value: error }, span);
        let lowered = self.stmts(&around.cleanup, Tail::Drop);
        self.scopes.pop();
        self.plain_rounds = rounds;
        self.on_failure.push(around);
        lowered
    }

    /// Leaves with a failure written in this function: from a `parallel` loop's round that's
    /// lowered as an ordinary loop it's one whose block hasn't run its cleanup yet.
    fn leave_with_error(&mut self, error: Operand, span: Span) -> Lowered<()> {
        if self.plain_rounds > 0 {
            return self.pass_up_error(error, span);
        }
        self.return_error(error, span)
    }

    /// `fail error`.
    pub(super) fn fail(&mut self, error: &Expr, span: Span) -> Lowered<()> {
        let error = self.expr(error)?;
        let error = self.traced(error, "error.raised", span)?;
        self.leave_with_error(error, span)
    }

    /// An error leaving here, by `fail` (`error.raised`) or `?` (`error.passed`), with this place
    /// put in its call chain, which goes under it if it ends the program. Only an error, which
    /// has notes to keep it in, carries one.
    fn traced(&mut self, error: Operand, how: &str, span: Span) -> Lowered<Operand> {
        let ty = type_in(&self.locals, &error).ok_or("a value has no type")?;
        if !self.takes_notes(&ty) {
            return Ok(error);
        }
        let held = match error {
            Operand::Local(local) => local,
            other => {
                let held = self.temporary(ty);
                self.emit(InstKind::Copy { dest: held, value: other }, span);
                held
            }
        };
        let args = vec![Operand::Local(held)];
        self.emit(InstKind::Builtin { dest: None, name: how.to_owned(), args, changes: true }, span);
        Ok(Operand::Local(held))
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
        let passed = self.traced(passed, "error.passed", span)?;
        self.leave_with_error(passed, span)?;
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
        if self.generating.is_some() || matches!(self.type_at(expr.span), Some(Type::Unit)) {
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
            Pattern::Variant { name, dotted: false, args, .. } if name == "Err" => (true, args.first()),
            Pattern::Variant { name, dotted: false, args, .. } if name == "Ok" => (false, args.first()),
            Pattern::Variant { name, dotted: false, .. } if self.failure_side(name, error) => {
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
            if matches!(inner, Pattern::Wildcard) {
                return Ok(());
            }
            return Err("it matches what a result of nothing holds".to_owned());
        }
        let dest = self.temporary(ok.clone());
        self.emit(InstKind::GetOk { dest, value: value.clone() }, span);
        self.pattern(inner, &Operand::Local(dest), ok, fail, span)
    }

    /// Whether an undotted name in a `case` over a result is the failure's, as the checker reads it:
    /// an error type, or a case of one it fails with.
    fn failure_side(&self, name: &str, error: &Type) -> bool {
        let failing = match error {
            Type::Named(one) => vec![one.as_str()],
            Type::Errors(names) => names.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        };
        self.context.signatures.error_type(name).is_some()
            || failing.into_iter().any(|owner| {
                self.context
                    .enums
                    .get(owner)
                    .is_some_and(|found| found.cases.iter().any(|case| case.name == name))
            })
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

/// If a backend asks, makes `main` end the program when it fails, rather than give a result back,
/// so that it gives back nothing. Only a backend knows how to say what failed, as its text is
/// worked out then. The functions `fennec test` makes of tests, named `test:` and their number,
/// end the same way, so a test's failure says what failed as `main`'s does. A property test's
/// body, `test:` and its number and `_body`, is called with `?` for each input, so it doesn't.
pub(super) fn stop_on_failed_main(program: &mut Program, defs: &[(String, &FnDef)], options: Options) {
    if !options.stop_on_failed_main {
        return;
    }
    let stops = |name: &str| {
        name == "main"
            || name.strip_prefix("test:").is_some_and(|number| number.chars().all(|c| c.is_ascii_digit()))
    };
    for (key, def) in defs.iter().filter(|(key, _)| stops(key)) {
        if let Some(function) = program.functions.iter_mut().find(|function| function.name == *key) {
            stop_on_failure(function, def.span);
        }
    }
}

/// Makes `function`, which gives back a result with nothing in its `Ok`, stop when it fails.
fn stop_on_failure(function: &mut Function, span: Span) {
    let Type::Result(ok, error) = function.returns.clone() else { return };
    if *ok != Type::Unit {
        return;
    }
    function.returns = Type::Unit;
    let inst = |kind| Inst { kind, span };
    let mut added = Vec::new();
    let first = function.blocks.len();
    for block in &mut function.blocks {
        let Terminator::Return(Some(value)) = block.end.clone() else { continue };
        let (failed, cause) = (LocalId(function.locals.len()), LocalId(function.locals.len() + 1));
        function.locals.push(Local { name: None, ty: Type::Bool, copied: false });
        function.locals.push(Local { name: None, ty: (*error).clone(), copied: false });
        let (then, otherwise) = (BlockId(first + added.len()), BlockId(first + added.len() + 1));
        block.insts.push(inst(InstKind::IsErr { dest: failed, value: value.clone() }));
        block.end = Terminator::Branch { cond: Operand::Local(failed), then, otherwise };
        let says = InstKind::Builtin {
            dest: None,
            name: "main.failed".to_owned(),
            args: vec![Operand::Local(cause)],
            changes: false,
        };
        added.push(Block {
            insts: vec![inst(InstKind::GetErr { dest: cause, value }), inst(says)],
            // The text above is what stops it, and what a backend's checks see as stopping.
            end: Terminator::Stop { message: "`main` failed".to_owned(), span },
        });
        added.push(Block { insts: Vec::new(), end: Terminator::Return(None) });
    }
    function.blocks.extend(added);
}

/// The `f32` nearest a float, as the float it is exactly.
#[expect(clippy::cast_possible_truncation, reason = "rounding to the nearest f32 is what's meant")]
pub(crate) fn nearest_f32(value: f64) -> f64 {
    f64::from(value as f32)
}
