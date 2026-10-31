//! Looking packed lists up once per loop. Native code finds a packed list's items by asking
//! the runtime for the name's list, which costs two lookups by name. A loop that only ever
//! indexes a list (`xs[i]`, `xs[i] = v`, `xs[i].field`, `xs.len()`) can't make it grow, or
//! share it, or hand it anywhere: Fennec has no references, so nothing else in the loop can
//! reach it. So the loop asks once, before it starts, and every access inside uses the
//! answer.

use cranelift_codegen::ir::Value;
use fennec_syntax::{Diagnostic, Expr, ExprKind, FStrPart, Span, Stmt, StmtKind};

use super::Lowerer;

/// A packed list looked up once for a loop.
#[derive(Clone)]
pub(super) struct Hoisted {
    /// The variable of the name holding it: another name can be the same text, in code
    /// inlined here.
    var: cranelift_frontend::Variable,
    /// Where its items start.
    pub(super) items: Value,
    pub(super) length: Value,
    /// Whether the address is one items can be changed through.
    pub(super) writable: bool,
}

impl Lowerer<'_, '_> {
    /// Looks up, before a loop, every packed list its condition and body only index. The
    /// names the loop itself binds, like a `for` loop's, are never the outer lists.
    /// Gives back how many it looked up, to forget when the loop ends.
    pub(super) fn hoist_lists(
        &mut self,
        bound: &[String],
        cond: Option<&Expr>,
        body: &[Stmt],
    ) -> Result<usize, Diagnostic> {
        let mut mentioned = Vec::new();
        for stmt in body {
            super::fallback::stmt_names(stmt, &mut mentioned);
        }
        if let Some(cond) = cond {
            super::fallback::expr_names(cond, &mut mentioned);
        }
        mentioned.sort();
        mentioned.dedup();
        let mut count = 0;
        for name in mentioned.into_iter().filter(|name| !bound.contains(name)) {
            let Some((_, item)) =
                self.packed(&Expr { kind: ExprKind::Name(name.clone()), span: Span::default() })
            else {
                continue;
            };
            if self.hoisted(&name).is_some() {
                continue;
            }
            let Some(writes) = indexed_only(cond, body, &name) else { continue };
            let length = self.list_length(&name, item)?;
            let items = self.items_at(&name, item, writes)?;
            let var = self.local(&name).and_then(|local| local.var);
            let var = var.ok_or_else(|| super::internal("a list without a variable", Span::default()))?;
            self.hoisted.push(Hoisted { var, items, length, writable: writes });
            count += 1;
        }
        Ok(count)
    }

    /// Forgets the lists a loop looked up.
    pub(super) fn unhoist(&mut self, count: usize) {
        let keep = self.hoisted.len().saturating_sub(count);
        self.hoisted.truncate(keep);
    }

    /// A list the loops being lowered looked up, if one did.
    pub(super) fn hoisted(&self, name: &str) -> Option<&Hoisted> {
        let var = self.local(name)?.var?;
        self.hoisted.iter().rev().find(|hoisted| hoisted.var == var)
    }
}

/// Whether a loop's condition and body mention `name` only to index it or take its length.
/// Gives back whether any of that changes an item, or `None` when something else uses it.
fn indexed_only(cond: Option<&Expr>, body: &[Stmt], name: &str) -> Option<bool> {
    let mut mentions = 0;
    let mut indexed = 0;
    let mut count = |expr: &Expr| match &expr.kind {
        ExprKind::Name(found) if found == name => mentions += 1,
        ExprKind::Index { object, .. } if is_name(object, name) => indexed += 1,
        ExprKind::Call { callee, args }
            if args.is_empty()
                && matches!(&callee.kind, ExprKind::Attr { object, name: method, optional: false }
                    if method == "len" && is_name(object, name)) =>
        {
            indexed += 1;
        }
        _ => {}
    };
    if let Some(cond) = cond {
        each_expr(cond, &mut count);
    }
    for stmt in body {
        each_stmt_expr(stmt, &mut count);
    }
    if mentions != indexed {
        return None;
    }
    // Native code changes items only by assigning to them. Anything else that could
    // change one, like a method that isn't compiled, goes through the interpreter, which
    // could replace the list.
    let mut writes = false;
    let mut safe = true;
    for stmt in body {
        each_stmt(stmt, &mut |stmt| match &stmt.kind {
            StmtKind::Assign { target, .. } if rooted_at_item(target, name) => writes = true,
            StmtKind::For { names, is_mut, iter, .. } => {
                safe &= !names.iter().any(|bound| bound == name) && (!*is_mut || !rooted_at_item(iter, name));
            }
            StmtKind::With { name: Some(bound), .. } => safe &= bound != name,
            StmtKind::Def(_) => safe = false,
            _ => {}
        });
        each_stmt_expr(stmt, &mut |expr| match &expr.kind {
            ExprKind::Call { callee, args } => {
                let on_item =
                    matches!(&callee.kind, ExprKind::Attr { object, .. } if rooted_at_item(object, name));
                let changes_item = args.iter().any(|arg| arg.is_mut && rooted_at_item(&arg.value, name));
                safe &= !on_item && !changes_item;
            }
            ExprKind::Comprehension { names, .. } => safe &= !names.iter().any(|bound| bound == name),
            ExprKind::Lambda { params, .. } => safe &= !params.iter().any(|bound| bound == name),
            ExprKind::Match { arms, .. } => safe &= !arms.iter().any(|arm| binds(&arm.pattern, name)),
            _ => {}
        });
    }
    safe.then_some(writes)
}

/// Whether a pattern gives `name` a value.
fn binds(pattern: &fennec_syntax::Pattern, name: &str) -> bool {
    match pattern {
        fennec_syntax::Pattern::Bind(bound) => bound == name,
        fennec_syntax::Pattern::Variant { args, .. }
        | fennec_syntax::Pattern::Tuple(args)
        | fennec_syntax::Pattern::Or(args) => args.iter().any(|arg| binds(arg, name)),
        fennec_syntax::Pattern::Wildcard | fennec_syntax::Pattern::Literal(_) => false,
    }
}

fn is_name(expr: &Expr, name: &str) -> bool {
    matches!(&expr.kind, ExprKind::Name(found) if found == name)
}

/// Whether an expression is an item of the list `name`, or a field of one.
fn rooted_at_item(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::Index { object, .. } => is_name(object, name) || rooted_at_item(object, name),
        ExprKind::Attr { object, .. } => rooted_at_item(object, name),
        _ => false,
    }
}

/// Calls `visit` on a statement and every statement inside it.
pub(super) fn each_stmt(stmt: &Stmt, visit: &mut dyn FnMut(&Stmt)) {
    visit(stmt);
    for inner in inner_blocks(stmt).into_iter().flatten() {
        each_stmt(inner, visit);
    }
    // Statements inside `match` arms, inside expressions.
    each_stmt_expr_shallow(stmt, &mut |expr| {
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                for inner in &arm.body {
                    each_stmt(inner, visit);
                }
            }
        }
    });
}

/// The blocks directly inside a statement.
fn inner_blocks(stmt: &Stmt) -> Vec<&[Stmt]> {
    match &stmt.kind {
        StmtKind::If { branches, otherwise } => {
            branches.iter().map(|(_, body)| body.as_slice()).chain(otherwise.as_deref()).collect()
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            vec![body.as_slice()]
        }
        StmtKind::Def(def) => vec![def.body.as_slice()],
        _ => Vec::new(),
    }
}

/// The expressions written directly in a statement, not in the blocks inside it.
fn own_exprs(stmt: &Stmt) -> Vec<&Expr> {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Fail(expr) | StmtKind::Yield(expr) | StmtKind::Assert(expr) => {
            vec![expr]
        }
        StmtKind::Return(value) => value.iter().collect(),
        StmtKind::Assign { target, value, .. } => vec![target, value],
        StmtKind::If { branches, .. } => branches.iter().map(|(cond, _)| cond).collect(),
        StmtKind::While { cond, .. } => vec![cond],
        StmtKind::For { iter, .. } => vec![iter],
        StmtKind::With { value, .. } => vec![value],
        StmtKind::Def(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => Vec::new(),
    }
}

/// Calls `visit` on the top expressions written directly in a statement.
fn each_stmt_expr_shallow(stmt: &Stmt, visit: &mut dyn FnMut(&Expr)) {
    for expr in own_exprs(stmt) {
        each_expr(expr, visit);
    }
}

/// Calls `visit` on every expression in a statement, the blocks and arms inside it too.
pub(super) fn each_stmt_expr(stmt: &Stmt, visit: &mut dyn FnMut(&Expr)) {
    for expr in own_exprs(stmt) {
        each_expr(expr, visit);
    }
    for inner in inner_blocks(stmt).into_iter().flatten() {
        each_stmt_expr(inner, visit);
    }
}

/// Calls `visit` on an expression and every expression inside it, including arrow
/// functions' bodies and the statements of `match` arms.
fn each_expr(expr: &Expr, visit: &mut dyn FnMut(&Expr)) {
    visit(expr);
    let mut inner: Vec<&Expr> = Vec::new();
    match &expr.kind {
        ExprKind::FStr(parts) => inner.extend(parts.iter().filter_map(|part| match part {
            FStrPart::Expr { expr, .. } => Some(expr),
            FStrPart::Text(_) => None,
        })),
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => inner.extend(items),
        ExprKind::Dict(pairs) => inner.extend(pairs.iter().flat_map(|(key, value)| [key, value])),
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            inner.extend(
                [Some(elem.as_ref()), value.as_deref(), Some(iter.as_ref()), cond.as_deref()]
                    .into_iter()
                    .flatten(),
            );
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Attr { object: expr, .. }
        | ExprKind::Lambda { body: expr, .. } => {
            inner.push(expr);
        }
        ExprKind::Binary { lhs: a, rhs: b, .. }
        | ExprKind::Else { value: a, default: b }
        | ExprKind::Index { object: a, index: b } => inner.extend([a.as_ref(), b.as_ref()]),
        ExprKind::Cond { then, cond, otherwise } => {
            inner.extend([then.as_ref(), cond.as_ref(), otherwise.as_ref()]);
        }
        ExprKind::Try { value, context } => {
            inner.extend(std::iter::once(value.as_ref()).chain(context.as_deref()));
        }
        ExprKind::Call { callee, args } => {
            inner.push(callee);
            inner.extend(args.iter().map(|arg| &arg.value));
        }
        ExprKind::Slice { object, start, end } => {
            inner.extend(std::iter::once(object.as_ref()).chain(start.as_deref()).chain(end.as_deref()));
        }
        ExprKind::Match { subject, arms } => {
            inner.push(subject);
            for arm in arms {
                for stmt in &arm.body {
                    each_stmt_expr(stmt, visit);
                }
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Name(_)
        | ExprKind::Variant(_) => {}
    }
    for expr in inner {
        each_expr(expr, visit);
    }
}

#[cfg(test)]
mod tests {
    use fennec_syntax::{Item, StmtKind};

    use super::indexed_only;

    /// What `indexed_only` says about the `while` loop in a function's body, for `xs`.
    fn loop_uses(body: &str) -> Option<bool> {
        let source = format!("def f(mut xs: list[int]) -> int:\n    mut at = 0\n{body}    at\n");
        let module = fennec_syntax::parse(&source).unwrap();
        let Some(Item::Function(def)) = module.items.first() else { panic!("no function") };
        let Some(StmtKind::While { cond, body }) = def.body.get(1).map(|stmt| &stmt.kind) else {
            panic!("no loop")
        };
        indexed_only(Some(cond), body, "xs")
    }

    #[test]
    fn a_loop_that_indexes_a_list_and_takes_its_length_can_hoist_it() {
        assert_eq!(loop_uses("    while at < xs.len():\n        at += xs[at]\n"), Some(false));
        assert_eq!(loop_uses("    while at < xs.len():\n        xs[at] = 1\n        at += 1\n"), Some(true));
    }

    #[test]
    fn any_other_call_on_the_list_keeps_it_from_being_hoisted() {
        assert_eq!(loop_uses("    while at < xs.len():\n        xs.clear()\n        at += 1\n"), None);
        assert_eq!(loop_uses("    while at < 3:\n        xs.append(at)\n        at += 1\n"), None);
        assert_eq!(loop_uses("    while at < xs.len(1):\n        at += 1\n"), None);
    }
}
