//! `std.re`: regular expressions, which run the `fennec-re` engine.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "re",
    names: &["Regex", "Match", "is_regex"],
    source: include_str!("re.fen"),
    gives: &[("match", "Match"), ("fullmatch", "Match"), ("search", "Match"), ("find_all", "Match")],
};
