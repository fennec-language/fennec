//! The syntax tree as indented text: one node per line, its children two spaces deeper, and
//! its span at the end as `@start..end`.

use std::fmt::Write as _;

use super::{float, quoted};
use crate::ast::{
    Arg, Case, CompKind, Expr, ExprKind, FStrPart, Field, FnDef, Generic, Item, MatchArm, Module, Param,
    Pattern, Stmt, StmtKind, TypeDef, TypeExpr, UnaryOp,
};
use crate::span::Span;

#[derive(Default)]
pub(super) struct Tree {
    pub(super) text: String,
    depth: usize,
}

impl Tree {
    fn line(&mut self, label: &str) {
        let _ = writeln!(self.text, "{}{label}", "  ".repeat(self.depth));
    }

    fn spanned(&mut self, label: &str, span: Span) {
        self.line(&format!("{label} @{}..{}", span.start, span.end));
    }

    /// A line for a node, with `children` dumped one level deeper.
    fn node(&mut self, label: &str, span: Option<Span>, children: impl FnOnce(&mut Self)) {
        match span {
            Some(span) => self.spanned(label, span),
            None => self.line(label),
        }
        self.depth += 1;
        children(self);
        self.depth -= 1;
    }

    pub(super) fn module(&mut self, module: &Module) {
        self.node("module", None, |tree| {
            for item in &module.items {
                tree.item(item);
            }
        });
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Function(function) => self.function(function),
            Item::Struct(def) => self.type_def("struct", def),
            Item::Enum(def) => self.type_def("enum", def),
            Item::Error(def) => self.type_def("error", def),
            Item::Interface(interface) => {
                self.node(&format!("interface {}", interface.name), Some(interface.span), |tree| {
                    for method in &interface.methods {
                        tree.function(method);
                    }
                });
            }
            Item::Extend(extension) => {
                self.node(&format!("extend {}", extension.name), Some(extension.span), |tree| {
                    tree.generics(&extension.generics);
                    for method in &extension.methods {
                        tree.function(method);
                    }
                });
            }
            Item::TypeAlias { name, ty, span } => {
                self.node(&format!("type-alias {name}"), Some(*span), |tree| tree.type_expr(ty));
            }
            Item::Import(import) => {
                let mut label = format!("import {}", import.module.join("."));
                if !import.names.is_empty() {
                    label = format!("{label}: {}", import.names.join(", "));
                }
                self.spanned(&label, import.span);
            }
            Item::Test(test) => {
                self.node(&format!("test {}", quoted(&test.name)), Some(test.span), |tree| {
                    for input in &test.inputs {
                        tree.param(input);
                    }
                    tree.stmts(&test.body);
                });
            }
            Item::Stmt(stmt) => self.stmt(stmt),
        }
    }

    fn function(&mut self, function: &FnDef) {
        self.node(&format!("def {}", function.name), Some(function.span), |tree| {
            tree.generics(&function.generics);
            for param in &function.params {
                tree.param(param);
            }
            if let Some(ret) = &function.ret {
                tree.node("returns", function.arrow, |tree| tree.type_expr(ret));
            }
            tree.stmts(&function.body);
        });
    }

    /// Type parameters, each with the interfaces it's bound to.
    fn generics(&mut self, generics: &[Generic]) {
        for generic in generics {
            self.node(&format!("generic {}", generic.name), Some(generic.span), |tree| {
                for bound in &generic.bounds {
                    tree.type_expr(bound);
                }
            });
        }
    }

    fn param(&mut self, param: &Param) {
        let label = format!(
            "param {}{}{}",
            param.name,
            if param.is_mut { " mut" } else { "" },
            if param.is_self { " self" } else { "" }
        );
        self.node(&label, Some(param.span), |tree| {
            if let Some(ty) = &param.ty {
                tree.type_expr(ty);
            }
        });
    }

    fn type_expr(&mut self, ty: &TypeExpr) {
        let label = format!("type {}{}", ty.name, if ty.optional { "?" } else { "" });
        self.node(&label, Some(ty.span), |tree| {
            for arg in &ty.args {
                tree.type_expr(arg);
            }
        });
    }

    fn type_def(&mut self, kind: &str, def: &TypeDef) {
        self.node(&format!("{kind} {}", def.name), Some(def.span), |tree| {
            tree.generics(&def.generics);
            for field in &def.fields {
                tree.field(field);
            }
            for case in &def.cases {
                tree.case(case);
            }
            for method in &def.methods {
                tree.function(method);
            }
        });
    }

    fn field(&mut self, field: &Field) {
        self.node(&format!("field {}", field.name), Some(field.span), |tree| {
            if let Some(ty) = &field.ty {
                tree.type_expr(ty);
            }
            if let Some(default) = &field.default {
                tree.node("default", None, |tree| tree.expr(default));
            }
            if let Some(rule) = &field.rule {
                tree.node("where", None, |tree| tree.expr(rule));
            }
        });
    }

    fn case(&mut self, case: &Case) {
        self.node(&format!("case {}", case.name), Some(case.span), |tree| {
            for field in &case.fields {
                tree.field(field);
            }
        });
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        let span = Some(stmt.span);
        match &stmt.kind {
            StmtKind::Expr(expr) => self.node("expr", span, |tree| tree.expr(expr)),
            StmtKind::Assign { target, op, value, declare_mut, ty } => {
                let mut label = "assign".to_owned();
                if let Some(op) = op {
                    label = format!("{label} {}=", op.symbol());
                }
                if *declare_mut {
                    label.push_str(" mut");
                }
                self.node(&label, span, |tree| {
                    tree.expr(target);
                    if let Some(ty) = ty {
                        tree.type_expr(ty);
                    }
                    tree.expr(value);
                });
            }
            StmtKind::If { branches, otherwise } => self.node("if", span, |tree| {
                for (cond, body) in branches {
                    tree.node("branch", None, |tree| {
                        tree.expr(cond);
                        tree.stmts(body);
                    });
                }
                if let Some(otherwise) = otherwise {
                    tree.node("else", None, |tree| tree.stmts(otherwise));
                }
            }),
            StmtKind::While { cond, body } => self.node("while", span, |tree| {
                tree.expr(cond);
                tree.stmts(body);
            }),
            StmtKind::For { names, is_mut, iter, body } => {
                let label = format!("for{} {}", if *is_mut { " mut" } else { "" }, names.join(", "));
                self.node(&label, span, |tree| {
                    tree.expr(iter);
                    tree.stmts(body);
                });
            }
            StmtKind::Return(value) => self.node("return", span, |tree| {
                if let Some(value) = value {
                    tree.expr(value);
                }
            }),
            StmtKind::Fail(value) => self.node("fail", span, |tree| tree.expr(value)),
            StmtKind::Yield(value) => self.node("yield", span, |tree| tree.expr(value)),
            StmtKind::Assert(value) => self.node("assert", span, |tree| tree.expr(value)),
            StmtKind::With { value, name, is_mut, body } => {
                let mut label = "with".to_owned();
                if let Some(name) = name {
                    label = format!("{label} as{} {name}", if *is_mut { " mut" } else { "" });
                }
                self.node(&label, span, |tree| {
                    tree.expr(value);
                    tree.stmts(body);
                });
            }
            StmtKind::Def(function) => self.function(function),
            StmtKind::Break => self.spanned("break", stmt.span),
            StmtKind::Continue => self.spanned("continue", stmt.span),
            StmtKind::Pass => self.spanned("pass", stmt.span),
        }
    }

    fn exprs(&mut self, exprs: &[Expr]) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        let span = Some(expr.span);
        match &expr.kind {
            ExprKind::Int(value) => self.spanned(&format!("int {value}"), expr.span),
            ExprKind::Float(value) => self.spanned(&format!("float {}", float(*value)), expr.span),
            ExprKind::Str(text) => self.spanned(&format!("str {}", quoted(text)), expr.span),
            ExprKind::FStr(parts) => self.node("fstr", span, |tree| {
                for part in parts {
                    match part {
                        FStrPart::Text(text) => tree.line(&format!("text {}", quoted(text))),
                        FStrPart::Expr { expr, spec } => {
                            let label = spec.as_ref().map_or_else(
                                || "code".to_owned(),
                                |spec| format!("code spec {}", quoted(spec)),
                            );
                            tree.node(&label, None, |tree| tree.expr(expr));
                        }
                    }
                }
            }),
            ExprKind::Bool(value) => {
                self.spanned(if *value { "bool True" } else { "bool False" }, expr.span);
            }
            ExprKind::None => self.spanned("none", expr.span),
            ExprKind::Name(name) => self.spanned(&format!("name {name}"), expr.span),
            ExprKind::Variant(name) => self.spanned(&format!("variant {name}"), expr.span),
            ExprKind::List(items) => self.node("list", span, |tree| tree.exprs(items)),
            ExprKind::Tuple(items) => self.node("tuple", span, |tree| tree.exprs(items)),
            ExprKind::Set(items) => self.node("set", span, |tree| tree.exprs(items)),
            ExprKind::Dict(pairs) => self.node("dict", span, |tree| {
                for (key, value) in pairs {
                    tree.node("pair", None, |tree| {
                        tree.expr(key);
                        tree.expr(value);
                    });
                }
            }),
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                let kind = match kind {
                    CompKind::List => "list",
                    CompKind::Generator => "generator",
                    CompKind::Set => "set",
                    CompKind::Dict => "dict",
                };
                self.node(&format!("comprehension {kind} {}", names.join(", ")), span, |tree| {
                    tree.expr(elem);
                    if let Some(value) = value {
                        tree.node("value", None, |tree| tree.expr(value));
                    }
                    tree.node("in", None, |tree| tree.expr(iter));
                    if let Some(cond) = cond {
                        tree.node("if", None, |tree| tree.expr(cond));
                    }
                });
            }
            _ => self.compound(expr),
        }
    }

    /// Expressions built from operators, calls and blocks.
    fn compound(&mut self, expr: &Expr) {
        let span = Some(expr.span);
        match &expr.kind {
            ExprKind::Unary { op, expr } => {
                let op = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not",
                };
                self.node(&format!("unary {op}"), span, |tree| tree.expr(expr));
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.node(&format!("binary {}", op.symbol()), span, |tree| {
                    tree.expr(lhs);
                    tree.expr(rhs);
                });
            }
            ExprKind::Cond { then, cond, otherwise } => self.node("cond", span, |tree| {
                tree.expr(then);
                tree.expr(cond);
                tree.expr(otherwise);
            }),
            ExprKind::Else { value, default } => self.node("else", span, |tree| {
                tree.expr(value);
                tree.expr(default);
            }),
            ExprKind::Try { value, context } => self.node("try", span, |tree| {
                tree.expr(value);
                if let Some(context) = context {
                    tree.node("from", None, |tree| tree.expr(context));
                }
            }),
            ExprKind::Call { callee, args } => self.node("call", span, |tree| {
                tree.expr(callee);
                for arg in args {
                    tree.arg(arg);
                }
            }),
            ExprKind::Attr { object, name, optional } => {
                let label = format!("attr {}{name}", if *optional { "?." } else { "." });
                self.node(&label, span, |tree| tree.expr(object));
            }
            ExprKind::Index { object, index } => self.node("index", span, |tree| {
                tree.expr(object);
                tree.expr(index);
            }),
            ExprKind::Slice { object, start, end } => self.node("slice", span, |tree| {
                tree.expr(object);
                if let Some(start) = start {
                    tree.node("start", None, |tree| tree.expr(start));
                }
                if let Some(end) = end {
                    tree.node("end", None, |tree| tree.expr(end));
                }
            }),
            ExprKind::Lambda { params, body } => {
                self.node(&format!("lambda {}", params.join(", ")), span, |tree| tree.expr(body));
            }
            ExprKind::Match { subject, arms } => self.node("match", span, |tree| {
                tree.expr(subject);
                for arm in arms {
                    tree.arm(arm);
                }
            }),
            _ => {}
        }
    }

    fn arg(&mut self, arg: &Arg) {
        let mut label = "arg".to_owned();
        if arg.is_mut {
            label.push_str(" mut");
        }
        if let Some(name) = &arg.name {
            label = format!("{label} {name}=");
        }
        self.node(&label, None, |tree| tree.expr(&arg.value));
    }

    fn arm(&mut self, arm: &MatchArm) {
        self.node("arm", Some(arm.span), |tree| {
            tree.pattern(&arm.pattern);
            tree.stmts(&arm.body);
        });
    }

    fn pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Wildcard => self.line("pattern _"),
            Pattern::Bind(name) => self.line(&format!("pattern bind {name}")),
            Pattern::Literal(expr) => self.node("pattern literal", None, |tree| tree.expr(expr)),
            Pattern::Variant { name, dotted, args } => {
                let label = format!("pattern {}{name}", if *dotted { "." } else { "" });
                self.node(&label, None, |tree| {
                    for arg in args {
                        tree.pattern(arg);
                    }
                });
            }
            Pattern::Tuple(items) => self.node("pattern tuple", None, |tree| {
                for item in items {
                    tree.pattern(item);
                }
            }),
        }
    }
}
