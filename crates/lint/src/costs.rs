//! What may copy or allocate, from what the IR finds. A copy made each time round a loop
//! without saying so is a compile error. The rest are hints: they flag correct code, so they
//! never fail `fennec lint`, but whoever is looking at a hot path wants every one, with where it
//! runs.

use fennec_ir::{Cost, Finding, Shared};
use fennec_syntax::Diagnostic;

use crate::Lint;

/// How often a finding runs, in words, when that's more than once a call.
fn how_often(finding: &Finding) -> Option<String> {
    match (finding.loops, &finding.called_from_loop) {
        (0, None) => None,
        (0, Some(caller)) => Some(format!("this runs each time `{caller}` calls it from a loop")),
        (1, _) => Some("this runs on every pass of its loop".to_owned()),
        (loops, _) => Some(format!("this runs on every pass of {loops} nested loops")),
    }
}

/// A hint for each place that may copy once, and each that may allocate where it runs often.
pub(crate) fn of(findings: &[Finding]) -> Vec<Lint> {
    findings
        .iter()
        .filter_map(|finding| {
            let rule = match finding.cost {
                Cost::Copy { .. } if finding.per_iteration() => return None,
                Cost::Copy { .. } => "copies",
                Cost::Allocation { .. } if finding.is_hot() => "allocates",
                Cost::Allocation { .. } => return None,
            };
            let diagnostic = Diagnostic::new(finding.message(), finding.span);
            let diagnostic = match how_often(finding) {
                Some(often) => diagnostic.with_help(often),
                None => diagnostic,
            };
            Some(Lint::hint(rule, diagnostic))
        })
        .collect()
}

/// Each copy made each time round a loop, as a mistake.
pub(crate) fn implicit(findings: &[Finding]) -> Vec<Diagnostic> {
    findings.iter().filter(|finding| finding.per_iteration()).filter_map(problem).collect()
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
