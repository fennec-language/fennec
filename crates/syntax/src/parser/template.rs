//! `t"select * from users where name = {name}"`: a template string, as in Python's PEP 750. It's
//! written in as the standard library's `Template` it builds, holding the text between each `{}`
//! and each value apart, never joined:
//!
//! ```fennec
//! Template(strings=["select * from users where name = ", ""], values=[name],
//!     expressions=["name"], format_specs=[""])
//! ```
//!
//! What each value has to be comes from where the template goes, as for any generic type built:
//! a `Template[SqlValue]` parameter checks each `{}` against `SqlValue`. The text is
//! `LiteralStr`s, which only text written in the source is, so a `Template` can't be built by
//! hand from text made while running.

use crate::ast::{Arg, Expr, ExprKind};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::FPart;

/// The standard library type a template string builds.
pub(crate) const TEMPLATE: &str = "Template";

/// A template string's parts, written in as the `Template` call that builds it.
pub(super) fn template(parts: Vec<FPart>, span: Span) -> Result<Expr, Diagnostic> {
    let mut strings = Vec::new();
    let mut values = Vec::new();
    let mut expressions = Vec::new();
    let mut specs = Vec::new();
    let mut text = String::new();
    for part in parts {
        match part {
            FPart::Text(more) => text.push_str(&more),
            FPart::Expr { source, offset, spec } => {
                strings.push(std::mem::take(&mut text));
                values.push(super::expr::interpolated(&source, offset, "template")?);
                expressions.push(source);
                specs.push(spec.unwrap_or_default());
            }
        }
    }
    strings.push(text);
    let mut made = Made::new(span);
    // The values come first, so what's said about them, like a list that mixes types, points
    // at nearly the whole string.
    let values = Expr { kind: ExprKind::List(values), span: made.next() };
    let callee = Expr { kind: ExprKind::Name(TEMPLATE.to_owned()), span: made.next() };
    let strings = made.texts(strings);
    let expressions = made.texts(expressions);
    let specs = made.texts(specs);
    let arg = |name: &str, value: Expr| Arg { name: Some(name.to_owned()), is_mut: false, value };
    let args = vec![
        arg("strings", strings),
        arg("values", values),
        arg("expressions", expressions),
        arg("format_specs", specs),
    ];
    Ok(Expr { kind: ExprKind::Call { callee: Box::new(callee), args }, span })
}

/// Spans for the expressions a template string is written in as, which nothing's written for.
/// Expressions are told apart by span, so each gets its own inside the string's: first those
/// starting where it starts, longest first, then those ending where it ends, then the empty
/// ones at each end.
/// None is the span of anything written in the string's `{}`s, which start and end inside it.
/// There are always enough: a string of length `n` with `k` values has `2n` of them, and needs
/// `3k + 6`, while each value takes at least `{x}`.
struct Made {
    span: Span,
    at: usize,
}

impl Made {
    fn new(span: Span) -> Made {
        Made { span, at: 0 }
    }

    fn next(&mut self) -> Span {
        let Span { start, end } = self.span;
        let inner = usize::try_from(end.saturating_sub(start).saturating_sub(1)).unwrap_or(0);
        let at = self.at;
        self.at += 1;
        let after_start = |n: usize| start + 1 + u32::try_from(n).unwrap_or(0);
        if at < inner {
            Span { start, end: after_start(inner - 1 - at) }
        } else if at < 2 * inner {
            Span { start: after_start(at - inner), end }
        } else if at == 2 * inner {
            Span { start, end: start }
        } else {
            Span { start: end, end }
        }
    }

    /// A list of text written in the source, as string literals.
    fn texts(&mut self, texts: Vec<String>) -> Expr {
        let span = self.next();
        let items =
            texts.into_iter().map(|text| Expr { kind: ExprKind::Str(text), span: self.next() }).collect();
        Expr { kind: ExprKind::List(items), span }
    }
}
