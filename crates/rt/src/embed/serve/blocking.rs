//! The threads that run blocking work for parked tasks: a SQLite call, a file's read or write, a
//! DNS lookup. A task hands its work here and parks, so it doesn't hold its worker while the work
//! blocks.
//!
//! Threads start as work needs them, one each time work comes with none waiting. The first
//! `KEPT` stay once started. More are started while every thread is busy, up to `MOST` in all,
//! and each leaves again after `IDLE` with nothing to do. At `MOST`, work waits its turn for the
//! next thread to finish what it has. So a burst of blocking work runs at once, while a program
//! that's idle holds only the threads it once needed, and at most `KEPT`.
//!
//! The pool is for work that blocks. A call that waits for something else, like an answer from a
//! server, should park on it rather than hold a thread here: past `MOST` such calls, the rest of
//! the program's blocking work would wait behind them.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Work that blocks its thread.
pub(super) type Job = Box<dyn FnOnce() + Send>;

/// How many threads stay once started, ready for the next piece of blocking work.
const KEPT: usize = 8;

/// The most threads there are at once.
const MOST: usize = 64;

/// How long a thread past the first `KEPT` waits for work before it leaves.
const IDLE: Duration = Duration::from_secs(5);

/// The runtime's pool, started on first use.
static POOL: Blocking = Blocking::new(KEPT, MOST, IDLE);

/// Hands `job` to one of the runtime's blocking threads, as [`Blocking::run`] does.
pub(super) fn run(job: Job) -> Result<(), Job> {
    POOL.run(job)
}

/// A pool of threads for blocking work, growing while all of them are busy.
pub(super) struct Blocking {
    state: Mutex<State>,
    /// Woken when work is queued for a thread that's waiting.
    ready: Condvar,
    kept: usize,
    most: usize,
    idle: Duration,
}

/// What a pool's threads share, under its lock.
struct State {
    jobs: VecDeque<Job>,
    /// Threads waiting for work, apart from those already promised a job.
    waiting: usize,
    /// Wakes promised to waiting threads, each for a job queued since.
    promised: usize,
    /// Threads started that haven't left.
    threads: usize,
    /// Threads ever started, which names the next.
    started: usize,
}

impl Blocking {
    pub(super) const fn new(kept: usize, most: usize, idle: Duration) -> Self {
        let state = State { jobs: VecDeque::new(), waiting: 0, promised: 0, threads: 0, started: 0 };
        Self { state: Mutex::new(state), ready: Condvar::new(), kept, most, idle }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands `job` to a thread: one waiting for work, or a new one while there are fewer than
    /// `most`. With `most` busy, it waits its turn. Gives the job back only when there's no thread
    /// at all to run it, because none could be started, for the caller to run where it is.
    pub(super) fn run(&'static self, job: Job) -> Result<(), Job> {
        let mut state = self.lock();
        state.jobs.push_back(job);
        if state.waiting > 0 {
            state.waiting -= 1;
            state.promised += 1;
            drop(state);
            self.ready.notify_one();
            return Ok(());
        }
        if state.threads >= self.most {
            return Ok(());
        }
        state.threads += 1;
        let number = state.started;
        state.started += 1;
        drop(state);
        let spawned =
            std::thread::Builder::new().name(format!("fennec-blocking-{number}")).spawn(move || self.work());
        if spawned.is_ok() {
            return Ok(());
        }
        // The job stays queued for the threads there are, and comes back only when there are none.
        let mut state = self.lock();
        state.threads -= 1;
        if state.threads == 0
            && let Some(job) = state.jobs.pop_back()
        {
            return Err(job);
        }
        Ok(())
    }

    /// A thread's life: it runs the queued work, then waits for more. A thread past the first
    /// `kept` leaves once it's waited `idle` with nothing promised to it.
    fn work(&self) {
        let mut state = self.lock();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                drop(state);
                job();
                state = self.lock();
                continue;
            }
            state.waiting += 1;
            loop {
                let (next, waited) =
                    self.ready.wait_timeout(state, self.idle).unwrap_or_else(PoisonError::into_inner);
                state = next;
                // A promise is for a job already queued, which this thread takes, or one a thread
                // that finished its own took first.
                if state.promised > 0 {
                    state.promised -= 1;
                    break;
                }
                if waited.timed_out() && state.threads > self.kept {
                    state.waiting -= 1;
                    state.threads -= 1;
                    return;
                }
            }
        }
    }

    /// How many threads it has, waiting or busy.
    #[cfg(test)]
    fn threads(&self) -> usize {
        self.lock().threads
    }

    /// How many jobs are waiting for a thread.
    #[cfg(test)]
    fn queued(&self) -> usize {
        self.lock().jobs.len()
    }
}

#[cfg(test)]
mod tests;
