//! A copy made each time round a loop without saying so is a compile error. Changing a value
//! something else still holds copies it first. When a loop does that every time round, here or
//! further up the calls, the program says so with `copy(...)` or stops sharing the value. A copy
//! made once, the first time round, stays a hint.

use fennec_syntax::{Diagnostic, Module};

/// Each copy a resolved module makes each time round a loop without saying so.
pub(crate) fn implicit_copies(module: &Module, signatures: &fennec_check::Signatures) -> Vec<Diagnostic> {
    fennec_lint::implicit_copies(&fennec_ir::costs(&fennec_ir::lower(module, signatures)))
}

/// Checks a program to run, with its implicit copies each time round a loop as errors, and its
/// constants worked out while compiling.
pub(crate) fn check_to_run(module: &mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = baked(module, fennec_check::check_to_run)?;
    let copies = implicit_copies(module, &signatures);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// Like `check_to_run`, without working out the constants: what a check that only asks
/// whether the program is sound needs.
pub(crate) fn check_rules(module: &mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = fennec_check::check_to_run(module)?;
    let copies = implicit_copies(module, &signatures);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// Like `check_to_run`, checking all of a module, as tests do.
pub(crate) fn check_and_resolve(module: &mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = baked(module, fennec_check::check_and_resolve)?;
    let copies = implicit_copies(module, &signatures);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// The steps a constant may take to be worked out while compiling. One that takes more is
/// worked out when the program starts, with a hint.
const CONSTANT_STEPS: u64 = 1_000_000;

type Check = fn(&mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>>;

/// Checks a module with `check`, after working out its top-level constants while compiling.
fn baked(module: &mut Module, check: Check) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let mut resolved = module.clone();
    let signatures = check(&mut resolved)?;
    if bake_resolved(module, &resolved)? {
        return check(module);
    }
    *module = resolved;
    Ok(signatures)
}

/// Works out a module's top-level constants while compiling: each that isn't a literal already
/// becomes the literal of its value, in the module as written, which is still to be checked.
pub(crate) fn bake(module: &mut Module) -> Result<(), Vec<Diagnostic>> {
    let mut resolved = module.clone();
    fennec_check::check_to_run(&mut resolved)?;
    bake_resolved(module, &resolved).map(|_| ())
}

/// Writes into `module` the literal of each constant `resolved`, its checked copy, works out
/// within the budget, and says whether it wrote any. One that fails to work out is a compile
/// error, since it would fail every time the program started.
fn bake_resolved(module: &mut Module, resolved: &Module) -> Result<bool, Vec<Diagnostic>> {
    let worked_out =
        fennec_interp::bake_constants(resolved, CONSTANT_STEPS).map_err(|problem| vec![problem])?;
    let mut literals = Vec::new();
    for (name, baked) in worked_out {
        match baked {
            fennec_interp::Baked::Literal(value) => literals.push((name, value)),
            fennec_interp::Baked::TooLong => eprintln!(
                "hint: `{name}` takes more than {CONSTANT_STEPS} steps to work out while compiling, so it's worked out when the program starts"
            ),
            fennec_interp::Baked::Unwritable => {}
            fennec_interp::Baked::Failed(problem) => return Err(vec![problem]),
        }
    }
    for item in &mut module.items {
        if let fennec_syntax::Item::Const { name, value, .. } = item
            && let Some((_, literal)) = literals.iter().find(|(baked, _)| baked == name)
        {
            *value = literal.clone();
        }
    }
    Ok(!literals.is_empty())
}
