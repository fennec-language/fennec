//! Names across a kit's files. Every file of a kit shares one set of top-level names, and a
//! file uses another file's names only after importing them: `from enemies import spawn`,
//! or `import enemies` and then `enemies.spawn`. Uses of the second kind are rewritten to the
//! plain name, so the merged module reads like one file.

use std::collections::{BTreeSet, HashMap, HashSet};

use fennec_syntax::{
    Diagnostic, Expr, ExprKind, FStrPart, FnDef, Item, Pattern, Span, Stmt, StmtKind, TypeExpr,
};

use crate::load::{File, is_std};

/// What each file defines, and which file defines each name.
struct Kit {
    files: Vec<(String, String, BTreeSet<String>)>,
    owners: HashMap<String, usize>,
    /// The enums with each case name, and the file each is in.
    cases: HashMap<String, Vec<(String, usize)>>,
}

impl Kit {
    fn name(&self, file: usize) -> &str {
        self.files.get(file).map_or("", |(name, _, _)| name)
    }

    fn shown(&self, file: usize) -> &str {
        self.files.get(file).map_or("", |(_, shown, _)| shown)
    }

    fn defines(&self, file: usize, name: &str) -> bool {
        self.files.get(file).is_some_and(|(_, _, names)| names.contains(name))
    }

    /// A help line listing what a file defines.
    fn listing(&self, file: usize) -> String {
        let names: Vec<&str> = self
            .files
            .get(file)
            .map(|(_, _, names)| names.iter().map(String::as_str).collect())
            .unwrap_or_default();
        if names.is_empty() {
            format!("{} defines nothing yet", self.shown(file))
        } else {
            format!("{} defines {}", self.shown(file), names.join(", "))
        }
    }
}

/// Checks every file's imports and the names it uses from other files.
pub(crate) fn resolve(files: &mut [File], errors: &mut Vec<Diagnostic>) {
    let kit = gather(files, errors);
    let index: HashMap<String, usize> =
        files.iter().enumerate().map(|(at, file)| (file.name.clone(), at)).collect();
    for (at, file) in files.iter_mut().enumerate() {
        let mut scope = Resolver {
            kit: &kit,
            file: at,
            imported: HashMap::new(),
            modules: HashMap::new(),
            std_modules: HashSet::new(),
            locals: HashSet::new(),
            collecting: false,
            errors: &mut *errors,
        };
        for item in &file.module.items {
            let Item::Import(import) = item else { continue };
            if is_std(import) {
                scope.std_modules.extend(std_aliases(&import.module, &import.names));
                continue;
            }
            let module = import.module.join(".");
            // `from game import physics` binds the module `game.physics`, when there is one.
            let (submodules, names): (Vec<&String>, Vec<&String>) =
                import.names.iter().partition(|name| index.contains_key(&format!("{module}.{name}")));
            for name in submodules {
                if let Some(&from) = index.get(&format!("{module}.{name}")) {
                    scope.modules.insert(name.clone(), from);
                }
            }
            if let Some(&from) = index.get(&module)
                && (import.names.is_empty() || !names.is_empty())
            {
                let names: Vec<String> = names.into_iter().cloned().collect();
                scope.import(&module, &names, from, import.span);
            }
        }
        for item in &mut file.module.items {
            scope.item(item);
        }
    }
}

/// Finds what each file defines, reporting names defined in two files.
fn gather(files: &[File], errors: &mut Vec<Diagnostic>) -> Kit {
    let mut kit = Kit { files: Vec::new(), owners: HashMap::new(), cases: HashMap::new() };
    for (at, file) in files.iter().enumerate() {
        let mut names = BTreeSet::new();
        for item in &file.module.items {
            if let Item::Enum(def) = item {
                for case in &def.cases {
                    kit.cases.entry(case.name.clone()).or_default().push((def.name.clone(), at));
                }
            }
            let Some((name, span)) = defined(item) else { continue };
            match kit.owners.get(name) {
                Some(&owner) if owner == at => errors.push(
                    Diagnostic::new(format!("`{name}` is already defined in this file"), span)
                        .with_help("rename one of them"),
                ),
                Some(&owner) => errors.push(
                    Diagnostic::new(format!("`{name}` is already defined in {}", kit.shown(owner)), span)
                        .with_help(
                            "the files of a kit share one set of top-level names, so rename one of them",
                        ),
                ),
                None => {
                    kit.owners.insert(name.to_owned(), at);
                }
            }
            names.insert(name.to_owned());
        }
        kit.files.push((file.name.clone(), file.shown.clone(), names));
    }
    kit
}

/// The name an item defines at the top level, and where.
fn defined(item: &Item) -> Option<(&str, Span)> {
    match item {
        Item::Function(def) => Some((&def.name, def.span)),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some((&def.name, def.span)),
        Item::Interface(interface) => Some((&interface.name, interface.span)),
        Item::TypeAlias { name, span, .. } | Item::Const { name, span, .. } => Some((name, *span)),
        Item::Extend(_) | Item::Import(_) | Item::Test(_) | Item::Stmt(_) => None,
    }
}

/// Walks one file, checking the names it uses. Each function is walked twice: once to
/// collect the names it binds itself, and once to check the rest.
struct Resolver<'a> {
    kit: &'a Kit,
    file: usize,
    /// Names brought in by `from module import name`, and the file each is from.
    imported: HashMap<String, usize>,
    /// Modules brought in whole by `import module`.
    modules: HashMap<String, usize>,
    /// Standard library modules imported whole, by the name they're used under, like
    /// `std.json` or `json`. The checker resolves their names.
    std_modules: HashSet<String>,
    /// The names the function being walked binds anywhere in it.
    locals: HashSet<String>,
    collecting: bool,
    errors: &'a mut Vec<Diagnostic>,
}

impl Resolver<'_> {
    fn import(&mut self, module: &str, names: &[String], from: usize, span: Span) {
        if names.is_empty() {
            self.modules.insert(module.to_owned(), from);
        }
        for name in names {
            if self.kit.defines(from, name) {
                self.imported.insert(name.clone(), from);
            } else {
                self.errors.push(
                    Diagnostic::new(format!("`{module}` has nothing called `{name}`"), span)
                        .with_help(self.kit.listing(from)),
                );
            }
        }
    }

    fn item(&mut self, item: &mut Item) {
        match item {
            Item::Function(def) => self.function(def, &[]),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                let fields: Vec<String> = def.fields.iter().map(|field| field.name.clone()).collect();
                self.locals = fields.iter().cloned().collect();
                for field in
                    def.fields.iter_mut().chain(def.cases.iter_mut().flat_map(|case| case.fields.iter_mut()))
                {
                    self.optional_ty(field.ty.as_mut());
                    for expr in field.default.iter_mut().chain(field.rule.iter_mut()) {
                        self.expr(expr);
                    }
                }
                for method in &mut def.methods {
                    self.function(method, &fields);
                }
            }
            Item::Interface(interface) => {
                for method in &mut interface.methods {
                    self.function(method, &[]);
                }
            }
            // The type has to be one this file can see, like any other name it uses.
            Item::Extend(extension) => {
                self.qualified(&mut extension.name, extension.span);
                for method in &mut extension.methods {
                    self.function(method, &[]);
                }
            }
            Item::TypeAlias { ty, .. } => self.ty(ty),
            Item::Const { ty, value, .. } => {
                for collecting in [true, false] {
                    self.start(collecting, &[]);
                    self.optional_ty(ty.as_mut());
                    self.expr(value);
                }
            }
            Item::Test(test) => {
                for collecting in [true, false] {
                    self.start(collecting, &[]);
                    for input in &mut test.inputs {
                        self.bind(&input.name);
                        self.optional_ty(input.ty.as_mut());
                    }
                    self.stmts(&mut test.body);
                }
            }
            Item::Import(_) | Item::Stmt(_) => {}
        }
    }

    fn function(&mut self, def: &mut FnDef, fields: &[String]) {
        for collecting in [true, false] {
            self.start(collecting, fields);
            self.def(def);
        }
    }

    fn start(&mut self, collecting: bool, fields: &[String]) {
        self.collecting = collecting;
        if collecting {
            self.locals = fields.iter().cloned().collect();
        }
    }

    fn def(&mut self, def: &mut FnDef) {
        for param in &mut def.params {
            self.bind(&param.name);
            self.optional_ty(param.ty.as_mut());
        }
        self.optional_ty(def.ret.as_mut());
        self.stmts(&mut def.body);
    }

    fn bind(&mut self, name: &str) {
        if self.collecting {
            self.locals.insert(name.to_owned());
        }
    }

    fn stmts(&mut self, stmts: &mut [Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Fail(expr) | StmtKind::Yield(expr) | StmtKind::Assert(expr) => {
                self.expr(expr);
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            StmtKind::Assign { target, value, .. } => {
                self.bind_target(target);
                self.expr(target);
                self.expr(value);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.stmts(body);
                }
                if let Some(body) = otherwise {
                    self.stmts(body);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.stmts(body);
            }
            StmtKind::For { names, iter, body, .. } => {
                for name in names.iter() {
                    self.bind(name);
                }
                self.expr(iter);
                self.stmts(body);
            }
            StmtKind::With { value, name, body, .. } => {
                if let Some(name) = name {
                    self.bind(name);
                }
                self.expr(value);
                self.stmts(body);
            }
            StmtKind::Def(def) => {
                self.bind(&def.name);
                self.def(def);
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    /// Binds the names an assignment declares: `name = ...` and `a, b = ...`.
    fn bind_target(&mut self, target: &Expr) {
        match &target.kind {
            ExprKind::Name(name) => self.bind(name),
            ExprKind::Tuple(items) => {
                for item in items {
                    self.bind_target(item);
                }
            }
            _ => {}
        }
    }

    fn expr(&mut self, expr: &mut Expr) {
        if !self.collecting && (self.member(expr) || self.named_case(expr)) {
            return;
        }
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::None
            | ExprKind::Variant(_) => {}
            ExprKind::Name(name) => {
                let name = name.clone();
                self.name(&name, span);
            }
            ExprKind::FStr(parts) => {
                for part in parts {
                    if let FStrPart::Expr { expr, .. } = part {
                        self.expr(expr);
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
                self.exprs(items.iter_mut());
            }
            ExprKind::Dict(pairs) => self.exprs(pairs.iter_mut().flat_map(|(key, value)| [key, value])),
            ExprKind::Comprehension { elem, value, names, iter, cond, .. } => {
                for name in names.iter() {
                    self.bind(name);
                }
                self.exprs(
                    [elem, iter]
                        .into_iter()
                        .map(AsMut::as_mut)
                        .chain(value.as_deref_mut())
                        .chain(cond.as_deref_mut()),
                );
            }
            ExprKind::Unary { expr, .. } => self.expr(expr),
            ExprKind::Binary { lhs: first, rhs: second, .. }
            | ExprKind::Else { value: first, default: second }
            | ExprKind::Index { object: first, index: second } => {
                self.exprs([first, second].into_iter().map(AsMut::as_mut));
            }
            ExprKind::Cond { then, cond, otherwise } => {
                self.exprs([then, cond, otherwise].into_iter().map(AsMut::as_mut));
            }
            ExprKind::Try { value, context } => {
                self.exprs([value.as_mut()].into_iter().chain(context.as_deref_mut()));
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                self.exprs(args.iter_mut().map(|arg| &mut arg.value));
            }
            ExprKind::Attr { object, .. } => self.expr(object),
            ExprKind::Slice { object, start, end } => {
                self.exprs(
                    [object.as_mut()].into_iter().chain(start.as_deref_mut()).chain(end.as_deref_mut()),
                );
            }
            ExprKind::Lambda { params, body } => {
                for param in params.iter() {
                    self.bind(param);
                }
                self.expr(body);
            }
            ExprKind::Match { subject, arms } => {
                self.expr(subject);
                for arm in arms {
                    self.pattern(&mut arm.pattern, arm.span);
                    if let Some(guard) = &mut arm.guard {
                        self.expr(guard);
                    }
                    self.stmts(&mut arm.body);
                }
            }
        }
    }

    /// Writes the enum into a leading-dot case when only one enum this file can see has a
    /// case by that name. Returns whether it did.
    fn named_case(&self, expr: &mut Expr) -> bool {
        let ExprKind::Variant(case) = &expr.kind else { return false };
        let Some(owner) = self.only_visible(case) else { return false };
        fennec_syntax::visit::name_enum(expr, &owner);
        true
    }

    /// The one enum this file can see with a case called `case`, when enums in other files
    /// have a case by that name too. A leading-dot case looks only at the enums its file
    /// declares or imports, so a file the kit adds can't make it ambiguous.
    fn only_visible(&self, case: &str) -> Option<String> {
        let owners = self.kit.cases.get(case)?;
        if owners.len() < 2 {
            return None;
        }
        let visible: Vec<&String> = owners
            .iter()
            .filter(|(owner, file)| {
                *file == self.file
                    || self.imported.get(owner) == Some(file)
                    || self.modules.values().any(|module| module == file)
            })
            .map(|(owner, _)| owner)
            .collect();
        match visible.as_slice() {
            [only] => Some((*only).clone()),
            _ => None,
        }
    }

    fn exprs<'e>(&mut self, exprs: impl Iterator<Item = &'e mut Expr>) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn pattern(&mut self, pattern: &mut Pattern, span: Span) {
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Bind(name) => self.bind(name),
            Pattern::Literal(expr) => self.expr(expr),
            Pattern::Variant { name, dotted, args } => {
                if !*dotted {
                    self.qualified(name, span);
                }
                for arg in args {
                    self.pattern(arg, span);
                }
            }
            Pattern::Tuple(items) | Pattern::Or(items) => {
                for item in items {
                    self.pattern(item, span);
                }
            }
        }
    }

    /// Checks a name the file uses: it must be its own, imported, or not the kit's at all.
    fn name(&mut self, name: &str, span: Span) {
        if self.collecting
            || self.locals.contains(name)
            || self.kit.defines(self.file, name)
            || self.imported.contains_key(name)
        {
            return;
        }
        if let Some(&owner) = self.kit.owners.get(name) {
            self.errors.push(
                Diagnostic::new(
                    format!(
                        "`{name}` is defined in {}, which this file doesn't import",
                        self.kit.shown(owner)
                    ),
                    span,
                )
                .with_help(format!(
                    "add `from {} import {name}` at the top of this file",
                    self.kit.name(owner)
                )),
            );
        }
    }

    /// Rewrites `module.name`, for a module imported whole, to `name`. Returns whether
    /// `expr` was one.
    fn member(&mut self, expr: &mut Expr) -> bool {
        let ExprKind::Attr { object, name, optional: false } = &expr.kind else { return false };
        let Some((module, owner)) = self.module(object) else {
            if let ExprKind::Name(short) = &object.kind
                && !self.locals.contains(short)
                && let Some(problem) = self.short_form(short, name, expr.span)
            {
                self.errors.push(problem);
                return true;
            }
            return false;
        };
        if self.kit.defines(owner, name) {
            expr.kind = ExprKind::Name(name.clone());
        } else {
            self.errors.push(
                Diagnostic::new(format!("`{module}` has nothing called `{name}`"), expr.span)
                    .with_help(self.kit.listing(owner)),
            );
        }
        true
    }

    /// The mistake of writing `physics.Body` after `import game.physics`, which binds `game`.
    fn short_form(&self, short: &str, member: &str, span: Span) -> Option<Diagnostic> {
        let full =
            self.modules.keys().find(|path| path.rsplit_once('.').is_some_and(|(_, last)| last == short))?;
        let (parent, _) = full.rsplit_once('.')?;
        let package = full.split('.').next().unwrap_or_default();
        Some(Diagnostic::new(format!("`{short}` isn't imported here"), span).with_help(format!(
            "`import {full}` binds `{package}`, so write `{full}.{member}`, or import it with `from {parent} import {short}`"
        )))
    }

    /// The module imported whole that `expr` names, like `physics` or `game.physics`.
    fn module(&self, expr: &Expr) -> Option<(String, usize)> {
        let path = dotted(expr)?;
        let first = path.split('.').next().unwrap_or_default();
        if self.locals.contains(first) {
            return None;
        }
        let owner = *self.modules.get(&path)?;
        Some((path, owner))
    }

    fn optional_ty(&mut self, ty: Option<&mut TypeExpr>) {
        if let Some(ty) = ty {
            self.ty(ty);
        }
    }

    /// Checks a written type, rewriting `physics.Body` to `Body`.
    fn ty(&mut self, ty: &mut TypeExpr) {
        self.qualified(&mut ty.name, ty.span);
        for arg in &mut ty.args {
            self.ty(arg);
        }
    }

    /// Checks a type or case name, rewriting `physics.Body` to `Body`.
    fn qualified(&mut self, name: &mut String, span: Span) {
        if self.collecting {
            return;
        }
        let Some((module, member)) = name.rsplit_once('.') else {
            let name = name.clone();
            self.name(&name, span);
            return;
        };
        if self.std_modules.contains(module) {
            return;
        }
        match self.modules.get(module) {
            Some(&owner) if self.kit.defines(owner, member) => *name = member.to_owned(),
            Some(&owner) => self.errors.push(
                Diagnostic::new(format!("`{module}` has nothing called `{member}`"), span)
                    .with_help(self.kit.listing(owner)),
            ),
            None => {
                let problem = self.short_form(module, member, span).unwrap_or_else(|| {
                    Diagnostic::new(format!("`{module}` isn't imported here"), span)
                        .with_help(format!("add `import {module}` at the top of this file"))
                });
                self.errors.push(problem);
            }
        }
    }
}

/// The names a standard library import binds to modules: `std.json` for `import std.json`,
/// and `json` for `from std import json`.
fn std_aliases(module: &[String], names: &[String]) -> Vec<String> {
    match (module, names) {
        ([_], names) => names.to_vec(),
        (module, []) => vec![module.join(".")],
        _ => Vec::new(),
    }
}

/// `a.b.c` for a chain of names and attributes.
fn dotted(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Attr { object, name, optional: false } => Some(format!("{}.{name}", dotted(object)?)),
        _ => None,
    }
}
