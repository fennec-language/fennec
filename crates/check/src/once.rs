//! A generator is used once, and a second use is a compile error where that can be proved: the
//! same generator, by any of its names, used again on every way on from its first use, with no
//! new value given to the name between. `b = a` is two names for one generator. A use in only
//! some branches of an `if`, or inside a loop's body, doesn't prove one after it, so it's left to
//! the run, which stops the program with the same message.

use std::collections::HashMap;

use fennec_syntax::{Also, Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use crate::Body;
use crate::checker::Checker;
use crate::loops::{blocks, exprs};
use crate::types::Type;

/// Each provable second use of a generator in a function's body.
pub(crate) fn check_function(checker: &Checker, key: &str, def: &FnDef) -> Vec<Diagnostic> {
    let Some(body) = checker.typed.get(key) else { return Vec::new() };
    let mut walk = Walk { body, problems: Vec::new(), groups: 0 };
    let mut state = State::default();
    // A generator passed in is one the function can use once too.
    for param in &def.params {
        if param.ty.as_ref().is_some_and(|ty| ty.name == "Iterator" && !ty.optional) {
            walk.groups += 1;
            state.names.insert(param.name.clone(), walk.groups);
        }
    }
    walk.block(&def.body, &mut state);
    walk.problems
}

/// What's known along one way through a body: the generator each name holds, by a number shared
/// between names for one generator, and where each generator was first used.
#[derive(Clone, Default)]
struct State {
    names: HashMap<String, usize>,
    used: HashMap<usize, Span>,
}

struct Walk<'b> {
    body: &'b Body,
    problems: Vec<Diagnostic>,
    groups: usize,
}

impl Walk<'_> {
    fn is_generator(&self, expr: &Expr) -> bool {
        matches!(self.body.expr(expr.span), Some(Type::Iterator(_)))
    }

    /// Walks a block, giving back whether it goes on past its end.
    fn block(&mut self, stmts: &[Stmt], state: &mut State) -> bool {
        for stmt in stmts {
            if !self.stmt(stmt, state) {
                return false;
            }
        }
        true
    }

    fn stmt(&mut self, stmt: &Stmt, state: &mut State) -> bool {
        match &stmt.kind {
            StmtKind::Assign { target, value, .. } => {
                self.uses(value, state);
                if let ExprKind::Name(name) = &target.kind {
                    let shared = match &value.kind {
                        ExprKind::Name(from) => state.names.get(from).copied(),
                        _ => None,
                    };
                    if let Some(group) = shared {
                        state.names.insert(name.clone(), group);
                    } else if self.is_generator(target) || self.is_generator(value) {
                        self.groups += 1;
                        state.names.insert(name.clone(), self.groups);
                    } else {
                        state.names.remove(name);
                    }
                } else {
                    self.uses(target, state);
                }
                true
            }
            StmtKind::For { iter, body, .. } => {
                self.use_or_walk(iter, state);
                // A loop's body may not run, so what it uses proves nothing after it.
                self.block(body, &mut state.clone());
                true
            }
            StmtKind::While { cond, body } => {
                self.uses(cond, state);
                self.block(body, &mut state.clone());
                true
            }
            StmtKind::If { branches, otherwise } => self.if_stmt(branches, otherwise.as_deref(), state),
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.uses(value, state);
                }
                false
            }
            StmtKind::Fail(value) => {
                self.uses(value, state);
                false
            }
            StmtKind::Break | StmtKind::Continue => false,
            StmtKind::Def(_) => true,
            _ => {
                for expr in exprs(stmt) {
                    self.uses(expr, state);
                }
                for block in blocks(stmt) {
                    self.block(block, &mut state.clone());
                }
                true
            }
        }
    }

    /// An `if`: each branch on what's known when it's taken. What's used on every way that goes
    /// on past it is used after it.
    fn if_stmt(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: Option<&[Stmt]>,
        state: &mut State,
    ) -> bool {
        let mut on: Vec<State> = Vec::new();
        let mut before = state.clone();
        for (at, (cond, body)) in branches.iter().enumerate() {
            // Only the first condition is sure to be worked out.
            if at == 0 {
                self.uses(cond, &mut before);
            } else {
                self.uses(cond, &mut before.clone());
            }
            let mut taken = before.clone();
            if self.block(body, &mut taken) {
                on.push(taken);
            }
        }
        match otherwise {
            Some(body) => {
                let mut taken = before.clone();
                if self.block(body, &mut taken) {
                    on.push(taken);
                }
            }
            None => on.push(before),
        }
        let Some((first, rest)) = on.split_first() else { return false };
        let mut merged = first.clone();
        merged.used.retain(|group, _| rest.iter().all(|other| other.used.contains_key(group)));
        merged.names.retain(|name, group| rest.iter().all(|other| other.names.get(name) == Some(group)));
        *state = merged;
        true
    }

    /// A generator named where it's used: looped over, or given to a call or a new stage.
    fn use_or_walk(&mut self, expr: &Expr, state: &mut State) {
        if let ExprKind::Name(name) = &expr.kind
            && self.is_generator(expr)
        {
            self.used(name, expr.span, state);
        } else {
            self.uses(expr, state);
        }
    }

    /// The uses of generators in an expression. Printing one shows it and doesn't use it.
    fn uses(&mut self, expr: &Expr, state: &mut State) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                let shows =
                    matches!(&callee.kind, ExprKind::Attr { name, .. } if name == "print" || name == "write");
                self.uses(callee, state);
                for arg in args {
                    if shows {
                        self.uses(&arg.value, state);
                    } else {
                        self.use_or_walk(&arg.value, state);
                    }
                }
            }
            ExprKind::Comprehension { iter, .. } => {
                self.use_or_walk(iter, state);
                for inner in crate::effects::children(expr) {
                    if !std::ptr::eq(inner, iter.as_ref()) {
                        self.uses(inner, &mut state.clone());
                    }
                }
            }
            // What a `match` case does may not happen.
            ExprKind::Match { subject, arms } => {
                self.uses(subject, state);
                for arm in arms {
                    self.block(&arm.body, &mut state.clone());
                }
            }
            _ => {
                for inner in crate::effects::children(expr) {
                    self.uses(inner, state);
                }
            }
        }
    }

    fn used(&mut self, name: &str, at: Span, state: &mut State) {
        let Some(&group) = state.names.get(name) else { return };
        match state.used.get(&group) {
            Some(&first) => self.problems.push(
                Diagnostic::new("this generator has already been used", at)
                    .with_help("a generator can be used once: `list(...)` keeps its items to use again")
                    .also_at_the(Also::FirstUse, first),
            ),
            None => {
                state.used.insert(group, at);
            }
        }
    }
}
