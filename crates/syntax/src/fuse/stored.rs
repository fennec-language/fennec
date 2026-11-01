//! Moving a stage kept in a name to where that name is used.

use std::collections::HashSet;

use super::{has_yield, hoist_sources, is_generator_expression, is_name, is_stage, mentions, names_in};
use crate::ast::{Arg, CompKind, Expr, ExprKind, FnDef, Item, Module, Pattern, Stmt, StmtKind};
use crate::visit;

/// What a function declares and what it can change, for telling whether a stage's names keep
/// their values until it runs.
#[derive(Default)]
struct Declared {
    /// Each name declared, once for each time.
    names: Vec<String>,
    /// Names that can change after they're declared.
    changing: HashSet<String>,
}

impl Declared {
    fn of(def: &FnDef) -> Self {
        let mut declared = Self::default();
        for param in &def.params {
            declared.names.push(param.name.clone());
            if param.is_mut {
                declared.changing.insert(param.name.clone());
            }
        }
        declared.walk(&def.body);
        declared
    }

    fn walk(&mut self, body: &[Stmt]) {
        for stmt in body {
            match &stmt.kind {
                StmtKind::Assign { target, op, declare_mut, value, .. } => {
                    self.target(target, op.is_some(), *declare_mut);
                    self.in_match(value);
                }
                StmtKind::Expr(value) => self.in_match(value),
                StmtKind::If { branches, otherwise } => {
                    for (_, body) in branches {
                        self.walk(body);
                    }
                    self.walk(otherwise.as_deref().unwrap_or_default());
                }
                StmtKind::While { body, .. } => self.walk(body),
                StmtKind::For { names, is_mut, body, .. } => {
                    self.names.extend(names.iter().cloned());
                    if *is_mut {
                        self.changing.extend(names.iter().cloned());
                    }
                    self.walk(body);
                }
                StmtKind::With { name, is_mut, body, .. } => {
                    self.names.extend(name.iter().cloned());
                    if *is_mut {
                        self.changing.extend(name.iter().cloned());
                    }
                    self.walk(body);
                }
                StmtKind::Def(def) => self.names.push(def.name.clone()),
                _ => {}
            }
        }
    }

    fn in_match(&mut self, value: &Expr) {
        if let ExprKind::Match { arms, .. } = &value.kind {
            for arm in arms {
                pattern_names(&arm.pattern, &mut self.names);
                self.walk(&arm.body);
            }
        }
    }

    fn target(&mut self, target: &Expr, changed: bool, declare_mut: bool) {
        match &target.kind {
            ExprKind::Name(name) => {
                // Giving a name a new value is written just like declaring it.
                if changed || declare_mut || self.names.contains(name) {
                    self.changing.insert(name.clone());
                }
                if !changed {
                    self.names.push(name.clone());
                }
            }
            ExprKind::Tuple(items) => {
                for item in items {
                    self.target(item, changed, declare_mut);
                }
            }
            // A field or item changing changes the name it's in.
            ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => {
                self.target(object, true, declare_mut);
            }
            _ => {}
        }
    }

    /// Whether a name means the same thing, with the same value, everywhere in the function.
    fn steady(&self, name: &str) -> bool {
        !self.changing.contains(name) && self.names.iter().filter(|other| *other == name).count() <= 1
    }
}

fn pattern_names(pattern: &Pattern, names: &mut Vec<String>) {
    match pattern {
        Pattern::Bind(name) => names.push(name.clone()),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                pattern_names(arg, names);
            }
        }
        _ => {}
    }
}

/// Moves each stage kept in a name and used once to where it's used, in every function.
pub fn stored_stages(module: &mut Module) {
    let generators: HashSet<String> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(def) if has_yield(&def.body) => Some(def.name.clone()),
            _ => None,
        })
        .collect();
    if generators.is_empty() && !has_stage_expression(module) {
        return;
    }
    for item in &mut module.items {
        match item {
            Item::Function(def) => in_function(def, &generators),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                for method in &mut def.methods {
                    in_function(method, &generators);
                }
            }
            Item::Extend(extension) => {
                for method in &mut extension.methods {
                    in_function(method, &generators);
                }
            }
            _ => {}
        }
    }
}

fn has_stage_expression(module: &mut Module) -> bool {
    let mut found = false;
    visit::exprs_mut(module, &mut |value| found |= is_generator_expression(value));
    found
}

fn in_function(def: &mut FnDef, generators: &HashSet<String>) {
    let declared = Declared::of(def);
    let every = names_in(&def.body);
    let mut work = Moving { declared, every, generators };
    work.block(&mut def.body);
    let hidden_builtins = &work.declared.names;
    visit::in_stmts(&mut def.body, &mut |value| collected_in_place(value, hidden_builtins));
}

/// `list(x for x in xs)` is `[x for x in xs]`, and likewise for `set`.
fn collected_in_place(value: &mut Expr, locals: &[String]) {
    let ExprKind::Call { callee, args } = &mut value.kind else { return };
    let ExprKind::Name(called) = &callee.kind else { return };
    let made = match called.as_str() {
        "list" => CompKind::List,
        "set" => CompKind::Set,
        _ => return,
    };
    if locals.contains(called) {
        return;
    }
    let [Arg { name: None, is_mut: false, value: generator }] = args.as_mut_slice() else { return };
    if !is_generator_expression(generator) {
        return;
    }
    if let ExprKind::Comprehension { elem, names, iter, cond, .. } = &mut generator.kind {
        value.kind = ExprKind::Comprehension {
            kind: made,
            elem: elem.clone(),
            value: None,
            names: std::mem::take(names),
            iter: iter.clone(),
            cond: cond.take(),
        };
    }
}

struct Moving<'a> {
    declared: Declared,
    every: Vec<String>,
    generators: &'a HashSet<String>,
}

impl Moving<'_> {
    fn block(&mut self, body: &mut Vec<Stmt>) {
        let mut at = 0;
        while at < body.len() {
            if !self.move_stage(body, at) {
                if let Some(stmt) = body.get_mut(at) {
                    self.inner_blocks(stmt);
                }
                at += 1;
            }
        }
    }

    fn inner_blocks(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    self.block(body);
                }
                if let Some(body) = otherwise {
                    self.block(body);
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
                self.block(body);
            }
            _ => {}
        }
    }

    /// Moves the stage the statement at `at` keeps, if it can, leaving what has to be worked
    /// out at that point. Gives whether it did.
    fn move_stage(&mut self, body: &mut Vec<Stmt>, at: usize) -> bool {
        let Some(Stmt {
            kind:
                StmtKind::Assign {
                    target: Expr { kind: ExprKind::Name(name), .. },
                    op: None,
                    value,
                    declare_mut: false,
                    ty: None,
                },
            ..
        }) = body.get(at)
        else {
            return false;
        };
        let generators = |name: &str| self.generators.contains(name);
        if !is_stage(value, &generators) || !self.movable(name, value) {
            return false;
        }
        let (name, mut stage) = (name.clone(), value.clone());
        // Find the use first: nothing changes if there isn't one to move it to.
        let (_, later) = body.split_at_mut(at + 1);
        if !later.iter_mut().any(|stmt| use_of(stmt, &name, self.generators).is_some()) {
            return false;
        }
        let mut worked_out = Vec::new();
        let steady = |name: &str| self.declared.steady(name);
        hoist_sources(&mut stage, &steady, &generators, &mut worked_out);
        for stmt in later.iter_mut() {
            if let Some(slot) = use_of(stmt, &name, self.generators) {
                *slot = stage;
                break;
            }
        }
        body.splice(at..=at, worked_out);
        true
    }

    fn movable(&self, name: &str, stage: &Expr) -> bool {
        // Once where it's declared, and once where it's used.
        let used = self.every.iter().filter(|other| *other == name).count();
        if used != 2 || !self.declared.steady(name) {
            return false;
        }
        let mut seen = Vec::new();
        free_names(stage, &|name| self.generators.contains(name), &mut seen);
        seen.iter().all(|other| self.declared.steady(other))
    }
}

/// The names a stage reads later, when it runs, rather than when it's made.
fn free_names(stage: &Expr, generators: &dyn Fn(&str) -> bool, found: &mut Vec<String>) {
    if let ExprKind::Comprehension { elem, names, iter, cond, .. } = &stage.kind {
        let mut own = Vec::new();
        mentions(elem, &mut own);
        if let Some(cond) = cond {
            mentions(cond, &mut own);
        }
        found.extend(own.into_iter().filter(|name| !names.contains(name)));
        if is_stage(iter, generators) {
            free_names(iter, generators, found);
        }
    } else if let ExprKind::Call { args, .. } = &stage.kind {
        for arg in args {
            if is_stage(&arg.value, generators) {
                free_names(&arg.value, generators, found);
            }
        }
    }
}

/// Where a statement uses a name in a way a stage can take its place, running at most once.
fn use_of<'a>(stmt: &'a mut Stmt, name: &str, generators: &HashSet<String>) -> Option<&'a mut Expr> {
    match &mut stmt.kind {
        StmtKind::Expr(value) | StmtKind::Return(Some(value)) | StmtKind::Assign { value, .. } => {
            slot_in(value, name, generators)
        }
        StmtKind::For { iter, is_mut: false, .. } => {
            if is_name(iter, name) {
                Some(iter)
            } else {
                slot_in(iter, name, generators)
            }
        }
        StmtKind::If { branches, otherwise } => {
            for (_, body) in branches {
                for stmt in body {
                    if let Some(slot) = use_of(stmt, name, generators) {
                        return Some(slot);
                    }
                }
            }
            otherwise.iter_mut().flatten().find_map(|stmt| use_of(stmt, name, generators))
        }
        _ => None,
    }
}

/// What uses every item, or stops part way, of what it's given.
const CONSUMERS: [&str; 7] = ["sum", "min", "max", "sorted", "list", "any", "all"];

fn slot_in<'a>(value: &'a mut Expr, name: &str, generators: &HashSet<String>) -> Option<&'a mut Expr> {
    match &mut value.kind {
        ExprKind::Comprehension { iter, .. } => {
            if is_name(iter, name) {
                Some(iter)
            } else {
                slot_in(iter, name, generators)
            }
        }
        ExprKind::Call { callee, args } => {
            let takes_stage = matches!(&callee.kind, ExprKind::Name(called)
                if CONSUMERS.contains(&called.as_str()) || generators.contains(called));
            for arg in args {
                if takes_stage && !arg.is_mut && arg.name.is_none() && is_name(&arg.value, name) {
                    return Some(&mut arg.value);
                }
                if let Some(slot) = slot_in(&mut arg.value, name, generators) {
                    return Some(slot);
                }
            }
            None
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            slot_in(lhs, name, generators).or_else(|| slot_in(rhs, name, generators))
        }
        ExprKind::Unary { expr, .. } | ExprKind::Try { value: expr, .. } => slot_in(expr, name, generators),
        _ => None,
    }
}
