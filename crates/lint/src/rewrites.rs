//! Lints with one plain way to say a thing, which `fennec fix` writes.

use fennec_syntax::{
    BinOp, Diagnostic, Expr, ExprKind, FStrPart, Module, Sources, Span, Stmt, StmtKind, UnaryOp,
};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// The source an expression was written as.
fn written(sources: &Sources, span: Span) -> Option<String> {
    let (_, text, base) = sources.file_at(span)?;
    let start = usize::try_from(span.start.checked_sub(base)?).ok()?;
    let end = usize::try_from(span.end.checked_sub(base)?).ok()?;
    Some(text.get(start..end)?.to_owned())
}

/// Whether an expression's source can be replaced whole. Spans leave out the brackets around a
/// leading or trailing operand, so `(a or b) == True` starts inside its bracket. Those are left
/// alone rather than broken.
fn replaceable(sources: &Sources, expr: &Expr) -> bool {
    written(sources, expr.span).is_some_and(|text| {
        let mut depth = 0_i32;
        for ch in text.chars() {
            depth += i32::from(ch == '(') - i32::from(ch == ')');
            if depth < 0 {
                return false;
            }
        }
        depth == 0
    })
}

/// The source of an operand that's going beside a new operator, in brackets if it needs them:
/// spans leave out the brackets that were around it.
fn operand(sources: &Sources, expr: &Expr) -> Option<String> {
    let text = written(sources, expr.span)?;
    let loose = matches!(expr.kind, ExprKind::Binary { .. } | ExprKind::Unary { .. } | ExprKind::Cond { .. });
    Some(if loose { format!("({text})") } else { text })
}

/// What a comparison with `True`, `False` or `None` should say instead.
fn plain_compare(expr: &Expr, sources: &Sources) -> Option<Lint> {
    let ExprKind::Binary { op, lhs, rhs } = &expr.kind else { return None };
    if !replaceable(sources, expr) {
        return None;
    }
    let (is_eq, value) = match op {
        BinOp::Eq => (true, operand(sources, lhs)?),
        BinOp::NotEq => (false, operand(sources, lhs)?),
        _ => return None,
    };
    let (rule, fixed) = match rhs.kind {
        ExprKind::Bool(wanted) if wanted == is_eq => ("bool-compare", value),
        ExprKind::Bool(_) => ("bool-compare", format!("not {value}")),
        ExprKind::None if is_eq => ("none-compare", format!("{value} is None")),
        ExprKind::None => ("none-compare", format!("{value} is not None")),
        _ => return None,
    };
    let diagnostic =
        Diagnostic::new(format!("write this as `{fixed}`"), expr.span).with_fix(expr.span, fixed);
    Some(Lint::new(rule, diagnostic))
}

/// `not a == b` as `a != b`.
fn negated_compare(expr: &Expr, sources: &Sources) -> Option<Lint> {
    let ExprKind::Unary { op: UnaryOp::Not, expr: inner } = &expr.kind else { return None };
    let ExprKind::Binary { op, lhs, rhs } = &inner.kind else { return None };
    if !replaceable(sources, expr) {
        return None;
    }
    let opposite = match op {
        BinOp::Eq => "!=",
        BinOp::NotEq => "==",
        BinOp::Is => "is not",
        BinOp::IsNot => "is",
        BinOp::In => "not in",
        BinOp::NotIn => "in",
        _ => return None,
    };
    let fixed = format!("{} {opposite} {}", operand(sources, lhs)?, operand(sources, rhs)?);
    let diagnostic =
        Diagnostic::new(format!("write this as `{fixed}`"), expr.span).with_fix(expr.span, fixed);
    Some(Lint::new("negated-compare", diagnostic))
}

/// `f"ready"` as `"ready"`. Braces read differently in the two, so those are left alone.
fn plain_fstring(expr: &Expr, sources: &Sources) -> Option<Lint> {
    let ExprKind::FStr(parts) = &expr.kind else { return None };
    if parts.iter().any(|part| matches!(part, FStrPart::Expr { .. })) {
        return None;
    }
    let text = written(sources, expr.span)?;
    let plain = text.strip_prefix('f').filter(|rest| !rest.contains(['{', '}']))?;
    let diagnostic =
        Diagnostic::new(format!("this f-string has nothing to fill in: write `{plain}`"), expr.span)
            .with_fix(expr.span, plain);
    Some(Lint::new("plain-fstring", diagnostic))
}

/// Every lint here that looks at one expression.
fn expressions(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    // Walking changes nothing, but the only walk there is takes a module it could change.
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        found.extend(plain_compare(expr, sources));
        found.extend(negated_compare(expr, sources));
        found.extend(plain_fstring(expr, sources));
    });
    found
}

/// `pass` in a block that has something else in it.
fn needless_pass(stmts: &[Stmt], found: &mut Vec<Lint>) {
    if stmts.len() > 1 {
        for stmt in stmts.iter().filter(|stmt| matches!(stmt.kind, StmtKind::Pass)) {
            let diagnostic =
                Diagnostic::new("this block doesn't need `pass`", stmt.span).with_fix(stmt.span, "");
            found.push(Lint::new("needless-pass", diagnostic));
        }
    }
    for block in stmts.iter().flat_map(blocks) {
        needless_pass(block, found);
    }
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = expressions(module, sources);
    for def in functions(module) {
        needless_pass(&def.body, &mut found);
        // A function's last line is its value.
        if let Some(Stmt { kind: StmtKind::Return(Some(value)), span }) = def.body.last() {
            let word = Span { start: span.start, end: value.span.start };
            let diagnostic =
                Diagnostic::new("a function's last line is its value, so this needs no `return`", *span)
                    .with_fix(word, "");
            found.push(Lint::new("needless-return", diagnostic));
        }
    }
    found
}
