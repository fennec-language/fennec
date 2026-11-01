//! `std.csv`: reading and writing CSV, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module =
    Module { name: "csv", names: &["Csv"], source: include_str!("csv.fen"), gives: &[] };
