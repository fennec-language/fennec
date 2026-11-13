//! Giving a program's own types other names. A program can declare a type with the same name as
//! one in the standard library, and the library's code still means its own. The checker gives
//! the program's type a name nobody can write, and shows it as written.

use std::collections::BTreeMap;

use crate::ast::{ExprKind, FnDef, Generic, Item, Module, Pattern, Stmt, StmtKind, TypeExpr};

/// What a renamed type is shown as: the name it was written with.
pub fn shown_type_name(name: &str) -> &str {
    name.strip_suffix(RENAMED).unwrap_or(name)
}

/// A function's key as it's shown: `Grid.iter` for the method of a program's own `Grid`.
pub fn shown_key(key: &str) -> String {
    key.replace(RENAMED, "")
}

/// What marks a renamed type. It can't be written, so it can't clash with anything.
pub const RENAMED: &str = "'";

/// What stands for a name's `'` in source the checker writes and parses itself: a program's own
/// type named like one of the library's is checked as `Delivery'`, and a hidden type parameter
/// is `Shape'1`, which no source can write. Each is written with this and named back once
/// parsed, by [`named_back`].
pub const WRITTEN_QUOTE: &str = "__quote__";

/// Text the checker writes to parse, with each `'` written so it parses.
#[must_use]
pub fn writable(text: &str) -> String {
    text.replace('\'', WRITTEN_QUOTE)
}

/// A name from text [`writable`] wrote, as the checker names it.
#[must_use]
pub fn named_back(name: &str) -> String {
    name.replace(WRITTEN_QUOTE, "'")
}

/// The names in text [`writable`] wrote that stand for ones with a `'`, each beside the name it
/// stands for, to give [`rename_types`].
#[must_use]
pub fn written_names(text: &str) -> BTreeMap<String, String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|name| name.contains(WRITTEN_QUOTE))
        .map(|name| (name.to_owned(), named_back(name)))
        .collect()
}

/// Renames the types a program declares in `names`, everywhere the program names them.
pub fn rename_types(module: &mut Module, names: &BTreeMap<String, String>) {
    if names.is_empty() {
        return;
    }
    let renamer = Renamer { names, self_args: &[] };
    for item in &mut module.items {
        renamer.item(item);
    }
    crate::visit::exprs_mut(module, &mut |expr| renamer.in_expr(expr));
}

/// Gives each `_` a name of its own, since it throws away what it's given, whatever type that is.
/// No one can write the name, so nothing can read it.
pub fn discards_apart(module: &mut Module) {
    let mut made = 0;
    crate::visit::exprs_mut(module, &mut |expr| {
        if matches!(&expr.kind, ExprKind::Name(name) if name == "_") {
            expr.kind = ExprKind::Name(format!("_{}{made}", crate::visit::HIDDEN));
            made += 1;
        }
    });
}

/// Writes `Self`, in a type's own methods, as that type. An interface's methods keep it, since it
/// means each type that fits.
pub fn self_as_its_type(module: &mut Module) {
    for item in &mut module.items {
        let (owner, generics, methods) = match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                (&def.name, &def.generics, &mut def.methods)
            }
            Item::Extend(extension) => (&extension.name, &extension.generics, &mut extension.methods),
            _ => continue,
        };
        let names = BTreeMap::from([("Self".to_owned(), owner.clone())]);
        let self_args: Vec<TypeExpr> = generics
            .iter()
            .map(|generic| TypeExpr {
                name: generic.name.clone(),
                args: Vec::new(),
                optional: false,
                span: generic.span,
            })
            .collect();
        let renamer = Renamer { names: &names, self_args: &self_args };
        for method in methods {
            renamer.function(method);
            crate::visit::in_function(method, &mut |expr| renamer.in_expr(expr));
        }
    }
}

struct Renamer<'a> {
    names: &'a BTreeMap<String, String>,
    /// What `Self` takes in brackets, when it's what's being renamed.
    self_args: &'a [TypeExpr],
}

impl Renamer<'_> {
    fn name(&self, name: &mut String) {
        if let Some(renamed) = self.names.get(name.as_str()) {
            renamed.clone_into(name);
        } else if let Some((owner, rest)) = name.split_once('.')
            && let Some(renamed) = self.names.get(owner)
        {
            *name = format!("{renamed}.{rest}");
        }
    }

    fn in_expr(&self, expr: &mut crate::ast::Expr) {
        match &mut expr.kind {
            ExprKind::Name(name) => self.name(name),
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.pattern(&mut arm.pattern);
                    self.stmts(&mut arm.body);
                }
            }
            _ => {}
        }
    }

    fn type_expr(&self, ty: &mut TypeExpr) {
        if ty.name == "Self" && ty.args.is_empty() {
            ty.args = self.self_args.to_vec();
        }
        self.name(&mut ty.name);
        for arg in &mut ty.args {
            self.type_expr(arg);
        }
    }

    fn item(&self, item: &mut Item) {
        match item {
            Item::Function(def) => self.function(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                self.name(&mut def.name);
                self.bounds(&mut def.generics);
                let fields =
                    def.fields.iter_mut().chain(def.cases.iter_mut().flat_map(|case| case.fields.iter_mut()));
                for field in fields {
                    if let Some(ty) = &mut field.ty {
                        self.type_expr(ty);
                    }
                }
                def.methods.iter_mut().for_each(|method| self.function(method));
            }
            Item::Interface(interface) => {
                self.name(&mut interface.name);
                interface.methods.iter_mut().for_each(|method| self.function(method));
            }
            Item::Extend(extension) => {
                self.name(&mut extension.name);
                self.bounds(&mut extension.generics);
                extension.methods.iter_mut().for_each(|method| self.function(method));
            }
            Item::TypeAlias { name, ty, .. } => {
                self.name(name);
                self.type_expr(ty);
            }
            Item::Const { ty: Some(ty), .. } => self.type_expr(ty),
            Item::Test(test) => {
                for input in &mut test.inputs {
                    if let Some(ty) = &mut input.ty {
                        self.type_expr(ty);
                    }
                }
                self.stmts(&mut test.body);
            }
            Item::Stmt(stmt) => self.stmts(std::slice::from_mut(stmt)),
            Item::Const { ty: None, .. } | Item::Import(_) => {}
        }
    }

    fn function(&self, def: &mut FnDef) {
        let types = def
            .params
            .iter_mut()
            .filter_map(|param| param.ty.as_mut())
            .chain(def.ret.as_mut())
            .chain(def.fails.as_mut());
        for ty in types {
            self.type_expr(ty);
        }
        self.bounds(&mut def.generics);
        self.stmts(&mut def.body);
    }

    fn bounds(&self, generics: &mut [Generic]) {
        for generic in generics {
            generic.bounds.iter_mut().for_each(|bound| self.type_expr(bound));
        }
    }

    /// The types written in statements, which `exprs_mut` doesn't reach.
    fn stmts(&self, body: &mut [Stmt]) {
        for stmt in body {
            match &mut stmt.kind {
                StmtKind::Assign { ty: Some(ty), .. } => self.type_expr(ty),
                StmtKind::If { branches, otherwise } => {
                    for (_, block) in branches {
                        self.stmts(block);
                    }
                    self.stmts(otherwise.as_deref_mut().unwrap_or_default());
                }
                StmtKind::While { body: block, .. }
                | StmtKind::For { body: block, .. }
                | StmtKind::With { body: block, .. } => self.stmts(block),
                StmtKind::OnFailure { cleanup, body, .. } => {
                    self.stmts(cleanup);
                    self.stmts(body);
                }
                StmtKind::Def(def) => self.function(def),
                _ => {}
            }
        }
    }

    fn pattern(&self, pattern: &mut Pattern) {
        match pattern {
            Pattern::Variant { name, dotted: false, args, .. } => {
                self.name(name);
                for arg in args {
                    self.pattern(arg);
                }
            }
            Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
                for arg in args {
                    self.pattern(arg);
                }
            }
            Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) | Pattern::Literal(_) => {}
        }
    }
}
