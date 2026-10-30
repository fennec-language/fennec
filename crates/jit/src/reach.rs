//! Which standard library types a program uses, so the JIT compiles only their methods. A
//! method it skips still runs, in the interpreter, if it's called after all.

use std::collections::BTreeSet;

use fennec_syntax::{FnDef, Item, Module, TypeDef, TypeExpr};

use crate::lower::fallback::{expr_names, stmt_names};

/// Types a program gets values of without naming them, from a capability's methods.
const GIVEN: [(&str, &str); 1] = [("Clock", "Instant")];

/// The standard library types `module` names, and the ones those name in turn.
pub(crate) fn used_types(module: &Module, prelude: &[Module]) -> BTreeSet<String> {
    let types: Vec<&TypeDef> = prelude
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(def),
            _ => None,
        })
        .collect();
    let mut pending = Vec::new();
    for item in &module.items {
        item_names(item, &mut pending);
    }
    let mut used = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if let Some((_, given)) = GIVEN.iter().find(|(capability, _)| *capability == name) {
            pending.push((*given).to_owned());
        }
        if used.contains(&name) {
            continue;
        }
        if let Some(def) = types.iter().find(|def| def.name == name) {
            type_def_names(def, &mut pending);
            used.insert(name);
        }
    }
    used
}

fn item_names(item: &Item, names: &mut Vec<String>) {
    match item {
        Item::Function(def) => fn_names(def, names),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => type_def_names(def, names),
        Item::Stmt(stmt) => stmt_names(stmt, names),
        _ => {}
    }
}

fn type_def_names(def: &TypeDef, names: &mut Vec<String>) {
    for field in def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)) {
        if let Some(ty) = &field.ty {
            type_names(ty, names);
        }
        for expr in field.default.iter().chain(&field.rule) {
            expr_names(expr, names);
        }
    }
    for method in &def.methods {
        fn_names(method, names);
    }
}

fn fn_names(def: &FnDef, names: &mut Vec<String>) {
    for ty in def.params.iter().filter_map(|param| param.ty.as_ref()).chain(&def.ret) {
        type_names(ty, names);
    }
    for stmt in &def.body {
        stmt_names(stmt, names);
    }
}

fn type_names(ty: &TypeExpr, names: &mut Vec<String>) {
    names.push(ty.name.clone());
    for arg in &ty.args {
        type_names(arg, names);
    }
}
