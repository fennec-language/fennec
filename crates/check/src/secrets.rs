//! A `Secret` from `std.crypto` is never shown: printing one, putting it in an f-string, or
//! making text of it with `str()` is a compile error, and so is showing anything that holds
//! one. Its text comes out only through `expose_secret()`, so every read of a secret can be found
//! by that name. Saving one is refused as for any type that can't be saved, since it has no
//! `serialize`. Printed some other way, as through a generic function's `T`, it shows only
//! `Secret(hidden)`.

use fennec_syntax::{Diagnostic, Span};

use crate::function::FnCx;
use crate::types::Type;

/// The type that holds secrets.
const SECRET: &str = "Secret";

impl FnCx<'_> {
    /// Reports a value about to be shown as text, by `how`, when it's a `Secret` or holds one.
    pub(crate) fn not_a_secret(&mut self, ty: &Type, how: &str, at: Span) {
        let ty = self.checker.settled(ty);
        let Some(place) = self.secret_in(&ty, "", &mut Vec::new()) else { return };
        let message = if place.is_empty() {
            format!("{how} would show a secret: a `Secret` never shows its text")
        } else {
            format!("{how} would show a secret: `{place}` in it is a `Secret`, which never shows its text")
        };
        let help = "to show it on purpose, write `.expose_secret()` where you mean to reveal it";
        self.error(Diagnostic::new(message, at).with_help(help));
    }

    /// Where a `Secret` is in a value of this type, as a path from it, or "" when it's one itself.
    fn secret_in(&mut self, ty: &Type, path: &str, open: &mut Vec<String>) -> Option<String> {
        if let Some(name) = ty.type_name() {
            if name == SECRET {
                return Some(path.to_owned());
            }
            if !open.iter().any(|seen| seen == name) {
                open.push(name.to_owned());
                let found = self.secret_in_fields(name, path, open);
                open.pop();
                if found.is_some() {
                    return found;
                }
            }
        }
        let held: Vec<Type> = match ty {
            Type::Tuple(items) | Type::Union(items) => items.clone(),
            other => other.children().into_iter().cloned().chain(other.type_args()).collect(),
        };
        held.iter().find_map(|inner| self.secret_in(inner, path, open))
    }

    /// Where a `Secret` is among the fields of the struct or enum `name`.
    fn secret_in_fields(&mut self, name: &str, path: &str, open: &mut Vec<String>) -> Option<String> {
        let def = self.checker.program.type_info(name)?.def.clone();
        let fields = def.fields.iter().map(|field| (None, field));
        let in_cases = def.cases.iter().flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
        for (case, field) in fields.chain(in_cases) {
            let ty = self.checker.field_type(name, case, &field.name);
            let place = if path.is_empty() { field.name.clone() } else { format!("{path}.{}", field.name) };
            if let Some(found) = self.secret_in(&ty, &place, open) {
                return Some(found);
            }
        }
        None
    }
}
