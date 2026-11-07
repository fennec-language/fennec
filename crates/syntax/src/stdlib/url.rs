//! `std.url`: URLs taken apart and put back together, and the escapes and queries they use,
//! written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "url",
    names: &["Url", "parse", "quote", "unquote", "encode_query", "decode_query"],
    source: include_str!("url.fen"),
    gives: &[],
};
