//! `mut x = place`: a name for a place, so writing through it edits the real thing. It's worked
//! out from the function's statements once its types are known. A name that's only replaced
//! holds a value of its own, and one that's edited stands for the place.
//!
//! The rules keep two names from reaching one thing. The name can't be stored, returned or
//! captured, and what it came from can't be used until its last use, so there's nothing to
//! write about lifetimes. `mut x = other` followed by reading `other` asks for `copy(other)`.
//! What's written out here goes into the module as the place itself, so the engines only see
//! ordinary writes.

use std::collections::{BTreeSet, HashSet};

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use crate::Body;
use crate::builtins;
use crate::checker::Checker;

/// A step along a place.
#[derive(Clone)]
enum Step<'a> {
    Field(&'a str),
    Index(&'a Expr),
}

/// A place written out: a name, and the steps from it.
#[derive(Clone)]
struct Path<'a> {
    root: &'a str,
    steps: Vec<Step<'a>>,
}

/// The place an expression names, if it names one.
fn path(expr: &Expr) -> Option<Path<'_>> {
    match &expr.kind {
        ExprKind::Name(name) => Some(Path { root: name, steps: Vec::new() }),
        ExprKind::Attr { object, name, optional: false } => {
            let mut path = path(object)?;
            path.steps.push(Step::Field(name));
            Some(path)
        }
        ExprKind::Index { object, index } => {
            let mut path = path(object)?;
            path.steps.push(Step::Index(index));
            Some(path)
        }
        _ => None,
    }
}

/// Whether two places can be one thing: they can't when they go to different fields.
fn overlap(first: &Path<'_>, second: &Path<'_>) -> bool {
    first.root == second.root
        && first.steps.iter().zip(&second.steps).all(|pair| match pair {
            (Step::Field(one), Step::Field(other)) => one == other,
            _ => true,
        })
}

/// Everything after `mut x = place` that matters to it.
struct Uses<'a> {
    name: &'a str,
    checker: &'a Checker,
    body: Option<&'a Body>,
    /// Writes through the name, and writes to the name itself.
    edits: Vec<Span>,
    replaces: Vec<Span>,
    /// Where the name is stored, returned or captured.
    escapes: Vec<Span>,
    /// Every place read or written, with where.
    mentions: Vec<(Path<'a>, Span)>,
    /// Names given new values, with where.
    written: Vec<(&'a str, Span)>,
    /// The loops the rest of the block has.
    loops: Vec<Span>,
}

fn is_name(expr: &Expr, name: &str) -> bool {
    matches!(&expr.kind, ExprKind::Name(found) if found == name)
}

impl<'a> Uses<'a> {
    fn new(name: &'a str, checker: &'a Checker, body: Option<&'a Body>) -> Self {
        Uses {
            name,
            checker,
            body,
            edits: Vec::new(),
            replaces: Vec::new(),
            escapes: Vec::new(),
            mentions: Vec::new(),
            written: Vec::new(),
            loops: Vec::new(),
        }
    }

    fn rooted(&self, expr: &Expr) -> bool {
        path(expr).is_some_and(|found| found.root == self.name)
    }

    /// Whether a call to a method changes what it's called on.
    fn method_changes(&self, object: &Expr, method: &str) -> bool {
        let Some(ty) = self.body.and_then(|body| body.expr(object.span)) else { return false };
        if let Some(found) = builtins::method(ty, method) {
            return found.changes;
        }
        let Some(owner) = ty.type_name() else { return false };
        let found = self.checker.program.method(owner, method);
        found.is_some_and(|(_, entry)| entry.def.params.first().is_some_and(|param| param.is_mut))
    }

    fn stmts(&mut self, stmts: &'a [Stmt]) {
        for (at, stmt) in stmts.iter().enumerate() {
            self.stmt(stmt);
            self.named_again(stmt, stmts.get(at + 1..).unwrap_or_default());
        }
    }

    /// `mut other = x.part`: editing `other` edits `x`, which stays in use as long as `other`.
    fn named_again(&mut self, stmt: &'a Stmt, rest: &'a [Stmt]) {
        let StmtKind::Assign { target, value, declare_mut: true, .. } = &stmt.kind else { return };
        let ExprKind::Name(other) = &target.kind else { return };
        if !self.rooted(value) {
            return;
        }
        let mut later = Uses::new(other, self.checker, self.body);
        later.stmts(rest);
        if later.edits.is_empty() {
            return;
        }
        self.edits.push(value.span);
        let root = Path { root: self.name, steps: Vec::new() };
        let spans = later.mentions.iter().filter(|(path, _)| path.root == *other).map(|(_, span)| *span);
        let through: Vec<(Path<'a>, Span)> = spans.map(|span| (root.clone(), span)).collect();
        self.mentions.extend(through);
    }

    fn stmt(&mut self, stmt: &'a Stmt) {
        match &stmt.kind {
            StmtKind::Expr(value)
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value) => {
                self.expr(value);
            }
            StmtKind::Return(Some(value)) => {
                if is_name(value, self.name) {
                    self.escapes.push(value.span);
                }
                self.expr(value);
            }
            StmtKind::Assign { target, value, .. } => self.assign(target, value),
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.stmts(body);
                }
                self.stmts(otherwise.as_deref().unwrap_or_default());
            }
            StmtKind::While { cond, body } => {
                self.loops.push(stmt.span);
                self.expr(cond);
                self.stmts(body);
            }
            StmtKind::For { is_mut, iter, body, .. } => {
                self.loops.push(stmt.span);
                if *is_mut && self.rooted(iter) {
                    self.edits.push(iter.span);
                }
                self.expr(iter);
                self.stmts(body);
            }
            StmtKind::With { value, body, .. } => {
                self.expr(value);
                self.stmts(body);
            }
            StmtKind::Def(def) => self.captured_in_stmts(&def.body),
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    fn assign(&mut self, target: &'a Expr, value: &'a Expr) {
        if let ExprKind::Tuple(names) = &target.kind {
            for name in names {
                self.assign_to(name);
            }
        } else {
            self.assign_to(target);
        }
        if is_name(value, self.name) && !matches!(target.kind, ExprKind::Name(_) | ExprKind::Tuple(_)) {
            self.escapes.push(value.span);
        }
        self.expr(value);
    }

    fn assign_to(&mut self, target: &'a Expr) {
        if let ExprKind::Name(name) = &target.kind {
            self.written.push((name, target.span));
            if name == self.name {
                self.replaces.push(target.span);
            }
        } else {
            if self.rooted(target) {
                self.edits.push(target.span);
            }
            self.expr(target);
        }
    }

    fn expr(&mut self, expr: &'a Expr) {
        if let Some(found) = path(expr) {
            for step in &found.steps {
                if let Step::Index(index) = step {
                    self.expr(index);
                }
            }
            self.mentions.push((found, expr.span));
            return;
        }
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                self.call(callee, args.iter().map(|arg| (arg.is_mut, &arg.value)));
            }
            ExprKind::Lambda { body, .. } => self.captured(body),
            ExprKind::Match { subject, arms } => {
                self.expr(subject);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.stmts(&arm.body);
                }
            }
            _ => {
                for child in crate::effects::children(expr) {
                    self.expr(child);
                }
            }
        }
    }

    fn call(&mut self, callee: &'a Expr, args: impl Iterator<Item = (bool, &'a Expr)>) {
        let changes = match &callee.kind {
            ExprKind::Attr { object, name, optional: false } => {
                let changes = self.method_changes(object, name);
                if changes && self.rooted(object) {
                    self.edits.push(object.span);
                }
                changes
            }
            _ => false,
        };
        self.expr(callee);
        for (is_mut, value) in args {
            if is_mut && self.rooted(value) {
                self.edits.push(value.span);
            }
            // `items.append(x)` keeps what it's given.
            if changes && is_name(value, self.name) {
                self.escapes.push(value.span);
            }
            self.expr(value);
        }
    }

    /// An arrow function or nested function: naming the name in one captures it.
    fn captured(&mut self, body: &'a Expr) {
        let before = self.mentions.len();
        self.expr(body);
        self.note_captures(before);
    }

    fn captured_in_stmts(&mut self, body: &'a [Stmt]) {
        let before = self.mentions.len();
        self.stmts(body);
        self.note_captures(before);
    }

    fn note_captures(&mut self, since: usize) {
        let inside = self.mentions.get(since..).unwrap_or_default();
        let captures: Vec<Span> =
            inside.iter().filter(|(path, _)| path.root == self.name).map(|(_, span)| *span).collect();
        self.escapes.extend(captures);
    }
}

/// What a function's names for places come to.
struct Check<'a> {
    checker: &'a Checker,
    body: Option<&'a Body>,
    /// The names that can change, wherever they're declared.
    changeable: HashSet<&'a str>,
    /// The function's last statement, whose value it gives back.
    last: Option<&'a Stmt>,
    problems: Vec<Diagnostic>,
    /// The statements that name a place.
    places: Vec<Span>,
}

/// Works out one function's names for places, giving back what's wrong and which statements
/// name a place.
pub(crate) fn check_function(
    checker: &Checker,
    def: &FnDef,
    body: Option<&Body>,
) -> (Vec<Diagnostic>, Vec<Span>) {
    let mut changeable: HashSet<&str> =
        def.params.iter().filter(|param| param.is_mut).map(|param| param.name.as_str()).collect();
    collect_changeable(&def.body, &mut changeable);
    let mut check =
        Check { checker, body, changeable, last: def.body.last(), problems: Vec::new(), places: Vec::new() };
    check.block(&def.body);
    (check.problems, check.places)
}

fn collect_changeable<'a>(stmts: &'a [Stmt], names: &mut HashSet<&'a str>) {
    for_each_block(stmts, &mut |block| {
        for stmt in block {
            match &stmt.kind {
                StmtKind::Assign { target, declare_mut: true, .. } => {
                    if let ExprKind::Name(name) = &target.kind {
                        names.insert(name);
                    }
                }
                StmtKind::For { names: held, is_mut: true, .. } => {
                    names.extend(held.iter().map(String::as_str));
                }
                StmtKind::With { name: Some(name), is_mut: true, .. } => {
                    names.insert(name);
                }
                _ => {}
            }
        }
    });
}

/// Calls `each` on a block and every block inside it, but not a nested function's.
fn for_each_block<'a>(stmts: &'a [Stmt], each: &mut dyn FnMut(&'a [Stmt])) {
    each(stmts);
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    arms_in(cond, each);
                    for_each_block(body, each);
                }
                if let Some(otherwise) = otherwise {
                    for_each_block(otherwise, each);
                }
            }
            StmtKind::While { cond: value, body }
            | StmtKind::For { iter: value, body, .. }
            | StmtKind::With { value, body, .. } => {
                arms_in(value, each);
                for_each_block(body, each);
            }
            StmtKind::Expr(value)
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value)
            | StmtKind::Return(Some(value))
            | StmtKind::Assign { value, .. } => arms_in(value, each),
            StmtKind::Return(None)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass
            | StmtKind::Def(_) => {}
        }
    }
}

/// Calls `each` on the blocks of every `match` in an expression.
fn arms_in<'a>(expr: &'a Expr, each: &mut dyn FnMut(&'a [Stmt])) {
    if let ExprKind::Match { arms, .. } = &expr.kind {
        for arm in arms {
            for_each_block(&arm.body, each);
        }
    }
    for child in crate::effects::children(expr) {
        arms_in(child, each);
    }
}

impl<'a> Check<'a> {
    fn block(&mut self, stmts: &'a [Stmt]) {
        let mut blocks = Vec::new();
        for_each_block(stmts, &mut |block| blocks.push(block));
        for block in blocks {
            for (at, stmt) in block.iter().enumerate() {
                let rest = block.get(at + 1..).unwrap_or_default();
                if let StmtKind::Assign { target, op: None, value, declare_mut: true, .. } = &stmt.kind
                    && let ExprKind::Name(name) = &target.kind
                {
                    self.named(name, value, rest, stmt.span);
                }
            }
        }
    }

    /// Whether a name is a value, rather than a type, a module or a constant.
    fn is_local(&self, name: &str) -> bool {
        let program = &self.checker.program;
        let package = format!("{name}.");
        !program.is_type_name(name)
            && !program.constants.contains_key(name)
            && !program.modules.keys().any(|module| module == name || module.starts_with(&package))
    }

    /// `mut name = value`, and what comes after it in its block.
    fn named(&mut self, name: &'a str, value: &'a Expr, rest: &'a [Stmt], at: Span) {
        let Some(place) = path(value) else { return };
        if !self.is_local(place.root) {
            return;
        }
        let mut uses = Uses::new(name, self.checker, self.body);
        uses.stmts(rest);
        if let Some(StmtKind::Expr(last)) = self.last.map(|last| &last.kind)
            && is_name(last, name)
        {
            uses.escapes.push(last.span);
        }
        // A name that's only replaced is a value of its own, and one nothing changes needs
        // nothing from the place.
        let Some(edit) = uses.edits.first().copied() else { return };
        // `copy(...)` changes what the program does, so it's only suggested, never a fix that
        // `fennec fix` would make.
        if let Some(replaced) = uses.replaces.first() {
            let message = format!("`{name}` edits a place and is given a new value too");
            self.problems.push(
                Diagnostic::new(message, *replaced).with_help(
                    "use one name for each, or start it with `copy(...)` to edit a value of its own",
                ),
            );
            return;
        }
        if place.steps.is_empty() {
            self.named_for_a_name(&uses, &place);
            return;
        }
        if !self.changeable.contains(place.root) {
            let message = format!("`{name}` edits `{}`, which isn't mut", place.root);
            self.problems.push(Diagnostic::new(message, edit).with_help(format!(
                "write `mut {} = ...` where it's declared, or `mut {name} = copy(...)` to edit a value of its own",
                place.root
            )));
            return;
        }
        if let Some(escape) = uses.escapes.first() {
            let message = format!("`{name}` names a place, so it can't be kept or given back");
            self.problems.push(
                Diagnostic::new(message, *escape)
                    .with_help("start it with `copy(...)` to keep a value of its own"),
            );
            return;
        }
        if self.index_problem(&uses, &place, name, at) {
            return;
        }
        if self.locked(&uses, &place, name, at) {
            return;
        }
        self.places.push(at);
    }

    /// `mut name = other`: reading `other` after that would see a value that may have changed.
    fn named_for_a_name(&mut self, uses: &Uses<'a>, place: &Path<'a>) {
        let read = uses.mentions.iter().find(|(path, _)| path.root == place.root);
        if let Some((_, read)) = read {
            let message = format!("`{}` is read after `{}` changes it", place.root, uses.name);
            let fix =
                format!("write `mut {} = copy({})` to change a value of its own", uses.name, place.root);
            self.problems.push(Diagnostic::new(message, *read).with_help(fix));
        }
    }

    /// Whether an index of the place can change while the name is in use.
    fn index_problem(&mut self, uses: &Uses<'a>, place: &Path<'a>, name: &str, at: Span) -> bool {
        for step in &place.steps {
            let Step::Index(index) = step else { continue };
            let plain = matches!(index.kind, ExprKind::Int(_) | ExprKind::Str(_) | ExprKind::Name(_));
            if !plain {
                let message = format!("`{name}` names a place by an index that has to be worked out");
                self.problems.push(
                    Diagnostic::new(message, index.span)
                        .with_help("put the index in a name first, like `at = ...`"),
                );
                return true;
            }
            let ExprKind::Name(held) = &index.kind else { continue };
            let end = last_use(uses, at);
            if let Some((_, written)) =
                uses.written.iter().find(|(written, span)| written == held && span.start < end)
            {
                let message = format!("`{held}` changes while `{name}` names the place it picks");
                self.problems.push(
                    Diagnostic::new(message, *written)
                        .with_help("use the name up first, or hold the index in another name"),
                );
                return true;
            }
        }
        false
    }

    /// Whether what the name came from is used while the name is in use.
    fn locked(&mut self, uses: &Uses<'a>, place: &Path<'a>, name: &str, at: Span) -> bool {
        let end = last_use(uses, at);
        let found = uses
            .mentions
            .iter()
            .find(|(path, span)| span.start < end && path.root != name && overlap(path, place));
        let Some((path, span)) = found else { return false };
        let message = format!("`{}` can't be used here, while `{name}` names part of it", path.root);
        self.problems.push(
            Diagnostic::new(message, *span)
                .with_help(format!("use `{name}` here, or finish with it before this")),
        );
        true
    }
}

/// Where a name's last use ends. Inside a loop that started after the name, that's the end of
/// the loop, since the loop comes round to it again.
fn last_use(uses: &Uses<'_>, at: Span) -> u32 {
    let mut end = uses
        .mentions
        .iter()
        .filter(|(path, _)| path.root == uses.name)
        .map(|(_, span)| span.end)
        .max()
        .unwrap_or(at.end);
    for looped in &uses.loops {
        if looped.start < end && end <= looped.end {
            end = looped.end;
        }
    }
    end
}

/// Writes a function's names for places into it: each use of the name becomes the place.
pub(crate) fn write_in(body: &mut [Stmt], places: &BTreeSet<(u32, u32)>) {
    for at in 0..body.len() {
        let Some(stmt) = body.get_mut(at) else { continue };
        if places.contains(&(stmt.span.start, stmt.span.end))
            && let StmtKind::Assign { target, value, .. } = &stmt.kind
            && let ExprKind::Name(name) = &target.kind
        {
            let (name, place) = (name.clone(), value.clone());
            stmt.kind = StmtKind::Pass;
            let rest = body.get_mut(at + 1..).unwrap_or_default();
            fennec_syntax::visit::in_stmts(rest, &mut |seen| {
                if matches!(&seen.kind, ExprKind::Name(found) if *found == name) {
                    *seen = place.clone();
                }
            });
        }
    }
    for stmt in body.iter_mut() {
        match &mut stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches {
                    write_in(block, places);
                }
                if let Some(block) = otherwise {
                    write_in(block, places);
                }
            }
            StmtKind::While { body: block, .. }
            | StmtKind::For { body: block, .. }
            | StmtKind::With { body: block, .. } => write_in(block, places),
            _ => {}
        }
    }
    fennec_syntax::visit::in_stmts(body, &mut |seen| {
        if let ExprKind::Match { arms, .. } = &mut seen.kind {
            for arm in arms {
                write_in(&mut arm.body, places);
            }
        }
    });
}
