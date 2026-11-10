//! `std.pack`: a compact binary format, written in Fennec on the `Serializer` and `Deserializer`
//! protocol.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "pack",
    names: &["dumps", "loads", "Limits", "PackWriter", "PackReader", "Packer", "Unpacker", "dump", "load"],
    source: include_str!("pack.fen"),
    gives: &[],
};
