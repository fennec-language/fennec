//! Names as what they mean where they're written. Code the checker writes into a caller, as a
//! `with` source's body or a `lock`'s block, keeps each name meaning what it meant: a name is
//! changed where it means the one in sight, and left alone where something inside names it again.

use crate::ast::{Expr, ExprKind, FStrPart, Pattern, Stmt, StmtKind};

/// Calls `change` on each use of one of `names` in `body` that means that name as it's in sight
/// where `body` starts. A use under something that names it again isn't one: a loop's names, a
/// `with`'s, a case's, an `except`'s, a cleanup's failure, an arrow function's or a nested
/// function's parameters, or a comprehension's names. Neither is one after a nested function of
/// that name, or inside that function.
pub fn uses_in_sight(body: &mut [Stmt], names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    sight_stmts(body, names, change);
}

/// Gives each name in `renamed` its new name everywhere in `body`, where it's bound and where it's
/// used, so what each use means stays the same: every name `body` binds, whatever binds it, and
/// every use.
pub fn rename_everywhere(body: &mut [Stmt], renamed: &[(String, String)]) {
    let fresh = |name: &mut String| {
        if let Some((_, to)) = renamed.iter().find(|(from, _)| from == name) {
            name.clone_from(to);
        }
    };
    binders_in_stmts(body, &mut |name| fresh(name));
    crate::visit::in_stmts(body, &mut |expr| {
        if let ExprKind::Name(name) = &mut expr.kind {
            fresh(name);
        }
    });
}

/// Every name something in `body` binds, at any depth, as `rename_everywhere` finds them.
pub fn names_bound(body: &[Stmt]) -> Vec<String> {
    let mut body = body.to_vec();
    let mut found = Vec::new();
    binders_in_stmts(&mut body, &mut |name| found.push(name.clone()));
    found
}

/// `names` without those bound again.
fn without<'b>(names: &[String], bound: impl IntoIterator<Item = &'b String>) -> Vec<String> {
    let bound: Vec<&String> = bound.into_iter().collect();
    names.iter().filter(|name| !bound.contains(name)).cloned().collect()
}

fn sight_stmts(body: &mut [Stmt], names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    let mut names = names.to_vec();
    for each in body {
        if names.is_empty() {
            return;
        }
        sight_stmt(each, &names, change);
        // A nested function's name is its own for the rest of the block.
        if let StmtKind::Def(def) = &each.kind {
            names.retain(|name| *name != def.name);
        }
    }
}

fn sight_stmt(each: &mut Stmt, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    match &mut each.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value)) => sight_expr(value, names, change),
        StmtKind::Lend { value, otherwise, .. } => {
            sight_expr(value, names, change);
            sight_stmts(otherwise, names, change);
        }
        StmtKind::Assign { target, value, .. } => {
            sight_expr(target, names, change);
            sight_expr(value, names, change);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, block) in branches {
                sight_expr(cond, names, change);
                sight_stmts(block, names, change);
            }
            if let Some(block) = otherwise {
                sight_stmts(block, names, change);
            }
        }
        StmtKind::While { cond, body } => {
            sight_expr(cond, names, change);
            sight_stmts(body, names, change);
        }
        StmtKind::For { names: bound, iter, body, .. } => {
            sight_expr(iter, names, change);
            sight_stmts(body, &without(names, bound.iter()), change);
        }
        StmtKind::With { value, names: bound, body, .. } => {
            sight_expr(value, names, change);
            sight_stmts(body, &without(names, bound.iter().map(|held| &held.name)), change);
        }
        StmtKind::OnFailure { name, cleanup, body } => {
            sight_stmts(cleanup, &without(names, [&*name]), change);
            sight_stmts(body, names, change);
        }
        StmtKind::Def(def) => {
            for param in &mut def.params {
                if let Some(default) = &mut param.default {
                    sight_expr(default, names, change);
                }
            }
            let bound = def.params.iter().map(|param| &param.name).chain([&def.name]);
            let inside = without(names, bound);
            sight_stmts(&mut def.body, &inside, change);
        }
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

fn sight_expr(value: &mut Expr, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    if names.is_empty() {
        return;
    }
    match &mut value.kind {
        ExprKind::Name(name) => {
            if names.contains(name) {
                change(value);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Duration(_)
        | ExprKind::Bytes(_)
        | ExprKind::Folder(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Variant(_) => {}
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr: inner, .. } = part {
                    sight_expr(inner, names, change);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                sight_expr(item, names, change);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, item) in pairs {
                sight_expr(key, names, change);
                sight_expr(item, names, change);
            }
        }
        ExprKind::Comprehension { .. } => sight_comprehension(value, names, change),
        ExprKind::Lambda { params, body } => sight_expr(body, &without(names, params.iter()), change),
        ExprKind::Unary { expr: inner, .. } | ExprKind::Attr { object: inner, .. } => {
            sight_expr(inner, names, change);
        }
        ExprKind::Binary { lhs: first, rhs: second, .. }
        | ExprKind::Else { value: first, default: second, .. }
        | ExprKind::Index { object: first, index: second } => {
            sight_expr(first, names, change);
            sight_expr(second, names, change);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for inner in [then, cond, otherwise] {
                sight_expr(inner, names, change);
            }
        }
        ExprKind::Try { value: inner, context } => {
            sight_expr(inner, names, change);
            if let Some(context) = context {
                sight_expr(context, names, change);
            }
        }
        ExprKind::Call { callee, args } => {
            sight_expr(callee, names, change);
            for arg in args {
                sight_expr(&mut arg.value, names, change);
            }
        }
        ExprKind::Slice { object, start, end } => {
            for inner in std::iter::once(object).chain(start.iter_mut()).chain(end.iter_mut()) {
                sight_expr(inner, names, change);
            }
        }
        ExprKind::Match { subject, arms } => {
            sight_expr(subject, names, change);
            for arm in arms {
                sight_case(arm, names, change);
            }
        }
    }
}

/// A comprehension: what it goes over is worked out before its names are bound, and the rest
/// sees them.
fn sight_comprehension(value: &mut Expr, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    let ExprKind::Comprehension { elem, value: item, names: bound, iter, cond, .. } = &mut value.kind else {
        return;
    };
    sight_expr(iter, names, change);
    let inside = without(names, bound.iter());
    for inner in std::iter::once(elem).chain(item).chain(cond) {
        sight_expr(inner, &inside, change);
    }
}

/// A case: what its pattern compares with means what it does outside, and its guard and body see
/// the names it binds.
fn sight_case(arm: &mut crate::ast::MatchArm, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    literals_in_sight(&mut arm.pattern, names, change);
    let mut bound = Vec::new();
    pattern_binders(&mut arm.pattern, &mut |name| bound.push(name.clone()));
    let inside = without(names, bound.iter());
    if let Some(guard) = &mut arm.guard {
        sight_expr(guard, &inside, change);
    }
    sight_stmts(&mut arm.body, &inside, change);
}

/// The values a pattern compares with, which mean what they mean outside the case.
fn literals_in_sight(pattern: &mut Pattern, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    match pattern {
        Pattern::Literal(value) => sight_expr(value, names, change),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                literals_in_sight(arg, names, change);
            }
        }
        Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) => {}
    }
}

/// Each name a pattern binds.
fn pattern_binders(pattern: &mut Pattern, each: &mut dyn FnMut(&mut String)) {
    match pattern {
        Pattern::Bind(name) | Pattern::RefBind(name) => each(name),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                pattern_binders(arg, each);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

/// Each name something in `body` binds, at any depth: its loops', `with`s', cleanups', nested
/// functions' names and parameters, and in its expressions, arrow functions' parameters,
/// comprehensions' names and cases' names.
fn binders_in_stmts(body: &mut [Stmt], each: &mut dyn FnMut(&mut String)) {
    stmt_binders(body, each);
    crate::visit::in_stmts(body, &mut |expr| match &mut expr.kind {
        ExprKind::Lambda { params, .. } => params.iter_mut().for_each(&mut *each),
        ExprKind::Comprehension { names, .. } => names.iter_mut().for_each(&mut *each),
        // A case's own statements are reached here, as the walk reaches each `match` inside them
        // by itself.
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                pattern_binders(&mut arm.pattern, each);
                stmt_binders(&mut arm.body, each);
            }
        }
        _ => {}
    });
}

/// The names statements bind, in them and in the blocks of statements inside them, but not in
/// their expressions.
fn stmt_binders(body: &mut [Stmt], each: &mut dyn FnMut(&mut String)) {
    for stmt in body {
        match &mut stmt.kind {
            StmtKind::For { names, body, .. } => {
                names.iter_mut().for_each(&mut *each);
                stmt_binders(body, each);
            }
            StmtKind::With { names, body, .. } => {
                for held in names {
                    each(&mut held.name);
                }
                stmt_binders(body, each);
            }
            StmtKind::OnFailure { name, cleanup, body } => {
                each(name);
                stmt_binders(cleanup, each);
                stmt_binders(body, each);
            }
            StmtKind::Def(def) => {
                each(&mut def.name);
                for param in &mut def.params {
                    each(&mut param.name);
                }
                stmt_binders(&mut def.body, each);
            }
            StmtKind::While { body, .. } => stmt_binders(body, each),
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches {
                    stmt_binders(block, each);
                }
                if let Some(block) = otherwise {
                    stmt_binders(block, each);
                }
            }
            StmtKind::Lend { otherwise, .. } => stmt_binders(otherwise, each),
            StmtKind::Expr(_)
            | StmtKind::Assign { .. }
            | StmtKind::Return(_)
            | StmtKind::Fail(_)
            | StmtKind::Yield(_)
            | StmtKind::Assert(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(source: &str) -> Vec<Stmt> {
        let module = crate::parse(source).unwrap();
        let Some(crate::Item::Function(def)) = module.items.into_iter().next() else { panic!("no function") };
        def.body
    }

    fn renamed_in_sight(source: &str, name: &str) -> String {
        let mut body = body(source);
        uses_in_sight(&mut body, &[name.to_owned()], &mut |expr| {
            expr.kind = ExprKind::Name("changed".to_owned());
        });
        format!("{body:?}")
    }

    /// A use under something that names it again is that thing's, not the one in sight: a
    /// loop's, a case's, a nested function's parameter, an arrow function's or a comprehension's.
    #[test]
    fn only_the_uses_meaning_the_name_in_sight_are_changed() {
        let source = "def f(held: int):\n    \
                      print(held)\n    \
                      for held in [1]:\n        print(held)\n    \
                      match held:\n        case held:\n            print(held)\n    \
                      def inner(held: int) -> int:\n        return held\n    \
                      work = (held) => held\n    \
                      squares = [held for held in [held]]\n";
        let written = renamed_in_sight(source, "held");
        // The first `print`, the `match`'s subject and the comprehension's list are in sight.
        assert_eq!(written.matches("Name(\"changed\")").count(), 3, "{written}");
        assert_eq!(written.matches("Name(\"held\")").count(), 5, "{written}");
    }

    /// Renaming everywhere renames every binder of the name with its uses, so each use still
    /// means what it did: a nested function's parameter too.
    #[test]
    fn renaming_everywhere_renames_every_binder_with_its_uses() {
        let source = "def f(held: int):\n    \
                      def inner(held: int) -> int:\n        return held\n    \
                      match held:\n        case _:\n            for held in [1]:\n                print(held)\n";
        let mut body = body(source);
        rename_everywhere(&mut body, &[("held".to_owned(), "kept".to_owned())]);
        let written = format!("{body:?}");
        assert!(!written.contains("\"held\""), "{written}");
    }
}
