//! What a module imports from the standard library, and what the names it binds mean.

use std::collections::HashMap;

use fennec_syntax::{Expr, ExprKind, Import, Item, Module, Span};

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

/// What each file of a program imports, and what lowering imports for the code it writes in. A
/// name's import is the one in the file it's used in, never another file's.
#[derive(Default)]
pub(super) struct ByFile {
    files: Vec<Span>,
    /// Where each copy of the program's text written in went: code in one sees the imports of the
    /// file its text is in.
    copied: Vec<fennec_syntax::Copied>,
    each: Vec<Imports>,
    written: Imports,
}

impl ByFile {
    /// The imports code at `span` sees: its own file's, then what lowering imported, under names
    /// no file can write.
    fn at(&self, span: Span) -> impl Iterator<Item = &Imports> {
        let own = match fennec_check::place(&self.files, span.copied_from(&self.copied)) {
            fennec_check::Place::File(at) => self.each.get(at),
            fennec_check::Place::Std | fennec_check::Place::Written => None,
        };
        own.into_iter().chain([&self.written])
    }
}

pub(super) fn written(module: &Module) -> ByFile {
    let mut by_file =
        ByFile { files: module.files.clone(), copied: module.copied.clone(), ..ByFile::default() };
    for item in &module.items {
        let Item::Import(import) = item else { continue };
        let imports = match fennec_check::import_place(&module.files, import.span) {
            fennec_check::Place::File(at) => {
                if by_file.each.len() <= at {
                    by_file.each.resize_with(at + 1, Imports::default);
                }
                match by_file.each.get_mut(at) {
                    Some(imports) => imports,
                    None => &mut by_file.written,
                }
            }
            fennec_check::Place::Std | fennec_check::Place::Written => &mut by_file.written,
        };
        add(imports, import);
    }
    by_file
}

/// What a group of modules import, all together.
pub(super) fn of_library(modules: &[Module]) -> Imports {
    let mut imports = Imports::default();
    for item in modules.iter().flat_map(|module| &module.items) {
        if let Item::Import(import) = item {
            add(&mut imports, import);
        }
    }
    imports
}

/// The names an import from the standard library binds, put among `imports`.
fn add(imports: &mut Imports, import: &Import) {
    if import.module.first().map(String::as_str) != Some("std") {
        return;
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

impl Lowering<'_> {
    /// What the standard library calls a name imported from it: its full name when the
    /// engines provide it, like `math.sqrt`, and its own name when it's written in Fennec.
    pub(super) fn std_member(&self, expr: &Expr) -> Option<String> {
        let in_library = self.name.split('.').scan(String::new(), |path, part| {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(part);
            Some(path.clone())
        });
        let seen: Vec<&Imports> =
            if in_library.into_iter().any(|path| self.context.library.contains(path.as_str())) {
                vec![&self.context.library_imports]
            } else {
                self.context.imports.at(expr.span).collect()
            };
        let (module, name) = match &expr.kind {
            ExprKind::Name(bound) if self.lookup(bound).is_none() => {
                seen.iter().find_map(|imports| imports.names.get(bound))?.clone()
            }
            ExprKind::Attr { object, name, optional: false } => {
                let alias = fennec_syntax::dotted(object)?;
                if self.lookup(alias.split('.').next().unwrap_or_default()).is_some() {
                    return None;
                }
                (seen.iter().find_map(|imports| imports.modules.get(&alias))?.clone(), name.clone())
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
