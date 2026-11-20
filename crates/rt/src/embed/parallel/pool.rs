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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering, fence};
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
    /// What each worker published for the deadlock check as it last went to sleep.
    published: Vec<Mutex<crate::embed::deadlock::Published>>,
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
            published: (0..threads).map(|_| Mutex::default()).collect(),
        }));
        crate::embed::deadlock::pool_made(pool);
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
            if io.idle_stuck().is_none() {
                io.poll(io.timeout());
            } else {
                // With nothing left but stuck waits, it sleeps a moment first, as a wait that blocks
                // its thread does (see `deadlock::PATIENCE`), and only then leaves what the deadlock
                // check needs: most sleeps are shorter, and cost the check nothing.
                io.poll(Some(crate::embed::deadlock::PATIENCE));
                if !io.has_ready() && self.is_empty() && !mail.any.load(Ordering::SeqCst) {
                    self.sleep_idle(at, io);
                }
            }
        }
        self.sleeping.fetch_sub(1, Ordering::SeqCst);
        mail.asleep.store(false, Ordering::SeqCst);
    }

    /// Whether any worker has mail it hasn't taken, for the deadlock check: a wake or a stop on its
    /// way. A worker that's taken its mail is awake until the mail has run.
    pub(in crate::embed) fn has_mail(&self) -> bool {
        self.mail.iter().any(|mail| mail.any.load(Ordering::SeqCst))
    }

    /// What each worker published for the deadlock check as it last went to sleep.
    pub(in crate::embed) fn published(&self) -> &[Mutex<crate::embed::deadlock::Published>] {
        &self.published
    }

    /// Wakes worker `at` from its poll, with nothing sent it: for the deadlock check, which asked it
    /// for the notes it left out, which it leaves as it goes to sleep again.
    pub(in crate::embed) fn wake(&self, at: usize) {
        if let Some(mail) = self.mail.get(at) {
            let _ = mail.waker.wake();
        }
    }

    /// Sleeps in the worker's poll, for the deadlock check leaving whether it has nothing but stuck
    /// waits, their notes when a check could pass, and a look, as it may be the last to go to sleep.
    /// Whatever wakes it is mail or a job, which only something still moving sends, or a check
    /// asking for the notes it left out.
    fn sleep_idle(&self, at: usize, io: &mut Io) {
        // With time on its hands, its table gives back what it can.
        crate::embed::with(|state| state.heap.idle());
        let slot = self.published.get(at);
        let idle = io.idle_stuck();
        let left = crate::embed::deadlock::sleeps_stuck(idle.unwrap_or(0));
        if let Some(slot) = slot {
            let notes = io.stuck_notes();
            slot.lock().unwrap_or_else(PoisonError::into_inner).set(idle.is_some(), left.all_stuck(), notes);
        }
        crate::embed::deadlock::look();
        io.poll(io.timeout());
        // For tests: a woken worker held here, its slot still saying it sleeps, as a loaded
        // machine can hold one: the moment the deadlock check's look at the queues is for.
        if let Some(held) = slow_wake() {
            std::thread::sleep(held);
        }
        // Awake, its slot says so and the generation moves before it reads any mail, as it returns
        // to `work`: a check that finds no mail rests on that order (see `deadlock::proven`), as a
        // worker reading mail while its slot still said it slept could let one pass with mail run.
        if let (Some(slot), Some(stuck)) = (slot, idle) {
            slot.lock().unwrap_or_else(PoisonError::into_inner).woke();
            crate::embed::deadlock::woke_idle(stuck);
        }
    }

    /// Whether worker `at` has mail, or the pool has jobs waiting, for a task that's run its slice
    /// to make way for.
    pub(in crate::embed) fn has_work_for(&self, at: usize) -> bool {
        self.mail.get(at).is_some_and(|mail| mail.any.load(Ordering::Acquire)) || !self.is_empty()
    }

    fn is_empty(&self) -> bool {
        self.injector.is_empty() && self.stealers.iter().all(Stealer::is_empty)
    }

    /// Whether any job is queued, for the deadlock check: a job of a waiting `parallel` call that no
    /// worker has taken yet is the program still moving, though nothing counts it.
    pub(in crate::embed) fn has_jobs(&self) -> bool {
        !self.injector.is_empty() || self.stealers.iter().any(|stealer| !stealer.is_empty())
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

    /// Cuts short the waits of the ranges parked on any worker that are the task's told to stop
    /// with `stop`.
    fn cancel_ranges_of(&self, stop: &Arc<AtomicU8>) {
        for at in 0..self.threads() {
            let stop = Arc::clone(stop);
            self.run_on(at, Box::new(move |io: &mut Io| io.cancel_ranges_of(&stop)));
        }
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
/// A replay has it as its recording did, whatever it's run with.
pub(super) fn stressed() -> bool {
    static STRESSED: OnceLock<bool> = OnceLock::new();
    *STRESSED.get_or_init(|| crate::replay::stressed().unwrap_or_else(stress_asked))
}

/// Whether `FENNEC_PARALLEL_STRESS` is set: what a recording notes.
pub(crate) fn stress_asked() -> bool {
    std::env::var_os("FENNEC_PARALLEL_STRESS").is_some()
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
/// worth it, workers can have runtimes, and it has more than one (`shares`). None means the
/// calling thread runs every range itself, as a worker does with a call of its own, since it can't
/// lend its blocks while it keeps working.
pub(super) fn for_call(words: usize) -> Option<&'static Pool> {
    if WORKER.get() || !(words >= SEQUENTIAL_WORDS || stressed()) || SETUP.get().is_none() {
        return None;
    }
    pool().and_then(shares)
}

/// `pool`, if a thread's call shares its ranges with its workers there. A pool of one worker
/// doesn't: the thread only waits while its call runs, so the one worker would run the ranges one
/// after another as the thread can itself, and then hand back every block they made, one at a
/// time, for the thread to take in. `game_tick`'s 4,096 result lists cost it 27M instructions a
/// run that way. So the ranges run on the thread, as a small call's do, cut into the same halves,
/// and what they make is its own at once. A replay starts as many workers as its recording had,
/// so it runs them where the recording did. `FENNEC_PARALLEL_STRESS` still sends every call to the
/// pool, to test what bodies do on workers.
pub(super) fn shares(pool: &'static Pool) -> Option<&'static Pool> {
    (pool.threads() > 1 || stressed()).then_some(pool)
}

/// The pool a call whose body is heavy runs on, whenever it has two items to share: the shared
/// one, set up, with more than one worker (`shares`), and None on a worker, whose own call runs
/// its ranges itself.
pub(super) fn for_heavy(length: usize) -> Option<&'static Pool> {
    if WORKER.get() || length < 2 || SETUP.get().is_none() {
        return None;
    }
    pool().and_then(shares)
}

/// The pool a call whose body may wait runs on, however few its items: the shared one, when workers
/// can have runtimes, and None on a worker, whose own call runs its ranges itself.
pub(super) fn for_waiting() -> Option<&'static Pool> {
    if WORKER.get() || SETUP.get().is_none() {
        return None;
    }
    pool()
}

/// The pool a launched task's call hands its ranges to while the task parks, which of its workers
/// the task is on, and the task's stop: its own worker's pool. None where what's running isn't a launched task
/// that can park, and for a range of such a call, whose own calls run their ranges themselves, as
/// any range's do.
pub(super) fn for_task() -> Option<((&'static Pool, usize), Arc<AtomicU8>)> {
    let (here, stop) = (HERE.get()?, super::super::threads::task_stop()?);
    let spreads = !super::super::threads::in_range_of_task()
        && super::super::serve::can_park()
        && !fennec_parked::is_cancelled();
    spreads.then_some((here, stop))
}

/// Runs a launched task's call's ranges on its worker's pool: `start` hands it the first job, and
/// the task parks until `done` says the last has finished. Its worker runs on meanwhile, the
/// call's ranges among what it runs, so the ranges read what the task copied for them, and never
/// its worker's blocks. A stop wakes the task, which passes it on to its ranges, parked on any
/// worker, and waits on for them: they read what it lent until each has ended.
pub(super) fn install_for_task(
    (pool, at): (&'static Pool, usize),
    start: impl FnOnce(),
    done: &Latch,
    stop: &Arc<AtomicU8>,
) {
    // What the task printed comes before anything the ranges print.
    super::super::write_kept_output();
    start();
    let mut passed_on = 0;
    done.wait_parked((pool, at), || {
        let level = stop.load(Ordering::Acquire);
        if level > passed_on {
            passed_on = level;
            pool.cancel_ranges_of(stop);
        }
    });
}

/// Runs a call's ranges on `pool`: `start` hands it the first job, and this thread waits until
/// `done` says the last has finished, lending its blocks to the workers meanwhile.
pub(super) fn install(call: &super::Run, start: impl FnOnce(), done: &Latch) {
    // Before its blocks are lent, while they're still its own to count.
    crate::embed::live::publish_main();
    // What this thread printed comes before anything the ranges print.
    super::super::write_kept_output();
    *call.lent.write().unwrap_or_else(PoisonError::into_inner) = Some(super::super::threads::lend());
    start();
    done.wait_joined();
    if let Some(lent) = call.lent.write().unwrap_or_else(PoisonError::into_inner).take() {
        super::super::threads::take_back(lent);
    }
}

/// Says when the last of a number of jobs has finished, to the thread waiting for them.
#[derive(Default)]
pub(in crate::embed) struct Latch {
    /// How many jobs are still to finish.
    left: AtomicUsize,
    finished: Mutex<Finished>,
    woken: Condvar,
    /// Whether waiting for it is a stuck wait for the deadlock check: a waiting `parallel` call's
    /// join, whose ranges may wait. A call whose body can't wait, and a server, wait uncounted.
    counted: AtomicBool,
    /// What a join on it waits on, for its note: made only for a waiting call's latch, so a call
    /// whose body can't wait makes nothing for it.
    joined: OnceLock<Arc<Joined>>,
}

/// What a waiting call's join waits on, for the deadlock check: whether every job has finished,
/// set with `all` under the latch's lock, and the lowest range that failed, if one has, with its
/// crash's message if it crashed.
#[derive(Default)]
pub(in crate::embed) struct Joined {
    done: AtomicBool,
    failed: Mutex<Option<(usize, Option<String>)>>,
    /// The call's name in a recording or a replay.
    replay: Option<Arc<str>>,
}

impl crate::embed::deadlock::Awaited for Joined {
    fn holds(&self, _kind: crate::embed::deadlock::Kind) -> bool {
        !self.done.load(Ordering::SeqCst)
    }

    /// A range that failed keeps later ones from starting and wakes nothing, so a sibling waiting
    /// on it waits on, and the call can't end: the report says the failure the call holds, which it
    /// would have passed up at its end (Fable 5.2's review of the design, finding 1). A crash is
    /// said with its message; a failure's value is generated code's, which the runtime can't read.
    fn said(&self, _kind: crate::embed::deadlock::Kind) -> String {
        let failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        match &*failed {
            Some((from, Some(crash))) => {
                format!("for the ranges of its `parallel` call, whose range {from} crashed: {crash}")
            }
            Some((from, None)) => format!("for the ranges of its `parallel` call, whose range {from} failed"),
            None => "for the ranges of its `parallel` call".to_owned(),
        }
    }

    fn replay_place(&self) -> Option<Arc<str>> {
        self.replay.clone()
    }
}

/// For tests, in a debug build: how long a woken idle worker is held before its slot says it's
/// awake, as `FENNEC_TEST_SLOW_WAKE_MS` says. A release build never reads it.
fn slow_wake() -> Option<std::time::Duration> {
    #[cfg(debug_assertions)]
    {
        static HELD: OnceLock<Option<std::time::Duration>> = OnceLock::new();
        *HELD.get_or_init(|| {
            let asked = std::env::var("FENNEC_TEST_SLOW_WAKE_MS").ok()?.trim().parse().ok()?;
            Some(std::time::Duration::from_millis(asked))
        })
    }
    #[cfg(not(debug_assertions))]
    None
}

/// Whether a latch's jobs have all finished, and the task parked waiting for that, if one is.
#[derive(Default)]
struct Finished {
    all: bool,
    /// The pool and worker a task waits on, and the token it's parked on there.
    parked: Option<(&'static Pool, usize, mio::Token)>,
    /// Whether the thread waiting at a waiting call's join is counted as in a stuck wait, which the
    /// last job uncounts as it wakes it.
    waiter_counted: bool,
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
            let parked = {
                let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
                finished.all = true;
                // A join's note stops holding, and a counted waiter is uncounted, under the lock,
                // before it's woken.
                if let Some(joined) = self.joined.get() {
                    joined.done.store(true, Ordering::SeqCst);
                }
                if std::mem::take(&mut finished.waiter_counted) {
                    crate::embed::deadlock::stuck_ended();
                }
                finished.parked.take()
            };
            self.woken.notify_all();
            // Its worker takes its mail only once the task has parked, on that same thread.
            if let Some((pool, at, token)) = parked {
                pool.run_on(at, Box::new(move |io: &mut Io| io.wake_token(token)));
            }
        }
    }

    /// Whether every job has finished.
    pub(in crate::embed) fn is_done(&self) -> bool {
        self.finished.lock().unwrap_or_else(PoisonError::into_inner).all
    }

    pub(in crate::embed) fn wait(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !finished.all {
            finished = self.woken.wait(finished).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Says whether waiting for it is a stuck wait: a waiting call's join, named `replay` in a
    /// recording or a replay.
    pub(super) fn counts_joins(&self, counted: bool, replay: Option<Arc<str>>) {
        self.counted.store(counted, Ordering::Relaxed);
        if counted {
            let _ = self.joined.get_or_init(|| Arc::new(Joined { replay, ..Joined::default() }));
        }
    }

    /// Notes that the range starting at `from` failed, crashing with `crash` or failing with a
    /// value, for a deadlock's report: the lowest such range, whose failure the call keeps.
    pub(super) fn range_failed(&self, from: usize, crash: Option<String>) {
        let Some(joined) = self.joined.get() else { return };
        let mut failed = joined.failed.lock().unwrap_or_else(PoisonError::into_inner);
        if failed.as_ref().is_none_or(|(at, _)| from < *at) {
            *failed = Some((from, crash));
        }
    }

    /// The note of a join on it.
    fn join_note(&self) -> crate::embed::deadlock::Note {
        let joined: Arc<dyn crate::embed::deadlock::Awaited> =
            Arc::<Joined>::clone(self.joined.get_or_init(Arc::default));
        crate::embed::deadlock::Note::parked(joined, crate::embed::deadlock::Kind::Join)
    }

    /// `wait`, as a stuck wait when it's a waiting call's join: uncounted for the deadlock check's
    /// patience first, as every wait that blocks its thread is, then noted and counted under the
    /// latch's own lock, with a look when the counts say to, made with the lock let go. The last job
    /// uncounts it as it wakes it, under the same lock.
    pub(in crate::embed) fn wait_joined(&self) {
        if !self.counted.load(Ordering::Relaxed) {
            return self.wait();
        }
        let until = std::time::Instant::now() + crate::embed::deadlock::PATIENCE;
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !finished.all {
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            finished = self.woken.wait_timeout(finished, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        if finished.all {
            return;
        }
        let look = crate::embed::deadlock::thread_waits(self.join_note().blocking(), false);
        finished.waiter_counted = true;
        if look {
            drop(finished);
            crate::embed::deadlock::look();
            finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        }
        while !finished.all {
            finished = self.woken.wait(finished).unwrap_or_else(PoisonError::into_inner);
        }
        let still_counted = std::mem::take(&mut finished.waiter_counted);
        drop(finished);
        if still_counted {
            crate::embed::deadlock::thread_woke();
        } else {
            crate::embed::deadlock::thread_unnoted();
        }
    }

    /// Waits as a task on worker `at` of `pool` does: parked, woken by the last job as it
    /// finishes, with no looking again meanwhile. Anything else that wakes it, as a stop does, is
    /// told to `woken_early`, and it parks again: the wait ends only when every job has.
    pub(super) fn wait_parked(&self, (pool, at): (&'static Pool, usize), mut woken_early: impl FnMut()) {
        let token = super::super::serve::next_token();
        loop {
            {
                let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
                if finished.all {
                    return;
                }
                finished.parked = Some((pool, at, token));
            }
            // A cancelled task can't park. Only a server's rounds are cancelled, at their server's
            // end, and `for_task` gives them no pool, so this is never reached: it's here so that
            // if it ever were, the wait would block its thread rather than spin.
            if fennec_parked::is_cancelled() {
                self.wait();
                return;
            }
            // A waiting call's join parks with its note, for its worker to count as it sleeps.
            let stuck = self.counted.load(Ordering::Relaxed).then(|| self.join_note());
            let _ = super::super::serve::park_through_a_stop(token, stuck);
            if !self.is_done() {
                woken_early();
            }
        }
    }
}

pub(super) use super::super::threads::{
    BeforeRange, Calling, range_off_its_caller_ends, range_off_its_caller_starts,
};

/// Whether what's running here is a range of a launched task that's been stopped: if so it fails,
/// as stopped, as any wait it came to would make it.
pub(super) fn cut_short() -> bool {
    super::super::serve::cut_short()
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

/// Names the range of a waiting call running here `name`, for a deadlock's report, giving back the
/// name before, to put back as it ends.
pub(super) fn name_range(
    name: Option<crate::embed::deadlock::RangeName>,
) -> Option<crate::embed::deadlock::RangeName> {
    super::super::threads::name_range(name)
}

/// Runs a range of a call whose body may wait as a parked task on this worker, so it can park
/// rather than hold its thread: it starts once the job making it returns. Off the pool, it runs
/// here and now.
pub(super) fn as_task(range: impl FnOnce() -> i64 + 'static) {
    if WORKER.get() {
        // A context from now, before the job making it returns, until the range ends.
        let context = crate::embed::deadlock::Context::began();
        STARTING.with_borrow_mut(|starting| {
            starting.push(Box::new(move || {
                let ended = range();
                drop(context);
                ended
            }));
        });
    } else {
        let _ = range();
    }
}

/// Runs a task on the pool's workers, when they get to it. It's a context for the deadlock check
/// from now until it has run: unlike a range of a `parallel` call, whose caller waits for it,
/// counted as running, nothing that runs need wait for a stream's chunk.
pub(super) fn spawn(task: impl FnOnce() + Send + 'static) {
    match pool() {
        Some(pool) => {
            let context = crate::embed::deadlock::Context::began();
            pool.push(Box::new(move || {
                task();
                drop(context);
            }));
        }
        None => task(),
    }
}
