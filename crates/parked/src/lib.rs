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

use std::cell::{Cell, RefCell};
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
    /// This thread's stack for deep work, while nothing runs on it: made the first time it's
    /// wanted, and kept for the next.
    static DEEP: RefCell<Option<DefaultStack>> = const { RefCell::new(None) };
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

/// Whether the running task has been cancelled, so parking gives back at once and it can't wait for
/// anything.
pub fn is_cancelled() -> bool {
    current::is_cancelled()
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

/// Runs `work` on a stack of at least `bytes` that this thread keeps for deep work, when the task
/// running here has a smaller one of its own. While `work` runs, `stack_low` and `stack_size` say
/// where that stack is, so what checks how far its stack has grown checks that one. With no task
/// running, with one whose stack is big enough, with the deep stack already in use, or with none to
/// be had, `work` runs here. `work` mustn't park: that stops the process.
pub fn on_deep_stack<R>(bytes: usize, work: impl FnOnce() -> R) -> R {
    if current::stack_size().is_none_or(|size| size >= bytes) {
        return work();
    }
    let kept = DEEP.with_borrow_mut(Option::take);
    let mut stack = match kept {
        Some(stack) if stack.base().get() - stack.limit().get() >= bytes => stack,
        _ => match DefaultStack::new(bytes) {
            Ok(stack) => stack,
            Err(_) => return work(),
        },
    };
    let bounds = current::Bounds { low: stack.limit().get(), high: stack.base().get() };
    let result = corosensei::on_stack(&mut stack, || current::with_stack(bounds, work));
    DEEP.with_borrow_mut(|deep| *deep = Some(stack));
    result
}

struct Task {
    /// Taken out only once the task has ended, for its stack to be used again.
    coroutine: Option<Coroutine<Wake, Request, i64, DefaultStack>>,
    stack: current::Bounds,
    /// The size its stack was asked for, which an ended task's stack is kept by.
    stack_bytes: usize,
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

/// How many stacks of each size, of tasks that have ended, a scheduler keeps for its next tasks,
/// unless it's told otherwise. Each keeps the pages its task touched, so they're few.
const SPARE_STACKS: usize = 64;

/// A thread's tasks.
pub struct Scheduler {
    stack_bytes: usize,
    tasks: HashMap<TaskId, Task>,
    /// Stacks of tasks that have ended, for new tasks, by the size they were asked for: making a
    /// stack maps it, with its guard page, which costs a few system calls, and a thread that makes
    /// many short tasks pays that for each.
    spare: Vec<(usize, Vec<DefaultStack>)>,
    most_spare: usize,
}

impl Scheduler {
    /// A scheduler whose tasks each get a stack of `stack_bytes`, plus a guard page. A stack
    /// takes memory only for the pages a task touches.
    pub fn new(stack_bytes: usize) -> Scheduler {
        Scheduler::keeping(stack_bytes, SPARE_STACKS)
    }

    /// A scheduler that keeps at most `most_spare` stacks of each size, of ended tasks, for its
    /// next ones.
    pub fn keeping(stack_bytes: usize, most_spare: usize) -> Scheduler {
        Scheduler { stack_bytes, tasks: HashMap::new(), spare: Vec::new(), most_spare }
    }

    /// How many stacks it keeps for its next tasks now.
    pub fn spare_stacks(&self) -> usize {
        self.spare.iter().map(|(_, kept)| kept.len()).sum()
    }

    /// Makes a task that runs `body` on its own stack, from its first resume: one an ended task
    /// left, or a new one.
    pub fn spawn(&mut self, body: impl FnOnce() -> i64 + 'static) -> std::io::Result<TaskId> {
        self.spawn_with_stack(self.stack_bytes, body)
    }

    /// Makes a task as `spawn` does, on a stack of `stack_bytes` instead of the scheduler's own
    /// size, plus a guard page.
    pub fn spawn_with_stack(
        &mut self,
        stack_bytes: usize,
        body: impl FnOnce() -> i64 + 'static,
    ) -> std::io::Result<TaskId> {
        let kept =
            self.spare.iter_mut().find(|(bytes, _)| *bytes == stack_bytes).and_then(|(_, kept)| kept.pop());
        let stack = match kept {
            Some(stack) => stack,
            None => DefaultStack::new(stack_bytes)?,
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
        self.tasks
            .insert(task, Task { coroutine: Some(coroutine), stack: bounds, stack_bytes, cancelled: false });
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
                if let Some(ended) = running.coroutine.take() {
                    self.keep(running.stack_bytes, ended.into_stack());
                }
                Step::Done(result)
            }
        }
    }

    /// Keeps an ended task's stack, asked for as `stack_bytes`, for the next task wanting one that
    /// size, unless as many of that size are kept already.
    fn keep(&mut self, stack_bytes: usize, stack: DefaultStack) {
        match self.spare.iter_mut().find(|(bytes, _)| *bytes == stack_bytes) {
            Some((_, kept)) if kept.len() < self.most_spare => kept.push(stack),
            None if self.most_spare > 0 => self.spare.push((stack_bytes, vec![stack])),
            Some(_) | None => {}
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
