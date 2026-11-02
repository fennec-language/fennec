//! `assert` belongs in tests. Anywhere else it stops the program the way a panic does: a rule
//! a value must keep belongs in its field's `where`, and a way a call can go wrong belongs in a
//! `fail` its caller handles. The lint matches the exact form, so it has nothing to get wrong.

use fennec_syntax::{Diagnostic, ExprKind, Module, Stmt, StmtKind};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// Each `assert` in `stmts`, the blocks inside them and the cases of a `match` they hold.
fn asserts(stmts: &[Stmt], found: &mut Vec<Lint>) {
    for stmt in stmts {
        let value = match &stmt.kind {
            StmtKind::Assert(_) => {
                let diagnostic = Diagnostic::new("`assert` is for tests", stmt.span).with_help(
                    "a rule a value keeps goes in its field's `where`, and a way this can go wrong in a `fail` its caller handles",
                );
                found.push(Lint::new("assert-outside-tests", diagnostic));
                None
            }
            StmtKind::Expr(value) | StmtKind::Assign { value, .. } | StmtKind::Return(Some(value)) => {
                Some(value)
            }
            _ => None,
        };
        if let Some(ExprKind::Match { arms, .. }) = value.map(|value| &value.kind) {
            for arm in arms {
                asserts(&arm.body, found);
            }
        }
        for block in blocks(stmt) {
            asserts(block, found);
        }
    }
}

/// Each `assert` outside a test: in a function or a method. A test's own functions, nested in
/// it, are part of the test, and the top of a file can't hold an `assert`.
pub(crate) fn of(module: &Module) -> Vec<Lint> {
    let mut found = Vec::new();
    for def in functions(module) {
        asserts(&def.body, &mut found);
    }
    found
}
