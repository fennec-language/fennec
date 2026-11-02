//! A copy made each time round a loop without saying so is a compile error. Changing a value
//! something else still holds copies it first. When a loop does that every time round, here or
//! further up the calls, the program says so with `copy(...)` or stops sharing the value. A copy
//! made once, the first time round, stays a hint.

use fennec_syntax::{Diagnostic, Module};

/// Each copy a resolved module makes each time round a loop without saying so.
pub(crate) fn implicit_copies(module: &Module, signatures: &fennec_check::Signatures) -> Vec<Diagnostic> {
    fennec_lint::implicit_copies(&fennec_ir::costs(&fennec_ir::lower(module, signatures)))
}

/// Checks a program to run, with its implicit copies each time round a loop as errors.
pub(crate) fn check_to_run(module: &mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = fennec_check::check_to_run(module)?;
    let copies = implicit_copies(module, &signatures);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// Like `check_to_run`, checking all of a module, as tests do.
pub(crate) fn check_and_resolve(module: &mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = fennec_check::check_and_resolve(module)?;
    let copies = implicit_copies(module, &signatures);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}
