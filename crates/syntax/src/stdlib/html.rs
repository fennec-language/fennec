//! `std.html`: pages made from template strings, each value put where it can't change what the
//! page does, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "html",
    names: &["Html", "HtmlValue", "HtmlError", "render", "join", "escape"],
    source: include_str!("html.fen"),
    gives: &[],
};
