//! A change nothing can see. A local that's changed and then never read, returned or passed on
//! did nothing: `mut unit = find(units, id)`, then `unit.health = 1`, and `units` still holds what
//! it held. It's worked out backward through each function, from what's read later.
//!
//! Only a local that holds plain values counts. One holding a capability or a `shared` handle can
//! do something a read of it wouldn't show, a `mut` parameter is the caller's to read, and a name
//! for a place is the place. A call that gives back a value used for something counts as a read,
//! and so does passing the name on, even as `mut`.

use std::collections::{BTreeMap, HashMap, HashSet};

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use crate::Body;
use crate::builtins;
use crate::checker::Checker;
use crate::effects::children;
use crate::types::Type;

type Live = HashSet<String>;

/// Whether a type can do anything a read of it wouldn't show: a capability, or a `shared` handle,
/// in it or anywhere inside it. What's not known yet counts as able to.
fn does_more_than_hold(checker: &mut Checker, ty: &Type) -> bool {
    match ty {
        Type::Unknown | Type::Iterator(_) | Type::Function(_) | Type::Arrow(..) => true,
        _ => {
            checker.holds_capability(ty, &mut Vec::new())
                || checker.shared_inside(ty, &mut Vec::new()).is_some()
        }
    }
}

struct Lost<'a> {
    checker: &'a Checker,
    body: Option<&'a Body>,
    /// The locals that count, with what each started as.
    locals: HashMap<&'a str, &'a Expr>,
    /// The names a nested function reads, which it can do at any time.
    captured: HashSet<String>,
    /// What's live where `break` and `continue` go, innermost loop last.
    loops: Vec<(Live, Live)>,
    /// Whether this pass is the settled one, which reports.
    reporting: bool,
    /// The first lost change to each local.
    found: BTreeMap<&'a str, Span>,
}

/// Locals, each by its name and the start and end of its first value.
pub(crate) type Counted = HashSet<(String, u32, u32)>;

/// The locals that count among those a body declares, each by its name and where its first
/// value is: the ones holding plain values. Worked out before the body is walked, since it may
/// need to look into types.
pub(crate) fn counted(checker: &mut Checker, key: &str, stmts: &[Stmt]) -> Counted {
    let mut locals = Vec::new();
    declared(stmts, &mut locals);
    let typed: Vec<(String, Span, Type)> = {
        let body = checker.typed.get(key);
        locals
            .into_iter()
            .map(|(name, value)| {
                let ty = body.and_then(|body| body.expr(value.span)).cloned().unwrap_or(Type::Unknown);
                (name.to_owned(), value.span, ty)
            })
            .collect()
    };
    typed
        .into_iter()
        .filter(|(_, _, ty)| !does_more_than_hold(checker, ty))
        .map(|(name, span, _)| (name, span.start, span.end))
        .collect()
}

/// Reports each local in a function's or test's body whose change nothing can see, of those
/// `counted` gives.
pub(crate) fn check_body(checker: &Checker, key: &str, stmts: &[Stmt], counted: &Counted) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    let mut locals = Vec::new();
    declared(stmts, &mut locals);
    let body = checker.typed.get(key);
    let mut kept = HashMap::new();
    for (name, value) in locals {
        if counted.contains(&(name.to_owned(), value.span.start, value.span.end)) {
            kept.insert(name, value);
        }
    }
    let mut lost = Lost {
        checker,
        body,
        locals: kept,
        captured: HashSet::new(),
        loops: Vec::new(),
        reporting: true,
        found: BTreeMap::new(),
    };
    lost.block(stmts, Live::new());
    for (name, span) in std::mem::take(&mut lost.found) {
        if !lost.captured.contains(name) {
            problems.push(lost.problem(name, span));
        }
    }
    problems
}

/// Every `mut x = ...` in a block and what's inside it, but not a nested function's.
fn declared<'a>(stmts: &'a [Stmt], found: &mut Vec<(&'a str, &'a Expr)>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { target, value, declare_mut: true, .. } => {
                if let ExprKind::Name(name) = &target.kind {
                    found.push((name, value));
                }
            }
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches {
                    declared(block, found);
                }
                declared(otherwise.as_deref().unwrap_or_default(), found);
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => {
                declared(body, found);
            }
            StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
                for arm in arms {
                    declared(&arm.body, found);
                }
            }
            _ => {}
        }
    }
}

/// Where a local's value came from, as far as a message can say.
enum Started {
    /// It's a slot of a table, like `units[at]` or `units.get(at)`.
    Table(String),
    /// A call gave it back.
    Called(String),
    Other,
}

fn started_as(value: &Expr) -> Started {
    match &value.kind {
        ExprKind::Else { value, .. } | ExprKind::Try { value, .. } => started_as(value),
        ExprKind::Index { object, .. } | ExprKind::Attr { object, optional: false, .. } => {
            fennec_syntax::dotted(object).map_or(Started::Other, Started::Table)
        }
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Attr { object, name, .. } if name == "get" => {
                fennec_syntax::dotted(object).map_or(Started::Other, Started::Table)
            }
            ExprKind::Attr { name, .. } | ExprKind::Name(name) => Started::Called(name.clone()),
            _ => Started::Other,
        },
        _ => Started::Other,
    }
}

/// The name a place starts from, like `unit` in `unit.tags[0]`.
fn root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. } => root(object),
        _ => None,
    }
}

/// Adds every name an expression reads.
fn reads(expr: &Expr, live: &mut Live) {
    match &expr.kind {
        ExprKind::Name(name) => {
            live.insert(name.clone());
        }
        ExprKind::Lambda { body, .. } => reads(body, live),
        ExprKind::Match { subject, arms } => {
            reads(subject, live);
            for arm in arms {
                reads_in(&arm.body, live);
            }
        }
        _ => {
            for inner in children(expr) {
                reads(inner, live);
            }
        }
    }
}

/// Adds every name a block mentions anywhere: what's written is read, for a block whose order
/// isn't followed.
fn reads_in(stmts: &[Stmt], live: &mut Live) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Expr(value)
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value)
            | StmtKind::Lend { value, .. }
            | StmtKind::Return(Some(value)) => reads(value, live),
            StmtKind::Assign { target, value, .. } => {
                reads(target, live);
                reads(value, live);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, block) in branches {
                    reads(cond, live);
                    reads_in(block, live);
                }
                reads_in(otherwise.as_deref().unwrap_or_default(), live);
            }
            StmtKind::While { cond: value, body }
            | StmtKind::For { iter: value, body, .. }
            | StmtKind::With { value, body, .. } => {
                reads(value, live);
                reads_in(body, live);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                reads_in(cleanup, live);
                reads_in(body, live);
            }
            StmtKind::Def(def) => reads_in(&def.body, live),
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }
}

/// Adds what indexes read in a place being written, but not what it's rooted in.
fn reads_in_place(target: &Expr, live: &mut Live) {
    match &target.kind {
        ExprKind::Attr { object, .. } => reads_in_place(object, live),
        ExprKind::Index { object, index } => {
            reads_in_place(object, live);
            reads(index, live);
        }
        ExprKind::Name(_) => {}
        _ => reads(target, live),
    }
}

impl<'a> Lost<'a> {
    fn problem(&self, name: &str, span: Span) -> Diagnostic {
        let value = self.locals.get(name);
        let (message, help) = match value.map(|value| started_as(value)) {
            Some(Started::Table(table)) => (
                format!("this changes your own copy of what's in `{table}`, and nothing reads it"),
                match value
                    .filter(|value| matches!(value.kind, ExprKind::Index { .. } | ExprKind::Attr { .. }))
                {
                    Some(place) => format!(
                        "to change it where it is, write `ref {name} = {}`, or take out the change",
                        crate::loops::place_text(place)
                    ),
                    None => format!("to change `{table}`, write to it there, or take out the change"),
                },
            ),
            Some(Started::Called(called)) if !self.checker.program.is_type_name(&called) => (
                format!("this changes your own copy of what `{called}` gave back, and nothing reads it"),
                format!("use `{name}` after this, write it back where it came from, or take out the change"),
            ),
            _ => (
                format!("this change to `{name}` is lost: nothing reads `{name}` after this"),
                format!("use `{name}` after this, or take out the change"),
            ),
        };
        Diagnostic::new(message, span).with_help(help)
    }

    /// Notes a change to `name`, when what's after it doesn't read it.
    fn changed(&mut self, name: &str, span: Span, live: &Live) {
        if self.reporting
            && !live.contains(name)
            && let Some((known, _)) = self.locals.get_key_value(name)
        {
            let at = self.found.entry(*known).or_insert(span);
            if span.start < at.start {
                *at = span;
            }
        }
    }

    /// Whether a call gives back nothing. What one gives back might be used, as the last thing in a
    /// function is, so it counts as reading what it's called on.
    fn gives_back_nothing(&self, call: Span) -> bool {
        self.body.and_then(|body| body.expr(call)).is_some_and(|ty| *ty == Type::Unit)
    }

    /// Whether a method only changes what it's called on, with nothing it's given able to do more.
    fn only_changes(&self, object: &Expr, method: &str, args: &[fennec_syntax::Arg]) -> bool {
        let Some(ty) = self.body.and_then(|body| body.expr(object.span)) else { return false };
        // Something it's given can do more than hold, like a capability it writes through.
        let plain_args = args.iter().all(|arg| {
            let given = self.body.and_then(|body| body.expr(arg.value.span));
            !arg.is_mut
                && given.is_some_and(|given| {
                    !matches!(given, Type::Unknown | Type::Function(_) | Type::Arrow(..))
                })
                && given.is_some_and(|given| !crate::shared::is_capability_type(given))
        });
        if !plain_args {
            return false;
        }
        if let Some(found) = builtins::method(ty, method) {
            return found.changes;
        }
        let Some(owner) = ty.type_name() else { return false };
        let found = self.checker.program.method(owner, method);
        found.is_some_and(|(_, entry)| {
            entry.def.params.first().is_some_and(|param| param.is_self && param.is_mut)
        })
    }

    /// What's live before a block, given what's live after it.
    fn block(&mut self, stmts: &'a [Stmt], after: Live) -> Live {
        stmts.iter().rev().fold(after, |live, stmt| self.stmt(stmt, live))
    }

    /// What's live before a loop, given what's live after it. A loop's start can come after any
    /// point in it, so it's worked out until nothing more is found, and only then reported.
    fn looped(&mut self, body: &'a [Stmt], head_reads: &Live, bound: &[String], after: &Live) -> Live {
        let reporting = std::mem::replace(&mut self.reporting, false);
        let mut head: Live = after.union(head_reads).cloned().collect();
        loop {
            self.loops.push((after.clone(), head.clone()));
            let mut inside = self.block(body, head.clone());
            self.loops.pop();
            for name in bound {
                inside.remove(name);
            }
            let next: Live = head.union(&inside).cloned().collect();
            if next.len() == head.len() {
                break;
            }
            head = next;
        }
        self.reporting = reporting;
        if reporting {
            self.loops.push((after.clone(), head.clone()));
            self.block(body, head.clone());
            self.loops.pop();
        }
        head
    }

    /// What's live before an assignment. A new value hides the old one, and `+=` is only read for
    /// itself.
    fn assigned(
        &mut self,
        (target, value): (&'a Expr, &'a Expr),
        compound: bool,
        declares: bool,
        span: Span,
        mut live: Live,
    ) -> Live {
        let targets: Vec<&Expr> = match &target.kind {
            ExprKind::Tuple(items) => items.iter().collect(),
            _ => vec![target],
        };
        let before = live.clone();
        for target in targets {
            match (&target.kind, root(target)) {
                (ExprKind::Name(name), _) => {
                    if !declares {
                        self.changed(name, span, &before);
                    }
                    live.remove(name);
                    if compound && before.contains(name) {
                        live.insert(name.clone());
                    }
                }
                (_, Some(name)) => {
                    self.changed(name, span, &before);
                    reads_in_place(target, &mut live);
                }
                (_, None) => reads(target, &mut live),
            }
        }
        reads(value, &mut live);
        live
    }

    /// What's live before a block with a cleanup, which can run at any point inside it.
    fn on_failure(&mut self, cleanup: &'a [Stmt], body: &'a [Stmt], live: Live) -> Live {
        let mut before = self.block(body, live);
        reads_in(cleanup, &mut before);
        before
    }

    fn stmt(&mut self, stmt: &'a Stmt, mut live: Live) -> Live {
        match &stmt.kind {
            StmtKind::Assign { target, op, value, declare_mut, .. } => {
                self.assigned((target, value), op.is_some(), *declare_mut, stmt.span, live)
            }
            StmtKind::Expr(Expr { kind: ExprKind::Call { callee, args }, span })
                if self.gives_back_nothing(*span)
                    && matches!(&callee.kind, ExprKind::Attr { object, name, optional: false }
                    if root(object).is_some() && self.only_changes(object, name, args)) =>
            {
                let ExprKind::Attr { object, .. } = &callee.kind else { return live };
                if let Some(name) = root(object) {
                    self.changed(name, *span, &live);
                }
                reads_in_place(object, &mut live);
                for arg in args {
                    reads(&arg.value, &mut live);
                }
                live
            }
            StmtKind::Expr(Expr { kind: ExprKind::Match { subject, arms }, .. }) => {
                let mut before = Live::new();
                for arm in arms {
                    before.extend(self.block(&arm.body, live.clone()));
                }
                reads(subject, &mut before);
                before
            }
            StmtKind::Expr(value) | StmtKind::Yield(value) | StmtKind::Assert(value) => {
                reads(value, &mut live);
                live
            }
            StmtKind::Return(value) => {
                let mut before = Live::new();
                if let Some(value) = value {
                    reads(value, &mut before);
                }
                before
            }
            StmtKind::Fail(value) => {
                let mut before = Live::new();
                reads(value, &mut before);
                before
            }
            StmtKind::If { branches, otherwise } => {
                let mut before = self.block(otherwise.as_deref().unwrap_or_default(), live.clone());
                for (cond, block) in branches {
                    before.extend(self.block(block, live.clone()));
                    reads(cond, &mut before);
                }
                before
            }
            StmtKind::While { cond, body } => {
                let mut head = Live::new();
                reads(cond, &mut head);
                self.looped(body, &head, &[], &live)
            }
            StmtKind::For { names, iter, body, .. } => {
                let mut before = self.looped(body, &Live::new(), names, &live);
                reads(iter, &mut before);
                before
            }
            StmtKind::OnFailure { cleanup, body, .. } => self.on_failure(cleanup, body, live),
            StmtKind::With { value, body, .. } => {
                let mut before = self.block(body, live);
                reads(value, &mut before);
                before
            }
            StmtKind::Def(def) => {
                let mut mentioned = Live::new();
                reads_in(&def.body, &mut mentioned);
                self.captured.extend(mentioned.iter().cloned());
                live.extend(mentioned);
                live
            }
            StmtKind::Break => self.loops.last().map_or_else(Live::new, |(exit, _)| exit.clone()),
            StmtKind::Continue => self.loops.last().map_or_else(Live::new, |(_, head)| head.clone()),
            StmtKind::Pass => live,
            StmtKind::Lend { value, .. } => read_then(value, live),
        }
    }
}

/// What's live before reading `value`, given what's live after.
fn read_then(value: &Expr, mut live: Live) -> Live {
    reads(value, &mut live);
    live
}
