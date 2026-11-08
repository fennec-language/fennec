//! `std.sql`: what every database gives, written in Fennec: the values a query takes, how a
//! database fails, and the interface a database and a transaction have.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "sql",
    names: &["SqlValue", "SqlError", "Sql"],
    source: include_str!("sql.fen"),
    gives: &[],
};
