//! A channel to `.Every` gives every listener every message, each from a place of its own in
//! what's kept, so it has no one next message to take. A plain `receive` on one is stopped when it
//! runs, and refused here where the checker sees the channel made with `to=.Every` written out
//! and received from by the name it's lent as. Passed somewhere else, the stop still catches it.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span};

use crate::function::FnCx;

impl FnCx<'_> {
    /// Marks the name a `with` just lent its value as, when that's a channel to `.Every`.
    pub(crate) fn note_to_every(&mut self, name: &str, value: &Expr) {
        if self.made_to_every(value)
            && let Some(binding) = self.binding_mut(name)
        {
            binding.to_every = true;
        }
    }

    /// Whether a `with`'s value is std's `channel` called with `to=.Every` written out.
    fn made_to_every(&self, value: &Expr) -> bool {
        let ExprKind::Call { args, .. } = &value.kind else { return false };
        let Some(source) = self.checker.sources.get(&(value.span.start, value.span.end)) else {
            return false;
        };
        source.def.name == "concurrent.channel"
            && source.def.span.in_prelude()
            && args.iter().any(|arg| arg.name.as_deref() == Some("to") && is_every(&arg.value))
    }

    /// Refuses `receive` or `receive_within` called on the name a channel to `.Every` is lent as.
    pub(crate) fn receive_from_every(&mut self, object: &Expr, method: &str, span: Span) {
        if !matches!(method, "receive" | "receive_within") {
            return;
        }
        let ExprKind::Name(name) = &object.kind else { return };
        if !self.binding_mut(name).is_some_and(|binding| binding.to_every) {
            return;
        }
        let message =
            format!("`{name}` is a channel to every listener, so it has no one next message to take");
        let help = format!(
            "loop over it with `for message in {name}`, or take an inbox with `{name}.inbox()` and receive from that"
        );
        self.error(Diagnostic::new(message, span).with_help(help));
    }
}

/// Whether a value is the `Every` case written out. It's always written `.Every` there, as `to=`
/// says what type it is.
fn is_every(value: &Expr) -> bool {
    matches!(&value.kind, ExprKind::Variant(case) if case == "Every")
}
