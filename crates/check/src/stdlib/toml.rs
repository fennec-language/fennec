//! `std.toml`: reading and writing TOML.

use super::{Function, Module, formats, no_checks, no_methods};

pub(crate) const MODULE: Module = Module {
    functions: &[
        Function { name: "toml.dumps", arity: (1, 1), named: &[], needs_values: false },
        Function { name: "toml.loads", arity: (1, 1), named: &[], needs_values: false },
    ],
    result: formats::result,
    method: no_methods,
    construct: no_checks,
};
