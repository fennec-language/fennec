//! `short-case`: a case written with more than it needs where the type is known, as `.Case` is
//! when it can be. `paint(Color.Red)` is `paint(.Red)`, and `case Red:` is `case .Red:`. The checker
//! says where, as it knows what's expected in each place, so this looks at a program that checks.

use fennec_syntax::{Diagnostic, Module, Sources, Span};

use crate::Lint;
use crate::rewrites::written;

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let Ok(checked) = fennec_check::check_typed(module) else { return Vec::new() };
    let mut found = Vec::new();
    for span in checked.named_cases().filter(|span| !span.in_prelude()) {
        let Some(name) = written(sources, span) else { continue };
        let message = format!("the type here is known, so this case doesn't need `{name}`");
        found.push(Lint::new("short-case", Diagnostic::new(message, span).with_fix(span, "")));
    }
    for span in checked.undotted_cases().filter(|span| !span.in_prelude()) {
        let Some(at) = pattern_start(sources, span) else { continue };
        let message = "the type here is known, so write this case with a leading dot";
        found.push(Lint::new("short-case", Diagnostic::new(message, span).with_fix(at, ".")));
    }
    found.sort_by_key(|lint| lint.diagnostic.span.start);
    found
}

/// The empty span where the pattern starts, in the `case` at `arm`.
fn pattern_start(sources: &Sources, arm: Span) -> Option<Span> {
    let text = written(sources, arm)?;
    let rest = text.strip_prefix("case")?;
    let start = arm.start.checked_add(u32::try_from(text.len() - rest.trim_start().len()).ok()?)?;
    Some(Span { start, end: start })
}
