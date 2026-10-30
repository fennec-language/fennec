//! `std.game`: `IdMap` and the `Id`s it hands out, which the language builds in.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "game",
    names: &["Id", "IdMap"],
    source: "",
    gives: &[],
};
