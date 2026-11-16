//! Field defaults, worked out each time a value is built without one, apart from the
//! function building it.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Expr, Field, Item, Module, Span, TypeDef};

use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, Terminator};

/// A field by its type, its case for an enum, and its name.
pub(super) type FieldKey<'a> = (&'a str, Option<&'a str>, &'a str);

/// The default of every field that has one, in the module and the standard library.
pub(super) fn written(module: &Module) -> HashMap<FieldKey<'_>, &Expr> {
    let library = fennec_syntax::prelude_to_run().unwrap_or_default();
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
    let library = fennec_syntax::prelude_to_run().unwrap_or_default();
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
        let body = context.signatures.types().body(&format!("fields of {base}"));
        let body = body.ok_or_else(|| format!("the fields of `{owner}` have no types"))?;
        let copy_of = instance.as_ref().map(|copy| copy.subst.clone()).unwrap_or_default();
        let outer_subst = std::mem::replace(&mut self.subst, copy_of);
        let outer_body = std::mem::replace(&mut self.body, body);
        let outer_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        let value = self.expr(default);
        self.body = outer_body;
        self.scopes = outer_scopes;
        self.subst = outer_subst;
        let value = self.shared_when_empty(value?, default.span);
        self.coerce(value, ty, default.span)
    }

    /// An empty list, dict or set a field defaults to, read as a constant: made once, and handed
    /// to each value built with the default as one more owner of it. Making one for every value
    /// cost an allocation each, for a field most values never write, like a response's headers.
    /// The first write to one finds it shared and makes a block of its own then, as a write to
    /// any shared value does, so nothing written shows in another value or in the constant.
    fn shared_when_empty(&mut self, value: Operand, span: Span) -> Operand {
        let Operand::Local(dest) = value else { return value };
        let Some(ty) = self.locals.get(dest.0).map(|local| local.ty.clone()) else { return value };
        if !matches!(ty, Type::List(_) | Type::Set(_) | Type::Dict(..)) {
            return value;
        }
        // What made it, when that's the last thing done and it's made with nothing in it: `[]`,
        // `{}` or `set()`, however the default was written. The constant is made the same way.
        let last = self.blocks.get(self.current.0).and_then(|block| block.insts.last());
        let made = |held: LocalId| match last.map(|inst| &inst.kind) {
            Some(InstKind::MakeList { dest: made, items }) if *made == dest && items.is_empty() => {
                Some(InstKind::MakeList { dest: held, items: Vec::new() })
            }
            Some(InstKind::MakeSet { dest: made, items }) if *made == dest && items.is_empty() => {
                Some(InstKind::MakeSet { dest: held, items: Vec::new() })
            }
            Some(InstKind::MakeDict { dest: made, pairs }) if *made == dest && pairs.is_empty() => {
                Some(InstKind::MakeDict { dest: held, pairs: Vec::new() })
            }
            Some(InstKind::Builtin { dest: Some(made), name, args, changes })
                if *made == dest && name == "set" && args.is_empty() =>
            {
                Some(InstKind::Builtin {
                    dest: Some(held),
                    name: name.clone(),
                    args: Vec::new(),
                    changes: *changes,
                })
            }
            _ => None,
        };
        if made(dest).is_none() {
            return value;
        }
        let name = format!("empty {ty}");
        if !self.context.empties.borrow().iter().any(|function| function.name == name) {
            let instance = super::generics::FunctionInstance::plain(&name);
            let mut constant = Lowering::new(&instance, self.context, self.body, ty.clone());
            let held = constant.temporary(ty.clone());
            let Some(kind) = made(held) else { return value };
            constant.emit(kind, span);
            constant.finish(Terminator::Return(Some(Operand::Local(held))));
            self.context.empties.borrow_mut().extend(constant.finished(0, Vec::new()));
        }
        if let Some(block) = self.blocks.get_mut(self.current.0) {
            block.insts.pop();
        }
        self.emit(InstKind::Constant { dest, name }, span);
        value
    }
}
