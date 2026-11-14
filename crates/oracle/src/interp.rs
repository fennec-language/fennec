//! The evaluator: walks the syntax tree, one statement and one expression at a time.
//!
//! Each call has a frame of its own names. A name holds its own value, or, for a `ref` parameter,
//! a `for ref` loop's name or a local `ref`, a place: a name in an older frame and the fields and
//! positions under it. Reading a place copies what's there, and writing one writes there.

mod builtins;
mod calls;
mod expr;
mod library;
mod methods;
mod show;
mod sources;
pub(crate) mod stream;
mod tables;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fennec_syntax::{BinOp, Expr, ExprKind, Span, Stmt, StmtKind};

use crate::load::Program;
use crate::value::{Cap, Value};

/// How many expressions and statements a run may take, across every thread, before it's stopped
/// as one the oracle can't finish.
pub(crate) const MOST_STEPS: u64 = 40_000_000;

/// How deep calls may nest.
const MOST_DEPTH: usize = 1_500;

/// How deep a value may be nested: deeper ones are refused before copying one could run out of
/// stack.
const MOST_NESTING: usize = 5_000;

/// The most bytes a program may print.
const MOST_OUTPUT: usize = 64 << 20;

/// The most items a list, string or other collection may hold.
pub(crate) const MOST_ITEMS: usize = 20_000_000;

/// How a statement or expression ended, other than with a value.
#[derive(Debug)]
pub(crate) enum Flow {
    Return(Value),
    Break,
    Continue,
    /// A failure passing up, and where it was made.
    Fail(Value, Span),
    /// A bug that stops the program, with what the program says about it.
    Crash(String, Span),
    /// Something the oracle doesn't model.
    Unsupported(String),
    /// The generator this runs in was let go of before it finished.
    Abandoned,
}

pub(crate) type Run<T> = Result<T, Flow>;

pub(crate) fn unsupported<T>(what: impl Into<String>) -> Run<T> {
    Err(Flow::Unsupported(what.into()))
}

pub(crate) fn crash<T>(message: impl Into<String>, span: Span) -> Run<T> {
    Err(Flow::Crash(message.into(), span))
}

/// What every thread of one run shares.
pub(crate) struct Shared {
    pub(crate) program: &'static Program,
    /// How many steps the run may take.
    most_steps: u64,
    pub(crate) out: Mutex<Vec<u8>>,
    steps: AtomicU64,
    /// Generator threads still running.
    pub(crate) threads: AtomicUsize,
    /// Something a generator's thread found it couldn't model, after nobody was asking it for
    /// anything.
    pub(crate) refused: Mutex<Option<String>>,
    /// The tag the next `Table` made gets.
    next_tag: AtomicU64,
}

impl Shared {
    pub(crate) fn new(program: &'static Program, most_steps: u64) -> Shared {
        Shared {
            program,
            most_steps,
            out: Mutex::new(Vec::new()),
            steps: AtomicU64::new(0),
            threads: AtomicUsize::new(0),
            refused: Mutex::new(None),
            next_tag: AtomicU64::new(1),
        }
    }

    /// A tag no `Table` of this run has had.
    pub(crate) fn new_tag(&self) -> u64 {
        self.next_tag.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn refuse(&self, why: String) {
        if let Ok(mut refused) = self.refused.lock() {
            refused.get_or_insert(why);
        }
    }
}

/// A place a value is in: a name in a frame, then fields and positions under it.
#[derive(Clone, Debug)]
pub(crate) struct Place {
    pub(crate) frame: usize,
    pub(crate) name: String,
    pub(crate) path: Vec<Step>,
}

#[derive(Clone, Debug)]
pub(crate) enum Step {
    Field(String),
    /// A position in a list or tuple, or a key in a dict.
    At(Value),
}

impl Place {
    pub(crate) fn then(&self, step: Step) -> Place {
        let mut path = self.path.clone();
        path.push(step);
        Place { frame: self.frame, name: self.name.clone(), path }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Binding {
    Value(Value),
    Ref(Place),
}

/// One call's names.
pub(crate) struct Frame {
    pub(crate) id: u64,
    pub(crate) vars: Vec<(String, Binding)>,
    /// The frame of the call a nested function was written in, whose names it reads.
    pub(crate) parent: Option<u64>,
    /// The type whose method this call runs, which `Self` names.
    pub(crate) owner: Option<String>,
}

pub(crate) struct Interp {
    pub(crate) shared: Arc<Shared>,
    pub(crate) program: &'static Program,
    pub(crate) frames: Vec<Frame>,
    /// The frame names are read and written in: the newest, except while a `with` block runs
    /// inside its source.
    pub(crate) cur: usize,
    next_id: u64,
    pub(crate) lends: Vec<sources::Lending>,
    pub(crate) yields: Option<stream::YieldSide>,
    depth: usize,
    constants: HashMap<String, Value>,
    generators: HashMap<usize, bool>,
}

/// How `main` ended.
pub(crate) enum MainEnded {
    Returned(Value),
    Failed(Value, Span),
    Crashed(String, Span),
    Unsupported(String),
}

impl Interp {
    pub(crate) fn new(shared: Arc<Shared>, yields: Option<stream::YieldSide>) -> Interp {
        let program = shared.program;
        Interp {
            shared,
            program,
            frames: Vec::new(),
            cur: 0,
            next_id: 1,
            lends: Vec::new(),
            yields,
            depth: 0,
            constants: HashMap::new(),
            generators: HashMap::new(),
        }
    }

    /// Runs `main`, with standard output and empty arguments and input.
    pub(crate) fn run_main(&mut self) -> MainEnded {
        let main = self.program.main;
        let mut given = Vec::new();
        for param in &main.params {
            let cap = match param.ty.as_ref().map(|ty| ty.name.as_str()) {
                Some("Stdout") => Cap::Stdout,
                Some("Args") => Cap::Args,
                Some("Stdin") => Cap::Stdin,
                _ => return MainEnded::Unsupported("a capability main takes".to_owned()),
            };
            given.push(calls::ArgVal::Value(None, Value::Cap(cap)));
        }
        match self.call_def(main, None, None, given, main.span) {
            Ok(value) => MainEnded::Returned(value),
            Err(Flow::Fail(error, span)) => MainEnded::Failed(error, span),
            Err(Flow::Crash(message, span)) => MainEnded::Crashed(message, span),
            Err(Flow::Unsupported(why)) => MainEnded::Unsupported(why),
            Err(Flow::Abandoned) => MainEnded::Unsupported("a generator was let go of".to_owned()),
            Err(Flow::Return(_) | Flow::Break | Flow::Continue) => {
                MainEnded::Unsupported("a `break`, `continue` or `return` left its place".to_owned())
            }
        }
    }

    /// Counts a step, stopping a run that's gone on too long.
    pub(crate) fn tick(&self) -> Run<()> {
        if self.shared.steps.fetch_add(1, Ordering::Relaxed) > self.shared.most_steps {
            return unsupported("ran too long");
        }
        Ok(())
    }

    /// Counts a copy of a value against the run's steps, item by item, and refuses one nested
    /// too deeply to copy and let go of safely.
    pub(crate) fn charge(&self, value: &Value) -> Run<()> {
        let mut count: u64 = 0;
        let mut waiting: Vec<(&Value, usize)> = vec![(value, 1)];
        while let Some((held, depth)) = waiting.pop() {
            count += 1;
            if depth > MOST_NESTING {
                return unsupported("ran too long: a value nested too deeply");
            }
            match held {
                Value::List(items) | Value::Tuple(items) | Value::Set(items) => {
                    waiting.extend(items.iter().map(|item| (item, depth + 1)));
                }
                Value::Dict(pairs) => {
                    for (key, value) in pairs {
                        waiting.push((key, depth + 1));
                        waiting.push((value, depth + 1));
                    }
                }
                Value::Instance(instance) => {
                    waiting.extend(instance.fields.iter().map(|(_, field)| (field, depth + 1)));
                }
                Value::Table(table) => waiting.extend(table.live().map(|(_, item)| (item, depth + 1))),
                _ => {}
            }
        }
        if count > 1 && self.shared.steps.fetch_add(count, Ordering::Relaxed) > self.shared.most_steps {
            return unsupported("ran too long");
        }
        Ok(())
    }

    /// Writes text to standard output.
    pub(crate) fn print(&self, text: &str) -> Run<()> {
        let Ok(mut out) = self.shared.out.lock() else { return unsupported("standard output was lost") };
        if out.len() + text.len() > MOST_OUTPUT {
            return unsupported("ran too long: it printed too much");
        }
        out.extend_from_slice(text.as_bytes());
        Ok(())
    }

    // Frames.

    pub(crate) fn push_frame(&mut self, vars: Vec<(String, Binding)>, parent: Option<u64>) -> Run<usize> {
        self.depth += 1;
        if self.depth > MOST_DEPTH {
            self.depth -= 1;
            return unsupported("ran too long: calls nested too deeply");
        }
        let id = self.next_id;
        self.next_id += 1;
        let owner = parent
            .and_then(|parent| self.frames.iter().rfind(|frame| frame.id == parent))
            .and_then(|frame| frame.owner.clone());
        self.frames.push(Frame { id, vars, parent, owner });
        let previous = self.cur;
        self.cur = self.frames.len() - 1;
        Ok(previous)
    }

    pub(crate) fn pop_frame(&mut self, previous: usize) {
        self.frames.pop();
        self.depth = self.depth.saturating_sub(1);
        self.cur = previous;
    }

    fn frame(&self) -> Run<&Frame> {
        self.frames.get(self.cur).map_or_else(|| unsupported("no frame"), Ok)
    }

    fn frame_mut(&mut self) -> Run<&mut Frame> {
        let at = self.cur;
        self.frames.get_mut(at).map_or_else(|| unsupported("no frame"), Ok)
    }

    pub(crate) fn frame_id(&self) -> u64 {
        self.frames.get(self.cur).map_or(0, |frame| frame.id)
    }

    /// The type `Self` names here.
    pub(crate) fn self_type(&self) -> Option<String> {
        self.frames.get(self.cur).and_then(|frame| frame.owner.clone())
    }

    /// The type a name stands for: itself, or the type `Self` names.
    pub(crate) fn type_named<'n>(&self, name: &'n str) -> std::borrow::Cow<'n, str> {
        match (name, self.self_type()) {
            ("Self", Some(owner)) => std::borrow::Cow::Owned(owner),
            _ => std::borrow::Cow::Borrowed(name),
        }
    }

    /// Where a name is bound: in this frame, or in the frames a nested function reads.
    fn binding_of(&self, name: &str) -> Option<(usize, &Binding)> {
        let mut at = self.cur;
        loop {
            let frame = self.frames.get(at)?;
            if let Some((_, binding)) = frame.vars.iter().rev().find(|(bound, _)| bound == name) {
                return Some((at, binding));
            }
            let parent = frame.parent?;
            at = self.frames.iter().rposition(|frame| frame.id == parent)?;
        }
    }

    /// Whether a name is bound to a value or a place here.
    pub(crate) fn is_bound(&self, name: &str) -> bool {
        self.binding_of(name).is_some()
    }

    /// The value a name holds, if it's bound.
    pub(crate) fn lookup(&self, name: &str, span: Span) -> Run<Option<Value>> {
        match self.binding_of(name) {
            Some((_, Binding::Value(value))) => {
                self.charge(value)?;
                Ok(Some(value.clone()))
            }
            Some((_, Binding::Ref(place))) => {
                let place = place.clone();
                self.read_place(&place, span).map(Some)
            }
            None => Ok(None),
        }
    }

    /// Gives a new name a value, hiding any it had.
    pub(crate) fn bind(&mut self, name: &str, value: Value) -> Run<()> {
        if name != "_" {
            self.frame_mut()?.vars.push((name.to_owned(), Binding::Value(value)));
        }
        Ok(())
    }

    pub(crate) fn bind_ref(&mut self, name: &str, place: Place) -> Run<()> {
        if name != "_" {
            self.frame_mut()?.vars.push((name.to_owned(), Binding::Ref(place)));
        }
        Ok(())
    }

    /// Sets a name: a value it holds is replaced, a place it stands for is written, and a name
    /// it doesn't have yet is made.
    pub(crate) fn set_name(&mut self, name: &str, value: Value, span: Span) -> Run<()> {
        if name == "_" {
            return Ok(());
        }
        let frame = self.frame_mut()?;
        match frame.vars.iter_mut().rev().find(|(bound, _)| bound == name) {
            Some((_, Binding::Value(held))) => {
                *held = value;
                Ok(())
            }
            Some((_, Binding::Ref(place))) => {
                let place = place.clone();
                self.write_place(&place, value, span)
            }
            None => self.bind(name, value),
        }
    }

    // Places.

    /// The place an expression names, working out its positions now, or `None` for one that's
    /// not a place.
    pub(crate) fn place_of(&mut self, expr: &'static Expr) -> Run<Option<Place>> {
        match &expr.kind {
            ExprKind::Name(name) => Ok(match self.binding_of(name) {
                Some((_, Binding::Ref(place))) => Some(place.clone()),
                Some((frame, Binding::Value(_))) => {
                    Some(Place { frame, name: name.clone(), path: Vec::new() })
                }
                None => None,
            }),
            ExprKind::Attr { object, name, optional: false } => {
                Ok(self.place_of(object)?.map(|place| place.then(Step::Field(name.clone()))))
            }
            ExprKind::Index { object, index } => {
                let Some(place) = self.place_of(object)? else { return Ok(None) };
                let at = self.eval(index)?;
                Ok(Some(place.then(Step::At(at))))
            }
            _ => Ok(None),
        }
    }

    /// The value at the root of a place.
    fn root(&self, place: &Place) -> Run<&Value> {
        let frame =
            self.frames.get(place.frame).map_or_else(|| unsupported("a place outlived its frame"), Ok)?;
        match frame.vars.iter().rev().find(|(bound, _)| *bound == place.name) {
            Some((_, Binding::Value(value))) => Ok(value),
            _ => unsupported("a place whose name is gone"),
        }
    }

    fn root_mut(&mut self, place: &Place) -> Run<&mut Value> {
        let frame =
            self.frames.get_mut(place.frame).map_or_else(|| unsupported("a place outlived its frame"), Ok)?;
        match frame.vars.iter_mut().rev().find(|(bound, _)| *bound == place.name) {
            Some((_, Binding::Value(value))) => Ok(value),
            _ => unsupported("a place whose name is gone"),
        }
    }

    pub(crate) fn read_place(&self, place: &Place, span: Span) -> Run<Value> {
        let mut value = self.root(place)?;
        let mut char_at: Option<Value> = None;
        for step in &place.path {
            if char_at.is_some() {
                return unsupported("a place inside a character");
            }
            match step_into(value, step, span)? {
                Reached::Inside(inner) => value = inner,
                Reached::Made(made) => char_at = Some(made),
            }
        }
        if char_at.is_none() {
            self.charge(value)?;
        }
        Ok(char_at.unwrap_or_else(|| value.clone()))
    }

    /// Runs `change` on the value at a place. With `adding`, a key the last step names that a
    /// dict doesn't have yet is added, holding nothing until `change` gives it a value.
    pub(crate) fn change_place<R>(
        &mut self,
        place: &Place,
        span: Span,
        adding: bool,
        change: impl FnOnce(&mut Value) -> Run<R>,
    ) -> Run<R> {
        let last = place.path.len().saturating_sub(1);
        let mut value = self.root_mut(place)?;
        for (at, step) in place.path.iter().enumerate() {
            value = step_into_mut(value, step, span, adding && at == last)?;
        }
        let changed = change(value)?;
        self.rules_along(place, span)?;
        Ok(changed)
    }

    pub(crate) fn write_place(&mut self, place: &Place, new: Value, span: Span) -> Run<()> {
        self.change_place(place, span, true, |value| {
            *value = new;
            Ok(())
        })
    }

    /// Checks the `where` rules of every struct along a place just written.
    fn rules_along(&mut self, place: &Place, span: Span) -> Run<()> {
        if !self.program.has_rules {
            return Ok(());
        }
        let mut ruled = Vec::new();
        let mut value = self.root(place)?;
        for step in std::iter::once(None).chain(place.path.iter().map(Some)) {
            if let Some(step) = step {
                match step_into(value, step, span)? {
                    Reached::Inside(inner) => value = inner,
                    Reached::Made(_) => break,
                }
            }
            if let Value::Instance(instance) = value
                && self.program.types.get(instance.ty.as_str()).is_some_and(|declared| declared.ruled)
            {
                ruled.push((**instance).clone());
            }
        }
        for instance in ruled {
            if let Some(broken) = self.broken_rule(&instance)? {
                return crash(format!("this breaks the `where` rule on `{}.{broken}`", instance.ty), span);
            }
        }
        Ok(())
    }

    /// Looks at the value at a place without copying it.
    pub(crate) fn peek_place<R>(&self, place: &Place, span: Span, look: impl FnOnce(&Value) -> R) -> Run<R> {
        let mut value = self.root(place)?;
        for step in &place.path {
            match step_into(value, step, span)? {
                Reached::Inside(inner) => value = inner,
                Reached::Made(made) => return Ok(look(&made)),
            }
        }
        Ok(look(value))
    }

    // Statements.

    /// Where the current frame's names end, so a block's own names can go when it ends.
    pub(crate) fn mark(&self) -> (usize, usize) {
        (self.cur, self.frames.get(self.cur).map_or(0, |frame| frame.vars.len()))
    }

    /// Lets go of the names made since `mark`: a block's names last until it ends.
    pub(crate) fn restore(&mut self, (frame, len): (usize, usize)) {
        if let Some(frame) = self.frames.get_mut(frame) {
            frame.vars.truncate(len);
        }
    }

    /// Runs a block, whose new names last until it ends.
    pub(crate) fn block(&mut self, body: &'static [Stmt]) -> Run<()> {
        let mark = self.mark();
        let ran = self.stmts(body);
        self.restore(mark);
        ran
    }

    fn stmts(&mut self, body: &'static [Stmt]) -> Run<()> {
        for stmt in body {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    /// Runs a block whose last line is its value, as a `match` case's or an `except` block's are.
    pub(crate) fn block_value(&mut self, body: &'static [Stmt]) -> Run<Value> {
        let mark = self.mark();
        let value = self.stmts_value(body);
        self.restore(mark);
        value
    }

    fn stmts_value(&mut self, body: &'static [Stmt]) -> Run<Value> {
        let Some((last, rest)) = body.split_last() else { return Ok(Value::Unit) };
        self.stmts(rest)?;
        match &last.kind {
            StmtKind::Expr(expr) => self.eval(expr),
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.truth(cond)? {
                        return self.block_value(body);
                    }
                }
                match otherwise {
                    Some(body) => self.block_value(body),
                    None => Ok(Value::Unit),
                }
            }
            _ => {
                self.stmt(last)?;
                Ok(Value::Unit)
            }
        }
    }

    pub(crate) fn truth(&mut self, cond: &'static Expr) -> Run<bool> {
        match self.eval(cond)? {
            Value::Bool(truth) => Ok(truth),
            other => unsupported(format!("a {} used as a condition", other.kind())),
        }
    }

    fn stmt(&mut self, stmt: &'static Stmt) -> Run<()> {
        self.tick()?;
        match &stmt.kind {
            StmtKind::Expr(expr) => self.eval(expr).map(drop),
            StmtKind::Assign { target, op, value, .. } => {
                if let Some((name, place)) = stmt.kind.local_ref() {
                    let Some(place) = self.place_of(place)? else {
                        return unsupported("a local `ref` to something that isn't a place");
                    };
                    return self.bind_ref(name, place);
                }
                self.assign(target, *op, value, stmt.span)
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.truth(cond)? {
                        return self.block(body);
                    }
                }
                match otherwise {
                    Some(body) => self.block(body),
                    None => Ok(()),
                }
            }
            StmtKind::While { cond, body } => {
                while self.truth(cond)? {
                    match self.block(body) {
                        Ok(()) | Err(Flow::Continue) => {}
                        Err(Flow::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            StmtKind::For { names, is_mut, iter, body } => {
                if *is_mut {
                    self.for_ref(names, iter, body, stmt.span)
                } else {
                    self.for_each(names, iter, body, stmt.span)
                }
            }
            StmtKind::Return(value) => {
                let value = match value {
                    Some(value) => self.eval(value)?,
                    None => Value::Unit,
                };
                Err(Flow::Return(value))
            }
            StmtKind::Fail(error) => {
                let error = self.eval(error)?;
                Err(Flow::Fail(error, stmt.span))
            }
            StmtKind::Yield(value) => {
                let value = self.eval(value)?;
                self.yield_value(value)
            }
            StmtKind::Assert(cond) => {
                if cond.asserted_failure().is_some() {
                    return unsupported("`assert ... fails`");
                }
                if self.truth(cond)? { Ok(()) } else { crash("this assertion failed", stmt.span) }
            }
            StmtKind::With { value, names, body, lock } => {
                if *lock {
                    return unsupported("`lock`");
                }
                self.with(value, names, body)
            }
            StmtKind::Def(def) => {
                let frame = self.frame_id();
                let func = crate::value::Func::Nested { def, frame };
                self.bind(&def.name, Value::Func(Box::new(func)))
            }
            StmtKind::Lend { value, otherwise, .. } => self.lend(value, otherwise, stmt.span),
            StmtKind::OnFailure { .. } => unsupported("a written-in failure block"),
            StmtKind::Break => Err(Flow::Break),
            StmtKind::Continue => Err(Flow::Continue),
            StmtKind::Pass => Ok(()),
        }
    }

    fn assign(
        &mut self,
        target: &'static Expr,
        op: Option<BinOp>,
        value: &'static Expr,
        span: Span,
    ) -> Run<()> {
        match (&target.kind, op) {
            (ExprKind::Name(name), None) => {
                let value = self.eval(value)?;
                self.set_name(name, value, span)
            }
            (ExprKind::Tuple(targets), None) => {
                let value = self.eval(value)?;
                self.destructure(targets, value, span)
            }
            (ExprKind::Index { object, index }, _) => self.assign_indexed(object, index, op, value, span),
            _ => {
                let Some(place) = self.place_of(target)? else {
                    return unsupported("assigning to something that isn't a place");
                };
                let new = match op {
                    Some(op) => {
                        let old = self.read_place(&place, target.span)?;
                        let right = self.eval(value)?;
                        self.binary_values(op, old, right, target.span)?
                    }
                    None => self.eval(value)?,
                };
                self.write_place(&place, new, span)
            }
        }
    }

    /// `object[index] = value` or `object[index] op= value`: written where it is, or through a
    /// type's own `def []=`, reading through its `def []` first for `op=`.
    fn assign_indexed(
        &mut self,
        object: &'static Expr,
        index: &'static Expr,
        op: Option<BinOp>,
        value: &'static Expr,
        span: Span,
    ) -> Run<()> {
        let Some(place) = self.place_of(object)? else {
            return unsupported("assigning into something that isn't a place");
        };
        let own = self.peek_place(&place, span, |held| match held {
            Value::Instance(instance) => Some(instance.ty.clone()),
            _ => None,
        })?;
        let key = self.eval(index)?;
        let Some(ty) = own else {
            let place = place.then(Step::At(key));
            let new = match op {
                Some(op) => {
                    let old = self.read_place(&place, span)?;
                    let right = self.eval(value)?;
                    self.binary_values(op, old, right, span)?
                }
                None => self.eval(value)?,
            };
            return self.write_place(&place, new, span);
        };
        let Some(setter) = self.program.method(&ty, "[]=") else {
            return unsupported(format!("assigning into a {ty}"));
        };
        let new = match op {
            Some(op) => {
                let held = self.read_place(&place, span)?;
                let old = self.index_value(held, key.clone(), span)?;
                let right = self.eval(value)?;
                self.binary_values(op, old, right, span)?
            }
            None => self.eval(value)?,
        };
        let given = vec![calls::ArgVal::Value(None, key), calls::ArgVal::Value(None, new)];
        self.call_def(setter, None, Some(calls::Receiver::Place(place)), given, span).map(drop)
    }

    /// `a, b = pair`, a name or `_` for each part, and parts that are tuples again.
    fn destructure(&mut self, targets: &'static [Expr], value: Value, span: Span) -> Run<()> {
        let parts = match value {
            Value::Tuple(parts) | Value::List(parts) => parts,
            other => return unsupported(format!("taking a {} apart", other.kind())),
        };
        if parts.len() != targets.len() {
            return unsupported("taking apart a tuple of another size");
        }
        for (target, part) in targets.iter().zip(parts) {
            match &target.kind {
                ExprKind::Name(name) => self.set_name(name, part, span)?,
                ExprKind::Tuple(inner) => self.destructure(inner, part, span)?,
                _ => return unsupported("taking a tuple apart into something that isn't a name"),
            }
        }
        Ok(())
    }

    fn for_each(
        &mut self,
        names: &'static [String],
        iter: &'static Expr,
        body: &'static [Stmt],
        span: Span,
    ) -> Run<()> {
        let over = self.eval(iter)?;
        let mut items = self.items_of(over, iter.span)?;
        let _ = span;
        while let Some(item) = self.next_item(&mut items)? {
            let mark = self.mark();
            let ran = self.bind_fresh(names, item).and_then(|()| self.stmts(body));
            self.restore(mark);
            match ran {
                Ok(()) | Err(Flow::Continue) => {}
                Err(Flow::Break) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// `for ref item in items:`, `for at, ref item in enumerate(items):` and
    /// `for key, ref value in items.items():`, each name standing for the item where it is.
    fn for_ref(
        &mut self,
        names: &'static [String],
        iter: &'static Expr,
        body: &'static [Stmt],
        span: Span,
    ) -> Run<()> {
        let (place, numbered, keyed) = match (&iter.kind, names.len()) {
            (_, 1) => (self.place_of(iter)?, false, false),
            (ExprKind::Call { callee, args }, 2) => match (&callee.kind, args.as_slice()) {
                (ExprKind::Name(name), [arg]) if name == "enumerate" && !self.is_bound(name) => {
                    (self.place_of(&arg.value)?, true, false)
                }
                (ExprKind::Attr { object, name, .. }, []) if name == "items" => {
                    (self.place_of(object)?, false, true)
                }
                _ => (None, false, false),
            },
            _ => (None, false, false),
        };
        let Some(place) = place else { return unsupported("`for ref` over something that isn't a place") };
        let positions: Vec<Value> = match self.read_place(&place, iter.span)? {
            Value::List(items) if !keyed => (0..items.len()).map(|at| Value::Int(as_int(at))).collect(),
            Value::Dict(pairs) if keyed => pairs.into_iter().map(|(key, _)| key).collect(),
            // A `Table`'s items are reached by their `Id`s, in slot order.
            Value::Table(table) if !keyed => table.live().map(|(id, _)| Value::Id(id)).collect(),
            other => return unsupported(format!("`for ref` over a {}", other.kind())),
        };
        let item_name = names.last().map_or("_", String::as_str);
        let _ = span;
        for (count, position) in positions.into_iter().enumerate() {
            let mark = self.mark();
            if numbered {
                let first = names.first().map_or("_", String::as_str);
                self.bind(first, Value::Int(as_int(count)))?;
            } else if keyed {
                let first = names.first().map_or("_", String::as_str);
                self.bind(first, position.clone())?;
            }
            self.bind_ref(item_name, place.then(Step::At(position)))?;
            let ran = self.stmts(body);
            self.restore(mark);
            match ran {
                Ok(()) | Err(Flow::Continue) => {}
                Err(Flow::Break) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// The value of a constant, worked out the first time it's read.
    pub(crate) fn constant(&mut self, name: &str) -> Run<Option<Value>> {
        if let Some(value) = self.constants.get(name) {
            return Ok(Some(value.clone()));
        }
        let Some(expr) = self.program.constants.get(name).copied() else { return Ok(None) };
        let previous = self.push_frame(Vec::new(), None)?;
        let value = self.eval(expr);
        self.pop_frame(previous);
        let value = value?;
        self.constants.insert(name.to_owned(), value.clone());
        Ok(Some(value))
    }

    /// Whether a function's body yields, which makes calling it make a generator.
    pub(crate) fn is_generator(&mut self, def: &'static fennec_syntax::FnDef) -> bool {
        let key = std::ptr::from_ref(def) as usize;
        if let Some(known) = self.generators.get(&key) {
            return *known;
        }
        let yields = yields_in(&def.body);
        self.generators.insert(key, yields);
        yields
    }
}

/// Whether statements yield, outside any function written inside them.
fn yields_in(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Yield(_) => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| yields_in(body)) || otherwise.as_deref().is_some_and(yields_in)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            yields_in(body)
        }
        StmtKind::Expr(expr) => match &expr.kind {
            ExprKind::Match { arms, .. } => arms.iter().any(|arm| yields_in(&arm.body)),
            _ => false,
        },
        StmtKind::Lend { otherwise, .. } => yields_in(otherwise),
        _ => false,
    })
}

/// A count as an int.
pub(crate) fn as_int(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Where `at` is in something `len` long, counting from the end when it's negative.
pub(crate) fn position(at: i64, len: usize, span: Span) -> Run<usize> {
    let signed = as_int(len);
    let resolved = if at < 0 { at + signed } else { at };
    match usize::try_from(resolved) {
        Ok(found) if found < len => Ok(found),
        _ => crash(format!("index {at} is out of range for length {len}"), span),
    }
}

enum Reached<'v> {
    Inside(&'v Value),
    Made(Value),
}

/// One step into a value, for reading.
fn step_into<'v>(value: &'v Value, step: &Step, span: Span) -> Run<Reached<'v>> {
    match (value, step) {
        (Value::Instance(instance), Step::Field(name)) => match instance.field(name) {
            Some(field) => Ok(Reached::Inside(field)),
            None => unsupported(format!("the field `{name}`")),
        },
        (Value::List(items) | Value::Tuple(items), Step::At(Value::Int(at))) => {
            let at = position(*at, items.len(), span)?;
            items.get(at).map(Reached::Inside).map_or_else(|| crash("index out of range", span), Ok)
        }
        (Value::Str(text), Step::At(Value::Int(at))) => {
            let count = text.chars().count();
            let at = position(*at, count, span)?;
            Ok(Reached::Made(Value::Str(text.chars().nth(at).map(String::from).unwrap_or_default())))
        }
        (Value::Dict(pairs), Step::At(key)) => match find_key(pairs, key)? {
            Some(at) => pairs
                .get(at)
                .map(|(_, value)| Reached::Inside(value))
                .map_or_else(|| unsupported("a key"), Ok),
            None => crash(format!("{} isn't a key here", plain_key(key)), span),
        },
        (Value::Cap(Cap::Args), Step::At(Value::Int(at))) => {
            position(*at, 0, span)?;
            unsupported("an argument")
        }
        (Value::Table(table), Step::At(Value::Id(id))) => {
            match table.find(*id).and_then(|at| table.slots.get(at)).and_then(|slot| slot.item.as_ref()) {
                Some(item) => Ok(Reached::Inside(item)),
                None => crash(table.missing(*id), span),
            }
        }
        (other, _) => unsupported(format!("reaching into a {}", other.kind())),
    }
}

/// One step into a value, for writing.
fn step_into_mut<'v>(value: &'v mut Value, step: &Step, span: Span, adding: bool) -> Run<&'v mut Value> {
    match (value, step) {
        (Value::Instance(instance), Step::Field(name)) => match instance.field_mut(name) {
            Some(field) => Ok(field),
            None => unsupported(format!("the field `{name}`")),
        },
        (Value::List(items), Step::At(Value::Int(at))) => {
            let at = position(*at, items.len(), span)?;
            items.get_mut(at).map_or_else(|| crash("index out of range", span), Ok)
        }
        (Value::Dict(pairs), Step::At(key)) => {
            let found = find_key(pairs, key)?;
            let at = match found {
                Some(at) => at,
                None if adding => {
                    pairs.push((key.clone(), Value::Unit));
                    pairs.len() - 1
                }
                None => return crash(format!("{} isn't a key here", plain_key(key)), span),
            };
            pairs.get_mut(at).map(|(_, value)| value).map_or_else(|| unsupported("a key"), Ok)
        }
        (Value::Table(table), Step::At(Value::Id(id))) => {
            let missing = table.missing(*id);
            match table.find(*id).and_then(|at| table.slots.get_mut(at)).and_then(|slot| slot.item.as_mut()) {
                Some(item) => Ok(item),
                None => crash(missing, span),
            }
        }
        (other, _) => unsupported(format!("changing part of a {}", other.kind())),
    }
}

/// Where a key is among a dict's pairs, by plain equality, which is what a dict's keys use.
pub(crate) fn find_key(pairs: &[(Value, Value)], key: &Value) -> Run<Option<usize>> {
    for (at, (held, _)) in pairs.iter().enumerate() {
        if show::plain_equal(held, key)? {
            return Ok(Some(at));
        }
    }
    Ok(None)
}

/// A key as a message names it.
fn plain_key(key: &Value) -> String {
    match key {
        Value::Str(text) => crate::text::quoted(text),
        Value::Int(n) => n.to_string(),
        other => other.kind().to_owned(),
    }
}
