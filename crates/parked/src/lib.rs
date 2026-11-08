//! Parked tasks: a task waiting on the network gives up its thread, not its stack, so a few
//! threads serve thousands of connections (decisions.md: "A task waiting on the network is
//! parked, not its thread").
//!
//! Each task runs on a stack of its own, a corosensei coroutine with a guard page, of which
//! only the pages it touches take memory. A `Scheduler` holds a thread's tasks and resumes
//! them. A task that would wait calls `park` from wherever it is, however deep, which suspends
//! it with a `Request` saying what it waits for, until the scheduler resumes it with a `Wake`.
//! Tasks stay on the thread that made them, so each thread's runtime keeps owning what its
//! tasks make.
//!
//! A task is never dropped while suspended: that stops the process. Cancelling one resumes it
//! with `Wake::Cancelled`, and from then on `park` answers it at once with the same, so it
//! runs to its end. Dropping a scheduler cancels its tasks that way. A panic in a task stops
//! the process, in every build, as `panic=abort` does in release builds.

use std::cell::Cell;
use std::collections::HashMap;

use corosensei::stack::{DefaultStack, Stack as _};
use corosensei::{Coroutine, CoroutineResult};

#[expect(unsafe_code, reason = "following the running task's yielder, under the invariants in `current`")]
mod current;

/// A task's number, which no other task on its thread ever has.
pub type TaskId = u64;

thread_local! {
    /// The number the next task made on this thread gets.
    static NEXT: Cell<TaskId> = const { Cell::new(1) };
}

/// What a parked task waits for, as a number its scheduler's owner gives meaning to, such as
/// a socket's token and whether it waits to read or write.
pub type Request = u64;

/// Why a parked task was resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// What it waited for may have happened, with a number the scheduler's owner gives
    /// meaning to, such as ready, timed out or closed.
    Ready(u64),
    /// It's cancelled: it should give up what it's doing and end.
    Cancelled,
}

/// What a task did when resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// It parked, waiting for this.
    Parked(Request),
    /// It ended, with what its function gave back: 0, or anything else when it failed.
    Done(i64),
    /// There's no such task here: it ended, or it was never made here.
    Unknown,
}

/// Suspends the running task until it's resumed, asking for `request`. With no task running
/// on this thread, it gives back `None`, and the caller should wait as a plain thread would.
pub fn park(request: Request) -> Option<Wake> {
    let woken = current::suspend(request);
    if woken.is_some()
        && let Some(hook) = ON_RESUME.get()
    {
        hook();
    }
    woken
}

/// What's run on a task's own stack each time it comes back from parking.
static ON_RESUME: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Has `hook` run on a task's own stack each time it comes back from parking: the one place that
/// sees every wait end, for what belongs to the task but is held per thread. Only the first call
/// counts.
pub fn on_resume(hook: fn()) {
    let _ = ON_RESUME.set(hook);
}

/// The lowest address of the running task's stack, for what checks how far its stack has grown.
/// With no task running on this thread, `None`.
pub fn stack_low() -> Option<usize> {
    current::stack_low()
}

/// How big the running task's stack is. With no task running on this thread, `None`.
pub fn stack_size() -> Option<usize> {
    current::stack_size()
}

struct Task {
    /// Taken out only once the task has ended, for its stack to be used again.
    coroutine: Option<Coroutine<Wake, Request, i64, DefaultStack>>,
    stack: current::Bounds,
    cancelled: bool,
}

impl Drop for Task {
    /// A task that started and hasn't ended still has a stack in use, which nothing may drop.
    fn drop(&mut self) {
        if self.coroutine.as_ref().is_some_and(|coroutine| coroutine.started() && !coroutine.done()) {
            current::stop("a parked task was dropped before it ended");
        }
    }
}

/// How many stacks of tasks that have ended a scheduler keeps for its next tasks, unless it's
/// told otherwise. Each keeps the pages its task touched, so they're few.
const SPARE_STACKS: usize = 64;

/// A thread's tasks.
pub struct Scheduler {
    stack_bytes: usize,
    tasks: HashMap<TaskId, Task>,
    /// Stacks of tasks that have ended, for new tasks: making a stack maps it, with its guard
    /// page, which costs a few system calls, and a thread that makes many short tasks pays that
    /// for each.
    spare: Vec<DefaultStack>,
    most_spare: usize,
}

impl Scheduler {
    /// A scheduler whose tasks each get a stack of `stack_bytes`, plus a guard page. A stack
    /// takes memory only for the pages a task touches.
    pub fn new(stack_bytes: usize) -> Scheduler {
        Scheduler::keeping(stack_bytes, SPARE_STACKS)
    }

    /// A scheduler that keeps at most `most_spare` stacks of ended tasks for its next ones.
    pub fn keeping(stack_bytes: usize, most_spare: usize) -> Scheduler {
        Scheduler { stack_bytes, tasks: HashMap::new(), spare: Vec::new(), most_spare }
    }

    /// How many stacks it keeps for its next tasks now.
    pub fn spare_stacks(&self) -> usize {
        self.spare.len()
    }

    /// Makes a task that runs `body` on its own stack, from its first resume: one an ended task
    /// left, or a new one.
    pub fn spawn(&mut self, body: impl FnOnce() -> i64 + 'static) -> std::io::Result<TaskId> {
        let stack = match self.spare.pop() {
            Some(stack) => stack,
            None => DefaultStack::new(self.stack_bytes)?,
        };
        let bounds = current::Bounds { low: stack.limit().get(), high: stack.base().get() };
        let task = NEXT.get();
        NEXT.set(task + 1);
        let coroutine = Coroutine::with_stack(stack, move |yielder: &current::TaskYielder, _first: Wake| {
            current::enter(task, yielder);
            // A panic can't unwind out of a task's stack, and on Windows trying ends the process
            // with no word of why. So it's caught here, on the task's own stack, and stops the
            // process with a message, its own already printed, in every build and on every OS.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
                Ok(result) => result,
                Err(_) => current::stop("a parked task panicked"),
            }
        });
        self.tasks.insert(task, Task { coroutine: Some(coroutine), stack: bounds, cancelled: false });
        Ok(task)
    }

    /// Runs `task` until it parks or ends, its `park` giving back `wake`. `Wake::Cancelled`
    /// cancels it.
    pub fn resume(&mut self, task: TaskId, wake: Wake) -> Step {
        let Some(mut running) = self.tasks.remove(&task) else { return Step::Unknown };
        let Some(coroutine) = running.coroutine.as_mut() else { return Step::Unknown };
        running.cancelled |= wake == Wake::Cancelled;
        let result = {
            let _running = current::Running::start(task, running.stack, running.cancelled);
            coroutine.resume(wake)
        };
        match result {
            CoroutineResult::Yield(request) => {
                self.tasks.insert(task, running);
                Step::Parked(request)
            }
            CoroutineResult::Return(result) => {
                // It's ended, so nothing's on its stack any more, and its stack can be used again.
                if self.spare.len() < self.most_spare
                    && let Some(ended) = running.coroutine.take()
                {
                    self.spare.push(ended.into_stack());
                }
                Step::Done(result)
            }
        }
    }

    /// Cancels `task`: it's resumed with `Wake::Cancelled`, and parking again gives it the
    /// same at once, so it runs to its end.
    pub fn cancel(&mut self, task: TaskId) -> Step {
        self.resume(task, Wake::Cancelled)
    }

    /// How many tasks haven't ended.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

impl Drop for Scheduler {
    /// Cancels every task left, running each to its end, so no stack is dropped in use.
    fn drop(&mut self) {
        let mut left: Vec<TaskId> = self.tasks.keys().copied().collect();
        left.sort_unstable();
        for task in left {
            self.cancel(task);
        }
    }
}

#[cfg(test)]
mod tests;
