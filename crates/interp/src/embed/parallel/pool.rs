//! The work-stealing pool `parallel` runs on, with the `threads` feature. Each worker has a deque
//! of jobs of its own, which it pushes to and pops from at one end while idle workers steal from
//! the other (`crossbeam-deque`'s Chase-Lev deques), and jobs from outside the pool come in
//! through one shared queue. A job owns what it uses, so it's all safe Rust.
//!
//! Each worker takes a thread number and starts a runtime of its own before its first job,
//! through the setup the program gives (`set_worker_setup`). Each range hands what it made over
//! to its call, which the calling thread adopts when the call ends.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

/// Below this many words of items, a loop isn't worth threads, and its ranges run on the
/// calling thread.
const SEQUENTIAL_WORDS: usize = 4096;

/// Each worker's stack. Only what it touches is ever real memory.
const WORKER_STACK_BYTES: usize = 64 << 20;

/// How deep native code on a worker may go, below where the worker starts: the rest of its
/// stack is room for the runtime it calls.
const WORKER_NATIVE_BYTES: usize = 48 << 20;

/// What native code leaves free of a parked task's stack, for the runtime it calls.
const TASK_RUNTIME_BYTES: usize = 256 << 10;

/// How many times an idle worker looks for work again, giving up its thread's turn between
/// looks, before it sleeps. A program that makes many short `parallel` calls in a row then finds
/// its workers still awake, as rayon's do: waking a sleeping worker takes tens of microseconds.
const ROUNDS: u32 = 32;

/// What a worker does before its first job: start a runtime of its own, holding the program. An
/// executable gives it when it starts; with none, every loop runs on the calling thread.
static SETUP: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

static POOL: OnceLock<Option<&'static Pool>> = OnceLock::new();

/// Work for a worker, owning everything it uses.
pub(super) type Job = Box<dyn FnOnce() + Send>;

/// The workers and their queues.
pub(super) struct Pool {
    /// Jobs from threads that aren't this pool's workers.
    injector: Injector<Job>,
    /// The other end of each worker's own deque, to steal from.
    stealers: Vec<Stealer<Job>>,
    /// How many workers are asleep, read without the lock so a push to a busy pool takes none.
    sleeping: AtomicUsize,
    /// A count that goes up whenever there's new work while a worker sleeps, which a worker going
    /// to sleep checks, so it can't miss a job pushed as it does.
    pushed: Mutex<u64>,
    woken: Condvar,
}

thread_local! {
    /// Whether this thread is one of a pool's workers.
    static WORKER: Cell<bool> = const { Cell::new(false) };
    /// This worker's own deque.
    static OWN: RefCell<Option<Worker<Job>>> = const { RefCell::new(None) };
    /// The lowest address native code on this worker may grow its stack to, or 0 off the pool.
    static LIMIT: Cell<usize> = const { Cell::new(0) };
}

/// Gives what each worker runs before its first job. The first one given is kept.
pub fn set_worker_setup(setup: Box<dyn Fn() + Send + Sync>) {
    let _ = SETUP.set(setup);
}

/// Whether workers can start runtimes of their own: an executable says how when it starts.
pub fn has_worker_setup() -> bool {
    SETUP.get().is_some()
}

/// Starts this thread's runtime as a worker's, for a thread that isn't one of the pool's, like
/// a server's I/O worker.
pub fn run_worker_setup() {
    if let Some(setup) = SETUP.get() {
        setup();
    }
}

/// The lowest address native code on this thread may grow its stack to, when it's one of the
/// pool's workers, and 0 otherwise.
pub fn stack_limit() -> usize {
    // A task on a server's I/O worker has a stack of its own, and the runtime needs room below it.
    fennec_parked::stack_low().map_or_else(|| LIMIT.get(), |low| low + TASK_RUNTIME_BYTES)
}

/// How many workers `FENNEC_THREADS` asks for, or as many as there are cores.
fn threads_wanted() -> usize {
    let asked = std::env::var("FENNEC_THREADS").ok().and_then(|threads| threads.trim().parse().ok());
    asked
        .filter(|threads: &usize| *threads > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
}

impl Pool {
    /// A pool of `threads` workers, started now, which live as long as the process.
    fn start(threads: usize) -> Option<&'static Pool> {
        let deques: Vec<Worker<Job>> = (0..threads).map(|_| Worker::new_lifo()).collect();
        let stealers = deques.iter().map(Worker::stealer).collect();
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            injector: Injector::new(),
            stealers,
            sleeping: AtomicUsize::new(0),
            pushed: Mutex::new(0),
            woken: Condvar::new(),
        }));
        for (at, deque) in deques.into_iter().enumerate() {
            let started = std::thread::Builder::new()
                .name(format!("fennec-worker-{at}"))
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || pool.work(at, deque));
            if started.is_err() && at == 0 {
                return None;
            }
        }
        Some(pool)
    }

    /// A worker's life: set up, then run jobs as they come, sleeping when there are none.
    fn work(&'static self, at: usize, deque: Worker<Job>) {
        let here = 0_u8;
        LIMIT.set(std::ptr::from_ref(&here).addr().saturating_sub(WORKER_NATIVE_BYTES));
        super::super::threads::take_index();
        WORKER.set(true);
        OWN.set(Some(deque));
        run_worker_setup();
        loop {
            match self.find(at) {
                Some(job) => job(),
                None => self.sleep(at),
            }
        }
    }

    /// The next job for worker `at`: its own newest, then the shared queue's, then another
    /// worker's oldest.
    fn find(&self, at: usize) -> Option<Job> {
        let own = OWN.with_borrow(|own| own.as_ref().and_then(Worker::pop));
        if own.is_some() {
            return own;
        }
        loop {
            let shared = OWN.with_borrow(|own| match own {
                Some(own) => self.injector.steal_batch_and_pop(own),
                None => self.injector.steal(),
            });
            let stolen = shared.or_else(|| {
                let others = self.stealers.len();
                (1..others)
                    .map(|step| (at + step) % others)
                    .filter_map(|other| self.stealers.get(other))
                    .map(Stealer::steal)
                    .collect()
            });
            match stolen {
                Steal::Success(job) => return Some(job),
                Steal::Empty => return None,
                Steal::Retry => {}
            }
        }
    }

    /// Waits for work: looks again a few times, then sleeps until a job is pushed.
    fn sleep(&self, at: usize) {
        for _ in 0..ROUNDS {
            std::thread::yield_now();
            if !self.is_empty() {
                return;
            }
        }
        let mut pushed = self.pushed.lock().unwrap_or_else(PoisonError::into_inner);
        let seen = *pushed;
        // Saying it sleeps before it looks once more, against `push` adding before it looks at
        // `sleeping`: one of the two sees the other, so a job is never left with every worker asleep.
        self.sleeping.fetch_add(1, Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
        if self.is_empty() && self.stealers.get(at).is_none_or(Stealer::is_empty) {
            while *pushed == seen {
                pushed = self.woken.wait(pushed).unwrap_or_else(PoisonError::into_inner);
            }
        }
        self.sleeping.fetch_sub(1, Ordering::SeqCst);
    }

    fn is_empty(&self) -> bool {
        self.injector.is_empty() && self.stealers.iter().all(Stealer::is_empty)
    }

    /// Adds a job: to this worker's own deque on one of its workers, else to the shared queue.
    pub(super) fn push(&self, job: Job) {
        let left = OWN.with_borrow(|own| match own {
            Some(own) if WORKER.get() => {
                own.push(job);
                None
            }
            _ => Some(job),
        });
        if let Some(job) = left {
            self.injector.push(job);
        }
        std::sync::atomic::fence(Ordering::SeqCst);
        if self.sleeping.load(Ordering::SeqCst) > 0 {
            let mut pushed = self.pushed.lock().unwrap_or_else(PoisonError::into_inner);
            *pushed = pushed.wrapping_add(1);
            self.woken.notify_one();
        }
    }

    /// How many workers it has.
    pub(super) fn threads(&self) -> usize {
        self.stealers.len()
    }
}

/// The shared pool, started on first use.
fn pool() -> Option<&'static Pool> {
    *POOL.get_or_init(|| Pool::start(threads_wanted()))
}

/// Whether `FENNEC_PARALLEL_STRESS` asks every loop to run on the pool, however few its items,
/// and loops and comprehensions to split down to one item a range, for testing what bodies do on
/// workers with programs small enough to test many of.
pub(super) fn stressed() -> bool {
    static STRESSED: OnceLock<bool> = OnceLock::new();
    *STRESSED.get_or_init(|| std::env::var_os("FENNEC_PARALLEL_STRESS").is_some())
}

/// A pool of its own with `threads` workers, set up as the shared pool's are, for tests.
#[cfg(test)]
pub(super) fn with_threads(threads: usize) -> &'static Pool {
    Pool::start(threads).unwrap()
}

/// The pool a call's ranges run on: the shared one, when there are enough words of items to be
/// worth it and workers can have runtimes. None means the calling thread runs every range
/// itself, as a worker does with a call of its own, since it can't lend its blocks while it
/// keeps working.
pub(super) fn for_call(words: usize) -> Option<&'static Pool> {
    if WORKER.get() || !(words >= SEQUENTIAL_WORDS || stressed()) || SETUP.get().is_none() {
        return None;
    }
    pool()
}

/// Runs a call's ranges on `pool`: `start` hands it the first job, and this thread waits until
/// `done` says the last has finished, lending its blocks to the workers meanwhile.
pub(super) fn install(call: &super::Run, start: impl FnOnce(), done: &Latch) {
    // What this thread printed comes before anything the ranges print.
    super::super::write_kept_output();
    *call.lent.write().unwrap_or_else(PoisonError::into_inner) = Some(super::super::threads::lend());
    start();
    done.wait();
    if let Some(lent) = call.lent.write().unwrap_or_else(PoisonError::into_inner).take() {
        super::super::threads::take_back(lent);
    }
}

/// Says when the last of a number of jobs has finished, to the thread waiting for them.
#[derive(Default)]
pub(super) struct Latch {
    /// How many jobs are still to finish.
    left: AtomicUsize,
    finished: Mutex<bool>,
    woken: Condvar,
}

impl Latch {
    /// A latch waiting for one job, which may add more with `more` before it finishes.
    pub(super) fn one() -> Latch {
        Latch { left: AtomicUsize::new(1), ..Latch::default() }
    }

    /// Another job to wait for, added by one still running.
    pub(super) fn more(&self) {
        self.left.fetch_add(1, Ordering::Relaxed);
    }

    /// A job has finished.
    pub(super) fn finished(&self) {
        if self.left.fetch_sub(1, Ordering::AcqRel) == 1 {
            *self.finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.woken.notify_all();
        }
    }

    fn wait(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !*finished {
            finished = self.woken.wait(finished).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Starts a range on a worker, with the blocks lent for the call to read.
pub(super) fn range_starts(lent: Option<std::sync::Arc<crate::embed::table::Table>>) {
    super::super::threads::range_starts(lent);
}

/// Ends a range on a worker, giving back how it changed lent blocks' owners and what it printed.
pub(super) fn range_ends() -> (Vec<(u64, i64)>, Vec<u8>) {
    super::super::threads::range_ends()
}

/// Applies the changes a call's ranges made to the owners of this thread's blocks.
pub(super) fn settle(changes: std::collections::HashMap<u64, i64>) {
    super::super::threads::settle(changes);
}

/// How many chunks a stream over this many words of items runs ahead of its reader, on the
/// pool: twice its threads. 0 when it isn't worth threads, and the reader runs each chunk
/// itself.
pub(super) fn window(words: usize) -> usize {
    let pool = (words >= SEQUENTIAL_WORDS && SETUP.get().is_some()).then(pool).flatten();
    pool.map_or(0, |pool| 2 * pool.threads())
}

/// Runs a task on the pool's workers, when they get to it.
pub(super) fn spawn(task: impl FnOnce() + Send + 'static) {
    match pool() {
        Some(pool) => pool.push(Box::new(task)),
        None => task(),
    }
}
