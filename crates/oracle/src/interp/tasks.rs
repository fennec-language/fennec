//! `with launch(task)? as sends:`: a task that runs beside its block until the block ends. Here
//! the block runs first, its sends kept in the order they're made, and then the task, reading
//! them in that order and then finding its inbox closed, as it would once the block ended.
//!
//! That's what every order the two could run in gives only where neither sees the other run. So
//! printing, the file system, and a `shared` value one changes, are refused when both the block
//! and the task reach them, and so is a generator made before the task, read in it. Messages go
//! one way only, from the block to the task. A block that fails or crashes cuts its task short
//! at once, wherever it is, and a task that ends with messages left unread closes its inbox, so a
//! send after that fails with `Closed`, but one before it doesn't, so both are refused. A
//! `stop_after=` hangs on time, and is refused too. What the task ends with is what the `with`
//! ends with, once the block has ended: the task's failure, or `Crashed(why="the task crashed")`
//! for a crash.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use fennec_syntax::{Arg, Expr, ExprKind, Span, Stmt, WithName};

use super::parallel::{Outside, Scope};
use super::{Flow, Interp, Run, unsupported};
use crate::value::{Channel, Value};

/// A `with launch` running now: whether it's its task that runs, rather than its block, and what
/// each has reached.
pub(crate) struct Launched {
    task: bool,
    /// Whether the block, then the task, has printed, and reached the file system.
    reached: [[bool; 2]; 2],
    /// The `shared` values each has locked: which, whether by the task, and whether to change it.
    locked: Vec<(u64, bool, bool)>,
    /// How many rounds and tasks had started when the task started.
    started: u64,
}

impl Launched {
    pub(crate) fn reads_since(&self) -> u64 {
        if self.task { self.started } else { 0 }
    }

    /// Notes reaching outside, saying whether the other side hasn't.
    pub(crate) fn reach_out(&mut self, outside: Outside) -> bool {
        let kind = match outside {
            Outside::Printing => 0,
            Outside::Disk => 1,
        };
        let side = usize::from(self.task);
        let Some(reached) = self.reached.get_mut(kind) else { return false };
        if reached.get(1 - side).copied().unwrap_or(true) {
            return false;
        }
        if let Some(mine) = reached.get_mut(side) {
            *mine = true;
        }
        true
    }

    /// Notes a `lock`, saying whether the other side hasn't reached what this one changes.
    pub(crate) fn note_lock(&mut self, cell: u64, writes: bool) -> bool {
        let task = self.task;
        let clashes =
            self.locked.iter().any(|&(held, by, wrote)| held == cell && by != task && (wrote || writes));
        self.locked.push((cell, task, writes));
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
            && !self.program.functions.contains_key(name.as_str()))
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
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let launched = Launched { task: false, reached: [[false; 2]; 2], locked: Vec::new(), started: 0 };
        self.push_scope(Scope::Launch(launched))?;
        let ended = self.block_then_task(&task, inbox.then_some(&queue), names, body, call.span);
        self.pop_scope();
        if let Err(flow @ (Flow::Unsupported(_) | Flow::Abandoned)) = ended {
            return Err(flow);
        }
        // A task that ends with messages left closes its inbox, and whether the block's sends
        // after that failed with `Closed`, and so how the `with` ends, hangs on time.
        if queue.lock().map_or(true, |queue| !queue.is_empty()) {
            return unsupported("a launched task that ends with messages left in its inbox");
        }
        ended?.map_or(Ok(()), Err)
    }

    /// Runs the block, then the task, giving back how the block left, for after the task, or
    /// how the task ended, which is how the `with` ends.
    fn block_then_task(
        &mut self,
        task: &Value,
        queue: Option<&Arc<Mutex<VecDeque<Value>>>>,
        names: &'static [WithName],
        body: &'static [Stmt],
        span: Span,
    ) -> Run<Option<Flow>> {
        let mark = self.mark();
        let lent = queue
            .map_or(Value::Int(0), |queue| Value::Channel(Channel { sends: true, queue: Arc::clone(queue) }));
        let ran = self.lend_names(names, lent, span).and_then(|()| self.stmts(body));
        self.restore(mark);
        let pending = match ran {
            Ok(()) => None,
            Err(flow @ (Flow::Return(_) | Flow::Break | Flow::Continue)) => Some(flow),
            Err(Flow::Fail(..) | Flow::Crash(..)) => {
                return unsupported("a `with launch` block that fails, which cuts its task short at once");
            }
            Err(other) => return Err(other),
        };
        self.start_task()?;
        let given = queue
            .map(|queue| Value::Channel(Channel { sends: false, queue: Arc::clone(queue) }))
            .into_iter()
            .collect();
        match self.call_value(task, given, task_expr(task)?) {
            Ok(_) => Ok(pending),
            Err(Flow::Fail(error, _)) => Err(Flow::Fail(error, span)),
            Err(Flow::Crash(..)) => {
                let why = Value::Str("the task crashed".to_owned());
                Err(Flow::Fail(Value::instance("Crashed", None, vec![("why".to_owned(), why)]), span))
            }
            Err(Flow::Return(_) | Flow::Break | Flow::Continue) => {
                unsupported("a `return`, `break` or `continue` leaving a launched task")
            }
            Err(other) => Err(other),
        }
    }

    /// Notes that the task runs now, rather than the block.
    fn start_task(&self) -> Run<()> {
        let started = self.shared.rounds_started.fetch_add(1, Ordering::SeqCst) + 1;
        let Ok(mut scopes) = self.shared.scopes.lock() else { return unsupported("a launch was lost") };
        let Some(Scope::Launch(launched)) = scopes.last_mut() else {
            return unsupported("a launch was lost");
        };
        launched.task = true;
        launched.started = started;
        Ok(())
    }

    /// A method of an end of a task's channel: `send` on the block's, `receive` on the task's.
    pub(crate) fn channel_method(&mut self, end: &Channel, name: &str, args: &[Value]) -> Run<Value> {
        let Ok(mut queue) = end.queue.lock() else { return unsupported("a channel was lost") };
        match (end.sends, name, args) {
            (true, "send", [message]) => {
                self.charge(message)?;
                queue.push_back(message.clone());
                Ok(Value::Unit)
            }
            (false, "receive", []) => Ok(queue.pop_front().unwrap_or(Value::None)),
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
fn task_expr(task: &Value) -> Run<&'static Expr> {
    match task {
        Value::Func(func) => match &**func {
            crate::value::Func::Lambda { body, .. } => Ok(body),
            _ => unsupported("a launch of something other than an arrow function"),
        },
        _ => unsupported("a launch of something other than an arrow function"),
    }
}
