//! Lints with one plain way to say a thing, which `fennec fix` writes.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

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
                StmtKind::With { names, .. } => found.extend(names.iter().map(|held| held.name.clone())),
                _ => {}
            }
            for inner in blocks(stmt) {
                block(inner, found);
            }
        }
    }
    let mut found = HashSet::new();
    block(&def.body, &mut found);
    fennec_syntax::read::in_function(def, &mut |expr| match &expr.kind {
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
        Pattern::Bind(name) | Pattern::RefBind(name) => {
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
        fennec_syntax::read::in_function(def, &mut |expr| {
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

/// The operands of a chain of `+`, left to right. `a + b + c` is `(a + b) + c`, so the chain
/// runs down the left.
fn added(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Binary { op: BinOp::Add, lhs, rhs } => {
            let mut operands = added(lhs);
            operands.push(rhs);
            operands
        }
        _ => vec![expr],
    }
}

/// Text as it's written inside an f-string's quotes.
fn fstring_text(text: &str) -> String {
    let mut written = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => written.push_str("\\\\"),
            '"' => written.push_str("\\\""),
            '{' => written.push_str("{{"),
            '}' => written.push_str("}}"),
            '\n' => written.push_str("\\n"),
            '\t' => written.push_str("\\t"),
            '\r' => written.push_str("\\r"),
            ch if ch.is_control() => {
                let _ = write!(written, "\\u{:04x}", u32::from(ch));
            }
            ch => written.push(ch),
        }
    }
    written
}

/// What goes in an f-string's braces for one operand: what `str(...)` holds, or the operand
/// itself, which is text already. None when its source can't sit inside the quotes as it is:
/// a quote, a backslash, or a `:`, which would start a format.
fn filling(sources: &Sources, operand: &Expr) -> Option<String> {
    let inner = match &operand.kind {
        ExprKind::Call { callee, args }
            if matches!(&callee.kind, ExprKind::Name(name) if name == "str")
                && args.len() == 1
                && args.iter().all(|arg| arg.name.is_none() && !arg.is_mut) =>
        {
            &args.first()?.value
        }
        _ => operand,
    };
    let text = written(sources, inner.span)?;
    let fits = replaceable(sources, inner) && !text.contains(['"', '\'', '\\', ':', '\n', '#', '{', '}']);
    fits.then_some(text)
}

/// Text built with `+` and `str(...)`, as one f-string. A string literal in the chain makes
/// every `+` in it join text, since text only adds to text, so each other operand is text or
/// `str(...)` of something, and `f"{x}"` shows `x` just as `str(x)` does.
fn concat_to_fstring(expr: &Expr, sources: &Sources) -> Option<Lint> {
    let operands = added(expr);
    let literal = |operand: &&Expr| matches!(operand.kind, ExprKind::Str(_));
    let converted = |operand: &&Expr| matches!(&operand.kind, ExprKind::Call { callee, .. } if matches!(&callee.kind, ExprKind::Name(name) if name == "str"));
    if operands.len() < 2 || !operands.iter().any(literal) || !operands.iter().any(converted) {
        return None;
    }
    if !replaceable(sources, expr) || !written(sources, expr.span)?.starts_with(|ch: char| ch != '(') {
        return None;
    }
    let mut fixed = String::from("f\"");
    for operand in operands {
        match &operand.kind {
            // A raw string's source isn't what it holds, but its value is.
            ExprKind::Str(text) => fixed.push_str(&fstring_text(text)),
            _ => {
                let _ = write!(fixed, "{{{}}}", filling(sources, operand)?);
            }
        }
    }
    fixed.push('"');
    let diagnostic = Diagnostic::new(format!("build this text as one f-string: `{fixed}`"), expr.span)
        .with_fix(expr.span, fixed);
    Some(Lint::new("concat-to-fstring", diagnostic))
}

/// Every lint here that looks at one expression.
fn expressions(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    let bools = compared_bools(module);
    fennec_syntax::read::exprs(module, &mut |expr| {
        found.extend(plain_compare(expr, sources, &bools));
        found.extend(negated_compare(expr, sources));
        found.extend(plain_fstring(expr, sources));
        found.extend(concat_to_fstring(expr, sources));
    });
    // A chain of `+` holds shorter chains, which the whole one's fix covers.
    let chains: Vec<Span> = found
        .iter()
        .filter(|lint| lint.rule == "concat-to-fstring")
        .map(|lint| lint.diagnostic.span)
        .collect();
    let inside = |span: Span| {
        chains.iter().any(|chain| *chain != span && chain.start <= span.start && span.end <= chain.end)
    };
    found.retain(|lint| lint.rule != "concat-to-fstring" || !inside(lint.diagnostic.span));
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
    // A `def` gives its value with `return`, on its last line too, so no lint takes one off.
    for def in functions(module) {
        needless_pass(&def.body, &mut found);
    }
    found
}
