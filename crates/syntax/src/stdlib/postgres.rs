//! `std.postgres`: a Postgres database over `std.net`, written in Fennec, under `std.sql`. It has
//! nothing written in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "postgres",
    names: &["PgMessage", "PgReader", "PgScram", "startup", "password", "sasl_first", "sasl_next", "terminate", "query_messages", "rows_of", "failure_from_fields", "connect", "settings", "PgSettings", "PgPool", "PgDatabase", "PgTransaction"],
    source: include_str!("postgres.fen"),
    gives: &[],
};
