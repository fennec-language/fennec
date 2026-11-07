//! `std.string`: wrapping and indenting text, and Python's sets of characters, written in
//! Fennec. `str`'s own methods are in `str.fen`.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "string",
    names: &[
        "wrap",
        "fill",
        "dedent",
        "indent",
        "ASCII_LOWERCASE",
        "ASCII_UPPERCASE",
        "ASCII_LETTERS",
        "DIGITS",
        "HEX_DIGITS",
        "OCT_DIGITS",
        "PUNCTUATION",
        "WHITESPACE",
    ],
    source: include_str!("string.fen"),
    gives: &[],
};
