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
    hide_bound_and_free(body, outside, hide, &|_, _| None);
}

/// `hide_bound`, and each use of a name nothing in sight binds, one from outside `body` and
/// `outside` altogether, written as `free` gives it, if anything. `free` is told whether another
/// name is in sight there.
pub fn hide_bound_and_free(
    body: &mut [Stmt],
    outside: &[(String, Option<String>)],
    hide: &dyn Fn(Binder, &str) -> Option<String>,
    free: &dyn Fn(&str, &dyn Fn(&str) -> bool) -> Option<String>,
) {
    let sight: Vec<InSight> = outside
        .iter()
        .map(|(name, written)| InSight { name: name.clone(), written: written.clone(), params: Vec::new() })
        .collect();
    hide_stmts(body, &sight, &How { hide, free });
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
    hide_expr(value, &outside, &How { hide: &|_, _| None, free: &|_, _| None });
    crate::visit::in_expr(value, &mut |expr| unmarked(expr, names, change));
}

/// Every name `body` binds anywhere, nested functions and arrow functions included, with `params`.
#[must_use]
pub fn bound_in(body: &[Stmt], params: &[String]) -> std::collections::BTreeSet<String> {
    let bound = std::cell::RefCell::new(params.iter().cloned().collect::<std::collections::BTreeSet<_>>());
    hide_bound(&mut body.to_vec(), &[], &|_, name| {
        bound.borrow_mut().insert(name.to_owned());
        None
    });
    bound.into_inner()
}

/// `name` written as meaning what it does where a `with` source is written, a top-level function
/// or constant, though something in sight where the source is written in names it too. Only the
/// compiler writes it: the checker reads it as that top-level name, and IR lowering gives what's in
/// sight there other names first (`outer_apart`).
#[must_use]
pub fn as_outer(name: &str) -> String {
    format!("{name}{HIDDEN}outer")
}

/// The top-level name a name written by `as_outer` means, if it's one.
#[must_use]
pub fn outer(name: &str) -> Option<&str> {
    name.strip_suffix(format!("{HIDDEN}outer").as_str())
}

/// What a function's own name is written as where `outer_apart` gives it another.
#[must_use]
pub fn own(name: &str) -> String {
    format!("{name}{HIDDEN}own")
}

/// The name a name `own` gave another was checked with.
#[must_use]
pub fn checked_as(name: &str) -> &str {
    name.strip_suffix(format!("{HIDDEN}own").as_str()).unwrap_or(name)
}

/// Gives every name in `body` that a use written by `as_outer` names the new name `own` gives it,
/// with the uses in its sight, so nothing there names what the marked use means, and then writes
/// the marked uses as the names they mean. `params` are the names in sight where `body` starts.
/// Says which names were given new ones. A nested function's parameters keep theirs, and the marked
/// uses inside one are left for its own lowering, which gives its parameters names apart as this
/// does `params`.
///
/// # Errors
///
/// The name a marked use means, where something named that is still in sight of it, which nothing
/// written reaches: everything in sight is given a name apart.
pub fn outer_apart(body: &mut [Stmt], params: &[String]) -> Result<Vec<String>, String> {
    let mut apart: Vec<String> = Vec::new();
    crate::visit::in_stmts(body, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind
            && let Some(meant) = outer(name)
            && !apart.iter().any(|known| known == meant)
        {
            apart.push(meant.to_owned());
        }
    });
    if apart.is_empty() {
        return Ok(apart);
    }
    let named = |name: &str| apart.iter().any(|known| known == name);
    let outside: Vec<(String, Option<String>)> =
        params.iter().map(|param| (param.clone(), named(param).then(|| own(param)))).collect();
    // A nested function's parameters keep theirs, which calls name: its own lowering gives them
    // names apart, with the marked uses inside it.
    hide_bound(body, &outside, &|binder, name| {
        (named(name) && binder != Binder::Parameter).then(|| own(name))
    });
    let captured = std::cell::RefCell::new(None);
    hide_bound_and_free(body, &[], &|_, _| None, &|name, in_sight| {
        let meant = outer(name)?;
        // A nested function's own are its own to give names apart, with its parameters.
        if in_sight(&function_edge()) {
            return None;
        }
        if in_sight(meant) {
            captured.borrow_mut().get_or_insert_with(|| meant.to_owned());
            return None;
        }
        Some(meant.to_owned())
    });
    match captured.into_inner() {
        Some(meant) => Err(meant),
        None => Ok(apart),
    }
}

/// What's put in sight where a nested function's body starts, a name no program can write, so a
/// walk can tell it's inside one.
fn function_edge() -> String {
    format!("{HIDDEN}def")
}

/// How a walk changes names: the new name `hide` gives a binder, if any, and the name `free` writes
/// a use nothing in sight binds as, if another.
struct How<'h> {
    hide: &'h dyn Fn(Binder, &str) -> Option<String>,
    free: &'h dyn Fn(&str, &dyn Fn(&str) -> bool) -> Option<String>,
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

/// A use: written as what the name in sight it means is written as, and one nothing in sight binds
/// as `free` writes it.
fn use_name(name: &mut String, sight: &[InSight], how: &How<'_>) {
    match find(sight, name) {
        Some(InSight { written: Some(written), .. }) => name.clone_from(written),
        Some(_) => {}
        None => {
            if let Some(written) = (how.free)(name, &|other| find(sight, other).is_some()) {
                *name = written;
            }
        }
    }
}

/// A binder: the name it binds is in sight from here, given the hidden name `hide` gives it. A
/// `_` throws its value away and binds nothing to give another name.
fn bind(sight: &mut Vec<InSight>, name: &mut String, binder: Binder, how: &How<'_>) {
    let written = if name.contains(HIDDEN) || name == "_" { None } else { (how.hide)(binder, name) };
    sight.push(InSight { name: name.clone(), written: written.clone(), params: Vec::new() });
    if let Some(written) = written {
        *name = written;
    }
}

fn hide_stmts(body: &mut [Stmt], outer: &[InSight], how: &How<'_>) {
    let mut sight = outer.to_vec();
    for each in body {
        hide_stmt(each, &mut sight, how);
    }
}

fn hide_stmt(each: &mut Stmt, sight: &mut Vec<InSight>, how: &How<'_>) {
    match &mut each.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value)) => hide_expr(value, sight, how),
        StmtKind::Assign { target, op, value, declare_mut, ty } => {
            hide_expr(value, sight, how);
            let declares = *declare_mut || ty.is_some();
            hide_target(target, (declares, op.is_none()), sight, how);
        }
        StmtKind::Lend { value, otherwise, .. } => {
            hide_expr(value, sight, how);
            hide_stmts(otherwise, sight, how);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, block) in branches {
                hide_expr(cond, sight, how);
                hide_stmts(block, sight, how);
            }
            if let Some(block) = otherwise {
                hide_stmts(block, sight, how);
            }
        }
        StmtKind::While { cond, body } => {
            hide_expr(cond, sight, how);
            hide_stmts(body, sight, how);
        }
        // What's looped over, or held, is worked out before the names are bound.
        StmtKind::For { names, iter, body, .. } => {
            hide_expr(iter, sight, how);
            let mut inside = sight.clone();
            for held in names {
                bind(&mut inside, held, Binder::Loop, how);
            }
            hide_stmts(body, &inside, how);
        }
        StmtKind::With { value, names, body, .. } => {
            hide_expr(value, sight, how);
            let mut inside = sight.clone();
            for held in names {
                bind(&mut inside, &mut held.name, Binder::With, how);
            }
            hide_stmts(body, &inside, how);
        }
        StmtKind::OnFailure { name, cleanup, body } => {
            let mut inside = sight.clone();
            bind(&mut inside, name, Binder::Cleanup, how);
            hide_stmts(cleanup, &inside, how);
            hide_stmts(body, sight, how);
        }
        StmtKind::Def(def) => hide_def(def, sight, how),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

/// A nested function: its name is in sight from here on and inside it, with the new names of its
/// parameters, which a call naming them names, and its body sees its parameters. A parameter's
/// default is a plain value, which names nothing in sight.
fn hide_def(def: &mut crate::ast::FnDef, sight: &mut Vec<InSight>, how: &How<'_>) {
    let params: Vec<(String, String)> = def
        .params
        .iter()
        .filter(|param| !param.name.contains(HIDDEN) && param.name != "_")
        .filter_map(|param| Some((param.name.clone(), (how.hide)(Binder::Parameter, &param.name)?)))
        .collect();
    bind(sight, &mut def.name, Binder::Function, how);
    if let Some(own) = sight.last_mut() {
        own.params.clone_from(&params);
    }
    let mut inside = sight.clone();
    inside.push(InSight { name: function_edge(), written: None, params: Vec::new() });
    for param in &mut def.params {
        let written = params.iter().find(|(from, _)| *from == param.name).map(|(_, to)| to.clone());
        inside.push(InSight { name: param.name.clone(), written: written.clone(), params: Vec::new() });
        if let Some(written) = written {
            param.name = written;
        }
    }
    hide_stmts(&mut def.body, &inside, how);
}

/// What an assignment gives a value to. A declaration binds its names, and so does a first
/// assignment, of a name nothing in sight has, as in the program it was checked as; another
/// assignment is a use of the name it changes.
fn hide_target(target: &mut Expr, (declares, plain): (bool, bool), sight: &mut Vec<InSight>, how: &How<'_>) {
    match &mut target.kind {
        ExprKind::Name(name) if declares || (plain && find(sight, name).is_none()) => {
            bind(sight, name, Binder::Declared, how);
        }
        ExprKind::Tuple(items) if plain => {
            for item in items {
                hide_target(item, (declares, plain), sight, how);
            }
        }
        _ => hide_expr(target, sight, how),
    }
}

fn hide_expr(value: &mut Expr, sight: &[InSight], how: &How<'_>) {
    match &mut value.kind {
        ExprKind::Name(name) => use_name(name, sight, how),
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
                    hide_expr(inner, sight, how);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                hide_expr(item, sight, how);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, item) in pairs {
                hide_expr(key, sight, how);
                hide_expr(item, sight, how);
            }
        }
        ExprKind::Comprehension { .. } => hide_comprehension(value, sight, how),
        ExprKind::Lambda { params, body } => {
            let mut inside = sight.to_vec();
            for param in params {
                bind(&mut inside, param, Binder::Arrow, how);
            }
            hide_expr(body, &inside, how);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Attr { object: inner, .. } => {
            hide_expr(inner, sight, how);
        }
        ExprKind::Binary { lhs: first, rhs: second, .. }
        | ExprKind::Else { value: first, default: second, .. }
        | ExprKind::Index { object: first, index: second } => {
            hide_expr(first, sight, how);
            hide_expr(second, sight, how);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for inner in [then, cond, otherwise] {
                hide_expr(inner, sight, how);
            }
        }
        ExprKind::Try { value: inner, context } => {
            hide_expr(inner, sight, how);
            if let Some(context) = context {
                hide_expr(context, sight, how);
            }
        }
        ExprKind::Call { .. } => hide_call(value, sight, how),
        ExprKind::Slice { object, start, end } => {
            for inner in std::iter::once(object).chain(start.iter_mut()).chain(end.iter_mut()) {
                hide_expr(inner, sight, how);
            }
        }
        ExprKind::Match { subject, arms } => {
            hide_expr(subject, sight, how);
            for arm in arms {
                hide_case(arm, sight, how);
            }
        }
    }
}

/// A call: one of a nested function whose parameters have new names names them by those.
fn hide_call(value: &mut Expr, sight: &[InSight], how: &How<'_>) {
    let ExprKind::Call { callee, args } = &mut value.kind else { return };
    let params = match &callee.kind {
        ExprKind::Name(name) => find(sight, name).map(|known| known.params.clone()).unwrap_or_default(),
        _ => Vec::new(),
    };
    hide_expr(callee, sight, how);
    for arg in args {
        hide_expr(&mut arg.value, sight, how);
        if let Some(named) = &mut arg.name
            && let Some((_, written)) = params.iter().find(|(from, _)| from == named)
        {
            named.clone_from(written);
        }
    }
}

/// A comprehension: what it goes over is worked out before its names are bound.
fn hide_comprehension(value: &mut Expr, sight: &[InSight], how: &How<'_>) {
    let ExprKind::Comprehension { elem, value: item, names, iter, cond, .. } = &mut value.kind else {
        return;
    };
    hide_expr(iter, sight, how);
    let mut inside = sight.to_vec();
    for name in names {
        bind(&mut inside, name, Binder::Comprehension, how);
    }
    for inner in std::iter::once(elem).chain(item).chain(cond) {
        hide_expr(inner, &inside, how);
    }
}

/// A case: what its pattern compares with means what it does outside, and its guard and body see
/// the names it binds.
fn hide_case(arm: &mut crate::ast::MatchArm, sight: &[InSight], how: &How<'_>) {
    let mut inside = sight.to_vec();
    case_pattern(&mut arm.pattern, sight, &mut inside, how);
    if let Some(guard) = &mut arm.guard {
        hide_expr(guard, &inside, how);
    }
    hide_stmts(&mut arm.body, &inside, how);
}

/// A pattern's literals, seen from outside the case, and the names it binds, in sight inside it.
fn case_pattern(pattern: &mut Pattern, outside: &[InSight], inside: &mut Vec<InSight>, how: &How<'_>) {
    match pattern {
        Pattern::Literal(value) => hide_expr(value, outside, how),
        Pattern::Bind(name) | Pattern::RefBind(name) | Pattern::MutBind(name) => {
            bind(inside, name, Binder::Case, how);
        }
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                case_pattern(arg, outside, inside, how);
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

    /// A use marked as a `with` source's own is written as the name it means, and what's in sight of
    /// it named the same is given a name apart: a parameter, a loop's name. A nested function keeps
    /// its parameter's name and its own marked uses, which its own lowering gives names apart.
    #[test]
    fn marked_uses_mean_their_own_and_what_s_in_sight_is_named_apart() {
        let source = "def f(total: int):\n    \
                      print(total, outer_total)\n    \
                      for total in [1]:\n        print(total, outer_total)\n    \
                      def inner(total: int) -> int:\n        return total + outer_total\n";
        let mut body = body(source);
        crate::visit::in_stmts(&mut body, &mut |expr| {
            if matches!(&expr.kind, ExprKind::Name(name) if name == "outer_total") {
                expr.kind = ExprKind::Name(as_outer("total"));
            }
        });
        assert_eq!(outer_apart(&mut body, &["total".to_owned()]), Ok(vec!["total".to_owned()]));
        let written = format!("{body:?}");
        // The two marked uses outside `inner`, and `inner`'s parameter's use.
        assert_eq!(written.matches("Name(\"total\")").count(), 3, "{written}");
        assert_eq!(written.matches("Name(\"total'own\")").count(), 2, "{written}");
        assert_eq!(written.matches("Name(\"total'outer\")").count(), 1, "{written}");
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
