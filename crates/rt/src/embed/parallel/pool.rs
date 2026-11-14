//! The work-stealing pool `parallel` runs on, with the `threads` feature. Each worker has a deque
//! of jobs of its own, which it pushes to and pops from at one end while idle workers steal from
//! the other (`crossbeam-deque`'s Chase-Lev deques), and jobs from outside the pool come in
//! through one shared queue. A job owns what it uses, so it's all safe Rust.
//!
//! Each worker takes a thread number and starts a runtime of its own before its first job,
//! through the setup the program gives (`set_worker_setup`). Each range hands what it made over
//! to its call, which the calling thread adopts when the call ends.
//!
//! Each worker is a server's I/O worker too, with a poll of its own and the tasks of the
//! connections it serves (see `serve`). Its loop runs tasks that are ready, then jobs, and looks
//! at its poll between them while it serves anything. An idle worker sleeps in its poll, so a
//! socket getting ready, a job pushed, or work sent to it in particular (`run_on`) wakes it.

use std::cell::{Cell, RefCell};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use super::super::serve::Io;

/// Below this many words of items, a loop isn't worth threads, and its ranges run on the
/// calling thread.
const SEQUENTIAL_WORDS: usize = 4096;

/// Each worker's stack. Only what it touches is ever real memory.
const WORKER_STACK_BYTES: usize = 64 << 20;

/// How deep native code on a worker may go, below where the worker starts: the rest of its
/// stack is room for the runtime it calls.
const WORKER_NATIVE_BYTES: usize = 48 << 20;

/// What native code leaves free of a parked task's stack, for the runtime it calls: a quarter of
/// it, at least this, and at most a worker's.
const TASK_RUNTIME_BYTES: usize = 256 << 10;
const WORKER_RUNTIME_BYTES: usize = WORKER_STACK_BYTES - WORKER_NATIVE_BYTES;

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

/// Work for one worker in particular, given what it serves.
pub(in crate::embed) type IoJob = Box<dyn FnOnce(&mut Io) + Send>;

/// What a worker does before its first job, when it isn't the program's own setup.
type Setup = Arc<dyn Fn() + Send + Sync>;

/// The workers and their queues.
pub(in crate::embed) struct Pool {
    /// Jobs from threads that aren't this pool's workers.
    injector: Injector<Job>,
    /// The other end of each worker's own deque, to steal from.
    stealers: Vec<Stealer<Job>>,
    /// Each worker's work sent to it in particular, and how to wake it.
    mail: Vec<Mail>,
    /// How many workers are asleep, read without a lock so a push to a busy pool takes none.
    sleeping: AtomicUsize,
}

/// Work sent to one worker, and how to wake it from its poll.
struct Mail {
    jobs: Mutex<Vec<IoJob>>,
    /// Whether `jobs` has anything, read without the lock on every turn of the worker's loop.
    any: AtomicBool,
    asleep: AtomicBool,
    waker: mio::Waker,
}

thread_local! {
    /// Whether this thread is one of a pool's workers.
    static WORKER: Cell<bool> = const { Cell::new(false) };
    /// This worker's own deque.
    static OWN: RefCell<Option<Worker<Job>>> = const { RefCell::new(None) };
    /// The lowest address native code on this worker may grow its stack to, or 0 off the pool.
    static LIMIT: Cell<usize> = const { Cell::new(0) };
    /// Which pool and which of its workers this thread is, on a worker.
    static HERE: Cell<Option<(&'static Pool, usize)>> = const { Cell::new(None) };
    /// Ranges a job just made, to start as parked tasks once it returns (see `as_task`).
    static STARTING: RefCell<Vec<Box<dyn FnOnce() -> i64>>> = const { RefCell::new(Vec::new()) };
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

/// Sets native code's stack limit on the thread that runs it.
static SET_STACK_LIMIT: OnceLock<extern "C" fn(i64)> = OnceLock::new();

/// `fennec_on_task_resume(set)`: `set` sets this thread's stack limit in native code, and is
/// called each time a task comes back from parking, as other tasks have run here since.
pub extern "C" fn on_task_resume(set: extern "C" fn(i64)) {
    if SET_STACK_LIMIT.set(set).is_ok() {
        fennec_parked::on_resume(stack_limit_again);
    }
}

fn stack_limit_again() {
    if let Some(set) = SET_STACK_LIMIT.get() {
        set(i64::try_from(stack_limit()).unwrap_or(0));
    }
}

/// The lowest address native code on this thread may grow its stack to, when it's one of the
/// pool's workers, and 0 otherwise.
pub fn stack_limit() -> usize {
    // A task has a stack of its own, and the runtime needs room below it: a range's as big as a
    // worker's, a connection's small.
    let room = fennec_parked::stack_size()
        .map_or(TASK_RUNTIME_BYTES, |size| (size / 4).clamp(TASK_RUNTIME_BYTES, WORKER_RUNTIME_BYTES));
    fennec_parked::stack_low().map_or_else(|| LIMIT.get(), |low| low + room)
}

/// How many workers the pool has: as many as the recorded run had, in a replay, so its ranges are
/// cut the same way, and otherwise what `workers_wanted` says.
fn threads_wanted() -> usize {
    crate::replay::workers().filter(|workers| *workers > 0).unwrap_or_else(workers_wanted)
}

/// How many workers `FENNEC_THREADS` asks for, or as many as there are cores.
pub(crate) fn workers_wanted() -> usize {
    let asked = std::env::var("FENNEC_THREADS").ok().and_then(|threads| threads.trim().parse().ok());
    asked
        .filter(|threads: &usize| *threads > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
}

impl Pool {
    /// A pool of `threads` workers, started now, which live as long as the process. Each runs
    /// `setup` first, or the program's own when there's none.
    fn start(threads: usize, setup: Option<&Setup>) -> Option<&'static Pool> {
        let deques: Vec<Worker<Job>> = (0..threads).map(|_| Worker::new_lifo()).collect();
        let stealers = deques.iter().map(Worker::stealer).collect();
        let mut polls = Vec::with_capacity(threads);
        let mut mail = Vec::with_capacity(threads);
        for _ in 0..threads {
            let poll = mio::Poll::new().ok()?;
            let waker = mio::Waker::new(poll.registry(), super::super::serve::WAKER).ok()?;
            polls.push(poll);
            let (any, asleep) = (AtomicBool::new(false), AtomicBool::new(false));
            mail.push(Mail { jobs: Mutex::new(Vec::new()), any, asleep, waker });
        }
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            injector: Injector::new(),
            stealers,
            mail,
            sleeping: AtomicUsize::new(0),
        }));
        for (at, (deque, poll)) in deques.into_iter().zip(polls).enumerate() {
            let setup = setup.cloned();
            let started = std::thread::Builder::new()
                .name(format!("fennec-worker-{at}"))
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || pool.work(at, deque, poll, setup.as_deref()));
            match started {
                Ok(_) => {}
                // With no worker at all, there's no pool, and what wanted one says so.
                Err(_) if at == 0 => return None,
                // Work is already sent to every worker by number, so one missing would leave its
                // share waiting forever: the program stops and says why instead.
                Err(error) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "error: couldn't start worker thread {} of {threads}: {error}",
                        at + 1
                    );
                    crate::embed::aot::stop_now(1);
                }
            }
        }
        Some(pool)
    }

    /// A worker's life: set up, then run tasks and jobs as they come, sleeping when there are
    /// none.
    fn work(
        &'static self,
        at: usize,
        deque: Worker<Job>,
        poll: mio::Poll,
        setup: Option<&(dyn Fn() + Send + Sync)>,
    ) {
        let here = 0_u8;
        LIMIT.set(std::ptr::from_ref(&here).addr().saturating_sub(WORKER_NATIVE_BYTES));
        // The runtime has the rest of the worker's stack: a read into a tree may use most of it.
        crate::embed::heap::reads_may_reach(LIMIT.get().saturating_sub(WORKER_RUNTIME_BYTES / 4 * 3));
        super::super::threads::take_index();
        WORKER.set(true);
        HERE.set(Some((self, at)));
        OWN.set(Some(deque));
        match setup {
            Some(setup) => setup(),
            None => run_worker_setup(),
        }
        let mut io = Io::new(poll, self, at);
        let mut idle = 0;
        loop {
            self.read_mail(at, &mut io);
            io.run_ready();
            io.publish_live(false);
            for range in STARTING.take() {
                io.start_range(range);
            }
            if let Some(job) = self.find(at) {
                let serving = super::super::threads::job_starts();
                job();
                super::super::threads::job_ends(serving);
                for range in STARTING.take() {
                    io.start_range(range);
                }
                idle = 0;
                io.poll_now();
            } else if io.has_ready() {
                idle = 0;
                // A task that made way at a tick is ready again at once: the poll's looked at
                // before it runs on, so a connection or a wake that came meanwhile gets its turn.
                io.poll_now();
            } else if idle < ROUNDS {
                // A program that makes many short `parallel` calls in a row finds its workers awake.
                idle += 1;
                std::thread::yield_now();
                io.poll_now();
            } else {
                // Going idle, its counts are what it holds between bursts of work.
                io.publish_live(true);
                self.sleep(at, &mut io);
                idle = 0;
            }
        }
    }

    /// Runs what's been sent to worker `at` in particular.
    fn read_mail(&self, at: usize, io: &mut Io) {
        let Some(mail) = self.mail.get(at) else { return };
        if !mail.any.load(Ordering::Acquire) {
            return;
        }
        mail.any.store(false, Ordering::Release);
        let jobs = std::mem::take(&mut *mail.jobs.lock().unwrap_or_else(PoisonError::into_inner));
        for job in jobs {
            job(io);
        }
    }

    /// Sends worker `at` work of its own, waking it if it sleeps.
    pub(in crate::embed) fn run_on(&self, at: usize, job: IoJob) {
        let Some(mail) = self.mail.get(at) else { return };
        mail.jobs.lock().unwrap_or_else(PoisonError::into_inner).push(job);
        mail.any.store(true, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        if mail.asleep.load(Ordering::SeqCst) {
            let _ = mail.waker.wake();
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

    /// Sleeps in the worker's poll until a socket is ready, a wait times out, or it's woken for a
    /// job or for mail.
    fn sleep(&self, at: usize, io: &mut Io) {
        let Some(mail) = self.mail.get(at) else { return };
        // Saying it sleeps before it looks once more, against `push` and `run_on` adding before
        // they look at whether it sleeps: one of the two sees the other, so nothing's left with
        // every worker asleep.
        mail.asleep.store(true, Ordering::SeqCst);
        self.sleeping.fetch_add(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        if self.is_empty() && !mail.any.load(Ordering::SeqCst) {
            io.poll(io.timeout());
        }
        self.sleeping.fetch_sub(1, Ordering::SeqCst);
        mail.asleep.store(false, Ordering::SeqCst);
    }

    /// Whether worker `at` has mail, or the pool has jobs waiting, for a task that's run its slice
    /// to make way for.
    pub(in crate::embed) fn has_work_for(&self, at: usize) -> bool {
        self.mail.get(at).is_some_and(|mail| mail.any.load(Ordering::Acquire)) || !self.is_empty()
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
        fence(Ordering::SeqCst);
        if self.sleeping.load(Ordering::SeqCst) > 0
            && let Some(asleep) = self.mail.iter().find(|mail| mail.asleep.load(Ordering::SeqCst))
        {
            let _ = asleep.waker.wake();
        }
    }

    /// How many workers it has.
    pub(in crate::embed) fn threads(&self) -> usize {
        self.stealers.len()
    }
}

/// The pool this thread is a worker of, and which worker, for sending it work later, as a wait
/// that finishes on another thread does.
pub(in crate::embed) fn here() -> Option<(&'static Pool, usize)> {
    HERE.get()
}

/// The shared pool, started on first use.
fn pool() -> Option<&'static Pool> {
    *POOL.get_or_init(|| Pool::start(threads_wanted(), None))
}

/// The pool a server runs on: the shared one, once workers can have runtimes.
pub(in crate::embed) fn for_server() -> Option<&'static Pool> {
    SETUP.get().is_some().then(pool).flatten()
}

/// Whether `FENNEC_PARALLEL_STRESS` asks every loop to run on the pool, however few its items,
/// and loops and comprehensions to split down to one item a range, for testing what bodies do on
/// workers with programs small enough to test many of.
pub(super) fn stressed() -> bool {
    static STRESSED: OnceLock<bool> = OnceLock::new();
    *STRESSED.get_or_init(|| std::env::var_os("FENNEC_PARALLEL_STRESS").is_some())
}

/// Whether a stream's reader waits, for a test, for the workers to run every chunk in its window
/// before it takes the next, set by `FENNEC_STREAM_RUN_AHEAD`. Then chunks always run ahead of the
/// reader, as many as the window holds, however loaded the machine: without it, a reader that gets
/// to a chunk first runs it itself, and under load it can get to every one first.
pub(super) fn streams_run_ahead() -> bool {
    static AHEAD: OnceLock<bool> = OnceLock::new();
    *AHEAD.get_or_init(|| std::env::var_os("FENNEC_STREAM_RUN_AHEAD").is_some())
}

/// Whether this thread is one of the pool's workers.
pub(super) fn on_worker() -> bool {
    WORKER.get()
}

/// A pool of its own with `threads` workers, set up as the shared pool's are, for tests.
#[cfg(test)]
pub(super) fn with_threads(threads: usize) -> &'static Pool {
    Pool::start(threads, None).unwrap()
}

/// A pool of its own with `threads` workers, each running `setup` first, for tests.
#[cfg(test)]
pub(in crate::embed) fn with_setup(threads: usize, setup: &Setup) -> &'static Pool {
    Pool::start(threads, Some(setup)).unwrap()
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

/// The pool a call whose body is heavy runs on, whenever it has two items to share: the shared
/// one, set up, and None on a worker, whose own call runs its ranges itself.
pub(super) fn for_heavy(length: usize) -> Option<&'static Pool> {
    if WORKER.get() || length < 2 || SETUP.get().is_none() {
        return None;
    }
    pool()
}

/// The pool a call whose body may wait runs on, however few its items: the shared one, when workers
/// can have runtimes, and None on a worker, whose own call runs its ranges itself.
pub(super) fn for_waiting() -> Option<&'static Pool> {
    if WORKER.get() || SETUP.get().is_none() {
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
pub(in crate::embed) struct Latch {
    /// How many jobs are still to finish.
    left: AtomicUsize,
    finished: Mutex<bool>,
    woken: Condvar,
}

impl Latch {
    /// A latch waiting for one job, which may add more with `more` before it finishes.
    pub(super) fn one() -> Latch {
        Latch::of(1)
    }

    /// A latch waiting for `count` jobs.
    pub(in crate::embed) fn of(count: usize) -> Latch {
        Latch { left: AtomicUsize::new(count), ..Latch::default() }
    }

    /// Another job to wait for, added by one still running.
    pub(super) fn more(&self) {
        self.left.fetch_add(1, Ordering::Relaxed);
    }

    /// A job has finished.
    pub(in crate::embed) fn finished(&self) {
        if self.left.fetch_sub(1, Ordering::AcqRel) == 1 {
            *self.finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.woken.notify_all();
        }
    }

    /// Whether every job has finished.
    pub(in crate::embed) fn is_done(&self) -> bool {
        *self.finished.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(in crate::embed) fn wait(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !*finished {
            finished = self.woken.wait(finished).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Starts a range on a worker, with the blocks lent for the call to read.
pub(super) fn range_starts(lent: Option<Arc<crate::embed::table::Table>>) {
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

/// Runs a range of a call whose body may wait as a parked task on this worker, so it can park
/// rather than hold its thread: it starts once the job making it returns. Off the pool, it runs
/// here and now.
pub(super) fn as_task(range: impl FnOnce() -> i64 + 'static) {
    if WORKER.get() {
        STARTING.with_borrow_mut(|starting| starting.push(Box::new(range)));
    } else {
        let _ = range();
    }
}

/// Runs a task on the pool's workers, when they get to it.
pub(super) fn spawn(task: impl FnOnce() + Send + 'static) {
    match pool() {
        Some(pool) => pool.push(Box::new(task)),
        None => task(),
    }
}
