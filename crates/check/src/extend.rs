//! `extend Name:`, which adds methods to one of the program's own types from anywhere in
//! the kit. An extension's methods are the type's, exactly like the ones in its declaration.
//! A generic type's extension names its parameters, `extend Box[T]:`, and a bound on one,
//! `extend list[T: Eq]:`, makes its methods exist only where the type's arguments fit.

use fennec_syntax::{Diagnostic, Extension, FnDef, Generic};

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
            None if Type::built_in(name).is_some() || built_in_params(name).is_some() => None,
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
        // With its parameters written wrong, a user type's extension still adds its methods,
        // with the declaration's parameters, so their calls aren't reported too.
        if let Some(problem) = self.wrong_params(extension) {
            self.problems.push(problem);
            if self.type_info(name).is_some() {
                for method in &extension.methods {
                    self.add_method(name, method, prelude, &[]);
                }
            }
            return;
        }
        for bound in extension.generics.iter().flat_map(|generic| &generic.bounds) {
            if !self.is_interface(&bound.name) {
                self.problems.push(
                    Diagnostic::new(format!("`{}` isn't an interface", bound.name), bound.span).with_help(
                        "a type parameter can be bound to an interface, like `T: Ordered` or `T: Shape`",
                    ),
                );
            }
        }
        for method in &extension.methods {
            self.add_method(name, method, prelude, &extension.generics);
        }
    }

    /// An extension that doesn't name its type's parameters as the type declares them.
    fn wrong_params(&self, extension: &Extension) -> Option<Diagnostic> {
        let name = &extension.name;
        let written: Vec<&str> = extension.generics.iter().map(|generic| generic.name.as_str()).collect();
        let declared: Vec<String> = match built_in_params(name) {
            Some(params) if written.len() == params.len() => return None,
            Some(params) => params.iter().map(|param| (*param).to_owned()).collect(),
            None => self.type_params(name),
        };
        if written == declared {
            return None;
        }
        let diagnostic = if declared.is_empty() {
            Diagnostic::new(
                format!("`{name}` isn't generic, so its extension takes no type parameters"),
                extension.span,
            )
            .with_help(format!("write `extend {name}:`"))
        } else {
            Diagnostic::new(
                format!("`{name}` is generic, so its extension names its type parameters"),
                extension.span,
            )
            .with_help(format!("write `extend {name}[{}]:`", declared.join(", ")))
        };
        Some(diagnostic)
    }

    /// Adds one method to a type, unless the type already has a method of that name.
    /// `extension` is its extension's type parameters, if it comes from one of a generic type.
    pub(crate) fn add_method(
        &mut self,
        type_name: &str,
        method: &FnDef,
        prelude: bool,
        extension: &[Generic],
    ) {
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
        let entry = FnEntry {
            def: method.clone(),
            owner,
            prelude,
            inherited: Vec::new(),
            extension: extension.to_vec(),
        };
        self.functions.insert(key, entry);
    }
}

/// An extension of a type that isn't one of the program's own.
fn not_own(name: &str, why: &str, extension: &Extension) -> Diagnostic {
    Diagnostic::new(format!("`{name}` {why}, so it can't be extended"), extension.span).with_help(format!(
        "an extension adds methods to one of this kit's own types; write a function that takes a `{name}` instead"
    ))
}

/// The type parameters of a generic built-in type the standard library extends, as its
/// help suggests naming them.
fn built_in_params(name: &str) -> Option<&'static [&'static str]> {
    (name == "list").then_some(&["T"])
}
