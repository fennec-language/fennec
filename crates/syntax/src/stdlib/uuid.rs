//! `std.uuid`: UUIDs made from `Entropy` and a `Clock`, and read and written as text, written
//! in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "uuid",
    names: &["Uuid", "v4", "v7"],
    source: include_str!("uuid.fen"),
    gives: &[],
};
