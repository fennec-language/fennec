//! `std.process`: the methods of the `Process` capability. Running a program can fail with a
//! `ProcessError`. A test's fake `Process` is told what each program gives with `script`, and
//! says what ran with `ran`; those two work only in a test's own body.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    let output = || Type::named("Output");
    Some(match (type_name, name) {
        // The arguments are a list, never one string a shell would split.
        ("Process", "run") => {
            reads(&[Type::Str, Type::list(Type::Str)], Type::result(output(), Type::named("ProcessError")))
        }
        // A `Process` that can run only one program.
        ("Process", "only") => reads(&[Type::Str], Type::named("Process")),
        ("Process", "script") => reads(&[Type::Str, output()], Type::Unit),
        ("Process", "ran") => reads(&[], Type::list(Type::list(Type::Str))),
        _ => return None,
    })
}
