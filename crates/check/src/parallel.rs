//! `parallel(xs)` in a loop: each iteration changes only its own item. It can read anything
//! around it, but a write to a name from outside the loop could race with another iteration.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Stmt, StmtKind};

use crate::builtins;
use crate::function::FnCx;

/// What a loop goes over when it's `parallel(xs)`: `xs`.
pub(crate) fn parallel_items(iter: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let ExprKind::Name(name) = &callee.kind else { return None };
    let first = args.first().filter(|arg| arg.name.is_none() && !arg.is_mut)?;
    (name == "parallel").then_some(&first.value)
}

impl FnCx<'_> {
    /// Reports each write in a `parallel` loop's body to a name from outside the loop. `names`
    /// are the loop's own, which `for mut` may change.
    pub(crate) fn writes_only_its_own(&mut self, body: &[Stmt], names: &[String]) {
        let mut writes = Vec::new();
        self.writes_in(body, &mut writes);
        for (name, span) in writes {
            if names.contains(&name) || self.lookup(&name).is_none() {
                continue;
            }
            let message = format!("a `parallel` loop can't change `{name}`, which is outside it");
            self.error(Diagnostic::new(message, span).with_help(
                "each iteration changes only its own item: gather results with `sum(...)` or a list comprehension instead",
            ));
        }
    }

    fn writes_in(&self, body: &[Stmt], writes: &mut Vec<(String, fennec_syntax::Span)>) {
        for stmt in body {
            match &stmt.kind {
                StmtKind::Assign { target, value, .. } => {
                    let targets = match &target.kind {
                        ExprKind::Tuple(items) => items.iter().collect(),
                        _ => vec![target],
                    };
                    for target in targets {
                        if (!matches!(target.kind, ExprKind::Name(_)) || stmt_changes_name(stmt))
                            && let Some(root) = root_name(target)
                        {
                            writes.push((root.to_owned(), target.span));
                        }
                    }
                    self.writes_in_expr(value, writes);
                }
                StmtKind::Expr(value)
                | StmtKind::Fail(value)
                | StmtKind::Yield(value)
                | StmtKind::Assert(value)
                | StmtKind::Return(Some(value)) => self.writes_in_expr(value, writes),
                StmtKind::If { branches, otherwise } => {
                    for (cond, block) in branches {
                        self.writes_in_expr(cond, writes);
                        self.writes_in(block, writes);
                    }
                    self.writes_in(otherwise.as_deref().unwrap_or_default(), writes);
                }
                StmtKind::For { is_mut, iter, body: block, .. } => {
                    if *is_mut && let Some(root) = root_name(parallel_items(iter).unwrap_or(iter)) {
                        writes.push((root.to_owned(), iter.span));
                    }
                    self.writes_in_expr(iter, writes);
                    self.writes_in(block, writes);
                }
                StmtKind::While { cond: value, body: block } | StmtKind::With { value, body: block, .. } => {
                    self.writes_in_expr(value, writes);
                    self.writes_in(block, writes);
                }
                StmtKind::Def(_)
                | StmtKind::Return(None)
                | StmtKind::Break
                | StmtKind::Continue
                | StmtKind::Pass => {}
            }
        }
    }

    fn writes_in_expr(&self, expr: &Expr, writes: &mut Vec<(String, fennec_syntax::Span)>) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            for arg in args.iter().filter(|arg| arg.is_mut) {
                if let Some(root) = root_name(&arg.value) {
                    writes.push((root.to_owned(), arg.value.span));
                }
            }
            if let ExprKind::Attr { object, name, .. } = &callee.kind
                && let Some(root) = root_name(object)
                && self.changes_on(object, root, name)
            {
                writes.push((root.to_owned(), object.span));
            }
        }
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                self.writes_in(&arm.body, writes);
            }
        }
        for child in crate::effects::children(expr) {
            self.writes_in_expr(child, writes);
        }
    }

    /// Whether calling `method` on `object`, rooted at an outer name, changes it.
    fn changes_on(&self, object: &Expr, root: &str, method: &str) -> bool {
        let ty =
            if matches!(object.kind, ExprKind::Name(_)) { self.lookup(root).map(|(ty, _)| ty) } else { None };
        let Some(ty) = ty else { return false };
        if let Some(found) = builtins::method(&ty, method) {
            return found.changes;
        }
        let Some(owner) = ty.type_name() else { return false };
        let found = self.checker.program.method(owner, method);
        found.is_some_and(|(_, entry)| entry.def.params.first().is_some_and(|param| param.is_mut))
    }
}

/// Whether an assignment to a plain name gives an existing name a new value, rather than
/// declaring one: `x += 1`, or `x = 1` when `x` is already there.
fn stmt_changes_name(stmt: &Stmt) -> bool {
    matches!(
        &stmt.kind,
        StmtKind::Assign { op: Some(_), .. } | StmtKind::Assign { declare_mut: false, ty: None, .. }
    )
}

/// The name a place starts from: `units` in `units[i].hp`.
fn root_name(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => root_name(object),
        _ => None,
    }
}
