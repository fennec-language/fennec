//! Turns at something only one may use at once, as a SQLite connection or an open file is. A
//! task waiting its turn parks, holding no thread, and only the one whose turn it is runs on a
//! blocking thread. So a connection or a file holds at most one of the blocking pool's threads
//! however many tasks use it, and a slow query with 63 calls behind it can't take the pool from
//! the program's other blocking work. When a turn ends it goes to the next in line, first come
//! first served.
//!
//! Outside a task, or where a task can't park, nothing waits for a turn here: the work waits on
//! the thing's own lock where it runs, as it always did. A task stopped, or a round cancelled,
//! while it waits its turn fails as stopped there, and the work it waited to do isn't done.

#[cfg(any(test, feature = "threads"))]
use std::collections::VecDeque;
#[cfg(any(test, feature = "threads"))]
use std::sync::Arc;
#[cfg(any(test, feature = "threads"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

/// Something only one may use at once, and the line of tasks waiting their turn at it.
pub(crate) struct Held<T> {
    value: Mutex<T>,
    #[cfg(any(test, feature = "threads"))]
    line: Mutex<Line>,
}

/// Whether a turn is taken, and who waits for one, in the order they came.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
struct Line {
    taken: bool,
    waiting: VecDeque<Waiting>,
}

/// A task in line, and what says the turn's been handed to it.
#[cfg(any(test, feature = "threads"))]
struct Waiting {
    task: crate::embed::serve::ParkedTask,
    given: Arc<AtomicBool>,
}

/// A task stopped, or a round cancelled, while it waited its turn: it's failed as stopped, and
/// what it waited to do mustn't be done.
pub(crate) struct Stopped;

/// A task's turn, which goes to the next in line when it's dropped.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "a turn is taken only where tasks park")
)]
pub(crate) struct Turn<'a, T> {
    held: &'a Held<T>,
}

impl<T> Held<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
            #[cfg(any(test, feature = "threads"))]
            line: Mutex::new(Line::default()),
        }
    }

    /// What it holds, once no one else can have it.
    pub(crate) fn into_inner(self) -> T {
        self.value.into_inner().unwrap_or_else(PoisonError::into_inner)
    }

    /// Its lock, for a look that doesn't wait on anyone's turn.
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.value.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Its lock, taken by work on a blocking thread. Work in a task's turn finds it free; when it
    /// doesn't, as when a thread outside a task has it, the wait blocks this thread, and is
    /// counted with the waits that do under `FENNEC_COUNTS`.
    pub(crate) fn lock_for_work(&self) -> MutexGuard<'_, T> {
        match self.value.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                #[cfg(any(test, feature = "threads"))]
                crate::embed::serve::waited_blocked();
                self.lock()
            }
        }
    }

    #[cfg(any(test, feature = "threads"))]
    fn line(&self) -> MutexGuard<'_, Line> {
        self.line.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// This task's turn, waited for parked: `None` where a task can't park, for the work to wait
    /// on the lock where it runs instead, and `Stopped` when it was stopped while it waited, for
    /// the work not to be done. Done anyway, a statement a stopped round waited to run ran once
    /// the one before it ended, and so did a write to a file (#1951).
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "a turn is waited for, and a stop comes in the wait, only where tasks park"
        )
    )]
    pub(crate) fn turn(&self) -> Result<Option<Turn<'_, T>>, Stopped> {
        #[cfg(any(test, feature = "threads"))]
        {
            use crate::embed::serve::{Parked, ParkedTask, can_park, cut_short, next_token, park_on};
            if !can_park() {
                return Ok(None);
            }
            let mut line = self.line();
            if !line.taken {
                line.taken = true;
                return Ok(Some(Turn { held: self }));
            }
            let token = next_token();
            let Some(task) = ParkedTask::here(token) else { return Ok(None) };
            let given = Arc::new(AtomicBool::new(false));
            line.waiting.push_back(Waiting { task, given: Arc::clone(&given) });
            drop(line);
            loop {
                // A turn handed over before this task parks wakes it once it has: the wake goes
                // through its worker's mail, which the worker takes only after the task parks.
                if given.load(Ordering::Acquire) {
                    let turn = Turn { held: self };
                    // Stopped after the turn was handed to it, before it ran, as when its worker
                    // took the hand-over's wake ahead of the stop: it's failed as stopped, and the
                    // turn goes on to the next in line. It did the work and went on before (#1988).
                    if cut_short() {
                        drop(turn);
                        return Err(Stopped);
                    }
                    return Ok(Some(turn));
                }
                if park_on(token, None) == Parked::Cancelled {
                    let mut line = self.line();
                    if given.load(Ordering::Acquire) {
                        drop(line);
                        // Handed over as it was stopped: it goes on to the next in line.
                        drop(Turn { held: self });
                    } else {
                        line.waiting.retain(|waiting| !Arc::ptr_eq(&waiting.given, &given));
                    }
                    return Err(Stopped);
                }
            }
        }
        #[cfg(not(any(test, feature = "threads")))]
        {
            Ok(None)
        }
    }
}

impl<T> Drop for Turn<'_, T> {
    /// The turn goes to the next in line, or is free when there's no one.
    fn drop(&mut self) {
        #[cfg(any(test, feature = "threads"))]
        {
            let mut line = self.held.line();
            if let Some(next) = line.waiting.pop_front() {
                next.given.store(true, Ordering::Release);
                next.task.wake();
                return;
            }
            line.taken = false;
        }
    }
}
