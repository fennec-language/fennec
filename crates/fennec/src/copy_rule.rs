//! A copy made each time round a loop without saying so is a compile error. Changing a value
//! something else still holds copies it first. When a loop does that every time round, here or
//! further up the calls, the program says so with `copy(...)` or stops sharing the value. A copy
//! made once, the first time round, stays a hint.

use fennec_ir::{Cost, Finding, Shared};
use fennec_syntax::{Diagnostic, Module};

/// Each copy a resolved module makes each time round a loop without saying so.
pub(crate) fn implicit_copies(module: &Module, signatures: &fennec_check::Signatures) -> Vec<Diagnostic> {
    let lowered = fennec_ir::lower(module, signatures);
    fennec_ir::costs(&lowered).iter().filter(|finding| finding.per_iteration()).filter_map(problem).collect()
}

fn problem(finding: &Finding) -> Option<Diagnostic> {
    let Cost::Copy { value, why } = &finding.cost else { return None };
    let (because, later) = match why {
        Shared::HeldBy { name, later } => (format!("`{name}` still holds it and is used again"), *later),
        Shared::PutInto { container, later } => {
            (format!("it was put into `{container}`, which is used again"), *later)
        }
        Shared::Caller => ("the caller still holds it".to_owned(), None),
    };
    let when = match &finding.called_from_loop {
        Some(caller) if finding.loops == 0 => format!("each time a loop in `{caller}` gets here"),
        _ => "each time round the loop".to_owned(),
    };
    let message = format!("`{value}` is copied {when}, since {because}");
    let help = format!(
        "write `copy({value})` to copy it on purpose, or stop holding it elsewhere before this changes it"
    );
    let diagnostic = Diagnostic::new(message, finding.span).with_help(help);
    Some(match later {
        Some(later) => Diagnostic { also: Some(later), ..diagnostic },
        None => diagnostic,
    })
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
