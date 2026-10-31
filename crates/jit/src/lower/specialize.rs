//! Lowering a call for what it's passed. A function taking an arrow function, or a native
//! value where it's generic, is lowered again right at the call, with the arrow or the type
//! known: `change(value)` becomes the arrow's body, and `shape.area()` a direct call. A
//! name holding an arrow needs nothing at run time, unless something other than a call uses
//! it, which makes the arrow through the interpreter then.
//!
//! This is inlining, so it stops at recursion and at bodies past a size, and those calls go
//! through the interpreter's function values as before.

use std::rc::Rc;

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::{InstBuilder, Value};
use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FnDef, Span, StmtKind, TypeExpr};

use super::expr::Lowered;
use super::fallback::{expr_names, stmt_names};
use super::hoist::{each_stmt, each_stmt_expr};
use super::inline::{MAX_DEPTH, without_returns};
use super::{Lowerer, internal};
use crate::types::{Function, Ty};

/// How many expressions a body may have and still be lowered again at a call.
const MOST_EXPRS: usize = 200;

/// What a name holding a function stands for.
#[derive(Clone)]
pub(super) struct Known {
    arrow: Expr,
    /// How much of the lowering was open where the arrow is written: its body sees that.
    scopes: usize,
    inlining: Vec<(String, usize)>,
    call_site: Option<Span>,
    origin: Option<(Value, i64)>,
}

/// The body to lower at calls that pass an arrow or a native value to a generic parameter,
/// if this function can be.
pub(super) fn specializable(function: &Function<'_>) -> Option<Rc<FnDef>> {
    if function.fallible || function.params.iter().any(|param| param.is_mut) {
        return None;
    }
    let body = if matches!(function.ret, Ty::Struct(_)) {
        function.def.body.clone()
    } else {
        without_returns(&function.def.body)
    };
    let (mut exprs, mut simple) = (0, true);
    for stmt in &body {
        each_stmt(stmt, &mut |stmt| {
            simple &= !matches!(stmt.kind, StmtKind::Return(_) | StmtKind::Def(_) | StmtKind::Yield(_));
        });
        each_stmt_expr(stmt, &mut |_| exprs += 1);
    }
    (simple && exprs <= MOST_EXPRS).then(|| Rc::new(FnDef { body, ..function.def.clone() }))
}

/// Whether a written type mentions one of a function's type parameters.
fn mentions(written: &TypeExpr, generics: &[String]) -> bool {
    generics.contains(&written.name) || written.args.iter().any(|arg| mentions(arg, generics))
}

impl Lowerer<'_, '_> {
    /// What a name holding a function stands for, if it's known here.
    fn known_function(&self, expr: &Expr) -> Option<u32> {
        let ExprKind::Name(name) = &expr.kind else { return None };
        match self.local(name)?.ty {
            Ty::Known(at) => Some(at),
            _ => None,
        }
    }

    /// Lowers a call to `key` again for what it's passed, when that helps and it can.
    pub(super) fn specialized_call(
        &mut self,
        key: &str,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let Some(compiled) = self.shared.compiled.get(key) else { return Ok(None) };
        let Some(def) = compiled.specialized.clone() else { return Ok(None) };
        let (params, ret) = (compiled.params.clone(), compiled.ret);
        if self.function.key == key
            || self.inlining.len() >= MAX_DEPTH
            || self.inlining.iter().any(|(open, _)| open == key)
            || params.len() != args.len()
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
        {
            return Ok(None);
        }
        let generics: Vec<String> = def.generics.iter().map(|generic| generic.name.clone()).collect();
        let helps = args.iter().zip(&def.params).any(|(arg, param)| {
            matches!(arg.value.kind, ExprKind::Lambda { .. })
                || self.known_function(&arg.value).is_some()
                || (param.ty.as_ref().is_some_and(|ty| mentions(ty, &generics)) && self.is_native(&arg.value))
        });
        if !helps || self.names_collide(&def, args) {
            return Ok(None);
        }
        let mut lowered = Vec::with_capacity(args.len());
        for (arg, (_, to, _)) in args.iter().zip(&params) {
            lowered.push(self.specialized_arg(&arg.value, *to)?);
        }
        self.inlining.push((key.to_owned(), self.scopes.len()));
        let outer_site = self.call_site;
        let outer_origin = self.enter_origin(key, &def);
        if self.call_site.is_none() && !span.in_prelude() {
            self.call_site = Some(span);
        }
        self.open_scope();
        let mut bound = Ok(());
        for ((name, _, _), value) in params.iter().zip(&lowered) {
            bound = bound.and_then(|()| self.bind_lent(name, *value));
        }
        let result = bound.and_then(|()| self.keeping_temps(|lowerer| lowerer.tail(&def.body)));
        let closed = self.close_scope();
        self.inlining.pop();
        self.call_site = outer_site;
        self.inlined_origin = outer_origin;
        let (value, ty) = result?;
        closed?;
        let names_one = def.body.last().is_some_and(|stmt| {
            matches!(&stmt.kind, StmtKind::Expr(expr) if matches!(expr.kind, ExprKind::Name(_) | ExprKind::Attr { .. }))
        });
        let made = match (ret, ty, value) {
            (Ty::Unit | Ty::Cap(_), _, _) => (None, Ty::Unit),
            (Ty::Struct(_) | Ty::Obj, Ty::Struct(id), Some(value)) if names_one => {
                (Some(self.clone_struct(value, id)?), Ty::Struct(id))
            }
            (Ty::Obj, ty, value) => (value, ty),
            (ret, ty, value) => (Some(self.convert(value, ty, ret, span)?), ret),
        };
        for (value, ty) in lowered {
            self.consume(value, ty)?;
        }
        Ok(Some(made))
    }

    /// Whether native code holds an argument by itself, so its type's known here.
    fn is_native(&self, arg: &Expr) -> bool {
        self.static_ty(arg).or_else(|| self.checked_expr(arg)).is_some_and(|ty| ty != Ty::Obj)
    }

    /// Whether lowering a body here would confuse names in the runtime's frame, which the
    /// caller and the callee share: a name the callee mentions that the caller holds as an
    /// object, or one an arrow captures that the callee mentions too.
    fn names_collide(&self, def: &FnDef, args: &[Arg]) -> bool {
        let mut mentioned = Vec::new();
        for stmt in &def.body {
            stmt_names(stmt, &mut mentioned);
        }
        let held = mentioned.iter().any(|name| self.local(name).is_some_and(|local| local.ty == Ty::Obj));
        held || args.iter().any(|arg| {
            let ExprKind::Lambda { params, body } = &arg.value.kind else { return false };
            let mut captured = Vec::new();
            expr_names(body, &mut captured);
            captured.iter().any(|name| !params.contains(name) && mentioned.contains(name))
        })
    }

    /// An argument as the parameter it's for will hold it.
    fn specialized_arg(&mut self, arg: &Expr, to: Ty) -> Result<Lowered, Diagnostic> {
        if matches!(arg.kind, ExprKind::Lambda { .. }) {
            let at = u32::try_from(self.known.len()).unwrap_or(0);
            self.known.push(Known {
                arrow: arg.clone(),
                scopes: self.scopes.len(),
                inlining: self.inlining.clone(),
                call_site: self.call_site,
                origin: self.inlined_origin,
            });
            return Ok((None, Ty::Known(at)));
        }
        if let Some(at) = self.known_function(arg) {
            return Ok((None, Ty::Known(at)));
        }
        if matches!(to, Ty::Cap(_)) {
            return self.expr(arg);
        }
        let (value, ty) = self.expr(arg)?;
        if to == Ty::Obj || to == ty {
            return Ok((value, ty));
        }
        Ok((Some(self.convert(value, ty, to, arg.span)?), to))
    }

    /// Binds a name to a value the caller keeps alive for as long as the name lasts.
    pub(super) fn bind_lent(&mut self, name: &str, (value, ty): Lowered) -> Result<(), Diagnostic> {
        if let (Ty::Struct(id), Some(value)) = (ty, value) {
            return self.declare_struct(name, value, id, false);
        }
        if ty == Ty::Obj {
            let handle = value.ok_or_else(|| internal("a lost argument", Span::default()))?;
            return self.declare_object(name, handle, false);
        }
        // Reading the name marks its value as borrowed, and the caller's value mustn't be: the
        // caller lets go of it afterward.
        let value = if ty.counted() { value.map(|value| self.b.ins().iadd_imm_s(value, 0)) } else { value };
        self.bind_native(name, ty, value, false);
        if ty.counted()
            && let Some(var) = self.local(name).and_then(|local| local.var)
        {
            self.borrowed_names.insert(var.index());
        }
        Ok(())
    }

    /// A call to a name holding a known arrow: its body, right here, seeing what was in
    /// scope where it's written.
    pub(super) fn call_known(&mut self, callee: &Expr, args: &[Arg]) -> Result<Option<Lowered>, Diagnostic> {
        let Some(at) = self.known_function(callee) else { return Ok(None) };
        let known = self.known.get(usize::try_from(at).unwrap_or(0)).cloned();
        let known = known.ok_or_else(|| internal("a lost function", callee.span))?;
        let ExprKind::Lambda { params, body } = &known.arrow.kind else { return Ok(None) };
        if params.len() != args.len() || args.iter().any(|arg| arg.name.is_some() || arg.is_mut) {
            return Ok(None);
        }
        let mut lowered = Vec::with_capacity(args.len());
        for arg in args {
            lowered.push(self.expr(&arg.value)?);
        }
        let (result, closed) = self.in_scope_of(&known, |lowerer| {
            lowerer.open_scope();
            let mut bound = Ok(());
            for (name, value) in params.iter().zip(&lowered) {
                bound = bound.and_then(|()| lowerer.bind_lent(name, *value));
            }
            let result = bound
                .and_then(|()| lowerer.expr(body))
                .map(|(value, ty)| (value.map(|value| lowerer.own(value, ty)), ty));
            (result, lowerer.close_scope())
        });
        let made = result?;
        closed?;
        for (value, ty) in lowered {
            self.consume(value, ty)?;
        }
        Ok(Some(made))
    }

    /// Runs `lower` with what's in scope where an arrow is written.
    fn in_scope_of<T>(&mut self, known: &Known, lower: impl FnOnce(&mut Self) -> T) -> T {
        let hidden = self.scopes.split_off(known.scopes.min(self.scopes.len()));
        let inlining = std::mem::replace(&mut self.inlining, known.inlining.clone());
        let site = std::mem::replace(&mut self.call_site, known.call_site);
        let origin = std::mem::replace(&mut self.inlined_origin, known.origin);
        let result = lower(self);
        self.scopes.extend(hidden);
        self.inlining = inlining;
        self.call_site = site;
        self.inlined_origin = origin;
        result
    }

    /// An arrow as an object, for something other than a call to use.
    pub(super) fn known_as_object(&mut self, at: u32, span: Span) -> Result<Value, Diagnostic> {
        let known = self.known.get(usize::try_from(at).unwrap_or(0)).cloned();
        let known = known.ok_or_else(|| internal("a lost function", span))?;
        let (value, ty) = self.in_scope_of(&known, |lowerer| lowerer.fallback(&known.arrow))?;
        self.handle_of(value, ty, span)
    }
}
