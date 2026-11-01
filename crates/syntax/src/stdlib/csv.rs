//! `std.csv`: reading and writing CSV, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "csv",
    names: &["loads", "records", "dumps", "dumps_records"],
    source: include_str!("csv.fen"),
    gives: &[],
};
