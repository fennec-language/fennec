//! Field defaults, worked out each time a value is built without one, apart from the
//! function building it.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Expr, Field, Item, Module, TypeDef};

use super::{Lowered, Lowering};
use crate::Operand;

/// A field by its type, its case for an enum, and its name.
pub(super) type FieldKey<'a> = (&'a str, Option<&'a str>, &'a str);

/// The default of every field that has one, in the module and the standard library.
pub(super) fn written(module: &Module) -> HashMap<FieldKey<'_>, &Expr> {
    let library = fennec_syntax::prelude_modules().unwrap_or_default();
    let mut found = HashMap::new();
    for item in library.iter().chain([module]).flat_map(|module| &module.items) {
        if let Item::Struct(def) | Item::Enum(def) | Item::Error(def) = item {
            add(&mut found, def);
        }
    }
    found
}

/// The `where` rules of each struct in the module and the standard library.
pub(super) fn rules(module: &Module) -> HashMap<&str, Vec<(&str, &Expr)>> {
    let library = fennec_syntax::prelude_modules().unwrap_or_default();
    let mut found = HashMap::new();
    for item in library.iter().chain([module]).flat_map(|module| &module.items) {
        if let Item::Struct(def) = item {
            let rules: Vec<(&str, &Expr)> = def
                .fields
                .iter()
                .filter_map(|field| field.rule.as_ref().map(|rule| (field.name.as_str(), rule)))
                .collect();
            if !rules.is_empty() {
                found.insert(def.name.as_str(), rules);
            }
        }
    }
    found
}

fn add<'a>(found: &mut HashMap<FieldKey<'a>, &'a Expr>, def: &'a TypeDef) {
    let mut note = |case: Option<&'a str>, fields: &'a [Field]| {
        for field in fields {
            if let Some(default) = &field.default {
                found.insert((def.name.as_str(), case, field.name.as_str()), default);
            }
        }
    };
    note(None, &def.fields);
    for case in &def.cases {
        note(Some(case.name.as_str()), &case.fields);
    }
}

impl Lowering<'_> {
    /// The default of a field left out of a value being built.
    pub(super) fn field_default(
        &mut self,
        (owner, case): (&str, Option<&str>),
        field: &str,
        ty: &Type,
    ) -> Lowered<Operand> {
        let context = self.context;
        let instance = context.type_instance(owner);
        let base = instance.as_ref().map_or(owner, |copy| copy.base.as_str());
        let shown = case.map_or_else(|| owner.to_owned(), |case| format!("{owner}.{case}"));
        let default = context.defaults.get(&(base, case, field)).copied();
        let default = default.ok_or_else(|| format!("`{shown}` has no default for `{field}`"))?;
        let body = context.checked.types.body(&format!("fields of {base}"));
        let body = body.ok_or_else(|| format!("the fields of `{owner}` have no types"))?;
        let copy_of = instance.as_ref().map(|copy| copy.subst.clone()).unwrap_or_default();
        let outer_subst = std::mem::replace(&mut self.subst, copy_of);
        let outer_body = std::mem::replace(&mut self.body, body);
        let outer_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        let value = self.expr(default);
        self.body = outer_body;
        self.scopes = outer_scopes;
        self.subst = outer_subst;
        self.coerce(value?, ty, default.span)
    }
}
