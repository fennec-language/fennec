//! A collection can't change while a loop goes over it. Inside `for x in xs:`, nothing may
//! append to `xs`, remove from it, set one of its items, write through it with another `for mut`,
//! or pass it as `mut`, and neither may a function the body calls: each function's summary says
//! which places under its `mut` parameters it changes. The loop goes over `copy(xs)` instead, or
//! collects the changes and makes them after.
//!
//! A `mut` argument has its place to itself the same way: nothing else in the call can reach it.

use std::collections::{HashMap, HashSet};

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use crate::Body;
use crate::builtins;
use crate::checker::Checker;

/// A step along a place: a field, or an index, which could be any item.
type Step = Option<String>;

/// A place: a name and the steps after it, like `xs[i].hp`.
#[derive(Clone, Debug, PartialEq)]
struct Place {
    root: String,
    steps: Vec<Step>,
}

impl Place {
    /// Whether changing one can change the other: one is inside the other, where an index
    /// matches any item and different fields are different places.
    fn overlaps(&self, other: &Place) -> bool {
        self.root == other.root
            && self.steps.iter().zip(&other.steps).all(|(a, b)| a.is_none() || b.is_none() || a == b)
    }

    /// This place, with `inner` read as a place under it.
    fn under(&self, inner: &[Step]) -> Place {
        Place { root: self.root.clone(), steps: self.steps.iter().chain(inner).cloned().collect() }
    }
}

/// The place an expression names, if it names one.
fn place(expr: &Expr) -> Option<Place> {
    match &expr.kind {
        ExprKind::Name(name) => Some(Place { root: name.clone(), steps: Vec::new() }),
        ExprKind::Attr { object, name, optional: false } => {
            let mut found = place(object)?;
            found.steps.push(Some(name.clone()));
            Some(found)
        }
        ExprKind::Index { object, .. } => {
            let mut found = place(object)?;
            found.steps.push(None);
            Some(found)
        }
        _ => None,
    }
}

/// The collections a loop goes over: `xs` in `for x in xs`, also through `enumerate(xs)`,
/// `reversed(xs)`, `zip(xs, ys)` and a dict's `keys()`, `values()` and `items()`. A slice is
/// a copy, so a loop over one goes over nothing that can change.
fn looped_over(iter: &Expr) -> Vec<(Place, &Expr)> {
    match &iter.kind {
        ExprKind::Call { callee, args } => match &callee.kind {
            ExprKind::Name(name) if matches!(name.as_str(), "enumerate" | "reversed" | "zip") => {
                args.iter().filter(|arg| arg.name.is_none()).flat_map(|arg| looped_over(&arg.value)).collect()
            }
            ExprKind::Attr { object, name, .. }
                if args.is_empty() && matches!(name.as_str(), "keys" | "values" | "items") =>
            {
                looped_over(object)
            }
            _ => Vec::new(),
        },
        _ => place(iter).map(|found| vec![(found, iter)]).unwrap_or_default(),
    }
}

/// What a function changes under each of its `mut` parameters: the places relative to the
/// parameter, where an empty one is the whole parameter.
type Summary = HashMap<String, Vec<Vec<Step>>>;

struct Loops<'c> {
    checker: &'c Checker,
    summaries: HashMap<String, Summary>,
    /// The functions whose summaries are being worked out, for a call back into one.
    working: HashSet<String>,
    /// The functions whose summaries are settled.
    done: HashSet<String>,
    problems: Vec<Diagnostic>,
}

/// Checks a function's loops, reporting each change to what a loop goes over.
pub(crate) fn check_function(checker: &Checker, key: &str, def: &FnDef) -> Vec<Diagnostic> {
    let mut loops = Loops {
        checker,
        summaries: HashMap::new(),
        working: HashSet::new(),
        done: HashSet::new(),
        problems: Vec::new(),
    };
    loops.loops_in(&def.body, checker.typed.get(key));
    loops.problems
}

impl Loops<'_> {
    /// Checks every loop in a block, and the loops inside them.
    fn loops_in(&mut self, stmts: &[Stmt], body: Option<&Body>) {
        for stmt in stmts {
            if let StmtKind::For { iter, is_mut, body: block, .. } = &stmt.kind {
                let changes = self.changes_in(block, body);
                for (looped, written) in looped_over(iter) {
                    let shown = fennec_syntax::dotted(written).unwrap_or_else(|| "it".to_owned());
                    // A `for mut` over a copy would write to the copy.
                    let help = if *is_mut {
                        "collect the changes and make them after the loop".to_owned()
                    } else {
                        format!(
                            "loop over `copy({shown})`, or collect the changes and make them after the loop"
                        )
                    };
                    for (_, span) in changes.iter().filter(|(changed, _)| changed.overlaps(&looped)) {
                        let message = format!("`{shown}` changes while a loop goes over it");
                        self.problems.push(
                            Diagnostic::new(message, *span)
                                .with_help(help.clone())
                                .also_at_the(fennec_syntax::Also::Loop, written.span),
                        );
                    }
                }
            }
            for block in blocks(stmt) {
                self.loops_in(block, body);
            }
        }
    }

    /// Every place a block changes, with where, not counting a name given a new value.
    fn changes_in(&mut self, stmts: &[Stmt], body: Option<&Body>) -> Vec<(Place, Span)> {
        let mut found = Vec::new();
        for stmt in stmts {
            if let StmtKind::Assign { target, .. } = &stmt.kind {
                let targets = match &target.kind {
                    ExprKind::Tuple(items) => items.iter().collect(),
                    _ => vec![target],
                };
                for target in targets {
                    if let Some(changed) = place(target).filter(|_| !matches!(target.kind, ExprKind::Name(_)))
                    {
                        found.push((changed, target.span));
                    }
                }
            }
            // A `for mut` writes each item back, whatever its body does with it.
            if let StmtKind::For { iter, is_mut: true, .. } = &stmt.kind {
                for (looped, written) in looped_over(iter) {
                    found.push((looped.under(&[None]), written.span));
                }
            }
            for expr in exprs(stmt) {
                self.calls_in(expr, body, &mut found);
            }
            for block in blocks(stmt) {
                found.extend(self.changes_in(block, body));
            }
        }
        found
    }

    /// The places the calls in an expression change.
    fn calls_in(&mut self, expr: &Expr, body: Option<&Body>, found: &mut Vec<(Place, Span)>) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            if let ExprKind::Attr { object, name, .. } = &callee.kind
                && let Some(receiver) = place(object)
            {
                for inner in self.method_changes(object, name, body) {
                    found.push((receiver.under(&inner), expr.span));
                }
            }
            let callee_key = self.function_key(callee, body);
            for (at, arg) in args.iter().enumerate() {
                let Some(given) = place(&arg.value).filter(|_| arg.is_mut) else { continue };
                let inner =
                    callee_key.as_ref().and_then(|key| self.param_changes(key, at, arg.name.as_ref()));
                for inner in inner.unwrap_or_else(|| vec![Vec::new()]) {
                    found.push((given.under(&inner), arg.value.span));
                }
            }
        }
        if let ExprKind::Match { arms, .. } = &expr.kind {
            for arm in arms {
                found.extend(self.changes_in(&arm.body, body));
            }
        }
        for inner in crate::effects::children(expr) {
            self.calls_in(inner, body, found);
        }
    }

    /// What a method call changes under what it's called on: everything, for a built-in
    /// method that changes its value, and what the method's summary says for a `mut self` one.
    fn method_changes(&mut self, object: &Expr, method: &str, body: Option<&Body>) -> Vec<Vec<Step>> {
        let Some(ty) = body.and_then(|body| body.expr(object.span)) else { return Vec::new() };
        if let Some(found) = builtins::method(ty, method) {
            return if found.changes { vec![Vec::new()] } else { Vec::new() };
        }
        let Some(owner) = ty.type_name() else { return Vec::new() };
        let Some((key, entry)) = self.checker.program.method(owner, method) else { return Vec::new() };
        if !entry.def.params.first().is_some_and(|param| param.is_self && param.is_mut) {
            return Vec::new();
        }
        self.summary(&key).get("self").cloned().unwrap_or_default()
    }

    /// The key of the function a call names, when it's one of the program's.
    fn function_key(&self, callee: &Expr, body: Option<&Body>) -> Option<String> {
        match &callee.kind {
            ExprKind::Name(name) if self.checker.program.functions.contains_key(name) => Some(name.clone()),
            ExprKind::Attr { object, name, .. } => {
                let owner = body.and_then(|body| body.expr(object.span))?.type_name()?;
                self.checker.program.method(owner, name).map(|(key, _)| key)
            }
            _ => None,
        }
    }

    /// What a function changes under the parameter an argument is given for, by position or
    /// name. A method's `self` comes first, so positions skip it.
    fn param_changes(&mut self, key: &str, at: usize, named: Option<&String>) -> Option<Vec<Vec<Step>>> {
        let def = &self.checker.program.functions.get(key)?.def;
        let skip = usize::from(def.takes_self());
        let param = match named {
            Some(name) => def.params.iter().find(|param| &param.name == name)?,
            None => def.params.get(at + skip)?,
        };
        let name = param.name.clone();
        Some(self.summary(key).get(&name).cloned().unwrap_or_default())
    }

    /// What a function changes under each `mut` parameter, worked out once. Functions that
    /// call each other are worked out together: a call back into one being worked out sees
    /// what's known of it so far, and it's worked out again until nothing more is found.
    fn summary(&mut self, key: &str) -> Summary {
        if self.done.contains(key) || self.working.contains(key) {
            return self.summaries.get(key).cloned().unwrap_or_default();
        }
        let Some(entry) = self.checker.program.functions.get(key) else { return Summary::new() };
        let def = entry.def.clone();
        let mutable: Vec<String> =
            def.params.iter().filter(|param| param.is_mut).map(|param| param.name.clone()).collect();
        self.working.insert(key.to_owned());
        loop {
            let mut summary = Summary::new();
            for (changed, _) in self.changes_in(&def.body, self.checker.typed.get(key)) {
                if mutable.contains(&changed.root) {
                    let found = summary.entry(changed.root).or_default();
                    if !found.contains(&changed.steps) {
                        found.push(changed.steps);
                    }
                }
            }
            let grew = self.summaries.get(key) != Some(&summary);
            self.summaries.insert(key.to_owned(), summary);
            if !grew {
                break;
            }
        }
        self.working.remove(key);
        self.done.insert(key.to_owned());
        self.summaries.get(key).cloned().unwrap_or_default()
    }
}

/// Reports each `mut` argument whose place another argument of the same call can reach, in a
/// function's body or a nested function's. What a method is called on counts as an argument.
pub(crate) fn check_arguments(stmts: &[Stmt]) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    arguments_in_block(stmts, &mut problems);
    problems
}

fn arguments_in_block(stmts: &[Stmt], problems: &mut Vec<Diagnostic>) {
    for stmt in stmts {
        for expr in exprs(stmt) {
            arguments_in(expr, problems);
        }
        for block in blocks(stmt) {
            arguments_in_block(block, problems);
        }
        if let StmtKind::Def(def) = &stmt.kind {
            arguments_in_block(&def.body, problems);
        }
    }
}

fn arguments_in(expr: &Expr, problems: &mut Vec<Diagnostic>) {
    if let ExprKind::Call { callee, args } = &expr.kind {
        overlapping_arguments(callee, args, problems);
    }
    if let ExprKind::Match { arms, .. } = &expr.kind {
        for arm in arms {
            arguments_in_block(&arm.body, problems);
        }
    }
    for inner in crate::effects::children(expr) {
        arguments_in(inner, problems);
    }
}

fn overlapping_arguments(callee: &Expr, args: &[Arg], problems: &mut Vec<Diagnostic>) {
    let receiver = match &callee.kind {
        ExprKind::Attr { object, .. } => place(object).map(|found| (found, object.as_ref())),
        _ => None,
    };
    let mut earlier: Vec<(Place, &Expr, bool)> = Vec::new();
    for arg in args {
        let Some(given) = place(&arg.value) else { continue };
        let clash = earlier.iter().find(|(other, written, is_mut)| {
            (arg.is_mut || *is_mut) && given.overlaps(other) && !written_apart(&arg.value, written)
        });
        let held = receiver.as_ref().filter(|(called_on, written)| {
            arg.is_mut && given.overlaps(called_on) && !written_apart(&arg.value, written)
        });
        if let Some((_, other, is_mut)) = clash {
            let (first, second) = (written(other, *is_mut), written(&arg.value, arg.is_mut));
            let message = format!("{first} and {second} overlap");
            problems.push(Diagnostic::new(message, arg.value.span).with_help(OVERLAP_FIX));
        } else if let Some((_, called_on)) = held {
            let (first, second) = (written(&arg.value, true), written(called_on, false));
            let message = format!("{first} overlaps {second}, which this method is called on");
            problems.push(Diagnostic::new(message, arg.value.span).with_help(OVERLAP_FIX));
        }
        earlier.push((given, &arg.value, arg.is_mut));
    }
    picked_by_a_mut_argument(callee, args, problems);
}

/// Whether two places are written with different indexes that are known as written, like `xs[0]`
/// and `xs[1]`, so they can't be one item. `xs[-1]` could be `xs[0]`, so an index counted from the
/// end isn't known.
fn written_apart(first: &Expr, second: &Expr) -> bool {
    steps_of(first).iter().zip(&steps_of(second)).any(|(one, other)| match (&one.kind, &other.kind) {
        (ExprKind::Int(one), ExprKind::Int(other)) => one != other,
        (ExprKind::Str(one), ExprKind::Str(other)) => one != other,
        _ => false,
    })
}

/// What each step along a place goes by, from its root: an index, or for a field the field's own
/// expression, which is never told apart here, since `Place::overlaps` compares fields.
fn steps_of(expr: &Expr) -> Vec<&Expr> {
    let (object, step) = match &expr.kind {
        ExprKind::Attr { object, .. } => (object, expr),
        ExprKind::Index { object, index } => (object, index.as_ref()),
        _ => return Vec::new(),
    };
    let mut steps = steps_of(object);
    steps.push(step);
    steps
}

/// `units[at].step(mut at)` and `f(mut xs[i], mut i)`: a `mut` argument changes what picks the
/// place another argument names, so which place that is would depend on when it's read.
fn picked_by_a_mut_argument(callee: &Expr, args: &[Arg], problems: &mut Vec<Diagnostic>) {
    let receiver = match &callee.kind {
        ExprKind::Attr { object, .. } => Some(object.as_ref()),
        _ => None,
    };
    let others = || receiver.into_iter().chain(args.iter().map(|arg| &arg.value));
    for arg in args.iter().filter(|arg| arg.is_mut) {
        let Some(changed) = place(&arg.value) else { continue };
        let picked = others().find(|other| {
            other.span != arg.value.span && place(other).is_some() && picks_with(other, &changed)
        });
        if let Some(other) = picked {
            let (first, second) = (written(&arg.value, true), written(other, false));
            let message = format!("{first} changes what picks {second}");
            problems.push(
                Diagnostic::new(message, arg.value.span)
                    .with_help("hold the index in another name, and give that name as `mut`"),
            );
        }
    }
}

/// Whether an index along a place reads `changed`.
fn picks_with(expr: &Expr, changed: &Place) -> bool {
    match &expr.kind {
        ExprKind::Attr { object, .. } => picks_with(object, changed),
        ExprKind::Index { object, index } => reads(index, changed) || picks_with(object, changed),
        _ => false,
    }
}

fn reads(expr: &Expr, changed: &Place) -> bool {
    place(expr).is_some_and(|found| found.overlaps(changed))
        || crate::effects::children(expr).into_iter().any(|inner| reads(inner, changed))
}

const OVERLAP_FIX: &str = "a `mut` argument needs its place to itself: give the other `copy(...)` of what \
it reads, or make the changes one at a time";

/// An argument that names a place, in backticks as it's written.
fn written(expr: &Expr, is_mut: bool) -> String {
    let word = if is_mut { "mut " } else { "" };
    format!("`{word}{}`", place_text(expr))
}

fn place_text(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Name(name) => name.clone(),
        ExprKind::Attr { object, name, .. } => format!("{}.{name}", place_text(object)),
        ExprKind::Index { object, index } => {
            let at = match &index.kind {
                ExprKind::Name(name) => name.clone(),
                ExprKind::Int(value) => value.to_string(),
                _ => "...".to_owned(),
            };
            format!("{}[{at}]", place_text(object))
        }
        _ => "...".to_owned(),
    }
}

/// The blocks directly inside a statement, but not a nested function's.
pub(crate) fn blocks(stmt: &Stmt) -> Vec<&[Stmt]> {
    match &stmt.kind {
        StmtKind::If { branches, otherwise } => {
            branches.iter().map(|(_, block)| block.as_slice()).chain(otherwise.as_deref()).collect()
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => vec![body],
        _ => Vec::new(),
    }
}

/// The expressions written directly in a statement.
pub(crate) fn exprs(stmt: &Stmt) -> Vec<&Expr> {
    match &stmt.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value)) => vec![value],
        StmtKind::Assign { target, value, .. } => vec![target, value],
        StmtKind::If { branches, .. } => branches.iter().map(|(cond, _)| cond).collect(),
        StmtKind::While { cond: value, .. }
        | StmtKind::For { iter: value, .. }
        | StmtKind::With { value, .. } => {
            vec![value]
        }
        StmtKind::Return(None) | StmtKind::Def(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {
            Vec::new()
        }
    }
}
