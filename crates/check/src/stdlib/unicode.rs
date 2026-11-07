//! `std.unicode`: a character's general category, the normalization forms, and case folding,
//! each taking text and giving text.

use super::{Function, Module, no_checks, no_methods, takes};
use crate::function::FnCx;
use crate::types::{Type, a};
use fennec_syntax::{Diagnostic, Span};

pub(crate) const MODULE: Module = Module {
    functions: &FUNCTIONS,
    result,
    method: no_methods,
    construct: no_checks,
};

const FUNCTIONS: [Function; 7] = [
    takes("unicode.casefold", (1, 1)),
    takes("unicode.category", (1, 1)),
    takes("unicode.nfc", (1, 1)),
    takes("unicode.nfd", (1, 1)),
    takes("unicode.nfkc", (1, 1)),
    takes("unicode.nfkd", (1, 1)),
    takes("unicode.version", (0, 0)),
];

fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], _: Span) -> Type {
    for (ty, at) in plain {
        let ty = cx.checker.settled(ty);
        if !matches!(ty, Type::Str | Type::Unknown | Type::Var(_)) {
            let short = name.trim_start_matches("unicode.");
            cx.error(Diagnostic::new(format!("`{short}` takes a str, but this is {}", a(&ty)), *at));
        }
    }
    Type::Str
}
