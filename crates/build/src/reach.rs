//! What each test depends on, so a test whose code hasn't changed needn't run again.
//!
//! A test's key hashes the test and every definition it can reach, each by the hash of its
//! tree without positions (see `tree`). A definition reaches what it names: functions,
//! types, constants, aliases, enum cases and errors by name, and methods and fields by
//! member name, on any type, since which type is only known after checking. A type reaches
//! the methods the language calls without naming them: its operators, `[]`, `iter` and
//! `del`. Reaching too much only means running a test that didn't need it; anything that can
//! change a test's result is reached.

use std::collections::{BTreeSet, HashMap};

use fennec_blake3::{Hash, Hasher};
use fennec_syntax::{
    Expr, ExprKind, FStrPart, FnDef, Import, Item, Module, Pattern, Stmt, StmtKind, TypeDef, TypeExpr,
};

use crate::tree::tree_hash;

/// Methods the language calls on a value without the code naming them: indexing, looping,
/// cleanup. Operators, whose names aren't words, are called the same way.
const IMPLICIT: [&str; 3] = ["[]", "iter", "del"];

/// Each test's name and the key of everything its result depends on.
pub fn test_keys(module: &Module) -> Vec<(String, Hash)> {
    let graph = Graph::of(module, "");
    graph
        .definitions
        .iter()
        .enumerate()
        .filter_map(|(at, definition)| Some((definition.test.clone()?, graph.key(at))))
        .collect()
}

/// A definition: its key and tree hash, the names it uses, and for a test, its name.
pub(crate) struct Definition {
    pub(crate) key: String,
    pub(crate) hash: Hash,
    pub(crate) uses: Uses,
    pub(crate) test: Option<String>,
}

/// The names a definition uses: plain names, and member names after a dot.
#[derive(Default)]
pub(crate) struct Uses {
    pub(crate) names: BTreeSet<String>,
    pub(crate) members: BTreeSet<String>,
}

pub(crate) struct Graph {
    pub(crate) definitions: Vec<Definition>,
    /// The definitions each plain name may refer to.
    pub(crate) names: HashMap<String, Vec<usize>>,
    /// The methods, by name, of every type.
    pub(crate) members: HashMap<String, Vec<usize>>,
    /// The methods each type has that the language calls by itself.
    pub(crate) implicit: HashMap<usize, Vec<usize>>,
    /// Methods the language calls by itself, with the name of the type they're on, until
    /// every type is known.
    unowned: Vec<(String, usize)>,
    /// Which build of Fennec the keys are for, since another one's compiler may behave
    /// differently.
    pub(crate) build: String,
}

impl Graph {
    pub(crate) fn of(module: &Module, build: &str) -> Self {
        let mut graph = Graph {
            definitions: Vec::new(),
            names: HashMap::new(),
            members: HashMap::new(),
            implicit: HashMap::new(),
            unowned: Vec::new(),
            build: build.to_owned(),
        };
        for item in &module.items {
            graph.item(item);
        }
        for (owner, method) in std::mem::take(&mut graph.unowned) {
            for at in graph.names.get(&owner).cloned().unwrap_or_default() {
                graph.implicit.entry(at).or_default().push(method);
            }
        }
        graph
    }

    fn add(&mut self, key: String, hash: Hash, uses: Uses, test: Option<String>) -> usize {
        self.definitions.push(Definition { key, hash, uses, test });
        self.definitions.len() - 1
    }

    fn name(&mut self, name: &str, at: usize) {
        self.names.entry(name.to_owned()).or_default().push(at);
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Function(def) => {
                let at = self.add(format!("def {}", def.name), tree_hash(def), function_uses(def), None);
                self.name(&def.name, at);
            }
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => self.type_def(def),
            Item::Interface(interface) => {
                let mut uses = Uses::default();
                for method in &interface.methods {
                    uses.function(method);
                }
                let at = self.add(format!("interface {}", interface.name), tree_hash(interface), uses, None);
                self.name(&interface.name, at);
            }
            Item::Extend(extension) => {
                for method in &extension.methods {
                    self.method(&extension.name, method);
                }
            }
            Item::TypeAlias { name, ty, .. } => {
                let mut uses = Uses::default();
                uses.type_expr(ty);
                let at = self.add(format!("type {name}"), tree_hash(item), uses, None);
                self.name(name, at);
            }
            Item::Const { name, ty, value, .. } => {
                let mut uses = Uses::default();
                if let Some(ty) = ty {
                    uses.type_expr(ty);
                }
                uses.expr(value);
                let at = self.add(format!("const {name}"), tree_hash(item), uses, None);
                self.name(name, at);
            }
            Item::Test(test) => {
                let mut uses = Uses::default();
                test.inputs.iter().filter_map(|input| input.ty.as_ref()).for_each(|ty| uses.type_expr(ty));
                uses.stmts(&test.body);
                let named = (!test.bench).then(|| test.name.clone());
                let word = if test.bench { "bench" } else { "test" };
                self.add(format!("{word} \"{}\"", test.name), tree_hash(test), uses, named);
            }
            Item::Import(import) => self.import(import),
            Item::Stmt(_) => {}
        }
    }

    /// An import of the standard library: each name is a definition of its own, so changing
    /// where one name comes from affects only the tests that reach that name.
    fn import(&mut self, import: &Import) {
        let module = import.module.join(".");
        for (name, bound) in import.bound() {
            let key = format!("from {module} import {name}");
            let at = self.add(key, tree_hash(&module), Uses::default(), None);
            self.name(bound, at);
        }
        if import.names.is_empty() {
            let at = self.add(format!("import {module}"), tree_hash(&module), Uses::default(), None);
            for part in import.module.iter().chain(&import.alias) {
                self.name(part, at);
            }
        }
    }

    fn type_def(&mut self, def: &TypeDef) {
        let mut uses = Uses::default();
        uses.fields(def);
        let at = self.add(
            format!("type {}", def.name),
            tree_hash(&(&def.name, &def.fields, &def.cases)),
            uses,
            None,
        );
        self.name(&def.name, at);
        // A case is named on its own, as `.Circle` or in a pattern.
        for case in &def.cases {
            self.name(&case.name, at);
        }
        for method in &def.methods {
            self.method(&def.name, method);
        }
    }

    fn method(&mut self, owner: &str, method: &FnDef) {
        let at =
            self.add(format!("def {owner}.{}", method.name), tree_hash(method), function_uses(method), None);
        self.members.entry(method.name.clone()).or_default().push(at);
        if is_implicit(&method.name) {
            self.unowned.push((owner.to_owned(), at));
        }
    }

    /// The key of everything the definition at `start` reaches, itself included.
    pub(crate) fn key(&self, start: usize) -> Hash {
        let mut reached = BTreeSet::from([start]);
        let mut pending = vec![start];
        while let Some(at) = pending.pop() {
            let Some(definition) = self.definitions.get(at) else { continue };
            let named = definition.uses.names.iter().filter_map(|name| self.names.get(name));
            let members = definition.uses.members.iter().filter_map(|name| self.members.get(name));
            let implicit = self.implicit.get(&at);
            for next in named.chain(members).chain(implicit).flatten() {
                if reached.insert(*next) {
                    pending.push(*next);
                }
            }
        }
        let mut parts: Vec<(&str, &Hash)> = reached
            .iter()
            .filter_map(|at| self.definitions.get(*at))
            .map(|definition| (definition.key.as_str(), &definition.hash))
            .collect();
        parts.sort();
        let mut hasher = Hasher::new_derive_key("fennec build cache 2026-10-30 test key");
        crate::traced::platform(&mut hasher, &self.build);
        for (key, hash) in parts {
            hasher.update(key.as_bytes());
            hasher.update(&[0]);
            hasher.update(hash.as_bytes());
        }
        hasher.finalize()
    }
}

/// Whether the language calls a method by this name without the code naming it.
pub(crate) fn is_implicit(name: &str) -> bool {
    IMPLICIT.contains(&name) || !name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}

fn function_uses(def: &FnDef) -> Uses {
    let mut uses = Uses::default();
    uses.function(def);
    uses
}

impl Uses {
    /// What a type's fields and cases use.
    fn fields(&mut self, def: &TypeDef) {
        for field in def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)) {
            if let Some(ty) = &field.ty {
                self.type_expr(ty);
            }
            field.default.iter().chain(&field.rule).for_each(|expr| self.expr(expr));
        }
    }

    /// What a type uses, all of its methods included.
    pub(crate) fn type_def(&mut self, def: &TypeDef) {
        self.fields(def);
        def.methods.iter().for_each(|method| self.function(method));
    }

    pub(crate) fn function(&mut self, def: &FnDef) {
        for generic in &def.generics {
            for bound in &generic.bounds {
                self.type_expr(bound);
            }
        }
        def.params.iter().filter_map(|param| param.ty.as_ref()).for_each(|ty| self.type_expr(ty));
        if let Some(ty) = &def.ret {
            self.type_expr(ty);
        }
        self.stmts(&def.body);
    }

    pub(crate) fn type_expr(&mut self, ty: &TypeExpr) {
        self.names.insert(ty.name.clone());
        for arg in &ty.args {
            self.type_expr(arg);
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    pub(crate) fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Fail(expr) | StmtKind::Yield(expr) | StmtKind::Assert(expr) => {
                self.expr(expr);
            }
            StmtKind::Return(value) => value.iter().for_each(|expr| self.expr(expr)),
            StmtKind::Assign { target, value, ty, .. } => {
                self.expr(target);
                self.expr(value);
                if let Some(ty) = ty {
                    self.type_expr(ty);
                }
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
            StmtKind::For { iter, body, .. } => {
                self.expr(iter);
                self.stmts(body);
            }
            StmtKind::With { value, body, .. } => {
                self.expr(value);
                self.stmts(body);
            }
            StmtKind::Def(def) => self.function(def),
            StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    pub(crate) fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Name(name) | ExprKind::Variant(name) => {
                self.names.insert(name.clone());
            }
            ExprKind::Attr { object, name, .. } => {
                self.members.insert(name.clone());
                // `Shape.Circle` names a case of `Shape`, which `.Circle` also reaches.
                self.names.insert(name.clone());
                self.expr(object);
            }
            ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
                for item in items {
                    self.expr(item);
                }
            }
            ExprKind::Dict(pairs) => pairs.iter().for_each(|(key, value)| {
                self.expr(key);
                self.expr(value);
            }),
            ExprKind::FStr(parts) => parts.iter().for_each(|part| {
                if let FStrPart::Expr { expr, .. } = part {
                    self.expr(expr);
                }
            }),
            ExprKind::Comprehension { elem, value, iter, cond, .. } => {
                for part in [Some(elem), value.as_ref(), Some(iter), cond.as_ref()].into_iter().flatten() {
                    self.expr(part);
                }
            }
            ExprKind::Unary { expr, .. } => self.expr(expr),
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Else { value: lhs, default: rhs } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Cond { then, cond, otherwise } => {
                self.expr(then);
                self.expr(cond);
                self.expr(otherwise);
            }
            ExprKind::Try { value, context } => {
                self.expr(value);
                if let Some(context) = context {
                    self.expr(context);
                }
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for arg in args {
                    self.expr(&arg.value);
                }
            }
            ExprKind::Index { object, index } => {
                self.expr(object);
                self.expr(index);
            }
            ExprKind::Slice { object, start, end } => {
                self.expr(object);
                start.iter().chain(end).for_each(|part| self.expr(part));
            }
            ExprKind::Lambda { body, .. } => self.expr(body),
            ExprKind::Match { subject, arms } => {
                self.expr(subject);
                for arm in arms {
                    self.pattern(&arm.pattern);
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.stmts(&arm.body);
                }
            }
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::None => {
            }
        }
    }

    fn pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Variant { name, args, .. } => {
                self.names.insert(name.clone());
                for arg in args {
                    self.pattern(arg);
                }
            }
            Pattern::Tuple(items) | Pattern::Or(items) => items.iter().for_each(|item| self.pattern(item)),
            Pattern::Literal(expr) => self.expr(expr),
            Pattern::Bind(_) | Pattern::Wildcard => {}
        }
    }
}
