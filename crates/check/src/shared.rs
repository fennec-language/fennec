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

impl Program {
    /// Whether a type is `shared`.
    pub(crate) fn is_shared(&self, ty: &Type) -> bool {
        let (Type::Named(name) | Type::Generic(name, _)) = ty else { return false };
        self.type_info(name).is_some_and(|info| info.def.shared)
    }
}

impl Checker {
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
        self.push(false);
        for (held, ty) in names.iter().zip(types) {
            let origin = if self.checker.program.is_shared(&ty) { Origin::Locked } else { Origin::Local };
            self.declare_as(&held.name, ty, held.is_mut, origin, held.span);
        }
        let flow = self.stmts(body, used);
        self.pop();
        flow
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
