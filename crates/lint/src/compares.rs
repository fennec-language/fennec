//! `self-compare`: `x == x` or `x != x`, which always gives the same answer, so one side is
//! almost always a mistake. That holds for a float too, since `==` treats NaN as equal to NaN:
//! `x.is_nan()` asks about NaN, and `x != x` on a float is the checker's error. There's no fix,
//! since only the author knows what was meant. A
//! name or field of a plain type is all this looks at: a call may give something else the second
//! time, and a type of its own may mean anything by `==`. So this looks at a program that checks.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Module, Sources, Span};

use crate::Lint;
use crate::lookups::pure;
use crate::rewrites::written;

/// An `==` or `!=` with the same name or field on both sides, as the operator, that value, and
/// how it's written.
fn self_compared(expr: &Expr, sources: &Sources) -> Option<(BinOp, Span, String)> {
    let ExprKind::Binary { op: op @ (BinOp::Eq | BinOp::NotEq), lhs, rhs } = &expr.kind else { return None };
    if !matches!(lhs.kind, ExprKind::Name(_) | ExprKind::Attr { .. }) || !pure(lhs) {
        return None;
    }
    let text = written(sources, lhs.span)?;
    (Some(&text) == written(sources, rhs.span).as_ref()).then_some((*op, lhs.span, text))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut candidates = Vec::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        if !expr.span.in_prelude()
            && let Some((op, value, text)) = self_compared(expr, sources)
        {
            candidates.push((expr.span, op, value, text));
        }
    });
    if candidates.is_empty() {
        return Vec::new();
    }
    let Ok(checked) = fennec_check::check_typed(module) else { return Vec::new() };
    let types: HashMap<(u32, u32), &Type> = checked
        .types
        .iter()
        .flat_map(|(_, body)| body.exprs())
        .map(|(span, ty)| ((span.start, span.end), ty))
        .collect();
    let mut found = Vec::new();
    for (span, op, value, text) in candidates {
        let Some(ty) = types.get(&(value.start, value.end)) else { continue };
        if !matches!(ty, Type::Int | Type::Float | Type::Str | Type::Bool) {
            continue;
        }
        let (written, always) = if op == BinOp::Eq { ("==", "True") } else { ("!=", "False") };
        let help = if matches!(ty, Type::Float) {
            format!("`==` treats NaN as equal to NaN: `{text}.is_nan()` asks about NaN")
        } else {
            "one side was probably meant to be something else".to_owned()
        };
        let message = format!("`{text} {written} {text}` is always {always}");
        found.push(Lint::new("self-compare", Diagnostic::new(message, span).with_help(help)));
    }
    found
}
