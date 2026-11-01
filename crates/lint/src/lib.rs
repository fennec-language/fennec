//! The linter. It has no settings and no way to turn a lint off, so every lint either has an
//! automatic fix, which `fennec fix` makes, or can't be wrong, because it counts something or
//! matches an exact form.

mod calls;
mod flow;
mod rewrites;
mod sizes;

use fennec_syntax::{Diagnostic, Module, Sources};

/// One thing a lint found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lint {
    /// The lint's name, like `needless-return`.
    pub rule: &'static str,
    pub diagnostic: Diagnostic,
}

impl Lint {
    fn new(rule: &'static str, diagnostic: Diagnostic) -> Self {
        let message = format!("{rule}: {}", diagnostic.message);
        Lint { rule, diagnostic: Diagnostic { message, ..diagnostic } }
    }
}

/// Everything the lints find in a loaded program, in the order it's written.
pub fn lint(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = sizes::of(module, sources);
    found.extend(rewrites::of(module, sources));
    found.extend(calls::of(module));
    found.extend(flow::of(module, sources));
    found.sort_by_key(|lint| (lint.diagnostic.span.start, lint.rule));
    found
}
