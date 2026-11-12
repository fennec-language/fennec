//! A `for ref` whose body reads what it loops over sees its item as it is now. Every engine runs
//! a `for ref` by taking the item out and putting it back when the round ends, so a read of the
//! list in between would see it as it was. Such a loop is written in to go by position, or by
//! `Id` for a `Table`, with each use of its name the item where it is:
//!
//! ```fennec
//! for ref unit in world.units:
//!     unit.target = nearest(world.units, unit.at)
//! ```
//!
//! is checked, then written in as
//!
//! ```fennec
//! for unit__live7_at in range(world.units.len()):
//!     world.units[unit__live7_at].target = nearest(world.units, world.units[unit__live7_at].at)
//! ```
//!
//! The loop can't add to or take from what it goes over, as `loops` sees to, so its length is
//! read once. An index in the place, like `r` in `for ref cell in grid[r]`, is worked out once
//! ahead of the loop, as it was. A loop that never reads what it goes over is left alone, since
//! nothing can tell the two apart.
//!
//! `ref unit = world.units[actor]` is written in the same way, always: its indexes are held in
//! names where it's declared, and each use of `unit` after it in its block is the place itself,
//! `world.units[unit__place9_0]`. Nothing else changes what it's in while it's in use, as `loops`
//! sees to, so it costs nothing a program writing the place out wouldn't.

use std::collections::BTreeMap;

use fennec_syntax::{Arg, Expr, ExprKind, FnDef, Module, Pattern, Stmt, StmtKind};

use crate::checker::Checker;
use crate::lend_lowering::{assign, here, name, stmt};
use crate::loops::{Place, place};
use crate::types::Type;

/// How a `for ref` written in goes through what it loops over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Through {
    /// `range(len(items))`, with each item `items[at]`.
    Positions,
    /// `items.ids()`, with each item `items[id]`.
    Ids,
}

/// The `for ref` loops in a function whose bodies read what they loop over, by the loop's span.
pub(crate) fn live_loops(checker: &Checker, key: &str, def: &FnDef) -> Vec<((u32, u32), Through)> {
    let mut found = Vec::new();
    let body = checker.typed.get(key);
    loops_in(&def.body, &mut |each| {
        let StmtKind::For { names, is_mut: true, iter, body: block } = &each.kind else { return };
        let ([item], Some(looped)) = (names.as_slice(), place(iter)) else { return };
        let through = match body.and_then(|body| body.expr(iter.span)) {
            Some(Type::List(_) | Type::Array(..) | Type::Buffer(_)) => Through::Positions,
            Some(Type::Table(_)) => Through::Ids,
            _ => return,
        };
        let mut read = Vec::new();
        reads_in(block, &mut read);
        if read.iter().any(|read| read.overlaps(&looped)) && !binds(block, item) {
            found.push(((each.span.start, each.span.end), through));
        }
    });
    found
}

/// Calls `each` on every statement in a body, those in nested blocks and functions included.
fn loops_in(body: &[Stmt], each: &mut dyn FnMut(&Stmt)) {
    for stmt in body {
        each(stmt);
        for block in crate::loops::blocks(stmt) {
            loops_in(block, each);
        }
        if let StmtKind::Def(def) = &stmt.kind {
            loops_in(&def.body, each);
        }
    }
    fennec_syntax::read::in_stmts(body, &mut |expr| {
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                loops_in(&arm.body, each);
            }
        }
    });
}

/// The places some statements read, nested functions' included. A method call reads what it's
/// called on whole, and a name or place passed anywhere is read whole.
fn reads_in(body: &[Stmt], found: &mut Vec<Place>) {
    for stmt in body {
        for value in crate::loops::exprs(stmt) {
            reads(value, found);
        }
        for block in crate::loops::blocks(stmt) {
            reads_in(block, found);
        }
        match &stmt.kind {
            StmtKind::Def(def) => reads_in(&def.body, found),
            StmtKind::OnFailure { cleanup, .. } => reads_in(cleanup, found),
            StmtKind::Lend { otherwise, .. } => reads_in(otherwise, found),
            _ => {}
        }
    }
}

fn reads(value: &Expr, found: &mut Vec<Place>) {
    if let ExprKind::Call { callee, args } = &value.kind
        && let ExprKind::Attr { object, .. } = &callee.kind
    {
        reads(object, found);
        for arg in args {
            reads(&arg.value, found);
        }
        return;
    }
    if let Some(read) = place(value) {
        found.push(read);
        indexes_read(value, found);
        return;
    }
    match &value.kind {
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                reads_in(&arm.body, found);
            }
        }
        ExprKind::Lambda { body, .. } => reads(body, found),
        _ => {}
    }
    for inner in crate::effects::children(value) {
        reads(inner, found);
    }
}

/// What the indexes along a place read.
fn indexes_read(value: &Expr, found: &mut Vec<Place>) {
    match &value.kind {
        ExprKind::Attr { object, .. } => indexes_read(object, found),
        ExprKind::Index { object, index } => {
            reads(index, found);
            indexes_read(object, found);
        }
        _ => {}
    }
}

/// Whether a body gives `held` a meaning of its own anywhere: declares it again, loops with it,
/// or binds it in a function, arrow function, comprehension or case. Writing to it, `held = x`,
/// writes to the item.
fn binds(body: &[Stmt], held: &str) -> bool {
    let mut bound = false;
    loops_in(body, &mut |stmt| match &stmt.kind {
        StmtKind::Assign { target, declare_mut, ty, .. } => {
            bound |= (*declare_mut || ty.is_some()) && names_in(target).iter().any(|name| name == held);
        }
        StmtKind::For { names, .. } => bound |= names.iter().any(|name| name == held),
        StmtKind::With { names, .. } => bound |= names.iter().any(|name| name.name == held),
        StmtKind::OnFailure { name, .. } => bound |= name == held,
        StmtKind::Def(def) => {
            bound |= def.name == held || def.params.iter().any(|param| param.name == held);
        }
        _ => {}
    });
    fennec_syntax::read::in_stmts(body, &mut |expr| match &expr.kind {
        ExprKind::Lambda { params, .. } => bound |= params.iter().any(|param| param == held),
        ExprKind::Comprehension { names, .. } => bound |= names.iter().any(|name| name == held),
        ExprKind::Match { arms, .. } => {
            bound |= arms.iter().any(|arm| pattern_binds(&arm.pattern, held));
        }
        _ => {}
    });
    bound
}

fn names_in(target: &Expr) -> Vec<String> {
    match &target.kind {
        ExprKind::Name(name) => vec![name.clone()],
        ExprKind::Tuple(items) => items.iter().flat_map(names_in).collect(),
        _ => Vec::new(),
    }
}

fn pattern_binds(pattern: &Pattern, held: &str) -> bool {
    match pattern {
        Pattern::Bind(name) | Pattern::RefBind(name) => name == held,
        Pattern::Variant { args: patterns, .. } | Pattern::Tuple(patterns) | Pattern::Or(patterns) => {
            patterns.iter().any(|pattern| pattern_binds(pattern, held))
        }
        Pattern::Wildcard | Pattern::Literal(_) => false,
    }
}

/// Writes in each `ref name = place`, and each `for ref` loop the check found reading what it
/// loops over, and says whether there was one.
pub(crate) fn lower(module: &mut Module, live: &BTreeMap<(u32, u32), Through>) -> bool {
    let mut written = false;
    fennec_syntax::visit::functions_mut(module, &mut |def| stmts(&mut def.body, live, &mut written));
    written
}

fn stmts(body: &mut Vec<Stmt>, live: &BTreeMap<(u32, u32), Through>, written: &mut bool) {
    let mut at = 0;
    while at < body.len() {
        let Some(each) = body.get_mut(at) else { return };
        inner(each, live, written);
        if let Some((held, rest)) = named_place(body, at) {
            *written = true;
            let count = held.len();
            body.splice(at..=at, held);
            at += count;
            body.truncate(at);
            body.extend(rest);
            continue;
        }
        let Some(each) = body.get_mut(at) else { return };
        let through = live.get(&(each.span.start, each.span.end)).copied();
        match through.and_then(|through| by_place(each, through)) {
            Some(lowered) => {
                *written = true;
                let count = lowered.len();
                body.splice(at..=at, lowered);
                at += count;
            }
            None => at += 1,
        }
    }
}

/// The blocks inside a statement, written in first.
fn inner(each: &mut Stmt, live: &BTreeMap<(u32, u32), Through>, written: &mut bool) {
    match &mut each.kind {
        StmtKind::If { branches, otherwise } => {
            for (_, body) in branches {
                stmts(body, live, written);
            }
            if let Some(body) = otherwise {
                stmts(body, live, written);
            }
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::OnFailure { body, .. } => stmts(body, live, written),
        StmtKind::Lend { otherwise, .. } => stmts(otherwise, live, written),
        StmtKind::Def(def) => stmts(&mut def.body, live, written),
        _ => {}
    }
    fennec_syntax::visit::in_stmts(std::slice::from_mut(each), &mut |expr| {
        if let ExprKind::Match { arms, .. } = &mut expr.kind {
            for arm in arms {
                stmts(&mut arm.body, live, written);
            }
        }
    });
}

/// `ref unit = units[at]` at `at` in a block, written in as what holds its indexes, and the
/// rest of the block with each use of `unit` the place itself.
fn named_place(body: &[Stmt], at: usize) -> Option<(Vec<Stmt>, Vec<Stmt>)> {
    let each = body.get(at)?;
    let (name, place) = each.kind.local_ref()?;
    let tag = format!("{name}__place{}", each.span.start);
    let mut held = Vec::new();
    let named = indexes_held(place, &tag, &mut held);
    let mut rest = body.get(at + 1..).unwrap_or_default().to_vec();
    fennec_syntax::visit::in_stmts(&mut rest, &mut |expr| {
        if matches!(&expr.kind, ExprKind::Name(used) if used == name) {
            *expr = respanned(&named);
        }
    });
    Some((held, rest))
}

/// `for ref item in items:` written in as a loop by position or `Id`, the indexes `items` is
/// reached by held ahead of it.
fn by_place(each: &Stmt, through: Through) -> Option<Vec<Stmt>> {
    let StmtKind::For { names, is_mut: true, iter, body } = &each.kind else { return None };
    let [item] = names.as_slice() else { return None };
    let tag = format!("{item}__live{}", each.span.start);
    let mut held = Vec::new();
    let items = indexes_held(iter, &tag, &mut held);
    let count = format!("{tag}_at");
    let span = each.span;
    let looped = match through {
        Through::Positions => call(name("range", span), vec![method(&items, "len")]),
        Through::Ids => method(&items, "ids"),
    };
    let mut body = body.clone();
    fennec_syntax::visit::in_stmts(&mut body, &mut |expr| {
        if matches!(&expr.kind, ExprKind::Name(named) if named == item) {
            let index = Box::new(name(&count, expr.span));
            *expr = Expr {
                kind: ExprKind::Index { object: Box::new(respanned(&items)), index },
                span: here(expr.span),
            };
        }
    });
    let kind = StmtKind::For { names: vec![count], is_mut: false, iter: looped, body };
    held.push(stmt(kind, span));
    Some(held)
}

/// `items` with each index along it that isn't a number held in a name ahead of the loop.
fn indexes_held(items: &Expr, tag: &str, held: &mut Vec<Stmt>) -> Expr {
    let kind = match &items.kind {
        ExprKind::Attr { object, name: field, optional } => ExprKind::Attr {
            object: Box::new(indexes_held(object, tag, held)),
            name: field.clone(),
            optional: *optional,
        },
        ExprKind::Index { object, index } => {
            let object = Box::new(indexes_held(object, tag, held));
            let index = if matches!(index.kind, ExprKind::Int(_)) {
                index.clone()
            } else {
                let holder = format!("{tag}_{}", held.len());
                held.push(assign(&holder, index.as_ref().clone(), false, None));
                Box::new(name(&holder, index.span))
            };
            ExprKind::Index { object, index }
        }
        other => other.clone(),
    };
    Expr { kind, span: items.span }
}

/// A copy of an expression with spans of its own, so the types it's checked again with are its.
fn respanned(value: &Expr) -> Expr {
    let mut copy = value.clone();
    fennec_syntax::visit::in_expr(&mut copy, &mut |inner| inner.span = here(inner.span));
    copy
}

/// `items.method_name()`.
fn method(items: &Expr, method_name: &str) -> Expr {
    let object = Box::new(respanned(items));
    let callee = ExprKind::Attr { object, name: method_name.to_owned(), optional: false };
    call(Expr { kind: callee, span: here(items.span) }, Vec::new())
}

fn call(callee: Expr, args: Vec<Expr>) -> Expr {
    let span = here(callee.span);
    let args = args.into_iter().map(|value| Arg { name: None, is_mut: false, value }).collect();
    Expr { kind: ExprKind::Call { callee: Box::new(callee), args }, span }
}
