//! Not for landing: the `ref` decisions' rules, reported so they can be measured over every program
//! before any of them is turned on. A value left unused, and a change to something that isn't `mut`.

use std::collections::HashSet;

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Stmt, StmtKind};

use crate::Body;
use crate::builtins;
use crate::checker::Checker;
use crate::types::Type;

pub(crate) fn check_function(checker: &Checker, def: &FnDef, body: Option<&Body>) -> Vec<Diagnostic> {
    let mut changeable: HashSet<&str> =
        def.params.iter().filter(|param| param.is_mut).map(|param| param.name.as_str()).collect();
    crate::alias::collect_changeable(&def.body, &mut changeable);
    let mut rules = Rules { checker, body, changeable, problems: Vec::new() };
    rules.block(&def.body, true);
    rules.problems
}

pub(crate) fn check_test(checker: &Checker, stmts: &[Stmt], body: Option<&Body>) -> Vec<Diagnostic> {
    let mut changeable = HashSet::new();
    crate::alias::collect_changeable(stmts, &mut changeable);
    let mut rules = Rules { checker, body, changeable, problems: Vec::new() };
    rules.block(stmts, false);
    rules.problems
}

struct Rules<'a> {
    checker: &'a Checker,
    body: Option<&'a Body>,
    changeable: HashSet<&'a str>,
    problems: Vec<Diagnostic>,
}

fn root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. } => root(object),
        _ => None,
    }
}

impl<'a> Rules<'a> {
    /// A block. With `gives`, its last statement is the value of the block.
    fn block(&mut self, stmts: &'a [Stmt], gives: bool) {
        let last = stmts.len().saturating_sub(1);
        for (at, stmt) in stmts.iter().enumerate() {
            self.stmt(stmt, gives && at == last);
        }
    }

    fn stmt(&mut self, stmt: &'a Stmt, gives: bool) {
        match &stmt.kind {
            StmtKind::Expr(value) => {
                if !gives {
                    self.unused(value);
                }
                if let ExprKind::Match { subject, arms } = &value.kind {
                    self.expr(subject);
                    for arm in arms {
                        self.block(&arm.body, gives);
                    }
                } else {
                    self.expr(value);
                }
            }
            StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value)
            | StmtKind::Return(Some(value)) => self.expr(value),
            StmtKind::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.block(body, gives);
                }
                if let Some(body) = otherwise {
                    self.block(body, gives);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.block(body, false);
            }
            StmtKind::For { iter, is_mut, body, .. } => {
                if *is_mut {
                    self.needs_mut(iter, "for mut");
                }
                self.expr(iter);
                self.block(body, false);
            }
            StmtKind::With { value, body, .. } => {
                self.expr(value);
                self.block(body, gives);
            }
            StmtKind::Def(_) | StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    /// A statement that's only an expression, and gives a value nothing keeps.
    fn unused(&mut self, value: &Expr) {
        if matches!(value.kind, ExprKind::Match { .. }) {
            return;
        }
        let Some(ty) = self.body.and_then(|body| body.expr(value.span)) else { return };
        if matches!(ty, Type::Unit | Type::None | Type::Unknown) {
            return;
        }
        let what = match &value.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Attr { object, name, .. } => {
                    let owner = self
                        .body
                        .and_then(|body| body.expr(object.span))
                        .and_then(|ty| ty.type_name().map(str::to_owned).or_else(|| Some(ty.to_string())));
                    let builtin = self
                        .body
                        .and_then(|body| body.expr(object.span))
                        .is_some_and(|ty| builtins::method(ty, name).is_some());
                    format!("{} {}.{name}", if builtin { "builtin" } else { "own" }, owner.unwrap_or_default())
                }
                ExprKind::Name(name) => format!("function {name}"),
                _ => "call".to_owned(),
            },
            _ => "expression".to_owned(),
        };
        self.problems.push(Diagnostic::new(format!("ref-rule unused [{what}] gives {ty}"), value.span));
    }

    fn needs_mut(&mut self, place: &Expr, how: &str) {
        let Some(name) = root(place) else { return };
        let program = &self.checker.program;
        let package = format!("{name}.");
        let local = !program.is_type_name(name)
            && !program.constants.contains_key(name)
            && !program.modules.keys().any(|module| module == name || module.starts_with(&package));
        if local && !self.changeable.contains(name) {
            self.problems.push(Diagnostic::new(format!("ref-rule source [{how}] `{name}` isn't mut"), place.span));
        }
    }

    fn expr(&mut self, expr: &'a Expr) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            for arg in args.iter().filter(|arg| arg.is_mut) {
                self.needs_mut(&arg.value, "argument");
            }
            if let ExprKind::Attr { object, name, .. } = &callee.kind
                && let Some(ty) = self.body.and_then(|body| body.expr(object.span)).cloned()
            {
                let changes = match builtins::method(&ty, name) {
                    Some(found) => found.changes,
                    None => ty.type_name().and_then(|owner| self.checker.program.method(owner, name)).is_some_and(
                        |(_, entry)| entry.def.params.first().is_some_and(|param| param.is_self && param.is_mut),
                    ),
                };
                if changes {
                    self.needs_mut(object, "method");
                }
            }
        }
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                self.block(&arm.body, true);
            }
        }
        for inner in crate::effects::children(expr) {
            self.expr(inner);
        }
    }
}
