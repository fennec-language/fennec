//! What a module imports from the standard library, and what the names it binds mean.

use std::collections::HashMap;

use fennec_syntax::{Expr, ExprKind, Item, Module};

use super::{Lowered, Lowering};
use crate::{InstKind, Operand};

/// The names an import binds.
#[derive(Default)]
pub(super) struct Imports {
    /// A whole module, by what it's bound under.
    modules: HashMap<String, String>,
    /// One name from a module, by what it's bound under: its module, and its own name.
    names: HashMap<String, (String, String)>,
}

pub(super) fn written(module: &Module) -> Imports {
    let mut imports = Imports::default();
    for item in &module.items {
        let Item::Import(import) = item else { continue };
        if import.module.first().map(String::as_str) != Some("std") {
            continue;
        }
        let path = import.module.join(".");
        let last = import.module.last().cloned().unwrap_or_default();
        if import.module.len() == 1 {
            for (name, bound) in import.bound() {
                imports.modules.insert(bound.to_owned(), name.to_owned());
            }
        } else if import.names.is_empty() {
            imports.modules.insert(import.alias.clone().unwrap_or(path), last);
        } else {
            for (name, bound) in import.bound() {
                imports.names.insert(bound.to_owned(), (last.clone(), name.to_owned()));
            }
        }
    }
    imports
}

impl Lowering<'_> {
    /// What the standard library calls a name imported from it: its full name when the
    /// engines provide it, like `math.sqrt`, and its own name when it's written in Fennec.
    pub(super) fn std_member(&self, expr: &Expr) -> Option<String> {
        let imports = &self.context.imports;
        let (module, name) = match &expr.kind {
            ExprKind::Name(bound) if self.lookup(bound).is_none() => imports.names.get(bound)?.clone(),
            ExprKind::Attr { object, name, optional: false } => {
                let alias = fennec_syntax::dotted(object)?;
                if self.lookup(alias.split('.').next().unwrap_or_default()).is_some() {
                    return None;
                }
                (imports.modules.get(&alias)?.clone(), name.clone())
            }
            _ => return None,
        };
        let full = format!("{module}.{name}");
        Some(if fennec_check::is_builtin(&full) { full } else { name })
    }

    /// A value the engines provide, like `math.PI`.
    pub(super) fn std_value(&mut self, name: String, expr: &Expr) -> Lowered<Operand> {
        let dest = self.temporary(self.ty(expr)?);
        let kind = InstKind::Builtin { dest: Some(dest), name, args: Vec::new(), changes: false };
        self.emit(kind, expr.span);
        Ok(Operand::Local(dest))
    }
}
