//! `std.crypto`: hashes, HMAC and comparing secrets, which the interpreter does in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "crypto",
    names: &["blake3", "keyed_blake3", "derive_key", "sha256", "hmac_sha256", "equal"],
    source: "",
    gives: &[],
};
