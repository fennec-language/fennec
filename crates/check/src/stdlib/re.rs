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

/// `re.is_regex(pattern)`: whether some text is a pattern.
fn result(cx: &mut FnCx<'_>, _: &str, plain: &[(Type, Span)], _: Span) -> Type {
    if let Some((ty, at)) = plain.first()
        && !cx.checker.fits(&Type::Str, ty)
    {
        cx.error(crate::messages::mismatch("the pattern", &Type::Str, ty, *at));
    }
    Type::Bool
}

fn method(type_name: &str, name: &str) -> Option<Method> {
    let found = || Type::named("Match");
    let text = || Type::Str;
    Some(match (type_name, name) {
        ("Regex", "match" | "fullmatch" | "search") => reads(&[text()], Type::optional(found())),
        ("Regex", "find_all") => reads(&[text()], Type::list(found())),
        ("Regex", "replace") => reads(&[text(), text()], text()),
        ("Regex", "split") => reads(&[text()], Type::list(text())),
        _ => return None,
    })
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
