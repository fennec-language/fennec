//! `std.env`: the methods of the `Env` capability written in Rust. `secret` is in env.fen.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    Some(match (type_name, name) {
        ("Env", "get") => reads(&[Type::Str], Type::optional(Type::Str)),
        ("Env", "all") => reads(&[], Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
        // Only a test's fake `Env` is set.
        ("Env", "set") => reads(&[Type::Str, Type::Str], Type::Unit),
        _ => return None,
    })
}
