//! `std.base64`: bytes as text, in standard and URL-safe base64, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "base64",
    names: &["encode", "decode", "url_encode", "url_decode"],
    source: include_str!("base64.fen"),
    gives: &[],
};
