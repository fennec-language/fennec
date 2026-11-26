//! The threads that run blocking work for parked tasks: a SQLite call, a file's read or write, a
//! DNS lookup, which has a pool of its own. A task hands its work here and parks, so it doesn't
//! hold its worker while the work blocks.
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
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
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

/// The runtime's pool for blocking work.
pub(super) fn pool() -> &'static Blocking {
    &POOL
}

/// The most threads work that keeps a core busy for long gets at once.
const MOST_HEAVY: usize = 8;

/// The pool for work that keeps a core busy for long, as hashing a password does: a thread to a
/// core, at most `MOST_HEAVY`, and none kept once idle. A hash takes tens of milliseconds and
/// 19 MiB, so a burst of them runs a few at once and the rest wait their turn, their tasks parked,
/// rather than taking every core, and as much memory as there are tasks hashing.
static HEAVY: LazyLock<Blocking> = LazyLock::new(|| {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    Blocking::new(0, cores.min(MOST_HEAVY), IDLE)
});

/// The pool for work that keeps a core busy for long.
pub(super) fn heavy() -> &'static Blocking {
    &HEAVY
}

/// The most names looked up at once. Past them, a lookup waits its turn, its task parked.
const MOST_LOOKUPS: usize = 16;

/// The pool for names' lookups, apart from the rest, with none kept once idle: a name server that
/// never answers holds a lookup's thread for as long as the system's resolver takes, past its task's
/// deadline, so a burst of them holds at most `MOST_LOOKUPS` threads, all of them here, and none of
/// the threads the rest of the program's blocking work runs on.
static LOOKUPS: Blocking = Blocking::new(0, MOST_LOOKUPS, IDLE);

/// The pool for names' lookups.
pub(super) fn lookups() -> &'static Blocking {
    &LOOKUPS
}

/// Starts a thread named `name` running `body`.
type Starter = fn(String, Job) -> std::io::Result<()>;

/// Starts a thread, as the runtime's pool does.
fn start_thread(name: String, body: Job) -> std::io::Result<()> {
    std::thread::Builder::new().name(name).spawn(body).map(drop)
}

/// A pool of threads for blocking work, growing while all of them are busy.
pub(super) struct Blocking {
    state: Mutex<State>,
    /// Woken when work is queued for a thread that's waiting.
    ready: Condvar,
    kept: usize,
    most: usize,
    idle: Duration,
    start: Starter,
}

/// What a pool's threads share, under its lock.
struct State {
    jobs: VecDeque<Job>,
    /// Threads waiting for work, apart from those already promised a job.
    waiting: usize,
    /// Wakes promised to waiting threads, each for a job queued since.
    promised: usize,
    /// Threads running work or waiting for it, not counting those still being started.
    threads: usize,
    /// Threads being started that haven't begun running: a start that fails never does.
    starting: usize,
    /// Threads ever asked for, which names the next.
    started: usize,
}

impl Blocking {
    pub(super) const fn new(kept: usize, most: usize, idle: Duration) -> Self {
        Self::starting_with(kept, most, idle, start_thread)
    }

    /// A pool that starts its threads with `start`, which a test makes fail.
    const fn starting_with(kept: usize, most: usize, idle: Duration, start: Starter) -> Self {
        let state =
            State { jobs: VecDeque::new(), waiting: 0, promised: 0, threads: 0, starting: 0, started: 0 };
        Self { state: Mutex::new(state), ready: Condvar::new(), kept, most, idle, start }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether nothing's running here or about to: no job queued or promised, no thread being
    /// started, and every thread waiting for work. A deadlock is proven only with every pool idle,
    /// as a job here can end in a wake.
    pub(super) fn idle(&self) -> bool {
        let state = self.lock();
        state.jobs.is_empty() && state.promised == 0 && state.starting == 0 && state.waiting == state.threads
    }

    /// Hands `job` to a thread: one waiting for work, or a new one while there are fewer than
    /// `most`. With `most` busy, it waits its turn. Gives back jobs only when no thread is running
    /// or being started to run them, because none could be started, for the caller to run where it
    /// is: all that are queued, so none waits for a thread that won't come.
    pub(super) fn run(&'static self, job: Job) -> Result<(), Vec<Job>> {
        let mut state = self.lock();
        state.jobs.push_back(job);
        if state.waiting > 0 {
            state.waiting -= 1;
            state.promised += 1;
            drop(state);
            self.ready.notify_one();
            return Ok(());
        }
        if state.threads + state.starting >= self.most {
            return Ok(());
        }
        state.starting += 1;
        let number = state.started;
        state.started += 1;
        drop(state);
        if (self.start)(format!("fennec-blocking-{number}"), Box::new(move || self.work())).is_ok() {
            return Ok(());
        }
        // The jobs stay queued for a thread running, or one still being started, which runs them
        // or, failing to start, gives them back. With neither, they come back now.
        let mut state = self.lock();
        state.starting -= 1;
        if state.threads > 0 || state.starting > 0 {
            return Ok(());
        }
        Err(state.jobs.drain(..).collect())
    }

    /// A thread's life: it runs the queued work, then waits for more. A thread past the first
    /// `kept` leaves once it's waited `idle` with nothing promised to it.
    fn work(&self) {
        let mut state = self.lock();
        state.starting -= 1;
        state.threads += 1;
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
