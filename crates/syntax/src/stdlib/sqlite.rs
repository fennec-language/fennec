//! `std.sqlite`: a SQLite database, through the `Fs` it's in. Its types are declared in Fennec,
//! and what reaches SQLite is written in Rust, by the handle a `Database` holds.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "sqlite",
    names: &["open", "Database", "Transaction", "SqliteStatement"],
    source: include_str!("sqlite.fen"),
    gives: &[],
};
