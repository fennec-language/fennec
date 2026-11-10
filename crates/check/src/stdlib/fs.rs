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
        // What's in a folder, and every path under it, each folder's before what's in it.
        ("Fs", "list_dir" | "walk") => reads(&[path()], io(Type::list(Type::named("Path")))),
        ("Fs", "make_dir" | "remove") => reads(&[path()], io(Type::Unit)),
        ("Fs", "metadata") => reads(&[path()], io(Type::named("Metadata"))),
        // An `Fs` that reaches only what's in one folder.
        ("Fs", "only") => reads(&[path()], Type::named("Fs")),
        // What `fs.create` and `fs.open` are written with, in fs.fen: a file to write beside
        // `path` or one to read, and finishing or dropping it.
        ("Fs", "_create") => reads(&[path(), Type::Bool], io(Type::named("File"))),
        ("Fs", "_open") => reads(&[path()], io(Type::named("File"))),
        ("Fs", "_finish") => reads(&[Type::named("File")], io(Type::Unit)),
        ("Fs", "_discard") => reads(&[Type::named("File")], Type::Unit),
        // What `sqlite.open` is written with: the database at `path`.
        ("Fs", "_sqlite_open") => reads(&[path()], io(Type::named("Database"))),
        // `bytes`, or a `str` as its UTF-8.
        ("File", "write") => reads(&[Type::Unknown], io(Type::Unit)),
        ("File", "read") => reads(&[], io(Type::Bytes)),
        ("File", "read_chunk") => reads(&[Type::Int], io(Type::Bytes)),
        ("File", "sync" | "flush") => reads(&[], io(Type::Unit)),
        ("File", "read_line") => reads(&[], io(Type::optional(Type::Str))),
        _ => return None,
    })
}
