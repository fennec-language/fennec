//! The running task's yielder, for `park` to reach from deep in a call: native code calling
//! the runtime, calling a socket's read. This is the only `unsafe` in Fennec's own crates
//! outside exporting the runtime's functions by name.
//!
//! corosensei hands a task its `Yielder` only as an argument to the task's first function. To
//! suspend from further down, the task keeps a pointer to it in a thread-local, `CURRENT`,
//! next to the task's number. Following that pointer is the one `unsafe` here. It's sound
//! because of these invariants:
//!
//! 1. **Only a running task sets `CURRENT`, and only to its own yielder.** A task sets it when
//!    it starts (`enter`) and each time it's resumed, right after its `suspend` returns.
//! 2. **`CURRENT` never outlives the task running.** Each resume is bracketed by a `Running`,
//!    which takes `CURRENT` and `RUNNING` out before the task runs and puts back what was
//!    there when it suspends, ends or panics. So once a task stops running, `CURRENT` is what
//!    it was before: nothing, or the task that resumed it, which runs again from there.
//! 3. **A yielder lives as long as its task's stack.** corosensei keeps it on the task's
//!    stack, which isn't freed while the task can still run, and a task running is alive.
//! 4. **It never crosses threads.** `CURRENT` and `RUNNING` are thread-locals, and a task's
//!    coroutine isn't `Send`, so it's resumed only on the thread that made it.
//!
//! Before following the pointer, `suspend` checks that `CURRENT`'s task is the one `RUNNING`
//! says was resumed last, which only `Running` sets. If they differ, an invariant is broken,
//! and the process stops with a message rather than switching onto the wrong stack. The check
//! is one compare, kept in release builds.

use std::cell::Cell;
use std::io::Write as _;

use corosensei::Yielder;

use crate::{Request, TaskId, Wake};

/// The yielder corosensei gives a task: woken with a `Wake`, suspending with a `Request`.
pub(crate) type TaskYielder = Yielder<Wake, Request>;

/// The running task, with a pointer to its yielder.
#[derive(Clone, Copy)]
struct Current {
    task: TaskId,
    yielder: *const TaskYielder,
}

/// The task last resumed on this thread, and whether it was cancelled.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Resumed {
    task: TaskId,
    cancelled: bool,
}

thread_local! {
    /// The task running on this thread and its yielder. Set only by the task, to its own
    /// (invariant 1), and put back by `Running` once it stops (invariant 2).
    static CURRENT: Cell<Option<Current>> = const { Cell::new(None) };
    /// The task resumed on this thread, set only by `Running`.
    static RUNNING: Cell<Option<Resumed>> = const { Cell::new(None) };
}

/// A task running, from its resume until it suspends, ends or panics. Dropping it puts back
/// what was running before.
pub(crate) struct Running {
    current: Option<Current>,
    resumed: Option<Resumed>,
}

impl Running {
    /// Marks `task` as about to run. Its `CURRENT` is set by the task itself once it's on its
    /// own stack.
    pub(crate) fn start(task: TaskId, cancelled: bool) -> Running {
        Running {
            current: CURRENT.replace(None),
            resumed: RUNNING.replace(Some(Resumed { task, cancelled })),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        CURRENT.set(self.current);
        RUNNING.set(self.resumed);
    }
}

/// Called by a task first thing on its own stack, with the yielder corosensei gave it.
pub(crate) fn enter(task: TaskId, yielder: &TaskYielder) {
    check(task);
    CURRENT.set(Some(Current { task, yielder }));
}

/// Suspends the running task, asking for `request`, until it's resumed with a `Wake`. With no
/// task running here, it gives back `None` and the caller waits as a plain thread would. A
/// cancelled task isn't suspended again: it's told at once that it was cancelled.
pub(crate) fn suspend(request: Request) -> Option<Wake> {
    let current = CURRENT.get()?;
    check(current.task);
    if RUNNING.get().is_some_and(|resumed| resumed.cancelled) {
        return Some(Wake::Cancelled);
    }
    // SAFETY: `current.yielder` is the yielder of `current.task` (invariant 1), which `check`
    // just confirmed is the task running on this thread. A running task's stack is alive, and
    // its yielder with it (invariant 3), on this thread (invariant 4). So the pointer is to a
    // live `Yielder`, and this is a shared borrow of it, as corosensei's own `&Yielder` is,
    // used only before this task suspends and after it's resumed, while it's running.
    let yielder = unsafe { &*current.yielder };
    let wake = yielder.suspend(request);
    // Resumed: `Running` has taken `CURRENT` out and marked this task as the one running.
    check(current.task);
    CURRENT.set(Some(current));
    Some(wake)
}

/// Stops the process if `task` isn't the one last resumed on this thread, which would mean an
/// invariant above is broken.
fn check(task: TaskId) {
    if RUNNING.get().map(|resumed| resumed.task) != Some(task) {
        let message = format!(
            "fennec: parked task {task} tried to run while the thread's running task was {:?}; \
             stopping rather than switching onto the wrong stack\n",
            RUNNING.get().map(|resumed| resumed.task)
        );
        let _ = std::io::stderr().write_all(message.as_bytes());
        std::process::abort();
    }
}

/// For tests: marks another task as running, as a broken invariant would.
#[cfg(test)]
pub(crate) fn pretend_running(task: TaskId) {
    RUNNING.set(Some(Resumed { task, cancelled: false }));
}
