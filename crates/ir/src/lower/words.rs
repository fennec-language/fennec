//! An error's words: `message()`, which every error has, printing an error, and the note a new
//! failure carries of the one it was raised for.
//!
//! An error that writes `def message(self) -> str` has those words. One that doesn't has the words
//! printing gave it before it could have any: the failure as it is, like
//! `SellError.OutOfStock(item="apple")`. Nothing is kept for them in the error: they're made when
//! they're asked for. A failure that's one of several errors asks the one it holds, which is
//! tested for, member by member.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, ExprKind, Span};

use super::calls::owner_name;
use super::generics::FunctionInstance;
use super::{Lowered, Lowering};
use crate::{FormatPart, InstKind, LocalId, Operand, Terminator, type_in};

/// The function lowered beside a `main` that can fail, which gives how what it failed with is
/// shown when it ends the program.
pub const MAIN_SHOWN: &str = "main.shown";

impl Lowering<'_> {
    /// Whether a value of this type is a failure, and so is shown by its words: an error, with
    /// cases or without, or one of several.
    pub(super) fn is_failure(&self, ty: &Type) -> bool {
        let signatures = &self.context.signatures;
        match ty {
            Type::Errors(_) => true,
            ty => owner_name(ty).is_some_and(|owner| signatures.error_type(owner).is_some()),
        }
    }

    /// A failure's words: its own `def message`, or else the failure as it prints.
    pub(super) fn words_of(&mut self, error: Operand, span: Span) -> Lowered<Operand> {
        self.by_member(error, span, &|lowering, error, ty, span| {
            let dest = lowering.temporary(Type::Str);
            let made = if let Some(function) = lowering.own_message(&ty) {
                InstKind::Call { dest: Some(dest), function, args: vec![error] }
            } else {
                InstKind::Builtin {
                    dest: Some(dest),
                    name: "str".to_owned(),
                    args: vec![error],
                    changes: false,
                }
            };
            lowering.emit(made, span);
            Ok(Operand::Local(dest))
        })
    }

    /// How a failure that ends the program is shown after "`main` failed: ": its words, with the
    /// failure as it is on the next line, or just the failure, when it has no words of its own.
    fn shown_ending_main(&mut self, error: Operand, span: Span) -> Lowered<Operand> {
        self.by_member(error, span, &|lowering, error, ty, span| {
            let raw = lowering.temporary(Type::Str);
            let (name, args) = ("repr".to_owned(), vec![error.clone()]);
            lowering.emit(InstKind::Builtin { dest: Some(raw), name, args, changes: false }, span);
            let Some(function) = lowering.own_message(&ty) else { return Ok(Operand::Local(raw)) };
            let words = lowering.temporary(Type::Str);
            lowering.emit(InstKind::Call { dest: Some(words), function, args: vec![error] }, span);
            let dest = lowering.temporary(Type::Str);
            let parts = vec![
                FormatPart::Value { value: Operand::Local(words), spec: None },
                FormatPart::Text("\n  ".to_owned()),
                FormatPart::Value { value: Operand::Local(raw), spec: None },
            ];
            lowering.emit(InstKind::Format { dest, parts }, span);
            Ok(Operand::Local(dest))
        })
    }

    /// Lowers `main`'s helper for showing what it failed with (`MAIN_SHOWN`), when it can fail.
    pub(super) fn main_shown(&mut self, error: &Type, span: Span) -> Lowered<()> {
        if !self.is_failure(error) {
            return Ok(());
        }
        let instance = FunctionInstance {
            base: self.base.clone(),
            name: MAIN_SHOWN.to_owned(),
            subst: self.subst.clone(),
        };
        let mut made = Lowering::new(&instance, self.context, self.body, Type::Str);
        let value = made.declare("error", error.clone());
        made.scopes.push(HashMap::new());
        let shown = made.shown_ending_main(Operand::Local(value), span)?;
        made.finish(Terminator::Return(Some(shown)));
        self.lifted.extend(made.finished(1, Vec::new()));
        Ok(())
    }

    /// The function giving an error's own words, when it writes `def message`.
    fn own_message(&self, ty: &Type) -> Option<String> {
        let owner = owner_name(ty)?;
        self.resolve(&format!("{owner}.message"), &[Some(ty.clone())], None).map(|(function, _)| function)
    }

    /// What `each` gives for a failure: for one of several errors, for the one it holds, which is
    /// tested for, member by member, and read as itself.
    fn by_member(
        &mut self,
        error: Operand,
        span: Span,
        each: &dyn Fn(&mut Self, Operand, Type, Span) -> Lowered<Operand>,
    ) -> Lowered<Operand> {
        let ty = type_in(&self.locals, &error).ok_or("a value has no type")?;
        let Type::Errors(members) = &ty else { return each(self, error, ty, span) };
        let dest = self.temporary(Type::Str);
        let join = self.new_block();
        for member in members {
            let is = self.temporary(Type::Bool);
            let type_name = member.clone();
            self.emit(InstKind::IsError { dest: is, value: error.clone(), type_name }, span);
            let (this, next) = (self.new_block(), self.new_block());
            self.finish(Terminator::Branch { cond: Operand::Local(is), then: this, otherwise: next });
            self.current = this;
            let one = Type::Named(member.clone());
            let held = self.temporary(one.clone());
            self.emit(InstKind::AsError { dest: held, value: error.clone() }, span);
            let given = each(self, Operand::Local(held), one, span)?;
            self.emit(InstKind::Copy { dest, value: given }, span);
            self.finish(Terminator::Jump(join));
            self.current = next;
        }
        // It's one of those.
        self.finish(Terminator::Unreachable);
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// `repr(value)`, the value as it shows inside another, where that isn't the backend's to work
    /// out: through its type's own `def str`, as it shows in a list, and `None`. `None` for any other
    /// call.
    pub(super) fn repr_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        keep: Option<&Type>,
        span: Span,
    ) -> Option<Lowered<Option<LocalId>>> {
        let ExprKind::Name(name) = &callee.kind else { return None };
        let [Arg { name: None, value, .. }] = args else { return None };
        if name != "repr" || self.lookup(name).is_some() || self.own_function(name) {
            return None;
        }
        let held = self.type_at(value.span)?;
        let shown = if held == Type::None {
            Some(Ok(Operand::Str("None".to_owned())))
        } else {
            self.own_str(&held).map(|function| {
                let writer = Type::Named("StrWriter".to_owned());
                self.expr(value).and_then(|value| self.own_text_of(value, function, (writer, span)))
            })
        }?;
        Some(shown.map(|shown| {
            let dest = keep.map(|ty| self.temporary(ty.clone()));
            if let Some(dest) = dest {
                self.emit(InstKind::Copy { dest, value: shown }, span);
            }
            dest
        }))
    }

    /// `failure.message()` and `failure.with_note(text)` on an error that doesn't write its own:
    /// the failure's words, and a copy of it carrying one more note. `None` for any other call.
    pub(super) fn errors_own_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        keep: Option<&Type>,
        span: Span,
    ) -> Option<Lowered<Option<LocalId>>> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
        if !matches!(name.as_str(), "message" | "with_note") {
            return None;
        }
        let ty = self.type_at(object.span)?;
        let written = owner_name(&ty)
            .is_some_and(|owner| self.context.signatures.get(&format!("{owner}.{name}")).is_some());
        if !self.is_failure(&ty) || (written && !matches!(ty, Type::Errors(_))) {
            return None;
        }
        Some(self.errors_own(object, name, args, keep, span))
    }

    fn errors_own(
        &mut self,
        object: &Expr,
        name: &str,
        args: &[Arg],
        keep: Option<&Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let error = self.expr(object)?;
        let given = if name == "message" {
            self.words_of(error, span)?
        } else {
            let [Arg { value: text, .. }] = args else { return Err("`with_note` takes one note".to_owned()) };
            let ty = type_in(&self.locals, &error).ok_or("a value has no type")?;
            // A copy, which the note goes on: the failure it's made from keeps the notes it had.
            let copy = self.temporary(ty);
            self.emit(InstKind::Copy { dest: copy, value: error }, span);
            let text = self.expr(text)?;
            let (name, args) = ("error.add_note".to_owned(), vec![Operand::Local(copy), text]);
            self.emit(InstKind::Builtin { dest: None, name, args, changes: true }, span);
            Operand::Local(copy)
        };
        let Some(ty) = keep else { return Ok(None) };
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Copy { dest, value: given }, span);
        Ok(Some(dest))
    }

    /// A new failure raised inside `except why:` carries `why`'s words as a note, as Python's
    /// `raise ... from why` keeps its cause. One that was made already, as `why`, a copy of it, or
    /// `why.with_note(...)`, carries what it had.
    pub(super) fn with_cause_noted(&mut self, raised: &Expr, error: Operand, span: Span) -> Lowered<Operand> {
        let Some((_, why)) = self.excepts.last().cloned() else { return Ok(error) };
        if raised.made_already() {
            return Ok(error);
        }
        let ty = type_in(&self.locals, &error).ok_or("a value has no type")?;
        if !self.is_failure(&ty) {
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
        let words = self.words_of(Operand::Local(why), span)?;
        let (note, args) = ("error.add_note".to_owned(), vec![Operand::Local(held), words]);
        self.emit(InstKind::Builtin { dest: None, name: note, args, changes: true }, span);
        Ok(Operand::Local(held))
    }
}
