//! `std.sqlite`: the methods of a `Database` and a `Transaction` written in Rust, each private to
//! sqlite.fen, which gives them a query's text and its values in a list for each kind.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    let fails = |ty: Type| Type::result(ty, Type::named("SqlError"));
    let query = || {
        [
            Type::Str,
            Type::list(Type::Int),
            Type::list(Type::Int),
            Type::list(Type::Float),
            Type::list(Type::Str),
            Type::list(Type::Bytes),
        ]
    };
    Some(match (type_name, name) {
        ("Database" | "Transaction", "_execute") => reads(&query(), fails(Type::Int)),
        ("Database" | "Transaction", "_query") => reads(&query(), fails(Type::named("SqliteRows"))),
        // A transaction on a connection of its own, with the database's writer taken.
        ("Database", "_begin") => reads(&[], fails(Type::named("Transaction"))),
        ("Database", "_close") | ("Transaction", "_rollback") => reads(&[], Type::Unit),
        ("Transaction", "_commit") => reads(&[], fails(Type::Unit)),
        _ => return None,
    })
}
