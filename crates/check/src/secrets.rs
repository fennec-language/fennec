//! A `Secret` from `std.crypto` is never shown: printing one, putting it in an f-string, or
//! making text of it with `str()` is a compile error, and so is showing anything that holds
//! one. Its text comes out only through `expose_secret()`, so every read of a secret can be found
//! by that name. Saving one is refused as for any type that can't be saved, since it has no
//! `serialize`. Printed some other way, as through a generic function's `T`, it shows only
//! `Secret(hidden)`.

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FStrPart, Span};

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

    /// What `Stdout.print` and `Stdout.write` are given is shown, so none of it can be a secret.
    pub(crate) fn no_secret_printed(&mut self, ty: &Type, method: &str, given: &[(Type, Span)]) {
        if *ty == Type::named("Stdout") && matches!(method, "print" | "write") {
            for (given, at) in given {
                self.not_a_secret(given, "printing this", *at);
            }
        }
    }

    /// `==` on a `Secret` compares in constant time, which works for text and bytes: a secret of
    /// anything else is refused there rather than compared some way that could leak timing.
    pub(crate) fn secrets_compared(&mut self, left: &Type, at: Span) {
        let ty = self.checker.settled(left);
        if ty.type_name() != Some(SECRET) {
            return;
        }
        let Some(inside) = ty.type_args().into_iter().next() else { return };
        let held = self.checker.settled(&inside);
        if matches!(held, Type::Str | Type::Bytes | Type::Unknown | Type::Var(_) | Type::Param(_)) {
            return;
        }
        let message = format!(
            "`==` compares secrets in constant time, which it can do for text or bytes, not {}",
            crate::types::a(&held)
        );
        let fix = "hold it as text or bytes, or compare what `expose_secret()` gives";
        self.error(Diagnostic::new(message, at).with_help(fix));
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
        let in_cases = def
            .cases
            .iter()
            .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
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

/// The `std.postgres` functions that take a database URL, with where the URL goes when it's
/// given by position.
const URL_TAKERS: [(&str, usize); 2] = [("postgres.connect", 2), ("postgres.settings", 0)];

impl FnCx<'_> {
    /// `connect(...)` after `from std.postgres import connect`, checked as `postgres.connect` is
    /// where it's found through the module.
    pub(crate) fn no_password_in_imported_url(&mut self, callee: &Expr, args: &[Arg]) {
        if let ExprKind::Name(name) = &callee.kind
            && self.lookup(name).is_none()
            && let Some(function) = self.checker.program.std_functions.get(name).cloned()
        {
            self.no_password_in_written_url(&function, args);
        }
    }

    /// A database URL written in the source with a password in it is refused: the password would
    /// be in a plain `str`, and in the source too. A URL with a password has to be a `Secret`,
    /// read from where it's kept, and `postgres.connect` refuses a plain one with a password
    /// when it runs as well.
    pub(crate) fn no_password_in_written_url(&mut self, function: &str, args: &[Arg]) {
        let Some((_, at)) = URL_TAKERS.iter().find(|(name, _)| *name == function) else { return };
        let given = args
            .iter()
            .find(|arg| arg.name.as_deref() == Some("url"))
            .or_else(|| args.iter().filter(|arg| arg.name.is_none()).nth(*at));
        let Some(given) = given else { return };
        let written = match &given.value.kind {
            ExprKind::Str(text) => text.clone(),
            ExprKind::FStr(parts) => parts
                .iter()
                .map_while(|part| match part {
                    FStrPart::Text(text) => Some(text.as_str()),
                    FStrPart::Expr { .. } => None,
                })
                .collect(),
            _ => return,
        };
        if has_password(&written) {
            let help = "read the URL into a `Secret` from where it's kept, as `env.secret(\"DATABASE_URL\")?` does, or leave the password out of the URL and pass it as `password=`, a `Secret`";
            self.error(
                Diagnostic::new("this database URL has its password written in the source", given.value.span)
                    .with_help(help),
            );
        }
    }
}

/// Whether a URL's text has a password: a `:` in what comes before the `@` that ends who's
/// connecting, as in `postgres://user:password@host`.
pub(crate) fn has_password(url: &str) -> bool {
    let after = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after.split(['/', '?', '#']).next().unwrap_or_default();
    authority.rsplit_once('@').is_some_and(|(who, _)| who.contains(':'))
}
