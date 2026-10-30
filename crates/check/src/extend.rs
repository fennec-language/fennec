//! `extend Name:`, which adds methods to one of the program's own types from anywhere in
//! the kit. An extension's methods are the type's, exactly like the ones in its declaration.

use fennec_syntax::{Diagnostic, Extension, FnDef};

use crate::program::{BUILT_IN_TYPES, FnEntry, Program, unknown_type};
use crate::types::Type;

impl Program {
    /// Adds an extension's methods to its type, once every type is known. The standard
    /// library's own code may extend its own types, built-in ones included.
    pub(crate) fn add_extension(&mut self, extension: &Extension, prelude: bool) {
        let name = &extension.name;
        let problem = match self.type_info(name) {
            Some(info) if info.prelude && !prelude => {
                Some(not_own(name, "is from the standard library", extension))
            }
            Some(_) => None,
            None if BUILT_IN_TYPES.contains(&name.as_str()) && !prelude => {
                Some(not_own(name, "is built in", extension))
            }
            None if Type::built_in(name).is_some() => None,
            None if self.is_interface(name) => Some(
                Diagnostic::new(format!("`{name}` is an interface, so it can't be extended"), extension.span)
                    .with_help(format!("declare its methods in `interface {name}:`")),
            ),
            None => Some(unknown_type(name, extension.span)),
        };
        if let Some(problem) = problem {
            self.problems.push(problem);
            return;
        }
        for method in &extension.methods {
            self.add_method(name, method, prelude);
        }
    }

    /// Adds one method to a type, unless the type already has a method of that name.
    pub(crate) fn add_method(&mut self, type_name: &str, method: &FnDef, prelude: bool) {
        let key = format!("{type_name}.{}", method.name);
        if let Some(existing) = self.functions.get(&key) {
            let problem = Diagnostic::new(
                format!("`{type_name}` already has a method called `{}`", method.name),
                method.span,
            )
            .with_help("rename one of them")
            .also_at(existing.def.span);
            self.problems.push(problem);
            return;
        }
        let owner = Some(type_name.to_owned());
        let entry = FnEntry { def: method.clone(), owner, prelude, inherited: Vec::new() };
        self.functions.insert(key, entry);
    }
}

/// An extension of a type that isn't one of the program's own.
fn not_own(name: &str, why: &str, extension: &Extension) -> Diagnostic {
    Diagnostic::new(format!("`{name}` {why}, so it can't be extended"), extension.span).with_help(format!(
        "an extension adds methods to one of this kit's own types; write a function that takes a `{name}` instead"
    ))
}
