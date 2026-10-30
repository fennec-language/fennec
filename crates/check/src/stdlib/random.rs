//! `std.random`: drawing a seed's numbers by position. `Random` is written in Fennec.

use fennec_syntax::{Diagnostic, Span};

use super::{Function, Module, no_checks, no_methods, takes};
use crate::function::FnCx;
use crate::types::{Type, a};

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

/// The number or item drawn, and how many numbers were drawn after it. Each takes the seed
/// and how many were drawn; `random_int` also takes the range, and `random_choice` a list.
fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
    let ints = if name == "random.random_int" { 4 } else { 2 };
    let all_ints = plain.iter().take(ints).all(|(ty, _)| cx.checker.fits(&Type::Int, ty));
    if !all_ints {
        cx.error(Diagnostic::new(format!("`{name}` takes ints"), span));
    }
    let value = match name {
        "random.random_int" => Type::Int,
        "random.random_float" => Type::Float,
        _ => match plain.get(2) {
            Some((Type::List(item), _)) => item.as_ref().clone(),
            Some((other, at)) => {
                let message = format!("`{name}` picks from a list, but this is {}", a(other));
                cx.error(Diagnostic::new(message, *at));
                Type::Unknown
            }
            None => Type::Unknown,
        },
    };
    Type::Tuple(vec![value, Type::Int])
}
