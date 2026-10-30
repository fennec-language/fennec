//! `std.toml`: `toml.loads` and `toml.dumps`. The reading and writing itself is in
//! `formats`, which `std.json` shares.

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants};
use crate::call::CallArg;
use crate::interp::Interp;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &["toml.dumps", "toml.loads"],
    call,
    constant: no_constants,
    methods: &[],
};

fn call(interp: &mut Interp<'_>, name: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
    interp.format_builtin(name, args, span)
}
