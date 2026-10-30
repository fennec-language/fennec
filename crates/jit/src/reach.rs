//! Which functions a run can reach, starting from `main`, so the JIT compiles only those.
//! Skipping a function is always safe: if one is called after all, through the
//! interpreter, say, the interpreter runs it.
//!
//! The search errs toward including: a method is reached when any code calls a method of
//! that name, a type's own operators when the operator appears anywhere, and a struct's
//! field defaults whenever the struct's name does. A standard library type's methods are
//! reached only once its name appears too, or a method that gives one back is called, so a
//! program that never uses a `Deque` doesn't compile `Deque.append` for its lists.

use std::collections::HashSet;

use fennec_syntax::{BinOp, Expr, ExprKind, FStrPart, Item, Module, Pattern, Stmt, StmtKind, TypeDef};

use crate::types::Function;

/// Standard library types a program gets values of without naming them, by the method
/// that gives one back.
const GIVEN: [(&str, &str); 5] = [
    ("now", "Instant"),
    ("match", "Match"),
    ("fullmatch", "Match"),
    ("search", "Match"),
    ("find_all", "Match"),
];

/// The names code mentions: plain names, names after a dot, and operators' method names.
#[derive(Default)]
pub(crate) struct Mentions {
    names: HashSet<String>,
    members: HashSet<String>,
}

/// The keys of the functions reachable from `main`, and everything their code mentions.
pub(crate) fn reachable(functions: &[Function<'_>], modules: &[&Module]) -> (HashSet<String>, Mentions) {
    let types: Vec<&TypeDef> = modules
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(def),
            _ => None,
        })
        .collect();
    let mut reached: HashSet<String> = HashSet::new();
    let mut seen_types: HashSet<String> = HashSet::new();
    let mut mentions = Mentions::default();
    mentions.names.insert("main".to_owned());
    loop {
        let before = reached.len() + seen_types.len();
        for (method, given) in GIVEN {
            if mentions.members.contains(method) {
                mentions.names.insert(given.to_owned());
            }
        }
        for def in &types {
            if mentions.names.contains(&def.name) && seen_types.insert(def.name.clone()) {
                for default in def.fields.iter().filter_map(|field| field.default.as_ref()) {
                    expr(default, &mut mentions);
                }
            }
        }
        for function in functions {
            if reached.contains(&function.key) || !is_mentioned(&function.key, &mentions) {
                continue;
            }
            if function.prelude
                && let Some((owner, _)) = function.key.rsplit_once('.')
                && !seen_types.contains(owner)
            {
                continue;
            }
            reached.insert(function.key.clone());
            for stmt in &function.def.body {
                stmt_mentions(stmt, &mut mentions);
            }
        }
        if reached.len() + seen_types.len() == before {
            return (reached, mentions);
        }
    }
}

/// Whether code that runs mentions a function or method by this key.
pub(crate) fn is_mentioned(key: &str, mentions: &Mentions) -> bool {
    match key.rsplit_once('.') {
        Some((_, method)) => mentions.members.contains(method),
        None => mentions.names.contains(key),
    }
}

fn stmt_mentions(stmt: &Stmt, mentions: &mut Mentions) {
    match &stmt.kind {
        StmtKind::Expr(value) | StmtKind::Fail(value) | StmtKind::Yield(value) | StmtKind::Assert(value) => {
            expr(value, mentions);
        }
        StmtKind::Return(value) => {
            if let Some(value) = value {
                expr(value, mentions);
            }
        }
        StmtKind::Assign { target, op, value, .. } => {
            expr(target, mentions);
            expr(value, mentions);
            if let Some(op) = op {
                operator(*op, mentions);
            }
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                expr(cond, mentions);
                block(body, mentions);
            }
            if let Some(body) = otherwise {
                block(body, mentions);
            }
        }
        StmtKind::While { cond: value, body }
        | StmtKind::For { iter: value, body, .. }
        | StmtKind::With { value, body, .. } => {
            expr(value, mentions);
            block(body, mentions);
            // A `for` loop over a value calls its `iter`; a `with` block, its `del`.
            mentions.members.extend(["iter".to_owned(), "del".to_owned()]);
        }
        StmtKind::Def(def) => block(&def.body, mentions),
        StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

fn block(stmts: &[Stmt], mentions: &mut Mentions) {
    for stmt in stmts {
        stmt_mentions(stmt, mentions);
    }
}

/// A type's own definition of an operator, like `def +`, is a method named after it.
fn operator(op: BinOp, mentions: &mut Mentions) {
    if let Some(method) = op.method_name() {
        mentions.members.insert(method.to_owned());
    }
    // `>`, `<=` and `>=` use a type's `def <`.
    if matches!(op, BinOp::Gt | BinOp::LtEq | BinOp::GtEq) {
        mentions.members.insert("<".to_owned());
    }
}

fn expr(value: &Expr, mentions: &mut Mentions) {
    match &value.kind {
        ExprKind::Name(name) => {
            mentions.names.insert(name.clone());
        }
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::None => {}
        ExprKind::Variant(name) => {
            mentions.members.insert(name.clone());
        }
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr: inner, .. } = part {
                    expr(inner, mentions);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => all(items, mentions),
        ExprKind::Dict(pairs) => {
            for (key, item) in pairs {
                expr(key, mentions);
                expr(item, mentions);
            }
        }
        ExprKind::Comprehension { elem, value: item, iter, cond, .. } => {
            for part in [Some(elem), item.as_ref(), Some(iter), cond.as_ref()].into_iter().flatten() {
                expr(part, mentions);
            }
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Lambda { body: inner, .. } => expr(inner, mentions),
        ExprKind::Binary { op, lhs, rhs } => {
            operator(*op, mentions);
            expr(lhs, mentions);
            expr(rhs, mentions);
        }
        ExprKind::Cond { then, cond, otherwise } => all_boxed(&[then, cond, otherwise], mentions),
        ExprKind::Else { value: inner, default } => all_boxed(&[inner, default], mentions),
        ExprKind::Try { value: inner, context } => {
            expr(inner, mentions);
            if let Some(context) = context {
                expr(context, mentions);
            }
        }
        ExprKind::Call { callee, args } => {
            expr(callee, mentions);
            for arg in args {
                expr(&arg.value, mentions);
            }
        }
        ExprKind::Attr { object, name, .. } => {
            mentions.members.insert(name.clone());
            expr(object, mentions);
        }
        ExprKind::Index { object, index } => {
            // Indexing a value may use its type's `def []` or `def []=`.
            mentions.members.extend(["[]".to_owned(), "[]=".to_owned()]);
            all_boxed(&[object, index], mentions);
        }
        ExprKind::Slice { object, start, end } => {
            expr(object, mentions);
            for part in start.iter().chain(end.iter()) {
                expr(part, mentions);
            }
        }
        ExprKind::Match { subject, arms } => {
            expr(subject, mentions);
            for arm in arms {
                pattern(&arm.pattern, mentions);
                block(&arm.body, mentions);
            }
        }
    }
}

fn pattern(pattern_: &Pattern, mentions: &mut Mentions) {
    match pattern_ {
        Pattern::Literal(value) => expr(value, mentions),
        Pattern::Variant { name, args, .. } => {
            mentions.names.insert(name.clone());
            mentions.members.insert(name.clone());
            for arg in args {
                pattern(arg, mentions);
            }
        }
        Pattern::Tuple(args) => {
            for arg in args {
                pattern(arg, mentions);
            }
        }
        Pattern::Wildcard | Pattern::Bind(_) => {}
    }
}

fn all(items: &[Expr], mentions: &mut Mentions) {
    for item in items {
        expr(item, mentions);
    }
}

fn all_boxed(items: &[&Expr], mentions: &mut Mentions) {
    for item in items {
        expr(item, mentions);
    }
}
