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
    pub(crate) fn add_extension<'m>(
        &mut self,
        extension: &'m Extension,
        prelude: bool,
        hold: &impl crate::program::Holds<'m>,
    ) {
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
                    self.add_method(name, method, prelude, &[], hold);
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
            // A protocol method takes what the protocol passes, as one in the type's body does.
            if !prelude && let Some(problem) = crate::protocols::protocol_problem(name, method) {
                self.problems.push(problem);
                continue;
            }
            self.add_method(name, method, prelude, &extension.generics, hold);
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
    pub(crate) fn add_method<'m>(
        &mut self,
        type_name: &str,
        method: &'m FnDef,
        prelude: bool,
        extension: &[Generic],
        hold: &impl crate::program::Holds<'m>,
    ) {
        let plain = format!("{type_name}.{}", method.name);
        let first = self.functions.get(&plain).map(|existing| existing.def.clone());
        let key = method_key(type_name, method, first.as_deref());
        if let Some(existing) = self.functions.get(&key) {
            let problem = match operator_right(method) {
                Some(right) => Diagnostic::new(
                    format!("`{type_name}` already has a `{}` taking {right} on its right", method.name),
                    method.span,
                )
                .with_help("an operator is defined once for each type on its right: take another type, or remove one"),
                None => Diagnostic::new(
                    format!("`{type_name}` already has a method called `{}`", method.name),
                    method.span,
                )
                .with_help("rename one of them"),
            }
            .also_at(existing.def.span);
            self.problems.push(problem);
            return;
        }
        let owner = Some(type_name.to_owned());
        let entry = FnEntry {
            def: hold.function(method),
            owner,
            prelude,
            inherited: Vec::new(),
            extension: extension.to_vec(),
        };
        self.functions.insert(key, entry);
    }
}

/// What starts the end of the key of an operator defined again for another type on its right.
pub(crate) const OTHER_RIGHT: &str = "@";

/// The key a method of `owner` is known by, `first` being the method of its name it has already,
/// if any. An operator may be defined again for another type on its right, as in `Mat4 * Mat4`
/// and `Mat4 * Vec4`: the first has the operator's own key, and each other one's ends with the
/// type it takes, `Mat4.*@Vec4`.
#[must_use]
pub fn method_key(owner: &str, method: &FnDef, first: Option<&FnDef>) -> String {
    let key = format!("{owner}.{}", method.name);
    match (first, operator_right(method)) {
        (Some(first), Some(other)) if operator_right(first).is_some_and(|right| right != other) => {
            format!("{key}{OTHER_RIGHT}{other}")
        }
        _ => key,
    }
}

/// The type an operator method takes on its right, as written, if it's an operator.
fn operator_right(method: &FnDef) -> Option<String> {
    let operator = method.name.chars().next().is_some_and(|first| !first.is_alphanumeric() && first != '_')
        && !method.name.starts_with('[');
    let right = method.params.get(1)?.ty.as_ref()?;
    operator.then(|| crate::program::written(right))
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
    match name {
        "list" | "set" | "Buffer" | "Optional" | "Id" | "Table" => Some(&["T"]),
        "dict" => Some(&["K", "V"]),
        _ => None,
    }
}
