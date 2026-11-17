//! Names as what they mean where they're written. Code the checker writes into another place, as a
//! `with` source's body into its caller, or rewrites in place, as a `lock`'s block, keeps each name
//! meaning what it meant. A name is changed where it means the one being changed, and left alone
//! where something nearer names it again: a loop, a `with`, a case, an `except`, a cleanup, a nested
//! function or its parameters, an arrow function, a comprehension, or a declaration.

use crate::ast::{Expr, ExprKind, FStrPart, Pattern, Stmt, StmtKind};
use crate::visit::HIDDEN;

/// What binds a name, as `hide_bound` says when it asks whether to give it a hidden one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binder {
    /// A declaration: a name's first assignment, or one with `mut`, a type, `ref` or `shared`,
    /// which holds for the rest of its block.
    Declared,
    /// A loop's names.
    Loop,
    /// A `with`'s names.
    With,
    /// A case's names, an `except`'s among them.
    Case,
    /// The failure a cleanup is given.
    Cleanup,
    /// A nested function's own name, from where it's written to the end of its block.
    Function,
    /// A nested function's parameter. A call to the function naming it names its new name.
    Parameter,
    /// An arrow function's parameter.
    Arrow,
    /// A comprehension's names.
    Comprehension,
}

/// Gives each name something in `body` binds the new name `hide` gives it, if any, and each use in
/// its sight that name too, so every use means what it did. A binder `hide` gives nothing keeps its
/// name, and so do the uses in its sight. `outside` are the names in sight where `body` starts, each
/// with the name to write in its uses there, when that's another. A name with `'` in it, which only
/// the compiler writes, is never given another, and neither is `_`.
pub fn hide_bound(
    body: &mut [Stmt],
    outside: &[(String, Option<String>)],
    hide: &dyn Fn(Binder, &str) -> Option<String>,
) {
    let sight: Vec<InSight> = outside
        .iter()
        .map(|(name, written)| InSight { name: name.clone(), written: written.clone(), params: Vec::new() })
        .collect();
    hide_stmts(body, &sight, hide);
}

/// Writes each use of `name` in `body` that means the one in sight where `body` starts as `to`.
pub fn rename_in_sight(body: &mut [Stmt], name: &str, to: &str) {
    hide_bound(body, &[(name.to_owned(), Some(to.to_owned()))], &|_, _| None);
}

/// Calls `change` on each use of one of `names` in `body` that means that name as it's in sight
/// where `body` starts, as a place it's assigned to too: not one in sight of something nearer that
/// names it again.
pub fn uses_in_sight(body: &mut [Stmt], names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    let outside = marked(names);
    hide_bound(body, &outside, &|_, _| None);
    crate::visit::in_stmts(body, &mut |expr| unmarked(expr, names, change));
}

/// `uses_in_sight` in an expression.
pub fn uses_in_sight_of_expr(value: &mut Expr, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    let outside: Vec<InSight> = marked(names)
        .into_iter()
        .map(|(name, written)| InSight { name, written, params: Vec::new() })
        .collect();
    hide_expr(value, &outside, &|_, _| None);
    crate::visit::in_expr(value, &mut |expr| unmarked(expr, names, change));
}

/// Each name, written as a mark no program can write, to find its uses by once the walk is done.
fn marked(names: &[String]) -> Vec<(String, Option<String>)> {
    names.iter().map(|name| (name.clone(), Some(format!("{name}{HIDDEN}in_sight")))).collect()
}

/// A use marked by `marked`, written back as the name it was and handed to `change`.
fn unmarked(expr: &mut Expr, names: &[String], change: &mut dyn FnMut(&mut Expr)) {
    let ExprKind::Name(written) = &expr.kind else { return };
    let Some(name) = written.strip_suffix(&format!("{HIDDEN}in_sight")) else { return };
    if names.iter().any(|known| known == name) {
        expr.kind = ExprKind::Name(name.to_owned());
        change(expr);
    }
}

/// A name in sight: what its uses are written as, when that's another name, and for a nested
/// function, the new names of its parameters, which a call naming them names.
#[derive(Clone)]
struct InSight {
    name: String,
    written: Option<String>,
    params: Vec<(String, String)>,
}

fn find<'s>(sight: &'s [InSight], name: &str) -> Option<&'s InSight> {
    sight.iter().rev().find(|known| known.name == name)
}

/// A use: written as what the name in sight it means is written as.
fn use_name(name: &mut String, sight: &[InSight]) {
    if let Some(InSight { written: Some(written), .. }) = find(sight, name) {
        name.clone_from(written);
    }
}

/// A binder: the name it binds is in sight from here, given the hidden name `hide` gives it. A
/// `_` throws its value away and binds nothing to give another name.
fn bind(
    sight: &mut Vec<InSight>,
    name: &mut String,
    binder: Binder,
    hide: &dyn Fn(Binder, &str) -> Option<String>,
) {
    let written = if name.contains(HIDDEN) || name == "_" { None } else { hide(binder, name) };
    sight.push(InSight { name: name.clone(), written: written.clone(), params: Vec::new() });
    if let Some(written) = written {
        *name = written;
    }
}

fn hide_stmts(body: &mut [Stmt], outer: &[InSight], hide: &dyn Fn(Binder, &str) -> Option<String>) {
    let mut sight = outer.to_vec();
    for each in body {
        hide_stmt(each, &mut sight, hide);
    }
}

fn hide_stmt(each: &mut Stmt, sight: &mut Vec<InSight>, hide: &dyn Fn(Binder, &str) -> Option<String>) {
    match &mut each.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value)) => hide_expr(value, sight, hide),
        StmtKind::Assign { target, op, value, declare_mut, ty } => {
            hide_expr(value, sight, hide);
            let declares = *declare_mut || ty.is_some();
            hide_target(target, (declares, op.is_none()), sight, hide);
        }
        StmtKind::Lend { value, otherwise, .. } => {
            hide_expr(value, sight, hide);
            hide_stmts(otherwise, sight, hide);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, block) in branches {
                hide_expr(cond, sight, hide);
                hide_stmts(block, sight, hide);
            }
            if let Some(block) = otherwise {
                hide_stmts(block, sight, hide);
            }
        }
        StmtKind::While { cond, body } => {
            hide_expr(cond, sight, hide);
            hide_stmts(body, sight, hide);
        }
        // What's looped over, or held, is worked out before the names are bound.
        StmtKind::For { names, iter, body, .. } => {
            hide_expr(iter, sight, hide);
            let mut inside = sight.clone();
            for held in names {
                bind(&mut inside, held, Binder::Loop, hide);
            }
            hide_stmts(body, &inside, hide);
        }
        StmtKind::With { value, names, body, .. } => {
            hide_expr(value, sight, hide);
            let mut inside = sight.clone();
            for held in names {
                bind(&mut inside, &mut held.name, Binder::With, hide);
            }
            hide_stmts(body, &inside, hide);
        }
        StmtKind::OnFailure { name, cleanup, body } => {
            let mut inside = sight.clone();
            bind(&mut inside, name, Binder::Cleanup, hide);
            hide_stmts(cleanup, &inside, hide);
            hide_stmts(body, sight, hide);
        }
        StmtKind::Def(def) => hide_def(def, sight, hide),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

/// A nested function: its name is in sight from here on and inside it, with the new names of its
/// parameters, which a call naming them names, and its body sees its parameters. A parameter's
/// default is a plain value, which names nothing in sight.
fn hide_def(
    def: &mut crate::ast::FnDef,
    sight: &mut Vec<InSight>,
    hide: &dyn Fn(Binder, &str) -> Option<String>,
) {
    let params: Vec<(String, String)> = def
        .params
        .iter()
        .filter(|param| !param.name.contains(HIDDEN) && param.name != "_")
        .filter_map(|param| Some((param.name.clone(), hide(Binder::Parameter, &param.name)?)))
        .collect();
    bind(sight, &mut def.name, Binder::Function, hide);
    if let Some(own) = sight.last_mut() {
        own.params.clone_from(&params);
    }
    let mut inside = sight.clone();
    for param in &mut def.params {
        let written = params.iter().find(|(from, _)| *from == param.name).map(|(_, to)| to.clone());
        inside.push(InSight { name: param.name.clone(), written: written.clone(), params: Vec::new() });
        if let Some(written) = written {
            param.name = written;
        }
    }
    hide_stmts(&mut def.body, &inside, hide);
}

/// What an assignment gives a value to. A declaration binds its names, and so does a first
/// assignment, of a name nothing in sight has, as in the program it was checked as; another
/// assignment is a use of the name it changes.
fn hide_target(
    target: &mut Expr,
    (declares, plain): (bool, bool),
    sight: &mut Vec<InSight>,
    hide: &dyn Fn(Binder, &str) -> Option<String>,
) {
    match &mut target.kind {
        ExprKind::Name(name) if declares || (plain && find(sight, name).is_none()) => {
            bind(sight, name, Binder::Declared, hide);
        }
        ExprKind::Tuple(items) if plain => {
            for item in items {
                hide_target(item, (declares, plain), sight, hide);
            }
        }
        _ => hide_expr(target, sight, hide),
    }
}

fn hide_expr(value: &mut Expr, sight: &[InSight], hide: &dyn Fn(Binder, &str) -> Option<String>) {
    match &mut value.kind {
        ExprKind::Name(name) => use_name(name, sight),
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
                    hide_expr(inner, sight, hide);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                hide_expr(item, sight, hide);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, item) in pairs {
                hide_expr(key, sight, hide);
                hide_expr(item, sight, hide);
            }
        }
        ExprKind::Comprehension { .. } => hide_comprehension(value, sight, hide),
        ExprKind::Lambda { params, body } => {
            let mut inside = sight.to_vec();
            for param in params {
                bind(&mut inside, param, Binder::Arrow, hide);
            }
            hide_expr(body, &inside, hide);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Attr { object: inner, .. } => {
            hide_expr(inner, sight, hide);
        }
        ExprKind::Binary { lhs: first, rhs: second, .. }
        | ExprKind::Else { value: first, default: second, .. }
        | ExprKind::Index { object: first, index: second } => {
            hide_expr(first, sight, hide);
            hide_expr(second, sight, hide);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for inner in [then, cond, otherwise] {
                hide_expr(inner, sight, hide);
            }
        }
        ExprKind::Try { value: inner, context } => {
            hide_expr(inner, sight, hide);
            if let Some(context) = context {
                hide_expr(context, sight, hide);
            }
        }
        ExprKind::Call { .. } => hide_call(value, sight, hide),
        ExprKind::Slice { object, start, end } => {
            for inner in std::iter::once(object).chain(start.iter_mut()).chain(end.iter_mut()) {
                hide_expr(inner, sight, hide);
            }
        }
        ExprKind::Match { subject, arms } => {
            hide_expr(subject, sight, hide);
            for arm in arms {
                hide_case(arm, sight, hide);
            }
        }
    }
}

/// A call: one of a nested function whose parameters have new names names them by those.
fn hide_call(value: &mut Expr, sight: &[InSight], hide: &dyn Fn(Binder, &str) -> Option<String>) {
    let ExprKind::Call { callee, args } = &mut value.kind else { return };
    let params = match &callee.kind {
        ExprKind::Name(name) => find(sight, name).map(|known| known.params.clone()).unwrap_or_default(),
        _ => Vec::new(),
    };
    hide_expr(callee, sight, hide);
    for arg in args {
        hide_expr(&mut arg.value, sight, hide);
        if let Some(named) = &mut arg.name
            && let Some((_, written)) = params.iter().find(|(from, _)| from == named)
        {
            named.clone_from(written);
        }
    }
}

/// A comprehension: what it goes over is worked out before its names are bound.
fn hide_comprehension(value: &mut Expr, sight: &[InSight], hide: &dyn Fn(Binder, &str) -> Option<String>) {
    let ExprKind::Comprehension { elem, value: item, names, iter, cond, .. } = &mut value.kind else {
        return;
    };
    hide_expr(iter, sight, hide);
    let mut inside = sight.to_vec();
    for name in names {
        bind(&mut inside, name, Binder::Comprehension, hide);
    }
    for inner in std::iter::once(elem).chain(item).chain(cond) {
        hide_expr(inner, &inside, hide);
    }
}

/// A case: what its pattern compares with means what it does outside, and its guard and body see
/// the names it binds.
fn hide_case(
    arm: &mut crate::ast::MatchArm,
    sight: &[InSight],
    hide: &dyn Fn(Binder, &str) -> Option<String>,
) {
    let mut inside = sight.to_vec();
    case_pattern(&mut arm.pattern, sight, &mut inside, hide);
    if let Some(guard) = &mut arm.guard {
        hide_expr(guard, &inside, hide);
    }
    hide_stmts(&mut arm.body, &inside, hide);
}

/// A pattern's literals, seen from outside the case, and the names it binds, in sight inside it.
fn case_pattern(
    pattern: &mut Pattern,
    outside: &[InSight],
    inside: &mut Vec<InSight>,
    hide: &dyn Fn(Binder, &str) -> Option<String>,
) {
    match pattern {
        Pattern::Literal(value) => hide_expr(value, outside, hide),
        Pattern::Bind(name) | Pattern::RefBind(name) | Pattern::MutBind(name) => {
            bind(inside, name, Binder::Case, hide);
        }
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                case_pattern(arg, outside, inside, hide);
            }
        }
        Pattern::Wildcard => {}
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

    /// A use under something that names it again is that thing's, not the one in sight: a
    /// loop's, a case's, a nested function's parameter, an arrow function's, a comprehension's, or
    /// a declaration's for the rest of its block. Assigning to the one in sight is a use of it.
    #[test]
    fn only_the_uses_meaning_the_name_in_sight_are_changed() {
        let source = "def f(held: int):\n    \
                      print(held)\n    \
                      for held in [1]:\n        print(held)\n    \
                      match held:\n        case held:\n            print(held)\n    \
                      def inner(held: int) -> int:\n        return held\n    \
                      work = (held) => held\n    \
                      squares = [held for held in [held]]\n    \
                      held = 3\n    \
                      if True:\n        ref held = squares[0]\n        print(held)\n    \
                      print(held)\n";
        let mut body = body(source);
        uses_in_sight(&mut body, &["held".to_owned()], &mut |expr| {
            expr.kind = ExprKind::Name("changed".to_owned());
        });
        let written = format!("{body:?}");
        // The first `print`, the `match`'s subject, the comprehension's list, the assignment and
        // the last `print` mean the one in sight.
        assert_eq!(written.matches("Name(\"changed\")").count(), 5, "{written}");
        assert_eq!(written.matches("Name(\"held\")").count(), 7, "{written}");
    }

    /// Hiding gives each binder it picks a new name with the uses in its sight, and leaves a use
    /// that means something else alone: a function the body calls with the name a comprehension
    /// binds. A nested function's parameter given a new name is named by it where it's called.
    #[test]
    fn hiding_renames_each_binder_with_the_uses_in_its_sight() {
        let source = "def f():\n    \
                      sizes = [len(total) for total in [[1]]]\n    \
                      def scaled(value: int, by=2) -> int:\n        return value * by\n    \
                      print(total(sizes), scaled(1, by=3))\n";
        let mut body = body(source);
        hide_bound(&mut body, &[], &|binder, name| {
            (binder != Binder::Comprehension).then(|| format!("{name}_hidden"))
        });
        let written = format!("{body:?}");
        assert!(written.contains("Name(\"total\")"), "{written}");
        assert!(written.contains("Name(\"sizes_hidden\")"), "{written}");
        assert!(written.contains("name: Some(\"by_hidden\")"), "{written}");
        assert!(!written.contains("\"by\""), "{written}");
    }
}
