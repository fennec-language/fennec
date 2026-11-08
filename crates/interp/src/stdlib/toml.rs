//! `std.toml`: `toml.loads` and `toml.dumps`. The reading and writing itself is in
//! `formats`, which `std.json` shares.

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants};
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &["toml.dumps", "toml.loads"],
    call,
    constant: no_constants,
    methods: &[],
};

