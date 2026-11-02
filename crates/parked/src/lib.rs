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
//! A task is never dropped while suspended, since unwinding its stack would take
//! `panic=unwind`. Cancelling one resumes it with `Wake::Cancelled`, and from then on `park`
//! answers it at once with the same, so it runs to its end. Dropping a scheduler cancels its
//! tasks that way.

use std::collections::HashMap;

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult};

#[expect(unsafe_code, reason = "following the running task's yielder, under the invariants in `current`")]
mod current;

/// A task's number, unique on its thread for the life of the thread's scheduler.
pub type TaskId = u64;

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
    current::suspend(request)
}

struct Task {
    coroutine: Coroutine<Wake, Request, i64, DefaultStack>,
    cancelled: bool,
}

/// A thread's tasks.
pub struct Scheduler {
    stack_bytes: usize,
    tasks: HashMap<TaskId, Task>,
    next: TaskId,
}

impl Scheduler {
    /// A scheduler whose tasks each get a stack of `stack_bytes`, plus a guard page. A stack
    /// takes memory only for the pages a task touches.
    pub fn new(stack_bytes: usize) -> Scheduler {
        Scheduler { stack_bytes, tasks: HashMap::new(), next: 1 }
    }

    /// Makes a task that runs `body` on its own stack, from its first resume.
    pub fn spawn(&mut self, body: impl FnOnce() -> i64 + 'static) -> std::io::Result<TaskId> {
        let task = self.next;
        let stack = DefaultStack::new(self.stack_bytes)?;
        let coroutine = Coroutine::with_stack(stack, move |yielder: &current::TaskYielder, _first: Wake| {
            current::enter(task, yielder);
            body()
        });
        self.next += 1;
        self.tasks.insert(task, Task { coroutine, cancelled: false });
        Ok(task)
    }

    /// Runs `task` until it parks or ends, its `park` giving back `wake`. `Wake::Cancelled`
    /// cancels it.
    pub fn resume(&mut self, task: TaskId, wake: Wake) -> Step {
        let Some(mut running) = self.tasks.remove(&task) else { return Step::Unknown };
        running.cancelled |= wake == Wake::Cancelled;
        let result = {
            let _running = current::Running::start(task, running.cancelled);
            running.coroutine.resume(wake)
        };
        match result {
            CoroutineResult::Yield(request) => {
                self.tasks.insert(task, running);
                Step::Parked(request)
            }
            CoroutineResult::Return(result) => Step::Done(result),
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
    /// Cancels every task left, running each to its end, so no stack is dropped suspended.
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
