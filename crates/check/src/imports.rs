//! Standard library types that exist only once imported, like `Deque` from
//! `std.collections`. Using one without its import is reported once, where it's first used.

use std::collections::BTreeSet;

use fennec_syntax::{
    Diagnostic, Expr, ExprKind, FStrPart, FnDef, Item, Module, Span, Stmt, StmtKind, TypeExpr,
};

use crate::messages::unknown_name;
use crate::program::Program;

struct Uses<'p> {
    program: &'p Program,
    reported: BTreeSet<String>,
    found: Vec<Diagnostic>,
}

/// The standard library types `module` uses without importing them.
pub(crate) fn unimported(module: &Module, program: &Program) -> Vec<Diagnostic> {
    let mut uses = Uses { program, reported: BTreeSet::new(), found: Vec::new() };
    for item in &module.items {
        match item {
            Item::Function(def) => uses.function(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                for field in def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)) {
                    uses.type_expr(field.ty.as_ref());
                    uses.expr(field.default.as_ref());
                    uses.expr(field.rule.as_ref());
                }
                for method in &def.methods {
                    uses.function(method);
                }
            }
            Item::TypeAlias { ty, .. } => uses.type_expr(Some(ty)),
            Item::Test(test) => {
                for input in &test.inputs {
                    uses.type_expr(input.ty.as_ref());
                }
                uses.stmts(&test.body);
            }
            Item::Extend(extension) => {
                for method in &extension.methods {
                    uses.function(method);
                }
            }
            Item::Stmt(stmt) => uses.stmt(stmt),
            Item::Interface(_) | Item::Import(_) => {}
        }
    }
    uses.found
}

impl Uses<'_> {
    fn name(&mut self, name: &str, span: Span) {
        let defined_here = self.program.type_info(name).is_some_and(|info| !info.prelude);
        if fennec_syntax::importable_type(name).is_none()
            || defined_here
            || self.program.imported_types.contains(name)
            || !self.reported.insert(name.to_owned())
        {
            return;
        }
        self.found.push(unknown_name(name, span));
    }

    fn function(&mut self, def: &FnDef) {
        for param in &def.params {
            self.type_expr(param.ty.as_ref());
        }
        self.type_expr(def.ret.as_ref());
        self.stmts(&def.body);
    }

    fn type_expr(&mut self, ty: Option<&TypeExpr>) {
        let Some(ty) = ty else { return };
        self.name(&ty.name, ty.span);
        for arg in &ty.args {
            self.type_expr(Some(arg));
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(value)
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value) => {
                self.expr(Some(value));
            }
            StmtKind::Return(value) => self.expr(value.as_ref()),
            StmtKind::Assign { target, value, .. } => {
                self.expr(Some(target));
                self.expr(Some(value));
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.expr(Some(cond));
                    self.stmts(body);
                }
                self.stmts(otherwise.as_deref().unwrap_or_default());
            }
            StmtKind::While { cond: value, body }
            | StmtKind::For { iter: value, body, .. }
            | StmtKind::With { value, body, .. } => {
                self.expr(Some(value));
                self.stmts(body);
            }
            StmtKind::Def(def) => self.function(def),
            StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    fn exprs<'e>(&mut self, exprs: impl IntoIterator<Item = &'e Expr>) {
        for expr in exprs {
            self.expr(Some(expr));
        }
    }

    fn expr(&mut self, expr: Option<&Expr>) {
        let Some(expr) = expr else { return };
        match &expr.kind {
            ExprKind::Name(name) => self.name(name, expr.span),
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::None
            | ExprKind::Variant(_) => {}
            ExprKind::FStr(parts) => self.exprs(parts.iter().filter_map(|part| match part {
                FStrPart::Expr { expr, .. } => Some(expr),
                FStrPart::Text(_) => None,
            })),
            ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => self.exprs(items),
            ExprKind::Dict(pairs) => self.exprs(pairs.iter().flat_map(|(key, value)| [key, value])),
            ExprKind::Comprehension { elem, value, iter, cond, .. } => {
                self.exprs(
                    [Some(elem), value.as_ref(), Some(iter), cond.as_ref()]
                        .into_iter()
                        .flatten()
                        .map(AsRef::as_ref),
                );
            }
            ExprKind::Unary { expr: inner, .. }
            | ExprKind::Attr { object: inner, .. }
            | ExprKind::Lambda { body: inner, .. } => {
                self.expr(Some(inner));
            }
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Index { object: lhs, index: rhs }
            | ExprKind::Else { value: lhs, default: rhs } => self.exprs([lhs.as_ref(), rhs.as_ref()]),
            ExprKind::Cond { then, cond, otherwise } => {
                self.exprs([then.as_ref(), cond.as_ref(), otherwise.as_ref()]);
            }
            ExprKind::Try { value, context } => {
                self.expr(Some(value));
                self.expr(context.as_deref());
            }
            ExprKind::Call { callee, args } => {
                self.expr(Some(callee));
                self.exprs(args.iter().map(|arg| &arg.value));
            }
            ExprKind::Slice { object, start, end } => {
                self.expr(Some(object));
                self.expr(start.as_deref());
                self.expr(end.as_deref());
            }
            ExprKind::Match { subject, arms } => {
                self.expr(Some(subject));
                for arm in arms {
                    self.stmts(&arm.body);
                }
            }
        }
    }
}
