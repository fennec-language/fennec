//! `std.fs`: the methods of the `Fs` capability. Each one that can fail gives an `IoError`.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    let io = |value: Type| Type::result(value, Type::named("IoError"));
    // A path is a `str` here, and the checker takes a `Path` wherever `Fs` takes one.
    let path = || Type::Str;
    Some(match (type_name, name) {
        ("Fs", "exists") => reads(&[path()], Type::Bool),
        ("Fs", "read_text") => reads(&[path()], io(Type::Str)),
        ("Fs", "write_text") => reads(&[path(), Type::Str], io(Type::Unit)),
        ("Fs", "read_bytes") => reads(&[path()], io(Type::Bytes)),
        ("Fs", "write_bytes") => reads(&[path(), Type::Bytes], io(Type::Unit)),
        ("Fs", "list_dir") => reads(&[path()], io(Type::list(Type::named("Path")))),
        ("Fs", "make_dir" | "remove") => reads(&[path()], io(Type::Unit)),
        ("Fs", "metadata") => reads(&[path()], io(Type::named("Metadata"))),
        // An `Fs` that reaches only what's in one folder.
        ("Fs", "only") => reads(&[path()], Type::named("Fs")),
        _ => return None,
    })
}
