//! What may copy or allocate, from what the IR finds. A copy made each time round a loop
//! without saying so is a compile error, and so is anything that costs in a `@hot` function or
//! what it calls. The rest are hints: they flag correct code, so they
//! never fail `fennec lint`, but whoever is looking at a hot path wants every one, with where it
//! runs.

use fennec_ir::{Cost, Finding, HotReach, Shared};
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

pub(crate) fn hot(findings: &[Finding], reach: &HotReach) -> Vec<Diagnostic> {
    let key = |name: &str| name.split_once('[').map_or(name, |(key, _)| key).to_owned();
    let whose = |function: &str, root: &str| {
        if key(function) == root {
            format!("`@hot` `{root}`")
        } else {
            format!("`{}`, which `@hot` `{root}` calls", key(function))
        }
    };
    let mut problems: Vec<Diagnostic> = findings
        .iter()
        .filter(|finding| !(finding.per_iteration() && matches!(finding.cost, Cost::Copy { .. })))
        .filter_map(|finding| {
            let (_, root) = reach.reached.iter().find(|(name, _)| *name == finding.function)?;
            let help = match &finding.cost {
                Cost::Allocation { .. } => {
                    "a `@hot` function and what it calls may not allocate: make it before the hot path and pass it in, or take `@hot` off".to_owned()
                }
                Cost::Copy { value, .. } => format!(
                    "a `@hot` function and what it calls may not copy: stop holding `{value}` elsewhere while this changes it, or take `@hot` off"
                ),
            };
            let message = format!("{} in {}", finding.message(), whose(&finding.function, root));
            Some(Diagnostic::new(message, finding.span).with_help(help))
        })
        .collect();
    for (span, callee, root) in &reach.unlowered {
        let message = format!(
            "`@hot` `{root}` calls `{}`, which can't be shown not to allocate or copy, since it isn't built natively yet",
            key(callee)
        );
        problems.push(Diagnostic::new(message, *span).with_help("take `@hot` off until it is"));
    }
    problems
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
        Some(later) => diagnostic.also_at(later),
        None => diagnostic,
    })
}
