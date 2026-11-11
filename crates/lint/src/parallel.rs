//! Writes to standard output or error from inside a `parallel` loop. Its body runs on several
//! tasks at once, so what they print comes out in whatever order they reach it, which can change
//! from one run to the next. That's allowed, and sometimes wanted, as for progress lines, so it's
//! a hint: for the items' order, each item's line comes out of the loop, and one place prints them.

use std::collections::BTreeSet;

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Module, Span, Stmt, StmtKind};

use crate::Lint;
use crate::sizes::{blocks, functions};

pub(crate) fn of(module: &Module) -> Vec<Lint> {
    let mut found = BTreeSet::new();
    for def in functions(module) {
        let streams = streams(def);
        if !streams.is_empty() {
            in_block(&def.body, &streams, &mut found);
        }
    }
    found.into_iter().map(|(start, end)| Lint::hint("parallel-print", hint(Span { start, end }))).collect()
}

/// The parameters a function is given standard output or error by.
fn streams(def: &FnDef) -> Vec<&str> {
    def.params
        .iter()
        .filter(|param| param.ty.as_ref().is_some_and(|ty| matches!(ty.name.as_str(), "Stdout" | "Stderr")))
        .map(|param| param.name.as_str())
        .collect()
}

/// Where each `parallel` loop in `stmts`, at any depth, uses one of `streams`. A use inside two
/// nested loops is found once.
fn in_block(stmts: &[Stmt], streams: &[&str], found: &mut BTreeSet<(u32, u32)>) {
    for stmt in stmts {
        if let StmtKind::For { iter, body, .. } = &stmt.kind
            && is_parallel(iter)
        {
            fennec_syntax::read::in_stmts(body, &mut |expr| {
                if let Some(at) = writes(expr, streams) {
                    found.insert((at.start, at.end));
                }
            });
        }
        for block in blocks(stmt) {
            in_block(block, streams, found);
        }
    }
}

/// Whether a loop goes over `parallel(...)`.
fn is_parallel(iter: &Expr) -> bool {
    matches!(&iter.kind, ExprKind::Call { callee, .. } if matches!(&callee.kind, ExprKind::Name(name) if name == "parallel"))
}

/// Where `expr` calls a method of one of `streams`, as `stdout.print(...)` or `stderr.write(...)`.
fn writes(expr: &Expr, streams: &[&str]) -> Option<Span> {
    let ExprKind::Call { callee, .. } = &expr.kind else { return None };
    let ExprKind::Attr { object, .. } = &callee.kind else { return None };
    let ExprKind::Name(name) = &object.kind else { return None };
    streams.contains(&name.as_str()).then_some(expr.span)
}

fn hint(at: Span) -> Diagnostic {
    Diagnostic::new(
        "what a `parallel` loop's body prints has no fixed order: its tasks print as each gets there",
        at,
    )
    .with_help(
        "for the items' order, give each one's line from the loop and print them in one place: \
         `for line in (render(x) for x in parallel(xs)): stdout.print(line)`; printing here is \
         fine where any order will do, as for progress",
    )
}
