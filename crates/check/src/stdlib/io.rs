//! `std.io`: the methods of `Stderr` written in Rust. `Stdout`'s and `Stdin`'s are with the other
//! capabilities', in `builtins`.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    let io = || Type::result(Type::Unit, Type::named("IoError"));
    Some(match (type_name, name) {
        ("Stderr", "write") => reads(&[Type::Bytes], io()),
        ("Stderr", "write_text" | "write_line") => reads(&[Type::Str], io()),
        ("Stderr", "flush") => reads(&[], io()),
        // What a test's fake `Stderr` was given.
        ("Stderr", "captured") => reads(&[], Type::Str),
        _ => return None,
    })
}
