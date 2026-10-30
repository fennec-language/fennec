//! `std.random`: drawing a seed's numbers by position. `Random` is written in Fennec.

use fennec_syntax::{Diagnostic, Span};

use super::{Function, Module, no_checks, no_methods, takes};
use crate::function::FnCx;
use crate::types::Type;

pub(crate) const MODULE: Module = Module {
    functions: &FUNCTIONS,
    result,
    method: no_methods,
    construct: no_checks,
};

const FUNCTIONS: [Function; 3] = [
    takes("random.random_choice", (3, 3)),
    takes("random.random_float", (2, 2)),
    takes("random.random_int", (4, 4)),
];

/// The number or item drawn, and how many numbers were drawn after it.
fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
    if name == "random.random_choice" {
        let item = plain.get(2).map_or(Type::Unknown, |(ty, at)| cx.item_type(ty, *at));
        return Type::Tuple(vec![item, Type::Int]);
    }
    if plain.iter().any(|(ty, _)| !matches!(ty, Type::Int | Type::Unknown)) {
        cx.error(Diagnostic::new(format!("`{name}` takes ints"), span));
    }
    let value = if name == "random.random_int" { Type::Int } else { Type::Float };
    Type::Tuple(vec![value, Type::Int])
}
