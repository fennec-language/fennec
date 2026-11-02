//! The linter. It has no settings and no way to turn a lint off, so every lint either has an
//! automatic fix, which `fennec fix` makes, or can't be wrong, because it counts something or
//! matches an exact form.

mod calls;
mod copies;
mod costs;
mod dead;
mod flow;
mod loops;
mod names;
mod paths;
mod rewrites;
mod sizes;

use fennec_syntax::{Diagnostic, Module, Sources};

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A lint, which fails `fennec lint` until it's fixed.
    Error,
    /// Correct code worth knowing about, like what may copy, which never fails anything.
    Hint,
}

/// One thing a lint found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lint {
    /// The lint's name, like `needless-return`.
    pub rule: &'static str,
    pub severity: Severity,
    pub diagnostic: Diagnostic,
}

impl Lint {
    fn new(rule: &'static str, diagnostic: Diagnostic) -> Self {
        let message = format!("{rule}: {}", diagnostic.message);
        Lint { rule, severity: Severity::Error, diagnostic: Diagnostic { message, ..diagnostic } }
    }

    fn hint(rule: &'static str, diagnostic: Diagnostic) -> Self {
        Lint { severity: Severity::Hint, ..Lint::new(rule, diagnostic) }
    }

    /// What it says, without the rule's name in front.
    #[must_use]
    pub fn message(&self) -> &str {
        let message = &self.diagnostic.message;
        message.strip_prefix(self.rule).and_then(|rest| rest.strip_prefix(": ")).unwrap_or(message)
    }
}

/// Everything the lints find in a loaded program, in the order it's written.
pub fn lint(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = sizes::of(module, sources);
    found.extend(rewrites::of(module, sources));
    found.extend(calls::of(module));
    found.extend(flow::of(module, sources));
    found.extend(dead::of(module, sources));
    found.extend(loops::of(module, sources));
    found.extend(names::of(module, sources));
    found.extend(paths::of(sources));
    found.sort_by_key(|lint| (lint.diagnostic.span.start, lint.rule));
    found
}

/// Every explicit `copy` that changes nothing, in a module that checks. Finding them checks the
/// whole program again for each `copy`, so they're asked for where the whole program is checked
/// anyway, apart from `lint`.
#[must_use]
pub fn needless_copies(module: &Module, sources: &Sources) -> Vec<Lint> {
    copies::of(module, sources)
}

/// A hint for each place a lowered program may copy, once, and each it may allocate in code
/// that runs often, from what `fennec_ir::costs` finds, in its order: hot code first. A copy
/// made each time round a loop is a mistake instead, which `implicit_copies` gives.
#[must_use]
pub fn costs(findings: &[fennec_ir::Finding]) -> Vec<Lint> {
    costs::of(findings)
}

/// Each copy made each time round a loop without saying so, as a compile error. Changing a
/// value something else still holds copies it first, and a loop that does that every time
/// round, here or further up the calls, says so with `copy(...)` or stops sharing the value.
#[must_use]
pub fn implicit_copies(findings: &[fennec_ir::Finding]) -> Vec<Diagnostic> {
    costs::implicit(findings)
}
