//! `with launch(task)?:`, and `with launch((inbox) => ...)? as sends:`: a task that runs beside
//! its block until the block ends, as `actors.rs` runs it, with an inbox of `inbox_size`
//! messages, 16 unless given, when it takes one. The block sends on it through what it lends, and
//! it closes when the block ends, so the task reads every message sent and then finds it closed.
//!
//! What each side reaches is held to what's exact. Printing and the file system from both are
//! held in order by vector clocks (`actors.rs`). A `shared` value one side changes and the other
//! reaches is refused, as is a generator made before the task and read in it. A block that fails
//! or crashes cuts its task short at once, wherever it is, and a task that ends with messages left
//! unread closes its inbox, so a send after that fails with `Closed` and one before it doesn't:
//! both are refused. A `stop_after=` hangs on time, and is refused too. A task that crashes cuts
//! short every wait its block is in, as a closed channel's, until the block ends (`actors.rs`). What the task ends with is
//! what the `with` ends with, once the block has ended: the task's failure, or
//! `Crashed(why="the task crashed")` for a crash.

use std::sync::{Arc, Mutex};

use fennec_syntax::{Arg, Expr, ExprKind, Span, Stmt, WithName};

use super::{Flow, Interp, Run, unsupported};
use crate::value::{Channel, Value};

/// A `with launch` running now, as one side of it sees it: whether it's its task, rather than its
/// block, and the `shared` values both have locked, which each side's view shares.
#[derive(Clone)]
pub(crate) struct Launched {
    pub(crate) task: bool,
    seen: Arc<Mutex<Seen>>,
    /// How many rounds and tasks had started when the task started.
    pub(crate) started: u64,
}

/// What a launch's block and its task have locked.
#[derive(Default)]
struct Seen {
    /// The `shared` values each has locked: which, whether by the task, and whether to change it.
    locked: Vec<(u64, bool, bool)>,
}

impl Launched {
    /// A launch's block's view of it, before its task starts.
    pub(crate) fn new() -> Launched {
        Launched { task: false, seen: Arc::new(Mutex::new(Seen::default())), started: 0 }
    }

    /// The task's view of the same launch, started as the `started`th round or task.
    pub(crate) fn task_view(&self, started: u64) -> Launched {
        Launched { task: true, seen: Arc::clone(&self.seen), started }
    }

    pub(crate) fn reads_since(&self) -> u64 {
        if self.task { self.started } else { 0 }
    }

    /// Notes a `lock`, saying whether the other side hasn't reached what this one changes.
    pub(crate) fn note_lock(&mut self, cell: u64, writes: bool) -> bool {
        let task = self.task;
        let Ok(mut seen) = self.seen.lock() else { return false };
        let clashes =
            seen.locked.iter().any(|&(held, by, wrote)| held == cell && by != task && (wrote || writes));
        seen.locked.push((cell, task, writes));
        !clashes
    }
}

/// `launch(task, inbox_size=n)`, as it's written.
struct Written {
    task: &'static Expr,
    size: Option<&'static Expr>,
}

impl Interp {
    /// `launch(...)`, when that's what a `with` takes.
    pub(crate) fn is_launch(&self, call: &'static Expr) -> bool {
        let ExprKind::Call { callee, .. } = &call.kind else { return false };
        matches!(&callee.kind, ExprKind::Name(name) if name == "launch" && !self.is_bound(name)
            && self.program.function_at(name, callee.span).is_none())
    }

    /// `with launch(task)? as sends:`, its block run first and then its task.
    pub(crate) fn launch(
        &mut self,
        call: &'static Expr,
        names: &'static [WithName],
        body: &'static [Stmt],
    ) -> Run<()> {
        let ExprKind::Call { args, .. } = &call.kind else {
            return unsupported("a launch that isn't a call");
        };
        let written = launch_args(args)?;
        let task = self.eval(written.task)?;
        // How many messages the inbox holds says only when a send waits, which changes nothing
        // here, but it has to be a count.
        if let Some(size) = written.size
            && !matches!(self.eval(size)?, Value::Int(size) if size > 0)
        {
            return unsupported("an `inbox_size` that isn't a count");
        }
        let inbox = match &written.task.kind {
            ExprKind::Lambda { params, .. } if params.len() <= 1 => params.len() == 1,
            _ => return unsupported("a launch of something other than an arrow function"),
        };
        // The task runs beside its block, as `actors.rs` says, with an inbox of its own of
        // `inbox_size`, 16 unless given, when it takes one.
        let size = match written.size {
            Some(size) => match self.eval(size)? {
                Value::Int(size) => usize::try_from(size).unwrap_or(0),
                _ => 0,
            },
            None => 16,
        };
        self.launch_beside(task, (inbox.then_some(size), names), body, call.span)
    }

    /// A method of an end of a task's channel: `send` on the block's, `receive` on the task's.
    pub(crate) fn channel_method(&mut self, end: &Channel, name: &str, args: &[Value]) -> Run<Value> {
        let number = end.number;
        match (end.sends, name, args) {
            (true, "send", [message]) => match self.channel_send(number, message, -1)? {
                Value::Int(0) => Ok(Value::Unit),
                _ => Err(Flow::Fail(Value::instance("Closed", None, Vec::new()), Span::default())),
            },
            (false, "receive", []) => self
                .channel_receive(number, -1)
                .map(|(message, why)| if why == 0 { Value::found(message) } else { message }),
            _ => unsupported(format!("the channel method `{name}`")),
        }
    }
}

/// The task and the `inbox_size=` a `launch` is given, refusing anything else.
fn launch_args(args: &'static [Arg]) -> Run<Written> {
    let mut task = None;
    let mut size = None;
    for arg in args {
        match arg.name.as_deref() {
            None if task.is_none() && !arg.is_mut => task = Some(&arg.value),
            Some("inbox_size") if size.is_none() => size = Some(&arg.value),
            Some("stop_after") => return unsupported("`stop_after`, which hangs on time"),
            _ => return unsupported("`launch` with these arguments"),
        }
    }
    match task {
        Some(task) => Ok(Written { task, size }),
        None => unsupported("`launch` with no task"),
    }
}

/// The arrow a task is, for calling it.
pub(crate) fn task_expr(task: &Value) -> Run<&'static Expr> {
    match task {
        Value::Func(func) => match &**func {
            crate::value::Func::Lambda { body, .. } => Ok(body),
            _ => unsupported("a launch of something other than an arrow function"),
        },
        _ => unsupported("a launch of something other than an arrow function"),
    }
}
