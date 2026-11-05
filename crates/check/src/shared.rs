//! `shared Name:` types: a counted handle to one cell, whose methods lock it. Assigning a handle
//! gives another name for the same cell, and a `mut self` method changes the cell, so calling
//! one needs no `mut` name. Anything else that changes a handle's fields would skip the lock.
//! A `shared` value holds plain values and `Id`s, never another `shared`, so handles can't form
//! a cycle, and none lives at the top of a file.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Stmt, WithName};

use crate::checker::Checker;
use crate::function::{FnCx, Origin};
use crate::program::Program;
use crate::stmt::Flow;
use crate::types::{Type, a};

/// Whether a type is a capability, through which all I/O goes.
fn is_capability(name: &str) -> bool {
    matches!(name, "Stdout" | "Fs" | "Clock" | "Args" | "Stdin" | "Process" | "Entropy")
}

/// Whether a type is a capability, by itself.
pub(crate) fn is_capability_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name) if is_capability(name) || name == "Net")
}

impl Program {
    /// Whether a type is `shared`.
    pub(crate) fn is_shared(&self, ty: &Type) -> bool {
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        self.type_info(name).is_some_and(|info| info.def.shared)
    }
}

impl Checker {
    /// Whether a value of this type holds a capability, in it or anything inside it.
    pub(crate) fn holds_capability(&mut self, ty: &Type, open: &mut Vec<String>) -> bool {
        if is_capability_type(ty) {
            return true;
        }
        if ty.children().iter().any(|child| self.holds_capability(&(*child).clone(), open)) {
            return true;
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        if open.contains(name) {
            return false;
        }
        let Some(info) = self.program.type_info(name) else { return false };
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        let mut found = false;
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            if self.holds_capability(&held, open) {
                found = true;
                break;
            }
        }
        open.pop();
        found
    }

    /// The `shared` type a value of this type holds, in it or anything inside it.
    pub(crate) fn shared_inside(&mut self, ty: &Type, open: &mut Vec<String>) -> Option<String> {
        if self.program.is_shared(ty) {
            return ty.type_name().map(str::to_owned);
        }
        for child in ty.children() {
            if let Some(found) = self.shared_inside(&child.clone(), open) {
                return Some(found);
            }
        }
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return None };
        if open.contains(name) {
            return None;
        }
        let info = self.program.type_info(name)?;
        let def = info.def.clone();
        open.push(name.clone());
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        let mut found = None;
        for (case, field) in fields.chain(in_cases) {
            let held = self.field_type(name, case, &field.name);
            found = self.shared_inside(&held, open);
            if found.is_some() {
                break;
            }
        }
        open.pop();
        found
    }

    /// Reports a `shared` type that holds another one.
    pub(crate) fn check_shared_types(&mut self) {
        let defs: Vec<_> = self
            .program
            .types
            .values()
            .filter(|info| info.def.shared && !info.prelude)
            .map(|info| info.def.clone())
            .collect();
        for def in defs {
            for field in &def.fields {
                let held = self.field_type(&def.name, None, &field.name);
                if let Some(inner) = self.shared_inside(&held, &mut vec![def.name.clone()]) {
                    let message = format!(
                        "`{}` is `shared`, so it can't hold `{inner}`, which is `shared` too",
                        def.name
                    );
                    self.diagnostics.push(Diagnostic::new(message, field.span).with_help(
                        "a `shared` value holds plain values and `Id`s: keep the other handle where both are used, or hold an `Id`",
                    ));
                }
            }
        }
    }
}

/// Why a lock body can't hand on what holds a handle, and what to do instead.
const RETAKES: &str =
    "the call could lock the handle again: do that work here, through the locked name, or after the block";

impl FnCx<'_> {
    /// `with value as name:`, or `with a, b as mut x, mut y:`. Several values are `shared`
    /// handles locked together, and a handle is always named: the block changes it through that
    /// name, which holds its lock, to write when it's `mut`.
    pub(crate) fn with_block(&mut self, value: &Expr, names: &[WithName], body: &[Stmt], used: bool) -> Flow {
        let ty = self.expr(value);
        let (items, types) = match (&value.kind, ty) {
            (ExprKind::Tuple(items), Type::Tuple(types)) if names.len() != 1 => {
                (items.iter().collect(), types)
            }
            (_, ty) => (vec![value], vec![ty]),
        };
        for (item, ty) in items.iter().zip(&types) {
            let shared = self.checker.program.is_shared(ty);
            if items.len() > 1 && !shared && *ty != Type::Unknown {
                let message = format!(
                    "a `with` holds several values only to lock `shared` handles, but this is {}",
                    a(ty)
                );
                self.error(
                    Diagnostic::new(message, item.span)
                        .with_help("nest one `with` inside another, one value each"),
                );
            }
            if shared && names.is_empty() {
                let written = fennec_syntax::dotted(item).unwrap_or_else(|| "it".to_owned());
                self.error(
                    Diagnostic::new(format!("this `with` locks {}, so it needs a name for it", a(ty)), item.span)
                        .with_help(format!(
                            "name it after `as`, like `with {written} as mut locked:`, and change it through that name"
                        )),
                );
            }
        }
        let locked: Vec<&WithName> = names
            .iter()
            .zip(&types)
            .filter(|(_, ty)| self.checker.program.is_shared(ty))
            .map(|(held, _)| held)
            .collect();
        if let (Some(held), Some(first)) = (
            self.lock.clone(),
            items
                .iter()
                .find(|item| self.recorded(item.span).is_some_and(|ty| self.checker.program.is_shared(&ty))),
        ) {
            self.error(
                Diagnostic::new(format!("this `with` takes another lock {held}"), first.span).with_help(
                    "locks never nest: lock them together in one `with`, like `with a, b as mut x, mut y:`",
                ),
            );
        }
        let outer = if locked.is_empty() {
            self.lock.clone()
        } else {
            let names: Vec<String> = locked.iter().map(|held| format!("`{}`", held.name)).collect();
            let (names, verb) = match names.as_slice() {
                [one] => (one.clone(), "is"),
                [rest @ .., last] => (format!("{} and {last}", rest.join(", ")), "are"),
                [] => (String::new(), "is"),
            };
            self.lock.replace(format!("while {names} {verb} locked"))
        };
        self.push(false);
        for (held, ty) in names.iter().zip(types) {
            let origin = if self.checker.program.is_shared(&ty) { Origin::Locked } else { Origin::Local };
            self.declare_as(&held.name, ty, held.is_mut, origin, held.span);
        }
        let flow = self.stmts(body, used);
        self.pop();
        self.lock = outer;
        flow
    }

    /// Whether an expression is a name that holds its handle's lock already: `self` in a
    /// `shared` type's method, or a name a `with` block gave a handle it locked.
    fn holds_lock(&self, expr: &Expr) -> bool {
        matches!(&expr.kind, ExprKind::Name(name) if name == "self" || self.origin(name) == Some(Origin::Locked))
    }

    /// Reports taking a `shared` handle's lock inside a lock body, which could wait forever on a
    /// lock another task holds while it waits for this one. `what` takes it: "calling `add`".
    pub(crate) fn locks_again(&mut self, object: &Expr, ty: &Type, what: &str) {
        let Some(held) = self.lock.clone() else { return };
        if !self.checker.program.is_shared(ty) || self.holds_lock(object) {
            return;
        }
        let handle = fennec_syntax::dotted(object).unwrap_or_else(|| "a handle".to_owned());
        self.error(Diagnostic::new(format!("{what} locks `{handle}` {held}"), object.span).with_help(
            "locks never nest: lock them together in one `with`, like `with a, b as mut x, mut y:`, and use the names it gives",
        ));
    }

    /// Reports a `shared` handle passed to a call inside a lock body, which could lock it again:
    /// on its own, inside a value or a list, or captured by a function.
    pub(crate) fn handle_passed(&mut self, arg: &Expr, ty: &Type) {
        let Some(held) = self.lock.clone() else { return };
        if self.is_handle_closure(arg) {
            let name = fennec_syntax::dotted(arg).unwrap_or_default();
            self.error(
                Diagnostic::new(
                    format!("`{name}` holds a `shared` handle, so it can't be passed to a call {held}"),
                    arg.span,
                )
                .with_help(RETAKES),
            );
            return;
        }
        if !self.checker.program.is_shared(ty) {
            if let Some(inner) = self.checker.shared_inside(ty, &mut Vec::new()) {
                let message =
                    format!("{} holds a `shared` `{inner}`, so it can't be passed to a call {held}", a(ty));
                self.error(Diagnostic::new(message, arg.span).with_help(RETAKES));
            }
            return;
        }
        self.error(
            Diagnostic::new(format!("a `shared` handle can't be passed to a call {held}"), arg.span)
                .with_help("the call could lock it again: do that work here, through the locked name, or after the block"),
        );
    }

    /// Reports a method called on a value that holds a `shared` handle inside a lock body, since
    /// the method could lock it. `object` itself being a handle is `locks_again`'s.
    pub(crate) fn method_on_holder(&mut self, object: &Expr, ty: &Type, method: &str) {
        let Some(held) = self.lock.clone() else { return };
        if self.checker.program.is_shared(ty) {
            return;
        }
        if let Some(inner) = self.checker.shared_inside(ty, &mut Vec::new()) {
            let message = format!("calling `{method}` on {} that holds a `shared` `{inner}` {held}", a(ty));
            self.error(Diagnostic::new(message, object.span).with_help(RETAKES));
        }
    }

    /// Remembers a function that captures a `shared` handle, by its name: an arrow function given
    /// to a name, or a nested function. `reads` is what its body reads.
    pub(crate) fn note_closure(&mut self, name: &str, reads: &[String], own: &[String]) {
        let holds = reads.iter().filter(|read| !own.contains(read)).any(|read| {
            self.handle_closures.contains(read)
                || self
                    .lookup(read)
                    .is_some_and(|(ty, _)| self.checker.shared_inside(&ty, &mut Vec::new()).is_some())
        });
        if holds {
            self.handle_closures.insert(name.to_owned());
        }
    }

    fn is_handle_closure(&self, expr: &Expr) -> bool {
        matches!(&expr.kind, ExprKind::Name(name) if self.handle_closures.contains(name))
    }

    /// Reports a call inside a lock body to a function that captures a `shared` handle.
    pub(crate) fn closure_locks_again(&mut self, callee: &Expr) {
        let Some(held) = self.lock.clone() else { return };
        if self.is_handle_closure(callee) {
            let name = fennec_syntax::dotted(callee).unwrap_or_default();
            let message = format!("calling `{name}`, which captures a `shared` handle, could lock it {held}");
            self.error(Diagnostic::new(message, callee.span).with_help(RETAKES));
        }
    }

    /// Reports what a lock body can't do: I/O, `?`, `fail` and `yield`. Each could leave the lock
    /// held for long, or leave what it guards half changed.
    pub(crate) fn not_under_lock(&mut self, what: &str, span: fennec_syntax::Span, help: &str) {
        if let Some(held) = self.lock.clone() {
            self.error(Diagnostic::new(format!("{what} {held}"), span).with_help(help));
        }
    }

    /// Reports a capability used inside a lock body: everything it does is I/O.
    pub(crate) fn io_under_lock(&mut self, expr: &Expr, ty: &Type) {
        let (ExprKind::Name(name), Type::Named(capability)) = (&expr.kind, ty) else { return };
        if is_capability(capability) {
            self.not_under_lock(
                &format!("`{name}` does I/O, which can't happen"),
                expr.span,
                "do the I/O before or after the lock, with what the block works out",
            );
        }
    }

    /// The `shared` value a place changes through without one of its methods, which would skip
    /// its lock: `cache._pages` outside `Cache`'s methods. Its own methods change it through
    /// `self`.
    pub(crate) fn through_shared(&mut self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(name) if name != "self" && self.origin(name) != Some(Origin::Locked) => {
                let (ty, _) = self.lookup(name)?;
                self.checker.program.is_shared(&ty).then(|| name.clone())
            }
            ExprKind::Attr { object, name, .. } => {
                if let Some(found) = self.through_shared(object) {
                    return Some(found);
                }
                let owner = self.declared_type_of(object)?;
                let field = self.checker.field_type(owner.type_name()?, None, name);
                self.checker
                    .program
                    .is_shared(&field)
                    .then(|| fennec_syntax::dotted(expr).unwrap_or_default())
            }
            ExprKind::Index { object, .. } => self.through_shared(object),
            _ => None,
        }
    }

    /// The type of a chain of names and fields, from their declarations.
    fn declared_type_of(&mut self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Name(name) => self.lookup(name).map(|(ty, _)| ty),
            ExprKind::Attr { object, name, .. } => {
                let owner = self.declared_type_of(object)?;
                Some(self.checker.field_type(owner.type_name()?, None, name))
            }
            _ => None,
        }
    }
}
