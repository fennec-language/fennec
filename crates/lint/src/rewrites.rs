//! Lints with one plain way to say a thing, which `fennec fix` writes.

use std::collections::{HashMap, HashSet};

use fennec_syntax::{
    BinOp, Diagnostic, Expr, ExprKind, FStrPart, FnDef, Module, Pattern, Sources, Span, Stmt, StmtKind,
    UnaryOp,
};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// The source an expression was written as.
pub(crate) fn written(sources: &Sources, span: Span) -> Option<String> {
    let (_, text, base) = sources.file_at(span)?;
    let start = usize::try_from(span.start.checked_sub(base)?).ok()?;
    let end = usize::try_from(span.end.checked_sub(base)?).ok()?;
    Some(text.get(start..end)?.to_owned())
}

/// Whether an expression's source can be replaced whole. Spans leave out the brackets around a
/// leading or trailing operand, so `(a or b) == True` starts inside its bracket. Those are left
/// alone rather than broken.
pub(crate) fn replaceable(sources: &Sources, expr: &Expr) -> bool {
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
pub(crate) fn operand(sources: &Sources, expr: &Expr) -> Option<String> {
    let text = written(sources, expr.span)?;
    let loose = matches!(expr.kind, ExprKind::Binary { .. } | ExprKind::Unary { .. } | ExprKind::Cond { .. });
    Some(if loose { format!("({text})") } else { text })
}

/// Whether an expression is a `bool` for certain: `True`, `False`, a comparison, `not`, `and`,
/// `or`, or a parameter written as a `bool` that nothing in the function binds again. A `bool?`
/// compared with `True` isn't the same as the `bool?` itself, so only these can lose the `== True`.
fn surely_bool(expr: &Expr, def: &FnDef, rebound: &HashSet<String>) -> bool {
    match &expr.kind {
        ExprKind::Bool(_) | ExprKind::Unary { op: UnaryOp::Not, .. } => true,
        ExprKind::Binary { op, .. } => matches!(
            op,
            BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or
                | BinOp::In
                | BinOp::NotIn
                | BinOp::Is
                | BinOp::IsNot
        ),
        ExprKind::Name(name) => {
            !rebound.contains(name)
                && def.params.iter().any(|param| {
                    param.name == *name
                        && param
                            .ty
                            .as_ref()
                            .is_some_and(|ty| ty.name == "bool" && !ty.optional && ty.args.is_empty())
                })
        }
        _ => false,
    }
}

/// The names a function binds besides its parameters: by assigning, looping, `with`, and in
/// comprehensions, arrow functions and `match` arms.
fn rebound(def: &FnDef) -> HashSet<String> {
    fn block(stmts: &[Stmt], found: &mut HashSet<String>) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Assign { target, .. } => {
                    if let ExprKind::Name(name) = &target.kind {
                        found.insert(name.clone());
                    }
                }
                StmtKind::For { names, .. } => found.extend(names.iter().cloned()),
                StmtKind::With { name: Some(name), .. } => {
                    found.insert(name.clone());
                }
                _ => {}
            }
            for inner in blocks(stmt) {
                block(inner, found);
            }
        }
    }
    let mut found = HashSet::new();
    block(&def.body, &mut found);
    let mut walked = def.clone();
    fennec_syntax::visit::in_function(&mut walked, &mut |expr| match &expr.kind {
        ExprKind::Comprehension { names, .. } | ExprKind::Lambda { params: names, .. } => {
            found.extend(names.iter().cloned());
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                bound_by(&arm.pattern, &mut found);
            }
        }
        _ => {}
    });
    found
}

/// The names a pattern binds.
fn bound_by(pattern: &Pattern, found: &mut HashSet<String>) {
    match pattern {
        Pattern::Bind(name) => {
            found.insert(name.clone());
        }
        Pattern::Variant { args: items, .. } | Pattern::Tuple(items) | Pattern::Or(items) => {
            for item in items {
                bound_by(item, found);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

/// Where each comparison with `True` or `False` starts and ends, and whether what it compares is
/// surely a `bool`, judged in the innermost function it's in: those come after the functions
/// they're in, so their judgment is the one kept.
fn compared_bools(module: &Module) -> HashSet<(u32, u32)> {
    let mut judged: HashMap<(u32, u32), bool> = HashMap::new();
    for def in functions(module) {
        let rebound = rebound(def);
        let mut walked = def.clone();
        fennec_syntax::visit::in_function(&mut walked, &mut |expr| {
            if let ExprKind::Binary { op: BinOp::Eq | BinOp::NotEq, lhs, rhs } = &expr.kind
                && matches!(rhs.kind, ExprKind::Bool(_))
            {
                judged.insert((expr.span.start, expr.span.end), surely_bool(lhs, def, &rebound));
            }
        });
    }
    judged.into_iter().filter_map(|(at, sure)| sure.then_some(at)).collect()
}

/// What a comparison with `True`, `False` or `None` should say instead. One with `True` or
/// `False` is only flagged when what it compares is surely a `bool`.
fn plain_compare(expr: &Expr, sources: &Sources, bools: &HashSet<(u32, u32)>) -> Option<Lint> {
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
        ExprKind::Bool(_) if !bools.contains(&(expr.span.start, expr.span.end)) => return None,
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
    let bools = compared_bools(module);
    // Walking changes nothing, but the only walk there is takes a module it could change.
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        found.extend(plain_compare(expr, sources, &bools));
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
