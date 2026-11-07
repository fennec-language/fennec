//! A function's or a test's own names, written as where they're bound rather than as they're
//! spelled, so renaming a local changes no hash and no test runs again for it.
//!
//! A name counts as the function's own when something in it binds the name: an assignment, a loop,
//! a `with`, a `match` case, a comprehension, an arrow function or a nested `def`. Parameters keep
//! their names, since a call can name them. So do names the program declares at the top, and
//! any name that's called, since out of a block's reach the same name can mean something else,
//! like a function of the standard library. Leaving a name as it's spelled can only cost a run,
//! never skip one.

use std::collections::HashSet;

use fennec_syntax::{Expr, ExprKind, FnDef, Item, Module, Pattern, Stmt, StmtKind, TestDef};

/// Every name a module declares at the top level, and the names its imports bring in.
pub(crate) fn top_names(module: &Module) -> HashSet<String> {
    let mut names = HashSet::new();
    for item in &module.items {
        match item {
            Item::Function(def) => {
                names.insert(def.name.clone());
            }
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                names.insert(def.name.clone());
                names.extend(def.cases.iter().map(|case| case.name.clone()));
            }
            Item::Interface(interface) => {
                names.insert(interface.name.clone());
            }
            Item::TypeAlias { name, .. } | Item::Const { name, .. } => {
                names.insert(name.clone());
            }
            Item::Import(import) => {
                names.extend(import.bound().map(|(_, bound)| bound.to_owned()));
                names.extend(import.module.iter().chain(&import.alias).cloned());
            }
            Item::Extend(_) | Item::Test(_) | Item::Stmt(_) => {}
        }
    }
    names
}

/// `def` with its own names written as where they're bound. `top` is every name the program
/// declares at the top level.
pub(crate) fn function(def: &FnDef, top: &HashSet<String>) -> FnDef {
    let mut def = def.clone();
    let kept: HashSet<&str> = def.params.iter().map(|param| param.name.as_str()).collect();
    let renamed = own_names(&mut def.body, &kept, top);
    rename(&mut def.body, &renamed);
    def
}

/// `test` with its own names written as where they're bound.
pub(crate) fn test(test: &TestDef, top: &HashSet<String>) -> TestDef {
    let mut test = test.clone();
    let kept: HashSet<&str> =
        test.capabilities.iter().chain(&test.inputs).map(|param| param.name.as_str()).collect();
    let renamed = own_names(&mut test.body, &kept, top);
    rename(&mut test.body, &renamed);
    test
}

/// The names `body` binds that can be written another way, each with what it's written as: the
/// order it was first bound in, which no program can write.
fn own_names(body: &mut [Stmt], kept: &HashSet<&str>, top: &HashSet<String>) -> Vec<(String, String)> {
    let mut bound = Vec::new();
    statements_binding(body, &mut |name| bound.push(name.clone()));
    let mut called = HashSet::new();
    fennec_syntax::visit::in_stmts(body, &mut |expr| {
        if let ExprKind::Call { callee, .. } = &expr.kind
            && let ExprKind::Name(name) = &callee.kind
        {
            called.insert(name.clone());
        }
        for name in names_bound_in(expr) {
            bound.push(name.clone());
        }
        if let ExprKind::Match { arms, .. } = &mut expr.kind {
            for arm in arms {
                statements_binding(&mut arm.body, &mut |name| bound.push(name.clone()));
            }
        }
    });
    let mut renamed: Vec<(String, String)> = Vec::new();
    for name in bound {
        let own = name != "_"
            && name != "self"
            && !kept.contains(name.as_str())
            && !top.contains(&name)
            && !called.contains(&name);
        if own && renamed.iter().all(|(seen, _)| *seen != name) {
            let written = format!("'{}", renamed.len());
            renamed.push((name, written));
        }
    }
    renamed
}

/// Writes each name in `renamed` as what it's paired with, wherever `body` binds or uses it.
fn rename(body: &mut [Stmt], renamed: &[(String, String)]) {
    if renamed.is_empty() {
        return;
    }
    let written = |name: &mut String| {
        if let Some((_, to)) = renamed.iter().find(|(from, _)| from == name) {
            name.clone_from(to);
        }
    };
    statements_binding(body, &mut |name| written(name));
    fennec_syntax::visit::in_stmts(body, &mut |expr| match &mut expr.kind {
        ExprKind::Name(name) => written(name),
        ExprKind::Comprehension { names, .. } => names.iter_mut().for_each(written),
        ExprKind::Lambda { params, .. } => params.iter_mut().for_each(written),
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                pattern_binding(&mut arm.pattern, &mut |name| written(name));
                statements_binding(&mut arm.body, &mut |name| written(name));
            }
        }
        _ => {}
    });
}

/// The names one expression binds itself, not counting what's inside it.
fn names_bound_in(expr: &mut Expr) -> Vec<&String> {
    match &mut expr.kind {
        ExprKind::Comprehension { names, .. } => names.iter().collect(),
        ExprKind::Lambda { params, .. } => params.iter().collect(),
        ExprKind::Match { arms, .. } => {
            let mut found = Vec::new();
            for arm in arms.iter() {
                pattern_names(&arm.pattern, &mut found);
            }
            found
        }
        _ => Vec::new(),
    }
}

fn pattern_names<'p>(pattern: &'p Pattern, found: &mut Vec<&'p String>) {
    match pattern {
        Pattern::Bind(name) => found.push(name),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                pattern_names(arg, found);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

fn pattern_binding(pattern: &mut Pattern, each: &mut dyn FnMut(&mut String)) {
    match pattern {
        Pattern::Bind(name) => each(name),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                pattern_binding(arg, each);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

/// Each name the statements bind, at any depth, leaving out what their expressions bind. A
/// nested `def`'s own name and parameters count, as only the function around it sees them.
fn statements_binding(body: &mut [Stmt], each: &mut dyn FnMut(&mut String)) {
    for stmt in body {
        match &mut stmt.kind {
            StmtKind::Assign { target, .. } => target_binding(target, each),
            StmtKind::For { names, body, .. } => {
                names.iter_mut().for_each(&mut *each);
                statements_binding(body, each);
            }
            StmtKind::With { names, body, .. } => {
                for held in names {
                    each(&mut held.name);
                }
                statements_binding(body, each);
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    statements_binding(body, each);
                }
                statements_binding(otherwise.as_deref_mut().unwrap_or_default(), each);
            }
            StmtKind::While { body, .. } => statements_binding(body, each),
            StmtKind::Def(def) => {
                each(&mut def.name);
                def.params.iter_mut().for_each(|param| each(&mut param.name));
                statements_binding(&mut def.body, each);
            }
            StmtKind::Expr(_)
            | StmtKind::Return(_)
            | StmtKind::Fail(_)
            | StmtKind::Lend { .. }
            | StmtKind::Yield(_)
            | StmtKind::Assert(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }
}

/// The names an assignment's target binds: `x`, or each of `a, b`.
fn target_binding(target: &mut Expr, each: &mut dyn FnMut(&mut String)) {
    match &mut target.kind {
        ExprKind::Name(name) => each(name),
        ExprKind::Tuple(items) => items.iter_mut().for_each(|item| target_binding(item, each)),
        _ => {}
    }
}
