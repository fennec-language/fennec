//! Fusing generators: a stage used once becomes part of what uses it, so a chain of them runs
//! as one plain loop, in the same order as asking for each item in turn. Nothing here depends
//! on how a program runs, and a backend that can't fuse something still gets a correct program.
//!
//! Two steps. [`stored_stages`] moves a stage kept in a name to where that name is used.
//! [`expand`] turns a `for` over a stage written in place into loops with no generator in them.

mod stored;

pub use stored::stored_stages;

use crate::Span;
use crate::ast::{Arg, BinOp, CompKind, Expr, ExprKind, FnDef, Stmt, StmtKind};
use crate::visit;

/// Copies of a yield function's body this big or bigger stay generators.
const MOST_COPIED: usize = 400;

/// The hidden name for something of a stage, unique to where it's written.
fn hidden(what: &str, span: Span) -> String {
    format!("\0{what}{}", span.start)
}

/// A span covering nothing, for what fusing writes itself: the checker has no type for it, so
/// nothing reads the type of what's written at the same place by mistake.
fn written_here(span: Span) -> Span {
    Span { start: span.start, end: span.start }
}

fn name_at(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Name(name.to_owned()), span }
}

fn binding(name: &str, value: Expr, declare_mut: bool) -> Stmt {
    let span = value.span;
    let target = name_at(name, span);
    Stmt { kind: StmtKind::Assign { target, op: None, value, declare_mut, ty: None }, span }
}

/// Whether a function is a generator: it has a `yield` of its own.
pub fn has_yield(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Yield(_) => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| has_yield(body)) || otherwise.as_deref().is_some_and(has_yield)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            has_yield(body)
        }
        StmtKind::Expr(value) | StmtKind::Assign { value, .. } => match &value.kind {
            ExprKind::Match { arms, .. } => arms.iter().any(|arm| has_yield(&arm.body)),
            _ => false,
        },
        _ => false,
    })
}

fn mentions(value: &Expr, found: &mut Vec<String>) {
    let mut value = value.clone();
    visit::in_expr(&mut value, &mut |inner| {
        if let ExprKind::Name(name) = &inner.kind {
            found.push(name.clone());
        }
    });
}

/// Every name some statements mention, with how many times.
pub fn names_in(body: &[Stmt]) -> Vec<String> {
    let mut found = Vec::new();
    let mut copy = body.to_vec();
    visit::in_stmts(&mut copy, &mut |inner| {
        if let ExprKind::Name(name) = &inner.kind {
            found.push(name.clone());
        }
    });
    found
}

fn is_generator_expression(value: &Expr) -> bool {
    matches!(&value.kind, ExprKind::Comprehension { kind: CompKind::Generator, .. })
}

/// Whether a value is a stage: a generator expression, or a call to a yield function.
fn is_stage(value: &Expr, generators: &dyn Fn(&str) -> bool) -> bool {
    match &value.kind {
        ExprKind::Call { callee, .. } => {
            matches!(&callee.kind, ExprKind::Name(name) if generators(name))
        }
        _ => is_generator_expression(value),
    }
}

/// Keeps what a stage works out when it's made in hidden names, so moving the stage doesn't
/// move that too.
fn hoist_sources(
    stage: &mut Expr,
    steady: &dyn Fn(&str) -> bool,
    generators: &dyn Fn(&str) -> bool,
    out: &mut Vec<Stmt>,
) {
    let hoist = |value: &mut Expr, out: &mut Vec<Stmt>| {
        if is_stage(value, generators) {
            hoist_sources(value, steady, generators, out);
            return;
        }
        let stays = simply_read(value, steady);
        if !stays && !is_constant(value) {
            let span = value.span;
            let name = hidden("source", span);
            let worked_out = std::mem::replace(value, name_at(&name, span));
            out.push(binding(&name, worked_out, false));
        }
    };
    match &mut stage.kind {
        ExprKind::Comprehension { iter, .. } => hoist(iter, out),
        ExprKind::Call { args, .. } => {
            for arg in args {
                hoist(&mut arg.value, out);
            }
        }
        _ => {}
    }
}

/// Whether working this out later gives the same as now, at no cost: a name that keeps its
/// value, or a `range` of those.
fn simply_read(value: &Expr, steady: &dyn Fn(&str) -> bool) -> bool {
    match &value.kind {
        ExprKind::Name(name) => steady(name),
        ExprKind::Call { callee, args } => {
            is_name(callee, "range")
                && args
                    .iter()
                    .all(|arg| !arg.is_mut && (simply_read(&arg.value, steady) || is_constant(&arg.value)))
        }
        _ => false,
    }
}

fn is_constant(value: &Expr) -> bool {
    matches!(
        value.kind,
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::None
    )
}

fn is_name(value: &Expr, name: &str) -> bool {
    matches!(&value.kind, ExprKind::Name(other) if other == name)
}

/// Plain statements for a `for` over a stage.
pub struct Expansion {
    pub stmts: Vec<Stmt>,
    /// The yield function copied in, whose types are under its own name.
    pub copied: Option<String>,
}

/// What [`expand`] needs to know about where the loop is.
pub struct Surroundings<'a> {
    /// A yield function that can be copied in.
    pub generator: &'a dyn Fn(&str) -> Option<FnDef>,
    /// Whether a name is a local here, hiding anything global of that name.
    pub is_local: &'a dyn Fn(&str) -> bool,
}

/// `for names in iter: body` with no generator, if `iter` is a stage that can be fused.
pub fn expand(names: &[String], iter: &Expr, body: &[Stmt], around: &Surroundings<'_>) -> Option<Expansion> {
    match &iter.kind {
        ExprKind::Comprehension {
            kind: CompKind::Generator, elem, names: inner, iter: source, cond, ..
        } => {
            let stmts = fused_expression(names, (elem, inner, source, cond.as_deref()), body, iter.span)?;
            Some(Expansion { stmts, copied: None })
        }
        ExprKind::Try { value, context: None } => called(names, value, body, around),
        ExprKind::Call { .. } => {
            enumerated(names, iter, body, around).or_else(|| called(names, iter, body, around))
        }
        _ => None,
    }
}

/// `for at, item in enumerate(stage)`: a loop over the stage, keeping count.
fn enumerated(names: &[String], iter: &Expr, body: &[Stmt], around: &Surroundings<'_>) -> Option<Expansion> {
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let (ExprKind::Name(called), [Arg { name: None, is_mut: false, value: stage }], [at, item]) =
        (&callee.kind, args.as_slice(), names)
    else {
        return None;
    };
    let generators = |name: &str| (around.generator)(name).is_some();
    if called != "enumerate" || (around.is_local)(called) || !is_stage(stage, &generators) {
        return None;
    }
    let span = written_here(iter.span);
    let (count, each) = (hidden("count", iter.span), hidden("each", iter.span));
    let zero = Expr { kind: ExprKind::Int(0), span };
    let one = Expr { kind: ExprKind::Int(1), span };
    let target = name_at(&count, span);
    let counted = StmtKind::Assign { target, op: Some(BinOp::Add), value: one, declare_mut: false, ty: None };
    let mut looped = vec![binding(at, name_at(&count, span), true), Stmt { kind: counted, span }];
    looped.push(binding(item, name_at(&each, span), true));
    looped.extend_from_slice(body);
    let kind = StmtKind::For { names: vec![each], is_mut: false, iter: stage.clone(), body: looped };
    Some(Expansion { stmts: vec![binding(&count, zero, true), Stmt { kind, span }], copied: None })
}

/// Makes `names` hold an item, then runs what the loop runs for it.
fn item_then_body(names: &[String], item: Expr, body: &[Stmt]) -> Vec<Stmt> {
    let span = item.span;
    let mut stmts = Vec::with_capacity(body.len() + 1);
    if let [name] = names {
        stmts.push(binding(name, item, true));
    } else {
        let items = names.iter().map(|name| name_at(name, span)).collect();
        let target = Expr { kind: ExprKind::Tuple(items), span };
        let kind = StmtKind::Assign { target, op: None, value: item, declare_mut: true, ty: None };
        stmts.push(Stmt { kind, span });
    }
    stmts.extend_from_slice(body);
    stmts
}

/// Names a copy has to change, and what to.
type Renames = Vec<(String, String)>;

type GeneratorParts<'a> = (&'a Expr, &'a [String], &'a Expr, Option<&'a Expr>);

fn fused_expression(
    names: &[String],
    (elem, inner, source, cond): GeneratorParts<'_>,
    body: &[Stmt],
    span: Span,
) -> Option<Vec<Stmt>> {
    let (mut elem, mut cond) = (elem.clone(), cond.cloned());
    if rebinds(&elem, inner) || cond.as_ref().is_some_and(|cond| rebinds(cond, inner)) {
        return None;
    }
    let fresh: Vec<String> = inner.iter().map(|name| hidden(name, span)).collect();
    let renamed: Vec<(String, String)> = inner.iter().cloned().zip(fresh.iter().cloned()).collect();
    rename_in(&mut elem, &renamed);
    if let Some(cond) = &mut cond {
        rename_in(cond, &renamed);
    }
    let run = item_then_body(names, elem, body);
    let looped = match cond {
        Some(cond) => {
            vec![Stmt { kind: StmtKind::If { branches: vec![(cond, run)], otherwise: None }, span }]
        }
        None => run,
    };
    let kind = StmtKind::For { names: fresh, is_mut: false, iter: source.clone(), body: looped };
    Some(vec![Stmt { kind, span }])
}

/// Whether something inside an expression binds one of these names again, so changing what
/// they're called would need to know about scopes.
fn rebinds(value: &Expr, names: &[String]) -> bool {
    let mut found = false;
    let mut value = value.clone();
    visit::in_expr(&mut value, &mut |inner| match &inner.kind {
        ExprKind::Comprehension { names: bound, .. } => {
            found |= bound.iter().any(|name| names.contains(name));
        }
        ExprKind::Lambda { .. } | ExprKind::Match { .. } => found = true,
        _ => {}
    });
    found
}

fn rename_in(value: &mut Expr, renamed: &[(String, String)]) {
    visit::in_expr(value, &mut |inner| rename_one(inner, renamed));
}

fn rename_one(inner: &mut Expr, renamed: &[(String, String)]) {
    if let ExprKind::Name(name) = &mut inner.kind
        && let Some((_, fresh)) = renamed.iter().find(|(from, _)| from == name)
    {
        fresh.clone_into(name);
    }
}

/// A `for` over a call to a yield function: its body, with each `yield` running the loop's.
fn called(names: &[String], call: &Expr, body: &[Stmt], around: &Surroundings<'_>) -> Option<Expansion> {
    let ExprKind::Call { callee, args } = &call.kind else { return None };
    let ExprKind::Name(key) = &callee.kind else { return None };
    if (around.is_local)(key) {
        return None;
    }
    let def = (around.generator)(key)?;
    let mut locals = Vec::new();
    if !can_copy(&def.body, &mut locals) || !def.generics.is_empty() || args.len() != def.params.len() {
        return None;
    }
    if yields(&def.body).saturating_mul(size(body).max(1)) + size(&def.body) >= MOST_COPIED {
        return None;
    }
    locals.extend(def.params.iter().map(|param| param.name.clone()));
    locals.sort();
    locals.dedup();
    // What the body means by a global has to mean the same in the loop's function.
    let free = names_in(&def.body);
    if free.iter().any(|name| !locals.contains(name) && (around.is_local)(name)) {
        return None;
    }
    let generators = |name: &str| (around.generator)(name).is_some();
    let (mut stmts, renamed) = bind_params(&def, args, call.span, &locals, &generators)?;
    let staged: Vec<String> = def
        .params
        .iter()
        .zip(args)
        .filter(|(_, arg)| is_stage(&arg.value, &generators))
        .map(|(param, _)| param.name.clone())
        .collect();
    let mut copy = def.body.clone();
    rename_stmts(&mut copy, &renamed, &staged);
    for (param, arg) in def.params.iter().zip(args) {
        if staged.contains(&param.name)
            && let Some(slot) = source_slot(&mut copy, &param.name)
        {
            let mut stage = arg.value.clone();
            // Nothing the copy does can change the loop's names before its source is read.
            hoist_sources(&mut stage, &|_| true, &generators, &mut stmts);
            *slot = stage;
        }
    }
    let leaving = exits_loop(body);
    let stop = hidden("stop", call.span);
    let span = written_here(call.span);
    let run = if leaving { one_time_body(body, &stop, span) } else { body.to_vec() };
    let stopping = leaving.then_some(stop.as_str());
    replace_yields(&mut copy, names, &run, stopping, span);
    if leaving {
        stmts.push(binding(&stop, Expr { kind: ExprKind::Bool(false), span }, true));
        copy.push(Stmt { kind: StmtKind::Break, span });
        let always = Expr { kind: ExprKind::Bool(true), span };
        stmts.push(Stmt { kind: StmtKind::While { cond: always, body: copy }, span });
    } else {
        stmts.extend(copy);
    }
    Some(Expansion { stmts, copied: Some(key.clone()) })
}

/// A loop's body that can leave early, as a loop that runs once. `continue` ends the body,
/// and `break` also says to stop, which each loop around it checks.
fn one_time_body(body: &[Stmt], stop: &str, span: Span) -> Vec<Stmt> {
    let mut once = body.to_vec();
    rewrite_exits(&mut once, stop);
    once.push(Stmt { kind: StmtKind::Break, span });
    let always = Expr { kind: ExprKind::Bool(true), span };
    vec![Stmt { kind: StmtKind::While { cond: always, body: once }, span }]
}

fn rewrite_exits(body: &mut Vec<Stmt>, stop: &str) {
    let mut at = 0;
    while at < body.len() {
        let Some(stmt) = body.get_mut(at) else { return };
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Continue => stmt.kind = StmtKind::Break,
            StmtKind::Break => {
                let target = name_at(stop, span);
                let value = Expr { kind: ExprKind::Bool(true), span };
                let kind = StmtKind::Assign { target, op: None, value, declare_mut: false, ty: None };
                body.insert(at, Stmt { kind, span });
                at += 1;
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    rewrite_exits(body, stop);
                }
                if let Some(body) = otherwise {
                    rewrite_exits(body, stop);
                }
            }
            StmtKind::With { body, .. } => rewrite_exits(body, stop),
            StmtKind::Expr(value) | StmtKind::Assign { value, .. } => {
                if let ExprKind::Match { arms, .. } = &mut value.kind {
                    for arm in arms {
                        rewrite_exits(&mut arm.body, stop);
                    }
                }
            }
            _ => {}
        }
        at += 1;
    }
}

/// Hidden names for a function's locals, and statements giving its parameters their values.
fn bind_params(
    def: &FnDef,
    args: &[Arg],
    span: Span,
    locals: &[String],
    generators: &dyn Fn(&str) -> bool,
) -> Option<(Vec<Stmt>, Renames)> {
    let renamed = locals.iter().map(|name| (name.clone(), hidden(name, span))).collect::<Vec<_>>();
    let mut stmts = Vec::new();
    for (param, arg) in def.params.iter().zip(args) {
        if param.is_mut || arg.is_mut || arg.name.is_some() {
            return None;
        }
        if is_stage(&arg.value, generators) {
            // It has to be used exactly once, as the source of a loop.
            if !used_once_as_source(&def.body, &param.name) {
                return None;
            }
            continue;
        }
        let (_, fresh) = renamed.iter().find(|(from, _)| *from == param.name)?;
        stmts.push(binding(fresh, arg.value.clone(), false));
    }
    Some((stmts, renamed))
}

fn used_once_as_source(body: &[Stmt], param: &str) -> bool {
    let used = names_in(body).iter().filter(|name| *name == param).count();
    used == 1 && source_slot(&mut body.to_vec(), param).is_some()
}

/// Where a `for` directly over this name is, outside any other loop.
fn source_slot<'a>(body: &'a mut [Stmt], param: &str) -> Option<&'a mut Expr> {
    for stmt in body {
        match &mut stmt.kind {
            StmtKind::For { iter, is_mut: false, .. } => {
                if is_name(iter, param) {
                    return Some(iter);
                }
                if let ExprKind::Call { callee, args } = &mut iter.kind
                    && is_name(callee, "enumerate")
                    && let [Arg { name: None, is_mut: false, value }] = args.as_mut_slice()
                    && is_name(value, param)
                {
                    return Some(value);
                }
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    if let Some(slot) = source_slot(body, param) {
                        return Some(slot);
                    }
                }
                if let Some(slot) = source_slot(otherwise.as_deref_mut().unwrap_or_default(), param) {
                    return Some(slot);
                }
            }
            _ => {}
        }
    }
    None
}

/// Gives a function's locals their hidden names, leaving parameters a stage takes the place of.
fn rename_stmts(body: &mut [Stmt], renamed: &[(String, String)], keep: &[String]) {
    let renamed: Vec<(String, String)> =
        renamed.iter().filter(|(from, _)| !keep.contains(from)).cloned().collect();
    visit::in_stmts(body, &mut |inner| rename_one(inner, &renamed));
    rename_for_names(body, &renamed);
}

fn rename_for_names(body: &mut [Stmt], renamed: &[(String, String)]) {
    for stmt in body {
        match &mut stmt.kind {
            StmtKind::For { names, body, .. } => {
                for name in names {
                    if let Some((_, fresh)) = renamed.iter().find(|(from, _)| from == name) {
                        fresh.clone_into(name);
                    }
                }
                rename_for_names(body, renamed);
            }
            StmtKind::While { body, .. } => rename_for_names(body, renamed),
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    rename_for_names(body, renamed);
                }
                rename_for_names(otherwise.as_deref_mut().unwrap_or_default(), renamed);
            }
            _ => {}
        }
    }
}

/// Whether a function's body is one this can copy, gathering the names it declares.
fn can_copy(body: &[Stmt], locals: &mut Vec<String>) -> bool {
    body.iter().all(|stmt| match &stmt.kind {
        StmtKind::With { .. } | StmtKind::Def(_) | StmtKind::Return(_) => false,
        StmtKind::If { branches, otherwise } => {
            branches.iter().all(|(cond, body)| plain(cond) && can_copy(body, locals))
                && can_copy(otherwise.as_deref().unwrap_or_default(), locals)
        }
        StmtKind::While { cond, body } => plain(cond) && can_copy(body, locals),
        StmtKind::For { names, is_mut, iter, body } => {
            locals.extend(names.iter().cloned());
            !is_mut && plain(iter) && can_copy(body, locals)
        }
        StmtKind::Assign { target, value, .. } => {
            target_names(target, locals);
            plain(value)
        }
        StmtKind::Expr(value) | StmtKind::Yield(value) | StmtKind::Fail(value) | StmtKind::Assert(value) => {
            plain(value)
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Pass => true,
    })
}

fn target_names(target: &Expr, locals: &mut Vec<String>) {
    match &target.kind {
        ExprKind::Name(name) => locals.push(name.clone()),
        ExprKind::Tuple(items) => {
            for item in items {
                target_names(item, locals);
            }
        }
        _ => {}
    }
}

/// Whether an expression has nothing that binds names of its own.
fn plain(value: &Expr) -> bool {
    let mut found = true;
    let mut value = value.clone();
    visit::in_expr(&mut value, &mut |inner| {
        if matches!(
            inner.kind,
            ExprKind::Lambda { .. } | ExprKind::Match { .. } | ExprKind::Comprehension { .. }
        ) {
            found = false;
        }
    });
    found
}

fn yields(body: &[Stmt]) -> usize {
    body.iter()
        .map(|stmt| match &stmt.kind {
            StmtKind::Yield(_) => 1,
            StmtKind::If { branches, otherwise } => {
                branches.iter().map(|(_, body)| yields(body)).sum::<usize>()
                    + yields(otherwise.as_deref().unwrap_or_default())
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } => yields(body),
            _ => 0,
        })
        .sum()
}

/// About how much code statements are.
fn size(body: &[Stmt]) -> usize {
    let mut count = body.len();
    let mut copy = body.to_vec();
    visit::in_stmts(&mut copy, &mut |_| count += 1);
    count
}

/// Whether a loop's body has a `break` or `continue` of its own. Copied inside the yield
/// function's loops, those would mean something else.
fn exits_loop(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Break | StmtKind::Continue => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| exits_loop(body))
                || exits_loop(otherwise.as_deref().unwrap_or_default())
        }
        StmtKind::With { body, .. } => exits_loop(body),
        StmtKind::Expr(value) | StmtKind::Assign { value, .. } => match &value.kind {
            ExprKind::Match { arms, .. } => arms.iter().any(|arm| exits_loop(&arm.body)),
            _ => false,
        },
        _ => false,
    })
}

/// Each `yield` becomes a scope with the item in the loop's names, running the loop's body.
/// When that can stop everything, whatever it's inside goes on to stop too.
fn replace_yields(body: &mut Vec<Stmt>, names: &[String], run: &[Stmt], stop: Option<&str>, span: Span) {
    let mut at = 0;
    while at < body.len() {
        let Some(stmt) = body.get_mut(at) else { return };
        let mut check = false;
        match &mut stmt.kind {
            StmtKind::Yield(value) => {
                let item = item_then_body(names, value.clone(), run);
                let always = Expr { kind: ExprKind::Bool(true), span: stmt.span };
                stmt.kind = StmtKind::If { branches: vec![(always, item)], otherwise: None };
                check = true;
            }
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    replace_yields(body, names, run, stop, span);
                }
                if let Some(body) = otherwise {
                    replace_yields(body, names, run, stop, span);
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                check = yields(body) > 0;
                replace_yields(body, names, run, stop, span);
            }
            _ => {}
        }
        if let (true, Some(stop)) = (check, stop) {
            let leave = Stmt { kind: StmtKind::Break, span };
            let kind = StmtKind::If { branches: vec![(name_at(stop, span), vec![leave])], otherwise: None };
            body.insert(at + 1, Stmt { kind, span });
            at += 1;
        }
        at += 1;
    }
}
