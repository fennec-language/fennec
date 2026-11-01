//! Giving a program's own types other names. A program can declare a type with the same name as
//! one in the standard library, and the library's code still means its own. The checker gives
//! the program's type a name nobody can write, and shows it as written.

use std::collections::BTreeMap;

use crate::ast::{ExprKind, FnDef, Item, Module, Pattern, Stmt, StmtKind, TypeExpr};

/// What a renamed type is shown as: the name it was written with.
pub fn shown_type_name(name: &str) -> &str {
    name.strip_suffix(RENAMED).unwrap_or(name)
}

/// What marks a renamed type. It can't be written, so it can't clash with anything.
pub const RENAMED: &str = "'";

/// Renames the types a program declares in `names`, everywhere the program names them.
pub fn rename_types(module: &mut Module, names: &BTreeMap<String, String>) {
    if names.is_empty() {
        return;
    }
    let renamer = Renamer { names };
    for item in &mut module.items {
        renamer.item(item);
    }
    crate::visit::exprs_mut(module, &mut |expr| match &mut expr.kind {
        ExprKind::Name(name) => renamer.name(name),
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                renamer.pattern(&mut arm.pattern);
                renamer.stmts(&mut arm.body);
            }
        }
        _ => {}
    });
}

struct Renamer<'a> {
    names: &'a BTreeMap<String, String>,
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

    fn type_expr(&self, ty: &mut TypeExpr) {
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
                interface.methods.iter_mut().for_each(|method| self.function(method));
            }
            Item::Extend(extension) => {
                self.name(&mut extension.name);
                extension.methods.iter_mut().for_each(|method| self.function(method));
            }
            Item::TypeAlias { ty, .. } | Item::Const { ty: Some(ty), .. } => self.type_expr(ty),
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
        let types = def.params.iter_mut().filter_map(|param| param.ty.as_mut()).chain(def.ret.as_mut());
        for ty in types {
            self.type_expr(ty);
        }
        for generic in &mut def.generics {
            generic.bounds.iter_mut().for_each(|bound| self.type_expr(bound));
        }
        self.stmts(&mut def.body);
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
                StmtKind::Def(def) => self.function(def),
                _ => {}
            }
        }
    }

    fn pattern(&self, pattern: &mut Pattern) {
        match pattern {
            Pattern::Variant { name, dotted: false, args } => {
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
            Pattern::Wildcard | Pattern::Bind(_) | Pattern::Literal(_) => {}
        }
    }
}
