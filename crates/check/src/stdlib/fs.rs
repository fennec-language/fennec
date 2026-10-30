//! `std.fs`: the methods of the `Fs` capability. Each one that can fail gives an `IoError`.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    let can_fail = |value: Type| Type::result(value, Type::named("IoError"));
    let (returns, arity) = match (type_name, name) {
        ("Fs", "exists") => (Type::Bool, 1),
        ("Fs", "read_text") => (can_fail(Type::Str), 1),
        ("Fs", "write_text") => (can_fail(Type::Unit), 2),
        ("Fs", "list_dir") => (can_fail(Type::list(Type::named("Path"))), 1),
        ("Fs", "make_dir" | "remove") => (can_fail(Type::Unit), 1),
        ("Fs", "metadata") => (can_fail(Type::named("Metadata")), 1),
        _ => return None,
    };
    Some(reads(returns, arity, arity))
}
