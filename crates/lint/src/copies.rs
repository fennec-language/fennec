//! What other lints that try a fix need to know: where a program names a place, and whether it
//! gives a builtin's name a meaning of its own.

use std::collections::HashSet;

use fennec_syntax::{Expr, ExprKind, Item, Module, Stmt, StmtKind};

use crate::sizes::{blocks, functions};

/// Whether the program gives a builtin's name a meaning of its own: a function, or a name some
/// function binds. Then a call to it isn't the builtin's.
pub(crate) fn redefined(module: &Module, builtin: &str) -> bool {
    let defined = module.items.iter().any(|item| matches!(item, Item::Function(def) if def.name == builtin));
    defined
        || functions(module)
            .iter()
            .any(|def| def.params.iter().any(|param| param.name == builtin) || binds(&def.body, builtin))
}

fn binds(stmts: &[Stmt], builtin: &str) -> bool {
    stmts.iter().any(|stmt| {
        let assigned = matches!(&stmt.kind, StmtKind::Assign { target, .. }
            if matches!(&target.kind, ExprKind::Name(name) if name == builtin));
        let looped =
            matches!(&stmt.kind, StmtKind::For { names, .. } if names.iter().any(|name| name == builtin));
        assigned || looped || blocks(stmt).into_iter().any(|block| binds(block, builtin))
    })
}

/// The start of a path: `make(x)` in `make(x).stats[0]`.
fn root(expr: &Expr) -> &Expr {
    match &expr.kind {
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => root(object),
        _ => expr,
    }
}

/// Where a place is named, by the span of the start of its path: what `mut x = ...` declares,
/// what `for ref` loops over, and each `ref` argument, anywhere: in a function, a test, a nested
/// function or a case of a `match`.
pub(crate) fn places(module: &Module) -> HashSet<(u32, u32)> {
    fn block(stmts: &[Stmt], found: &mut HashSet<(u32, u32)>) {
        for stmt in stmts {
            let named = match &stmt.kind {
                StmtKind::Assign { value, declare_mut: true, .. } => Some(value),
                StmtKind::For { is_mut: true, iter, .. } => Some(crate::dead::parallel_items(iter)),
                _ => None,
            };
            if let Some(value) = named {
                found.insert((root(value).span.start, root(value).span.end));
            }
            if let StmtKind::Def(def) = &stmt.kind {
                block(&def.body, found);
            }
            for inner in blocks(stmt) {
                block(inner, found);
            }
        }
    }
    let mut found = HashSet::new();
    for def in functions(module) {
        block(&def.body, &mut found);
    }
    for item in &module.items {
        if let Item::Test(test) = item {
            block(&test.body, &mut found);
        }
    }
    fennec_syntax::read::exprs(module, &mut |expr| match &expr.kind {
        ExprKind::Call { args, .. } => {
            for arg in args.iter().filter(|arg| arg.is_mut) {
                found.insert((root(&arg.value).span.start, root(&arg.value).span.end));
            }
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                block(&arm.body, &mut found);
            }
        }
        _ => {}
    });
    found
}
