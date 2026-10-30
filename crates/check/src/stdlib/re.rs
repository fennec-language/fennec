//! `std.re`: `re.is_regex`, and the methods of `Regex`, which run in Rust. `Regex` and
//! `Match` themselves are written in Fennec.

use fennec_syntax::{Arg, Diagnostic, ExprKind, Span};

use super::{Function, Module};
use crate::builtins::{Method, reads};
use crate::function::FnCx;
use crate::types::Type;

pub(crate) const MODULE: Module = Module {
    functions: &[Function { name: "re.is_regex", arity: (1, 1), named: &[], needs_values: false }],
    result,
    method,
    construct,
};

fn result(_: &mut FnCx<'_>, _: &str, _: &[(Type, Span)], _: Span) -> Type {
    Type::Bool
}

fn method(type_name: &str, name: &str) -> Option<Method> {
    let found = || Type::named("Match");
    match (type_name, name) {
        ("Regex", "match" | "fullmatch" | "search") => Some(reads(Type::optional(found()), 1, 1)),
        ("Regex", "find_all") => Some(reads(Type::list(found()), 1, 1)),
        ("Regex", "replace") => Some(reads(Type::Str, 2, 2)),
        ("Regex", "split") => Some(reads(Type::list(Type::Str), 1, 1)),
        _ => None,
    }
}

/// A `Regex` written with a literal pattern: a pattern that isn't valid is a mistake in the
/// program, so it's reported now rather than when it runs.
fn construct(cx: &mut FnCx<'_>, type_name: &str, args: &[Arg]) {
    if type_name != "Regex" {
        return;
    }
    let Some(arg) = args.first() else { return };
    let ExprKind::Str(pattern) = &arg.value.kind else { return };
    if let Err(error) = fennec_re::Regex::new(pattern) {
        cx.error(
            Diagnostic::new(format!("this pattern isn't valid: {}", error.message), arg.value.span)
                .with_help(format!("the problem is at character {} of the pattern", error.at)),
        );
    }
}
