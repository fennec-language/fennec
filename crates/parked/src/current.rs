//! The running task's yielder, for `park` to reach from deep in a call: native code calling
//! the runtime, calling a socket's read. This is the only `unsafe` in Fennec's own crates
//! outside exporting the runtime's functions by name.
//!
//! corosensei hands a task its `Yielder` only as an argument to the task's first function. To
//! suspend from further down, the task keeps a pointer to it in a thread-local, `CURRENT`,
//! next to the task's number. Following that pointer is the one `unsafe` here. It's sound
//! because of these invariants:
//!
//! 1. **`CURRENT` is written only by a running task, to its own yielder, and by `Running`.**
//!    A task writes it when it starts (`enter`) and each time it's resumed, right after its
//!    `suspend` returns. `Running`, the guard around every resume, writes back what was there
//!    before once the task stops running.
//! 2. **`CURRENT` never outlives the task running.** `Running` takes `CURRENT` out before a
//!    task runs and puts back what was there when it suspends or ends. So once a task stops
//!    running, `CURRENT` is what it was before: nothing, or the task that resumed it, which
//!    runs again from there.
//! 3. **A yielder lives as long as its task's stack.** corosensei keeps it on the task's
//!    stack. A started task is never dropped before it ends (`Task`'s `Drop` stops the
//!    process otherwise), and corosensei is built without unwinding, so a task's stack is
//!    never unwound from outside it.
//! 4. **It never crosses threads.** `CURRENT` and `RUNNING` are thread-locals, and a task's
//!    coroutine isn't `Send`, so it's resumed only on the thread that made it.
//!
//! Before following the pointer, and again once resumed, `check` confirms two things that
//! only `Running` records, in `RUNNING`: that `CURRENT`'s task is the one last resumed on this
//! thread, by a number no other task on the thread ever has, and that the code checking is
//! running on that task's own stack. If either fails, an invariant is broken, and the process
//! stops with a message rather than switching onto the wrong stack. The checks are a few
//! compares, kept in release builds.

use std::cell::Cell;
use std::io::Write as _;

use corosensei::Yielder;

use crate::{Request, TaskId, Wake};

/// The yielder corosensei gives a task: woken with a `Wake`, suspending with a `Request`.
pub(crate) type TaskYielder = Yielder<Wake, Request>;

/// Where a task's stack is: from its lowest address, its limit, up to its base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub(crate) low: usize,
    pub(crate) high: usize,
}

/// The running task, with a pointer to its yielder.
#[derive(Clone, Copy)]
pub(crate) struct Current {
    task: TaskId,
    yielder: *const TaskYielder,
}

/// The task last resumed on this thread, where its stack is, and whether it was cancelled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resumed {
    task: TaskId,
    stack: Bounds,
    cancelled: bool,
}

thread_local! {
    /// The task running on this thread and its yielder (invariants 1 and 2).
    static CURRENT: Cell<Option<Current>> = const { Cell::new(None) };
    /// The task resumed on this thread, written only by `Running`.
    static RUNNING: Cell<Option<Resumed>> = const { Cell::new(None) };
}

/// A task running, from its resume until it suspends or ends. Dropping it puts back what was
/// running before.
pub(crate) struct Running {
    current: Option<Current>,
    resumed: Option<Resumed>,
}

impl Running {
    /// Marks `task`, whose stack is `stack`, as about to run. Its `CURRENT` is written by the
    /// task itself once it's on its own stack.
    pub(crate) fn start(task: TaskId, stack: Bounds, cancelled: bool) -> Running {
        Running {
            current: CURRENT.replace(None),
            resumed: RUNNING.replace(Some(Resumed { task, stack, cancelled })),
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
    // SAFETY: `current.yielder` was written by `current.task` and points to its own yielder
    // (invariant 1). `check` just confirmed that task is the one running on this thread, and
    // that this code is on its stack. A running task's stack is alive, and its yielder with it
    // (invariant 3), on this thread (invariant 4). So this is a shared borrow of a live
    // `Yielder`, as corosensei's own `&Yielder` is. It's held across the switch: while the task
    // is suspended, the borrow sits on the task's own stack, which stays alive and untouched
    // until the task is resumed or ends (invariant 3), and nothing else uses it meanwhile.
    let yielder = unsafe { &*current.yielder };
    let wake = yielder.suspend(request);
    // Resumed: `Running` has taken `CURRENT` out and marked this task as the one running.
    check(current.task);
    CURRENT.set(Some(current));
    Some(wake)
}

/// The address of a byte on the stack this runs on.
#[inline(never)]
fn here() -> usize {
    let marker = 0_u8;
    std::hint::black_box(&raw const marker).addr()
}

/// Stops the process unless `task` is the one last resumed on this thread and this code runs
/// on its stack, which would mean an invariant above is broken.
fn check(task: TaskId) {
    let resumed = RUNNING.get();
    let at = here();
    let on_its_stack = resumed.is_some_and(|resumed| (resumed.stack.low..resumed.stack.high).contains(&at));
    if resumed.map(|resumed| resumed.task) != Some(task) || !on_its_stack {
        stop(&format!(
            "parked task {task} tried to run while the thread's running task was {:?}, on {}",
            resumed.map(|resumed| resumed.task),
            if on_its_stack { "its stack" } else { "another stack" }
        ));
    }
}

/// Stops the process with a message, rather than run on with an invariant broken.
pub(crate) fn stop(why: &str) -> ! {
    let message = format!("fennec: {why}; stopping rather than switching onto the wrong stack\n");
    let _ = std::io::stderr().write_all(message.as_bytes());
    std::process::abort();
}

/// For tests: what this thread's records hold, to break an invariant with.
#[cfg(test)]
pub(crate) fn records() -> (Option<Current>, Option<Resumed>) {
    (CURRENT.get(), RUNNING.get())
}

/// For tests: overwrites this thread's records, as a broken invariant would.
#[cfg(test)]
pub(crate) fn overwrite(current: Option<Current>, resumed: Option<Resumed>) {
    CURRENT.set(current);
    RUNNING.set(resumed);
}

/// For tests: marks another task as running, as a broken invariant would.
#[cfg(test)]
pub(crate) fn pretend_running(task: TaskId) {
    let stack = RUNNING.get().map_or(Bounds { low: 0, high: 0 }, |resumed| resumed.stack);
    RUNNING.set(Some(Resumed { task, stack, cancelled: false }));
}
