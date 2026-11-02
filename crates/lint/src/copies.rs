//! `needless-copy`: an explicit `copy(x)` that changes nothing, because nothing changes `x` or
//! the copy while both are in use, so they could share one value. Nothing the copy could share
//! may be changed from its statement on, counting the whole of any loop it's in and any method
//! call (which ones change a value isn't known here), so nothing would be copied while the
//! program runs. And it's tried: with it taken out, the program has to check, so no stale copy
//! can be read.
//!
//! A `copy` where a place is named is left alone: the whole value of `mut x = ...`, a `mut`
//! argument, or what `for mut` loops over, or the start of a path that is. There, taking it out
//! would make the name stand for the place, so changing it would change the original, which a
//! program may well do on purpose. Anywhere else, a copy never changes what a program computes,
//! so the fix is safe for `fennec fix`.

use std::collections::HashSet;

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Item, Module, Sources, Span, Stmt, StmtKind};

use crate::Lint;
use crate::dead::changed_in;
use crate::rewrites::written;
use crate::sizes::{blocks, end_of, functions};

/// A call to the builtin `copy` with one value, as `(call, value)`.
fn copy_call(expr: &Expr) -> Option<(Span, &Expr)> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Name(name) = &callee.kind else { return None };
    let [arg] = args.as_slice() else { return None };
    (name == "copy" && arg.name.is_none() && !arg.is_mut).then_some((expr.span, &arg.value))
}

/// Whether the program gives `copy` a meaning of its own: a function, or a name some function
/// binds. Then a call to it isn't the builtin's.
fn copy_is_redefined(module: &Module) -> bool {
    let defined = module.items.iter().any(|item| matches!(item, Item::Function(def) if def.name == "copy"));
    defined
        || functions(module)
            .iter()
            .any(|def| def.params.iter().any(|param| param.name == "copy") || binds_copy(&def.body))
}

fn binds_copy(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| {
        let assigned = matches!(&stmt.kind, StmtKind::Assign { target, .. }
            if matches!(&target.kind, ExprKind::Name(name) if name == "copy"));
        let looped =
            matches!(&stmt.kind, StmtKind::For { names, .. } if names.iter().any(|name| name == "copy"));
        assigned || looped || blocks(stmt).into_iter().any(binds_copy)
    })
}

/// The start of a path: `copy(x)` in `copy(x).stats[0]`.
fn root(expr: &Expr) -> &Expr {
    match &expr.kind {
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => root(object),
        _ => expr,
    }
}

/// Where a place is named, by the span of the start of its path: what `mut x = ...` declares,
/// what `for mut` loops over, and each `mut` argument.
fn places(module: &Module) -> HashSet<(u32, u32)> {
    fn block(stmts: &[Stmt], found: &mut HashSet<(u32, u32)>) {
        for stmt in stmts {
            let named = match &stmt.kind {
                StmtKind::Assign { value, declare_mut: true, .. } => Some(value),
                StmtKind::For { is_mut: true, iter, .. } => Some(iter),
                _ => None,
            };
            if let Some(value) = named {
                found.insert((root(value).span.start, root(value).span.end));
            }
            for inner in blocks(stmt) {
                block(inner, found);
            }
        }
    }
    let mut found = HashSet::new();
    for def in functions(module) {
        block(&def.body, &mut found);
    }
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        if let ExprKind::Call { args, .. } = &expr.kind {
            for arg in args.iter().filter(|arg| arg.is_mut) {
                found.insert((root(&arg.value).span.start, root(&arg.value).span.end));
            }
        }
    });
    found
}

/// The names a copied value could share: the start of a path, or every name in anything else,
/// since a call can give back what it's given. A literal shares nothing.
fn shared(value: &Expr) -> Vec<String> {
    if let ExprKind::Name(name) = &root(value).kind {
        return vec![name.clone()];
    }
    let mut names = Vec::new();
    let mut walked = FnDef {
        name: String::new(),
        generics: Vec::new(),
        params: Vec::new(),
        ret: None,
        arrow: None,
        body: vec![Stmt { kind: StmtKind::Expr(value.clone()), span: value.span }],
        span: value.span,
    };
    fennec_syntax::visit::in_function(&mut walked, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind {
            names.push(name.clone());
        }
    });
    names
}

/// Whether anything a copy at `call` could share may change from its statement on: in the
/// innermost function it's in, the statement in the function's own body that holds it, and
/// every one after.
fn changes_later(module: &Module, call: Span, value: &Expr) -> bool {
    let names = shared(value);
    if names.is_empty() {
        return false;
    }
    let holds = |stmt: &Stmt| stmt.span.start <= call.start && call.end <= end_of(std::slice::from_ref(stmt));
    let innermost = functions(module).into_iter().rev().find(|def| def.body.iter().any(holds));
    let Some(def) = innermost else { return true };
    let Some(at) = def.body.iter().position(holds) else { return true };
    let changed = changed_in(def.body.get(at..).unwrap_or_default(), &def.body);
    names.iter().any(|name| changed.contains(name))
}

/// Whether the module still checks.
fn checks(module: &Module) -> bool {
    fennec_check::check(module).is_ok()
}

/// The module with the call at `call` replaced by what it copies.
fn without(module: &Module, call: Span) -> Module {
    let mut trial = module.clone();
    fennec_syntax::visit::exprs_mut(&mut trial, &mut |expr| {
        if expr.span == call
            && let Some((_, value)) = copy_call(expr)
        {
            *expr = value.clone();
        }
    });
    trial
}

/// The fix that takes `copy(` and `)` away, when the text around the value is exactly those.
fn fix(sources: &Sources, call: Span, value: &Expr) -> Option<Diagnostic> {
    let opening = Span { start: call.start, end: value.span.start };
    let closing = Span { start: value.span.end, end: call.end };
    if written(sources, opening)? != "copy(" || written(sources, closing)? != ")" {
        return None;
    }
    let name = written(sources, value.span)?;
    let message = format!("this copy of `{name}` changes nothing: without it, nothing more is copied");
    Some(
        Diagnostic::new(message, call)
            .with_help(format!("use `{name}` itself, which shares its value until something changes it"))
            .with_fix(opening, "")
            .with_fix(closing, ""),
    )
}

/// Every needless `copy` in a module that checks. Each one found costs a check of the whole
/// program, and programs write few, so it runs only where the whole program is checked anyway.
pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    if copy_is_redefined(module) {
        return Vec::new();
    }
    let mut calls = Vec::new();
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        if let Some((call, value)) = copy_call(expr)
            && !call.in_prelude()
        {
            calls.push((call, value.clone()));
        }
    });
    let places = places(module);
    calls.retain(|(call, _)| !places.contains(&(call.start, call.end)));
    if calls.is_empty() {
        return Vec::new();
    }
    calls
        .into_iter()
        .filter(|(call, value)| !changes_later(module, *call, value))
        .filter_map(|(call, value)| {
            let diagnostic = fix(sources, call, &value)?;
            checks(&without(module, call)).then(|| Lint::new("needless-copy", diagnostic))
        })
        .collect()
}
