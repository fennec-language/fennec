//! `std.unicode`: a character's general category, the normalization forms NFC, NFD, NFKC and
//! NFKD, and full case folding, which the runtime does from Unicode's own tables.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "unicode",
    names: &["category", "nfc", "nfd", "nfkc", "nfkd", "casefold", "version"],
    source: "",
    gives: &[],
};
