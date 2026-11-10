//! `std.signals`: the methods of the `Signals` capability, all written in Rust.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    Some(match (type_name, name) {
        ("Signals", "stopping") => reads(&[], Type::Bool),
        // `wait` is a wait, which a task parks on, and only a test's fake `Signals` is stopped.
        ("Signals", "wait" | "stop") => reads(&[], Type::Unit),
        _ => return None,
    })
}
