//! `std.log`: one way for every kit to log, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module =
    Module { name: "log", names: &["Log", "LogLevel", "LogFormat"], source: include_str!("log.fen"), gives: &[] };
