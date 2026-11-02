//! What may copy or allocate, from what the IR finds, as hints. They flag correct code, so they
//! never fail `fennec lint`, but whoever is looking at a hot path wants every one, with where it
//! runs.

use fennec_ir::{Cost, Finding};
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

/// A hint for each place that may copy, and each that may allocate where it runs often.
pub(crate) fn of(findings: &[Finding]) -> Vec<Lint> {
    findings
        .iter()
        .filter_map(|finding| {
            let rule = match finding.cost {
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
