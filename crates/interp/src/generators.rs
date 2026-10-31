//! Generators: functions that `yield`, and `(x for x in xs)`. Nothing runs until an item is
//! asked for, and each asks runs just far enough to give one. A generator keeps its own frame
//! and where it stopped. `yield` is a statement, so no expression is ever half done: as a
//! `yield` leaves, each statement it's inside records its place, and resuming walks those
//! places back down, skipping what's already run.
//!
//! A generator is a handle: copies are one generator, and it can be used once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind, TypeExpr};

use crate::exec::Flow;
use crate::interp::{Interp, PASSING_UP};
use crate::place::Place;
use crate::scope::Frame;
use crate::value::Value;

/// A place a running generator stopped at, for one statement it was inside.
pub(crate) enum Position {
    /// Where to go on in a list of statements.
    Statement(usize),
    /// Which branch of an `if` or case of a `match`.
    Arm(usize),
    /// Inside a `while`'s body.
    Looping,
    /// Inside a `for`'s body, with what's still to come.
    For { items: Items, place: Option<Place>, key: Option<Value> },
    /// Inside a `with` body, holding what it closes as it stood at the `yield`.
    With { resource: Value, span: Span },
}

/// What a `for` or comprehension takes its items from.
pub(crate) enum Items {
    Listed(std::vec::IntoIter<(Option<Value>, Value)>),
    Generated(Rc<Generator>),
}

enum Body {
    Function(Rc<FnDef>),
    Statements(Vec<Stmt>),
    /// `enumerate` of a generator, with the next number.
    Enumerate(RefCell<(Items, i64)>),
    /// `zip` with a generator.
    Zip(RefCell<(Items, Items)>),
}

/// How a generator stands.
struct Progress {
    frame: Frame,
    /// Where it stopped, innermost first.
    path: Vec<Position>,
    running: bool,
    finished: bool,
}

pub(crate) struct Generator {
    body: Body,
    /// What printing it shows.
    label: String,
    used: Cell<bool>,
    progress: RefCell<Progress>,
}

impl std::fmt::Debug for Generator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}>", self.label)
    }
}

impl Generator {
    /// What printing it shows.
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    fn statements(&self) -> &[Stmt] {
        match &self.body {
            Body::Function(def) => &def.body,
            Body::Statements(stmts) => stmts,
            Body::Enumerate(_) | Body::Zip(_) => &[],
        }
    }
}

thread_local! {
    static ABANDONED: RefCell<Vec<(Value, Span)>> = const { RefCell::new(Vec::new()) };
}

impl Drop for Generator {
    fn drop(&mut self) {
        // Cleanup is program code, which can't run in the middle of dropping a value, so it
        // waits for the interpreter's next statement.
        let progress = self.progress.get_mut();
        if progress.finished {
            return;
        }
        // Innermost first, the order leaving them would close them in.
        for position in progress.path.drain(..) {
            if let Position::With { resource, span } = position {
                let _ = ABANDONED.try_with(|list| list.borrow_mut().push((resource, span)));
            }
        }
    }
}

/// What asking a generator for its next item gave.
pub(crate) enum Pulled {
    Item(Value),
    Done,
    /// It failed, with this `Err`: the same as a call that failed.
    Failed(Value),
}

impl Interp<'_> {
    /// A generator for a function's body, with its frame ready and nothing run.
    pub(crate) fn generator_of(&mut self, def: Rc<FnDef>, frame: Frame) -> Value {
        let label = def.ret.as_ref().map_or_else(|| "Iterator".to_owned(), type_text);
        new_generator(Body::Function(def), frame, label)
    }

    /// `(elem for names in iter if cond)`. The first `in` is worked out now, and everything
    /// else later, seeing what was in scope now.
    pub(crate) fn generator_expression(
        &mut self,
        (elem, cond, shown): (&Expr, Option<&Expr>, &str),
        names: &[String],
        iter: &Expr,
    ) -> Result<Value, Diagnostic> {
        const SOURCE: &str = "\0source";
        let source = self.eval(iter)?;
        let mut frame = Frame::new();
        for (name, value) in self.frame.visible() {
            frame.declare(&name, value, false);
        }
        frame.declare(SOURCE, source, false);
        let span = elem.span;
        let yielded = Stmt { kind: StmtKind::Yield(elem.clone()), span };
        let body = match cond {
            Some(cond) => vec![Stmt {
                kind: StmtKind::If { branches: vec![(cond.clone(), vec![yielded])], otherwise: None },
                span,
            }],
            None => vec![yielded],
        };
        let source = Expr { kind: ExprKind::Name(SOURCE.to_owned()), span: iter.span };
        let looping =
            Stmt { kind: StmtKind::For { names: names.to_vec(), is_mut: false, iter: source, body }, span };
        Ok(new_generator(Body::Statements(vec![looping]), frame, shown.to_owned()))
    }

    /// Marks a generator as used, failing if it has been already.
    pub(crate) fn start_generator(&self, generator: &Generator, span: Span) -> Result<(), Diagnostic> {
        if generator.used.replace(true) {
            return Err(Diagnostic::new("this generator has already been used", span)
                .with_help("a generator can be used once: `list(...)` keeps its items to use again"));
        }
        Ok(())
    }

    /// Starts taking items from a value.
    pub(crate) fn items_of(&mut self, value: Value, span: Span) -> Result<Items, Diagnostic> {
        if let Value::Generator(generator) = value {
            self.start_generator(&generator, span)?;
            return Ok(Items::Generated(generator));
        }
        Ok(Items::Listed(self.entries(value, span)?.into_iter()))
    }

    /// The next item, with the key it can be changed under when it has one. A generator's
    /// failure leaves the way a failed call's does.
    pub(crate) fn next_item(
        &mut self,
        items: &mut Items,
        span: Span,
    ) -> Result<Option<(Option<Value>, Value)>, Diagnostic> {
        match items {
            Items::Listed(items) => Ok(items.next()),
            Items::Generated(generator) => match self.pull(&Rc::clone(generator), span)? {
                Pulled::Item(value) => Ok(Some((None, value))),
                Pulled::Done => Ok(None),
                Pulled::Failed(value) => {
                    self.passing_up = Some(Flow::PassUp(value));
                    Err(Diagnostic::new(PASSING_UP, span))
                }
            },
        }
    }

    /// `enumerate` of a generator, which takes each item as it's asked for.
    pub(crate) fn enumerated(&mut self, items: Value, span: Span) -> Result<Value, Diagnostic> {
        let body = Body::Enumerate(RefCell::new((self.items_of(items, span)?, 0)));
        Ok(new_generator(body, Frame::new(), "Iterator".to_owned()))
    }

    /// `zip` with a generator, which takes each pair as it's asked for.
    pub(crate) fn zipped(&mut self, a: Value, b: Value, span: Span) -> Result<Value, Diagnostic> {
        let body = Body::Zip(RefCell::new((self.items_of(a, span)?, self.items_of(b, span)?)));
        Ok(new_generator(body, Frame::new(), "Iterator".to_owned()))
    }

    /// The next item of a generator made by `enumerate` or `zip`.
    fn next_pair(&mut self, body: &Body, span: Span) -> Result<Option<Value>, Diagnostic> {
        match body {
            Body::Enumerate(state) => {
                let (items, at) = &mut *state.borrow_mut();
                let Some((_, item)) = self.next_item(items, span)? else { return Ok(None) };
                *at += 1;
                Ok(Some(Value::tuple(vec![Value::Int(*at - 1), item])))
            }
            Body::Zip(state) => {
                let (a, b) = &mut *state.borrow_mut();
                let Some((_, first)) = self.next_item(a, span)? else { return Ok(None) };
                let Some((_, second)) = self.next_item(b, span)? else { return Ok(None) };
                Ok(Some(Value::tuple(vec![first, second])))
            }
            _ => Ok(None),
        }
    }

    /// Runs a generator until its next `yield`, or its end.
    pub(crate) fn pull(&mut self, generator: &Rc<Generator>, span: Span) -> Result<Pulled, Diagnostic> {
        if matches!(generator.body, Body::Enumerate(_) | Body::Zip(_)) {
            return match self.next_pair(&generator.body, span) {
                Ok(Some(pair)) => Ok(Pulled::Item(pair)),
                Ok(None) => Ok(Pulled::Done),
                Err(failure) if failure.message == PASSING_UP => match self.passing_up.take() {
                    Some(Flow::PassUp(value)) => Ok(Pulled::Failed(value)),
                    _ => Err(failure),
                },
                Err(failure) => Err(failure),
            };
        }
        let (frame, path) = {
            let mut progress = generator.progress.borrow_mut();
            if progress.running {
                return Err(Diagnostic::new("this generator is already running", span));
            }
            if progress.finished {
                return Ok(Pulled::Done);
            }
            progress.running = true;
            (std::mem::replace(&mut progress.frame, Frame::new()), std::mem::take(&mut progress.path))
        };
        let caller = self.enter(span)?;
        self.frame = frame;
        let resuming = std::mem::replace(&mut self.resuming, path);
        let flow = self.exec_stmts(generator.statements());
        self.resuming = resuming;
        let frame = std::mem::replace(&mut self.frame, Frame::new());
        self.leave(caller);
        let mut progress = generator.progress.borrow_mut();
        progress.running = false;
        progress.frame = frame;
        match flow {
            Ok(Flow::Yield(value)) => {
                progress.path = std::mem::take(&mut self.suspending);
                Ok(Pulled::Item(value))
            }
            Ok(Flow::PassUp(value)) => {
                progress.finished = true;
                Ok(Pulled::Failed(value))
            }
            Ok(_) => {
                progress.finished = true;
                Ok(Pulled::Done)
            }
            Err(failure) => {
                progress.finished = true;
                Err(failure)
            }
        }
    }

    /// Runs the cleanup of `with` blocks whose generators were abandoned, if there are any.
    pub(crate) fn clean_up_abandoned(&mut self) -> Result<(), Diagnostic> {
        let abandoned =
            ABANDONED.try_with(|list| std::mem::take(&mut *list.borrow_mut())).unwrap_or_default();
        if abandoned.is_empty() {
            return Ok(());
        }
        let resuming = std::mem::take(&mut self.resuming);
        let mut outcome = Ok(());
        for (resource, span) in abandoned {
            outcome = outcome.and(self.clean_up(resource, span));
        }
        self.resuming = resuming;
        outcome
    }

    /// Whether statements are being walked back into, after a `yield`.
    pub(crate) fn is_resuming(&self) -> bool {
        !self.resuming.is_empty()
    }

    /// The next place to go back to, outermost first.
    pub(crate) fn resume_at(&mut self) -> Option<Position> {
        self.resuming.pop()
    }
}

/// A generator's failure reached a function nothing told to expect one.
pub(crate) fn nowhere_to_pass(failure: &Value, span: Span) -> Diagnostic {
    Diagnostic::new(format!("this function can't pass up {}", crate::format::repr(failure)), span)
        .with_help("a function using a generator that can fail needs `?` where it gets it")
}

/// A written type, as a program would write it.
fn type_text(ty: &TypeExpr) -> String {
    let args: Vec<String> = ty.args.iter().map(type_text).collect();
    let args = if args.is_empty() { String::new() } else { format!("[{}]", args.join(", ")) };
    format!("{}{args}{}", ty.name, if ty.optional { "?" } else { "" })
}

fn new_generator(body: Body, frame: Frame, label: String) -> Value {
    let progress = Progress { frame, path: Vec::new(), running: false, finished: false };
    Value::Generator(Rc::new(Generator {
        body,
        label,
        used: Cell::new(false),
        progress: RefCell::new(progress),
    }))
}
