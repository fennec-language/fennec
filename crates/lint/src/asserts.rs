//! `assert` belongs in tests. Anywhere else it stops the program the way a panic does: a rule
//! a value must keep belongs in its field's `where`, and a way a call can go wrong belongs in a
//! `fail` its caller handles. Likewise `.expect(...)` in a kit's code: a library gives a failure
//! back with `?` and lets its caller decide. Both match an exact form, so they have nothing to
//! get wrong.

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Item, Module, Stmt, StmtKind};

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

/// Whether a call is `something.expect(...)`.
fn is_expect(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Call { callee, .. }
        if matches!(&callee.kind, ExprKind::Attr { name, .. } if name == "expect"))
}

/// Each `.expect(...)` in a function, its nested functions included.
fn expects(def: &FnDef, found: &mut Vec<Lint>) {
    fennec_syntax::read::in_function(def, &mut |expr| {
        if is_expect(expr) {
            let diagnostic = Diagnostic::new("a kit's code doesn't decide to stop the program", expr.span)
                .with_help("give the failure back with `?`, and let the caller decide what to do with it");
            found.push(Lint::new("expect-in-kit", diagnostic));
        }
    });
}

/// Whether a program declares a function or method named `expect`, nested ones too.
pub(crate) fn declares_expect(module: &Module) -> bool {
    functions(module).iter().any(|def| def.name == "expect")
}

/// Each `.expect(...)` in a kit's code outside `main` and its tests: a library gives a failure
/// back for its caller to handle. A kit that declares a method named `expect` itself is left
/// alone, since the lint can't tell its calls from the standard one.
pub(crate) fn in_kit(module: &Module, declares_expect: bool) -> Vec<Lint> {
    if declares_expect {
        return Vec::new();
    }
    let mut found = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) if def.name != "main" => expects(def, &mut found),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                for method in &def.methods {
                    expects(method, &mut found);
                }
            }
            Item::Extend(extension) => {
                for method in &extension.methods {
                    expects(method, &mut found);
                }
            }
            _ => {}
        }
    }
    found
}
