//! What the name at a place in a program refers to.
//!
//! A walk through the code to that place keeps track of the names each block declares, so a
//! name finds the declaration it sees. Names are never redeclared in Fennec, so the first
//! declaration in scope is the one. A name declared nowhere in scope is one of the program's
//! top-level names, which a kit's files share.
//!
//! For `object.name`, what `name` is depends on the object's type. When the program checks,
//! its typed tree says; while it has mistakes, every member with that name is a candidate.

use fennec_syntax::{
    Expr, ExprKind, FStrPart, FnDef, Item, Module, Pattern, Sources, Span, Stmt, StmtKind, TypeExpr,
};

/// What a name at a place refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A name declared in a function: a parameter, an assignment, a loop's names, a nested
    /// `def`. `name_at` is where its name is written; `declared` is what declares it, as the
    /// typed tree keys it; `function` is the key of the function it's declared in.
    Local { name: String, name_at: Span, declared: Span, function: String },
    /// One of the program's top-level names: a function, type, constant or alias.
    Global(String),
    /// `.Case`: a case of whichever enum has one with this name.
    Case(String),
    /// `object.name`, read in the function with key `function`.
    Member { object: Span, name: String, function: String },
    /// `Type.name`: a named constructor, a method or an enum case of a type named directly.
    TypeMember { owner: String, name: String },
}

/// What the name at `at` refers to, if there's a name there.
pub(crate) fn target(module: &Module, sources: &Sources, at: u32) -> Option<Target> {
    let mut finder = Finder { at, sources, module, scopes: Vec::new(), function: String::new(), found: None };
    for item in &module.items {
        finder.item(item);
        if finder.found.is_some() {
            break;
        }
    }
    finder.found
}

/// Where `name` is written within `span`, as a whole word. The span itself when it isn't.
pub(crate) fn name_in(sources: &Sources, span: Span, name: &str) -> Span {
    let Some((_, text, base)) = sources.file_at(span) else { return span };
    let local = |at: u32| usize::try_from(at.saturating_sub(base)).unwrap_or(0);
    let Some(written) = text.get(local(span.start)..local(span.end).min(text.len())) else { return span };
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let found = written.match_indices(name).find(|(at, _)| {
        let before = written.get(..*at).and_then(|text| text.chars().next_back());
        let after = written.get(at + name.len()..).and_then(|text| text.chars().next());
        !before.is_some_and(word) && !after.is_some_and(word)
    });
    let Some((offset, _)) = found else { return span };
    let start = span.start.saturating_add(u32::try_from(offset).unwrap_or(0));
    Span { start, end: start.saturating_add(u32::try_from(name.len()).unwrap_or(0)) }
}

fn local(name: &str, name_at: Span, declared: Span, function: String) -> Target {
    Target::Local { name: name.to_owned(), name_at, declared, function }
}

struct Finder<'a> {
    at: u32,
    sources: &'a Sources,
    module: &'a Module,
    /// The names each open block declares, innermost last: name, where it's written, what
    /// declares it, and the key of the function it's in.
    scopes: Vec<Vec<(String, Span, Span, String)>>,
    /// The key of the function being walked, as the typed tree keys it.
    function: String,
    found: Option<Target>,
}

impl Finder<'_> {
    fn contains(&self, span: Span) -> bool {
        span.start <= self.at && self.at <= span.end
    }

    fn found(&mut self, target: Target) {
        if self.found.is_none() {
            self.found = Some(target);
        }
    }

    /// Declares `name`, written somewhere in `declared`. On the name itself, that's what the
    /// place refers to.
    fn declare(&mut self, name: &str, declared: Span) {
        let name_at = name_in(self.sources, declared, name);
        if self.contains(name_at) {
            self.found(local(name, name_at, declared, self.function.clone()));
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.push((name.to_owned(), name_at, declared, self.function.clone()));
        }
    }

    fn lookup(&self, name: &str) -> Option<Target> {
        let mut declared = self.scopes.iter().flatten().filter(|(declared, ..)| declared == name);
        declared.next().map(|(name, name_at, span, function)| local(name, *name_at, *span, function.clone()))
    }

    /// On the name a declaration in `span` declares, that declaration is what's there.
    fn names(&mut self, span: Span, name: &str, target: impl FnOnce() -> Target) {
        if self.contains(name_in(self.sources, span, name)) {
            self.found(target());
        }
    }

    fn resolve(&self, name: &str) -> Target {
        self.lookup(name).unwrap_or_else(|| Target::Global(name.to_owned()))
    }

    fn scoped(&mut self, walk: impl FnOnce(&mut Self)) {
        self.scopes.push(Vec::new());
        walk(self);
        self.scopes.pop();
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Function(def) => {
                self.names(def.span, &def.name, || Target::Global(def.name.clone()));
                self.function(def, def.name.clone());
            }
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                self.names(def.span, &def.name, || Target::Global(def.name.clone()));
                let member =
                    |name: &str| Target::TypeMember { owner: def.name.clone(), name: name.to_owned() };
                for (span, name) in def.fields.iter().map(|field| (field.span, &field.name)) {
                    self.names(span, name, || member(name));
                }
                for (span, name) in def.cases.iter().map(|case| (case.span, &case.name)) {
                    self.names(span, name, || member(name));
                }
                for (span, name) in def.methods.iter().map(|method| (method.span, &method.name)) {
                    self.names(span, name, || member(name));
                }
                for field in def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)) {
                    self.type_expr(field.ty.as_ref());
                    self.function = format!("fields of {}", def.name);
                    self.scoped(|finder| {
                        field.default.iter().chain(&field.rule).for_each(|expr| finder.expr(expr));
                    });
                }
                for method in &def.methods {
                    self.function(method, format!("{}.{}", def.name, method.name));
                }
            }
            Item::Interface(interface) => {
                for method in &interface.methods {
                    self.function(method, format!("{}.{}", interface.name, method.name));
                }
            }
            Item::Extend(extension) => {
                for method in &extension.methods {
                    let member =
                        || Target::TypeMember { owner: extension.name.clone(), name: method.name.clone() };
                    self.names(method.span, &method.name, member);
                    self.function(method, format!("{}.{}", extension.name, method.name));
                }
            }
            Item::TypeAlias { name, ty, span } => {
                self.names(*span, name, || Target::Global(name.clone()));
                self.type_expr(Some(ty));
            }
            Item::Const { name, ty, value, span } => {
                self.names(*span, name, || Target::Global(name.clone()));
                self.type_expr(ty.as_ref());
                self.expr(value);
            }
            Item::Test(test) => {
                self.function = format!("test \"{}\"", test.name);
                self.scoped(|finder| {
                    for input in &test.inputs {
                        finder.type_expr(input.ty.as_ref());
                        finder.declare(&input.name, input.span);
                    }
                    finder.block(&test.body);
                });
            }
            Item::Import(_) | Item::Stmt(_) => {}
        }
    }

    /// Walks a function with key `key`. A nested function sees the names around it.
    fn function(&mut self, def: &FnDef, key: String) {
        let outer = std::mem::replace(&mut self.function, key);
        self.scoped(|finder| {
            for generic in &def.generics {
                generic.bounds.iter().for_each(|bound| finder.type_expr(Some(bound)));
            }
            for param in &def.params {
                finder.type_expr(param.ty.as_ref());
                finder.declare(&param.name, param.span);
            }
            finder.type_expr(def.ret.as_ref());
            finder.block(&def.body);
        });
        self.function = outer;
    }

    fn block(&mut self, body: &[Stmt]) {
        self.scoped(|finder| body.iter().for_each(|stmt| finder.stmt(stmt)));
    }

    fn stmt(&mut self, stmt: &Stmt) {
        if self.found.is_some() {
            return;
        }
        match &stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Fail(expr) | StmtKind::Yield(expr) | StmtKind::Assert(expr) => {
                self.expr(expr);
            }
            StmtKind::Return(value) => value.iter().for_each(|expr| self.expr(expr)),
            StmtKind::Assign { target, op, value, ty, .. } => {
                self.expr(value);
                self.type_expr(ty.as_ref());
                self.assign_target(target, op.is_none());
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.block(body);
                }
                if let Some(body) = otherwise {
                    self.block(body);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            StmtKind::For { names, iter, body, .. } => {
                self.expr(iter);
                self.scoped(|finder| {
                    for name in names {
                        finder.declare(name, stmt.span);
                    }
                    finder.block(body);
                });
            }
            StmtKind::With { value, name, body, .. } => {
                self.expr(value);
                self.scoped(|finder| {
                    if let Some(name) = name {
                        finder.declare(name, stmt.span);
                    }
                    finder.block(body);
                });
            }
            StmtKind::Def(def) => {
                self.declare(&def.name, def.span);
                self.function(def, format!("{}.{}", self.function, def.name));
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    /// The target of an assignment. A plain `name = value` declares the name, unless it's
    /// already declared and this changes it.
    fn assign_target(&mut self, target: &Expr, plain: bool) {
        match &target.kind {
            ExprKind::Name(name) if plain && self.lookup(name).is_none() => self.declare(name, target.span),
            ExprKind::Tuple(items) => items.iter().for_each(|item| self.assign_target(item, plain)),
            _ => self.expr(target),
        }
    }

    fn type_expr(&mut self, ty: Option<&TypeExpr>) {
        let Some(ty) = ty.filter(|ty| self.contains(ty.span)) else { return };
        let name_end = ty.span.start.saturating_add(u32::try_from(ty.name.len()).unwrap_or(0));
        if self.at <= name_end {
            self.found(Target::Global(ty.name.clone()));
        }
        ty.args.iter().for_each(|arg| self.type_expr(Some(arg)));
    }

    fn expr(&mut self, expr: &Expr) {
        if self.found.is_some() || !self.contains(expr.span) {
            return;
        }
        match &expr.kind {
            ExprKind::Name(name) => self.found(self.resolve(name)),
            ExprKind::Variant(name) => self.found(Target::Case(name.clone())),
            ExprKind::Attr { object, name, .. } => self.attr(expr.span, object, name),
            ExprKind::Comprehension { elem, value, names, iter, cond, .. } => {
                self.expr(iter);
                self.scoped(|finder| {
                    for name in names {
                        finder.declare(name, expr.span);
                    }
                    for part in [Some(elem), value.as_ref(), cond.as_ref()].into_iter().flatten() {
                        finder.expr(part);
                    }
                });
            }
            ExprKind::Lambda { params, body } => self.scoped(|finder| {
                for param in params {
                    finder.declare(param, expr.span);
                }
                finder.expr(body);
            }),
            ExprKind::Match { subject, arms } => {
                self.expr(subject);
                for arm in arms {
                    if !self.contains(arm.span) {
                        continue;
                    }
                    self.scoped(|finder| {
                        finder.pattern(&arm.pattern, arm.span);
                        finder.block(&arm.body);
                    });
                }
            }
            _ => children(expr, &mut |child| self.expr(child)),
        }
    }

    /// `object.name`: on the name, a member; anywhere else, whatever's there in the object.
    fn attr(&mut self, span: Span, object: &Expr, name: &str) {
        let name_start = span.end.saturating_sub(u32::try_from(name.len()).unwrap_or(0));
        if self.at < name_start {
            self.expr(object);
            return;
        }
        let owner = match &object.kind {
            ExprKind::Name(owner) if self.lookup(owner).is_none() && self.is_type(owner) => Some(owner),
            _ => None,
        };
        self.found(match owner {
            Some(owner) => Target::TypeMember { owner: owner.clone(), name: name.to_owned() },
            None => {
                Target::Member { object: object.span, name: name.to_owned(), function: self.function.clone() }
            }
        });
    }

    fn is_type(&self, name: &str) -> bool {
        self.module.items.iter().any(|item| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.name == name,
            _ => false,
        })
    }

    /// Declares the names a `match` case's pattern binds, written somewhere in `arm`.
    fn pattern(&mut self, pattern: &Pattern, arm: Span) {
        match pattern {
            Pattern::Bind(name) => self.declare(name, arm),
            Pattern::Variant { args, .. } | Pattern::Tuple(args) => {
                for arg in args {
                    self.pattern(arg, arm);
                }
            }
            Pattern::Literal(expr) => self.expr(expr),
            Pattern::Wildcard => {}
        }
    }
}

/// Calls `visit` on each expression directly inside `expr`, for the kinds that declare
/// nothing.
fn children(expr: &Expr, visit: &mut dyn FnMut(&Expr)) {
    match &expr.kind {
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => items.iter().for_each(visit),
        ExprKind::Dict(pairs) => pairs.iter().for_each(|(key, value)| {
            visit(key);
            visit(value);
        }),
        ExprKind::FStr(parts) => parts.iter().for_each(|part| {
            if let FStrPart::Expr { expr, .. } = part {
                visit(expr);
            }
        }),
        ExprKind::Unary { expr, .. } => visit(expr),
        ExprKind::Binary { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            visit(then);
            visit(cond);
            visit(otherwise);
        }
        ExprKind::Else { value, default } => {
            visit(value);
            visit(default);
        }
        ExprKind::Try { value, context } => {
            visit(value);
            if let Some(context) = context {
                visit(context);
            }
        }
        ExprKind::Call { callee, args } => {
            visit(callee);
            for arg in args {
                visit(&arg.value);
            }
        }
        ExprKind::Index { object, index } => {
            visit(object);
            visit(index);
        }
        ExprKind::Slice { object, start, end } => {
            visit(object);
            start.iter().chain(end).for_each(|part| visit(part));
        }
        _ => {}
    }
}
