//! `parallel(xs)` for native code: a loop body, a comprehension and a reduction run over a
//! collection's positions, split into ranges.
//!
//! A range is halved until it's no bigger than a chunk picked from the item's size, so the
//! ranges depend on the length alone, and a reduction's partials combine in the same order
//! whether one thread runs them or many. With the `threads` feature, the ranges of a loop big
//! enough to be worth it run on a work-stealing pool, each worker with its own runtime (see
//! `threads`). Otherwise they run one after another on the calling thread, which gives the
//! same results.
//!
//! Native code gives a body as a function of `(env, from, to, thread)`, where `env` holds what
//! the body reads from around it and `thread` is the running thread's number, for counting
//! (see `threads`). A body gives back 0, or anything else when it failed, with the failure in
//! its thread's runtime. Once one fails, no range after it starts, but those before it still
//! run, and on the pool ones after it that another thread already started finish: the failure
//! passed up is the lowest range's that fails, so it's the same however many threads run.
//!
//! What bodies print follows real call order, as decisions.md has it: each print whole, one
//! range's prints in order, and everything printed after the call after all of it. A worker
//! writes what a range printed as the range ends, so across ranges the order can differ between
//! runs. Where the calling thread's runtime keeps its output for a host rather than writing it, as
//! a test's does, the ranges' text is handed to it whole, by where each range starts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use fennec_syntax::Diagnostic;

use super::{State, attempt, with};

/// A loop body over the positions `from..to`.
pub type Body = extern "C" fn(env: i64, from: i64, to: i64, thread: i64) -> i64;

/// A comprehension's body, which writes each position's item into `out`, a list the runtime
/// made with room for all of them.
pub type MapBody = extern "C" fn(env: i64, from: i64, to: i64, thread: i64, out: i64) -> i64;

/// A reduction's body, which writes the range's partial into `partial`.
pub type ReduceBody = extern "C" fn(env: i64, from: i64, to: i64, thread: i64, partial: &mut i64) -> i64;

/// Combines two partials, the earlier range's first, into `out`.
pub type Combine = extern "C" fn(env: i64, a: i64, b: i64, out: &mut i64) -> i64;

/// The most bytes of items a range is split down to.
const CHUNK_BYTES: usize = 16 * 1024;

/// How many positions a range is split down to, for items of `words` words.
fn chunk(words: usize) -> usize {
    if pool::stressed() {
        return 1;
    }
    (CHUNK_BYTES / (8 * words.max(1))).max(1)
}

/// How many ranges a call whose body is heavy is cut into at least, however short its list: a
/// body with a loop or a call in it can take long on few items, so they're shared among the pool's
/// workers. It's fixed, not the number of workers, so the cut is the same on every machine.
const HEAVY_RANGES: usize = 64;

/// What a `parallel` call's body is like, as the compiler saw it, which says how its list is cut
/// and whether it goes to the pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Weight {
    /// Straight-line work: ranges of [`CHUNK_BYTES`] of items, on the pool when there are enough.
    Light,
    /// A loop or a call: at least [`HEAVY_RANGES`] ranges, on the pool whenever there are two items.
    Heavy,
    /// A body that may wait: an item a range, always on the pool, each range a task that can park.
    Waiting,
}

impl Weight {
    /// How many positions a range of a list `length` long, with items of `words` words, is split
    /// down to.
    fn chunk(self, length: usize, words: usize) -> usize {
        match self {
            Weight::Light => chunk(words),
            Weight::Heavy => chunk(words).min(length.div_ceil(HEAVY_RANGES)).max(1),
            Weight::Waiting => 1,
        }
    }

    /// The pool a call of `length` items of `words` words runs on, or None to run it here.
    fn pool(self, length: usize, words: usize) -> Option<&'static Pool> {
        match self {
            Weight::Light => pool::for_call(length * words),
            Weight::Heavy => pool::for_heavy(length),
            Weight::Waiting => pool::for_waiting(),
        }
    }
}

fn number(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// How many positions a list has and how many words each takes.
fn extent(state: &State, list: i64) -> Option<(usize, usize)> {
    let block = state.heap.block(list.cast_unsigned())?;
    let element = super::lists::element_in(state, block)?;
    let length = usize::try_from(*block.get(2)?).ok()?;
    Some((length, element.words()))
}

/// One `parallel` call's ranges: where they stop, and the lowest failure.
struct Run {
    chunk: usize,
    /// The thread that started the call, whose runtime adopts what the ranges make, when it waits
    /// for them as a thread does, lending its own blocks. `None` for a launched task's call on the
    /// pool: the task parks, and a range on any thread, its own worker's too, reads the copy the
    /// task lent and hands over what it makes.
    caller: Option<u16>,
    /// What its ranges take with them from the code that made it, when they run elsewhere: they're
    /// that code's, so what cuts its waits short cuts theirs.
    calling: pool::Calling,
    /// Where the lowest range that failed so far starts. Ranges after it don't start, and
    /// ranges before it still run, since one of them failing is the failure passed up.
    stop: AtomicUsize,
    /// In a replay, whether the recording said a stop or a cancel cut a range short: the code
    /// that made the call was then, and its end is to be as quiet as the run's (#1949).
    pieces_stopped: AtomicBool,
    /// Which ranges a failure kept from starting, each by where it starts and ends, as they were
    /// split: in a recording, noted to log when the call ends, and in a replay, those the recording
    /// says. Which had begun when the failure was noted depends on timing, so a replay skips
    /// exactly those its recording did, whatever its own timing, and runs every other (#1804).
    skipped: Skipped,
    /// Which ranges a stop of the launched task making the call kept from beginning, as `skipped`
    /// has a failure's.
    not_begun: Skipped,
    /// The failure from the lowest range that failed, by where it starts.
    failure: Mutex<Option<(usize, Failure)>>,
    /// The values of failures a lower range's beat, to let go of once they're the caller's.
    beaten: Mutex<Vec<u64>>,
    /// The call, as a task of the one making it, for naming its ranges' entries in a recording or a
    /// replay. `None` in a normal run.
    task: Option<crate::replay::Task>,
    /// The capture of the test making the call, if a test is, for what its ranges print.
    capture: Option<Arc<crate::fakes::Capture>>,
    /// The calling thread's blocks, lent to the ranges while it waits, when they're on the pool.
    lent: RwLock<Option<Arc<super::table::Table>>>,
    /// What each range made on a worker that outlives it, for the calling thread to adopt.
    made: Mutex<Vec<Box<[u64]>>>,
    /// How the ranges changed the owners of the calling thread's blocks, for it to apply.
    owners: Mutex<HashMap<u64, i64>>,
    /// What each range printed on a worker that keeps its output rather than writing it, whole,
    /// by where it starts, for the calling thread's runtime to take in that order. A worker in an
    /// executable writes as it goes, so this is empty there.
    printed: Mutex<Vec<(usize, Vec<u8>)>>,
    /// Says when the last range on the pool has finished, to the calling thread waiting for it.
    done: Latch,
    /// A reduction's result, when its ranges ran on the pool: None if it failed.
    reduced: Mutex<Option<i64>>,
    /// Whether its body may wait, so each range on the pool runs as a parked task.
    waiting: bool,
    /// For a call whose body may wait, how a deadlock's report names it, as "main's `parallel`
    /// call", for naming its join and its ranges.
    named: Option<Arc<super::deadlock::CallName>>,
}

/// Which of a call's ranges a failure kept from starting (see `Run::skipped`), or a stop (see
/// `Run::not_begun`).
enum Skipped {
    /// Neither a recording nor a replay: what's live says.
    Off,
    /// A recording: each kept back, as noted, logged under the call as it was (`note`).
    Noted(Mutex<Vec<(usize, usize)>>),
    /// A replay: those its recording logged, and only those.
    Replayed(std::collections::HashSet<(usize, usize)>),
}

/// What a recording logs under a `parallel` call: the ranges a failure kept from starting, each
/// as where it starts and ends.
const SKIPPED: &str = "skipped";

/// What a recording logs under a `parallel` call: the ranges a stop of the launched task making it
/// kept from beginning, each as where it starts and ends.
const NOT_BEGUN: &str = "not begun";

impl Skipped {
    /// For the call named `call`, in a recording or a replay, as logged under `entry`. A replay of a
    /// call its recording logged nothing for keeps nothing from starting: one with no failure or
    /// stop, or from a recording older than this.
    fn for_call(call: Option<&crate::replay::Task>, entry: &str) -> Skipped {
        let Some(call) = call else { return Skipped::Off };
        if !crate::replay::replaying() {
            return Skipped::Noted(Mutex::new(Vec::new()));
        }
        // Every entry logged for the call: a range is logged as it's kept back, so a run that never
        // comes to the call's end, as one that deadlocks in it, still has them (Fable 5.2's review of
        // deadlock record and replay, 2026-11-20). A log from before held them all in one entry.
        let mut recorded = Vec::new();
        while let Some(more) = crate::replay::logged_numbers(&call.name(), entry) {
            recorded.extend(more);
        }
        Skipped::Replayed(
            recorded
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|[from, to]| Some((usize::try_from(*from).ok()?, usize::try_from(*to).ok()?)))
                .collect(),
        )
    }

    /// In a recording, notes that the range `range` was kept back, and logs it under the call, as
    /// `entry`, at once and once: a call a deadlock stops never comes to its end, where it was logged,
    /// and its replay then started every one of them.
    fn note(&self, call: Option<&crate::replay::Task>, entry: &str, range: (usize, usize)) {
        let Skipped::Noted(noted) = self else { return };
        let mut noted = noted.lock().unwrap_or_else(PoisonError::into_inner);
        if noted.contains(&range) {
            return;
        }
        noted.push(range);
        if let Some(call) = call {
            let pair =
                [u64::try_from(range.0).unwrap_or(u64::MAX), u64::try_from(range.1).unwrap_or(u64::MAX)];
            crate::replay::log_numbers(&call.name(), entry, &pair);
        }
    }
}

/// How a range failed.
enum Failure {
    /// It crashed, with this.
    Crash(Diagnostic),
    /// It failed with a `fails` error, this value, which passes up out of the call.
    Value(u64),
}

/// What a range's body gives back when it failed with a value, handed over by `fennec_task_failed`
/// first. 1 is a crash, with the runtime's failure set.
const RANGE_FAILED: i64 = 2;

thread_local! {
    /// The failure the last `parallel` call on this thread passed up, for `fennec_parallel_failure`.
    static PASSED_UP: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `fennec_parallel_failure()`: the value of the failure the last `parallel` call on this thread
/// gave back 2 for, the caller's own.
pub extern "C" fn parallel_failure() -> i64 {
    i64::try_from(PASSED_UP.take()).unwrap_or(0)
}

/// What a loop's or a comprehension's range runs: copied into each job, so it owns what it uses.
#[derive(Clone, Copy)]
enum Leaf {
    For(Body, i64),
    /// A comprehension's body, its `env`, and the list it writes into.
    Map(MapBody, i64, i64),
}

impl Leaf {
    fn run(self, from: usize, to: usize, thread: i64) -> i64 {
        match self {
            Leaf::For(body, env) => body(env, number(from), number(to), thread),
            Leaf::Map(body, env, out) => body(env, number(from), number(to), thread, out),
        }
    }
}

/// A reduction's range that's split in two on the pool: the partials of its halves, as they
/// finish, and where its own goes once it's combined from them.
struct Node {
    from: usize,
    /// Where its range ends: with where it starts, what names its combining in a recording, as
    /// nodes down a left side start at the same place.
    to: usize,
    halves: Mutex<[Half; 2]>,
    up: Option<(Arc<Node>, usize)>,
}

/// Where one half of a split reduction is.
/// A range that's started on a thread: where what it makes starts, what was running before it, in a
/// recording or a replay, and where it starts.
struct Started {
    /// Where its thread's list of what it made was as it started, or `None` for a range run as a
    /// task of its own, which lists as a launched task does and hands over all it holds.
    since: Option<usize>,
    was: Option<crate::replay::Task>,
    from: usize,
    /// The capture what was running before printed into, put back as the range ends.
    capture: Option<Arc<crate::fakes::Capture>>,
    /// What ran on the thread before a range that runs off its caller's, put back as it ends.
    before: Option<pool::BeforeRange>,
}

#[derive(Clone, Copy)]
enum Half {
    Running,
    /// Its partial, or None if it failed.
    Done(Option<i64>),
}

impl Run {
    fn new(chunk: usize) -> Run {
        let task = crate::replay::child();
        let skipped = Skipped::for_call(task.as_ref(), SKIPPED);
        let not_begun = Skipped::for_call(task.as_ref(), NOT_BEGUN);
        Run {
            skipped,
            not_begun,
            chunk,
            caller: Some(super::threads::index()),
            calling: pool::Calling::here(),
            task,
            capture: super::capture_here(),
            stop: AtomicUsize::new(usize::MAX),
            pieces_stopped: AtomicBool::new(false),
            failure: Mutex::new(None),
            beaten: Mutex::new(Vec::new()),
            lent: RwLock::new(None),
            made: Mutex::new(Vec::new()),
            owners: Mutex::new(HashMap::new()),
            printed: Mutex::new(Vec::new()),
            done: Latch::one(),
            reduced: Mutex::new(None),
            waiting: false,
            named: None,
        }
    }

    /// The same call, for a body that may wait or not. A loop's or a comprehension's ranges are
    /// then an item each; a reduction's leaves stay as its length gives them.
    fn waiting(mut self, waiting: bool) -> Run {
        self.waiting = waiting;
        self
    }

    /// For a call about to go to a pool: when its body can wait, its join is a stuck wait for the
    /// deadlock check, and its ranges are named in a report. It's done here, and not in `waiting`,
    /// which every call goes through: a call whose body can't wait can't be part of a deadlock, and
    /// one that runs where it's made has no join, so neither pays for one.
    fn joins_on_a_pool(&mut self) {
        if self.waiting {
            self.done.counts_joins(true, self.task.as_ref().map(crate::replay::Task::name));
            self.named = Some(super::deadlock::CallName::here());
        }
    }

    /// For a range of a waiting call, starting at `from`, how a deadlock's report names it.
    fn range_named(&self, from: usize) -> Option<super::deadlock::RangeName> {
        self.named.as_ref().map(|call| super::deadlock::RangeName { call: Arc::clone(call), from })
    }

    /// The same call, made by the launched task told to stop with `stop`, for the pool: its
    /// ranges read `lent`, the copy of what they capture that the task made for them.
    fn of_task(mut self, stop: Arc<AtomicU8>, lent: Arc<super::table::Table>) -> Run {
        self.caller = None;
        self.calling = self.calling.of_task(stop);
        self.lent = RwLock::new(Some(lent));
        // A task's call always goes to its worker's pool.
        self.joins_on_a_pool();
        self
    }

    /// Whether this thread is the one that made the call and waits for it, whose own blocks the
    /// ranges read: a range it runs itself has nothing lent and hands nothing over.
    fn on_caller(&self) -> bool {
        self.caller == Some(super::threads::index())
    }

    /// Whether a stop of the launched task making the call keeps the range from `from` to `to` from
    /// beginning, failing it as stopped. When it comes depends on timing, so a recording notes
    /// each, and a replay keeps from beginning exactly those, whatever its own stop says (#1916).
    fn stop_keeps_from_beginning(&self, range: (usize, usize)) -> bool {
        match &self.not_begun {
            Skipped::Off => pool::cut_short(),
            Skipped::Noted(_) => {
                let cut = pool::cut_short();
                if cut {
                    self.not_begun.note(self.task.as_ref(), NOT_BEGUN, range);
                }
                cut
            }
            Skipped::Replayed(recorded) => {
                let cut = recorded.contains(&range);
                if cut {
                    crate::replay::stopped_as_recorded();
                }
                cut
            }
        }
    }

    /// Whether the range from `from` to `to`, as the call split it, is kept from starting: by a
    /// failure in a range before it, or in a replay, by its recording, which says which were.
    fn skips(&self, (from, to): (usize, usize)) -> bool {
        match &self.skipped {
            Skipped::Off => from > self.stop.load(Ordering::Relaxed),
            Skipped::Noted(_) => {
                let skipped = from > self.stop.load(Ordering::Relaxed);
                if skipped {
                    self.skipped.note(self.task.as_ref(), SKIPPED, (from, to));
                }
                skipped
            }
            Skipped::Replayed(recorded) => recorded.contains(&(from, to)),
        }
    }

    /// Notes that the range starting at `from` failed, as its body's `code` says: with a value
    /// it handed over, or with the failure in this thread's runtime. The lowest range's is kept.
    fn failed(&self, from: usize, code: i64) {
        // Its locks stay held, with what they guard half changed, and other bodies may be waiting
        // for them, so the program ends now rather than wait for every body to finish.
        if super::threads::holds_locks() {
            super::aot::stop_for_failure();
        }
        self.stop.fetch_min(from, Ordering::Relaxed);
        let value = if code == RANGE_FAILED { super::serve::take_failed_with() } else { None };
        let found = match value {
            Some(value) => Some(Failure::Value(value)),
            None => with(|state| state.failure.take().map(Failure::Crash)),
        };
        super::set_failed(0);
        let Some(found) = found else { return };
        // A waiting call's failure is said in a deadlock's report, should a sibling of this range
        // wait on it for good: said here, on the thread whose runtime holds it.
        if self.waiting {
            let crash = match &found {
                Failure::Crash(diagnostic) => Some(diagnostic.message.clone()),
                Failure::Value(_) => None,
            };
            self.done.range_failed(from, crash);
        }
        let mut kept = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        let beaten = if kept.as_ref().is_none_or(|(at, _)| from < *at) {
            kept.replace((from, found)).map(|(_, beaten)| beaten)
        } else {
            Some(found)
        };
        drop(kept);
        if let Some(Failure::Value(beaten)) = beaten {
            self.beaten.lock().unwrap_or_else(PoisonError::into_inner).push(beaten);
        }
    }

    /// Starts the range at `from` on this thread, giving back where what it makes starts, and what
    /// was running before it, in a recording or a replay, for `keep_made` to put back.
    fn range_starts(&self, from: usize, own: bool) -> Started {
        let before = (!self.on_caller()).then(|| {
            pool::range_starts(self.lent.read().unwrap_or_else(PoisonError::into_inner).clone());
            pool::range_off_its_caller_starts(&self.calling)
        });
        // In a recording or a replay, what was running printed goes out first, under its own name,
        // as the range's goes out under the range's as it ends: a range run on its caller's thread
        // wrote what its caller had kept as its own, so where that was cut depended on whether the
        // caller had waited, which a replay doesn't. A replay then held the caller's bytes for a
        // burst its range wrote, and wrote nothing more (the nightly's random programs).
        if crate::replay::active() {
            super::write_kept_output();
        }
        let mut was = crate::replay::range(self.task.as_ref(), from);
        crate::replay::swap(&mut was);
        let mut capture = self.capture.clone();
        super::swap_capture(&mut capture);
        // A range run as a task of its own lists what it makes as a launched task does, letting the
        // list go once it's long, and is found by its mark as it ends: marked, a range's list was
        // kept whole for as long as it ran, a word for every block it made (#1842).
        let since = (!own).then(super::threads::made_scope_starts);
        Started { since, was, from, capture, before }
    }

    /// Keeps what the range just run on this thread made since `since`, and how it changed the
    /// owners of the calling thread's blocks, when this thread is a worker.
    fn keep_made(&self, Started { since, mut was, from, mut capture, before }: Started) {
        // In a recording or a replay, what the range printed goes out as its own, under its name,
        // before what was running is put back.
        if crate::replay::active() {
            super::write_kept_output();
        }
        crate::replay::swap(&mut was);
        if was.as_ref().is_some_and(crate::replay::Task::was_stopped) {
            self.pieces_stopped.store(true, Ordering::Release);
        }
        super::swap_capture(&mut capture);
        if since.is_some() {
            super::threads::made_scope_ends(self.on_caller());
        }
        if let Some(before) = before {
            pool::range_off_its_caller_ends(before);
        }
        if !self.on_caller() {
            // Most ranges make nothing that outlives them, and every worker shares these locks.
            let made = match since {
                Some(since) => super::threads::hand_over(since),
                None => super::threads::hand_over_all(),
            };
            if !made.is_empty() {
                self.made.lock().unwrap_or_else(PoisonError::into_inner).extend(made);
            }
            let (changes, printed) = pool::range_ends();
            if !printed.is_empty() {
                self.printed.lock().unwrap_or_else(PoisonError::into_inner).push((from, printed));
            }
            if !changes.is_empty() {
                let mut owners = self.owners.lock().unwrap_or_else(PoisonError::into_inner);
                for (address, by) in changes {
                    *owners.entry(address).or_default() += by;
                }
            }
        }
    }

    /// Runs a range's `body` on this thread, between its start and its end, and gives back how the
    /// body ended. On a thread other than the one its caller waits on:
    /// - A range of a launched task's call whose body may wait, that hasn't begun when the task is
    ///   stopped, doesn't begin: it fails as stopped. The task is parked in that call, waiting for
    ///   what its ranges wait for, and a stop cuts short the wait a task is in. So the call fails
    ///   as stopped even if a higher range has failed with a value of its own, and the task ends
    ///   quietly, as a stopped task does. A call whose body never waits is no wait: its ranges
    ///   all run, as the task's own code would have run on to its next wait.
    /// - A body that failed may have left the runtime holding temporaries of the call it was in,
    ///   as one stopped while it waited does, and one may hold an owner of a block the range
    ///   made. They're let go of here, while the block is still this thread's to count: once
    ///   it's handed over, letting go of it would reach nothing, and the block would never go.
    fn ran(&self, (from, to): (usize, usize), body: impl FnOnce() -> i64) -> i64 {
        // Taken, so a range of a call the body makes, run here, isn't taken for its own task.
        let (flag, own) = super::threads::take_own_range();
        let started = self.range_starts(from, own);
        let off_its_caller = started.before.is_some();
        let code = if off_its_caller && self.waiting && self.stop_keeps_from_beginning((from, to)) {
            1
        } else {
            body()
        };
        // The generators it let go of partway that wait to be closed are closed as it ends, as a
        // task's are, where it ran rather than in whatever this thread runs next.
        super::heap::run_abandons();
        if code != 0 && off_its_caller {
            with(|state| {
                state.temps.clear();
                state.let_go();
            });
        }
        self.keep_made(started);
        super::threads::put_own_range(flag);
        code
    }

    /// Passes the failure up, in the calling thread's runtime, after adopting what the ranges
    /// made: 1 if there was one, else 0.
    fn finish(self) -> i64 {
        if self.pieces_stopped.load(Ordering::Acquire) {
            crate::replay::mark_stopped();
        }
        let made = self.made.into_inner().unwrap_or_else(PoisonError::into_inner);
        if self.calling.is_task_s() {
            // A task's own, as what it makes itself is: listed, for it to hand over when it ends.
            with(|state| state.heap.adopt_made(made));
        } else {
            super::threads::adopt(made);
        }
        pool::settle(self.owners.into_inner().unwrap_or_else(PoisonError::into_inner));
        let mut printed = self.printed.into_inner().unwrap_or_else(PoisonError::into_inner);
        printed.sort_by_key(|(from, _)| *from);
        super::print_handed_over(&printed.into_iter().flat_map(|(_, text)| text).collect::<Vec<u8>>());
        // Theirs now, as what the ranges made is.
        let beaten = self.beaten.into_inner().unwrap_or_else(PoisonError::into_inner);
        if !beaten.is_empty() {
            with(|state| {
                for value in beaten {
                    super::heap::release(state, value);
                }
            });
        }
        let failure = self.failure.into_inner().unwrap_or_else(PoisonError::into_inner);
        match failure {
            Some((_, Failure::Crash(failure))) => {
                attempt(|_: &mut State| -> Result<i64, Diagnostic> { Err(failure) }).max(1)
            }
            Some((_, Failure::Value(value))) => {
                PASSED_UP.set(value);
                RANGE_FAILED
            }
            None => 0,
        }
    }

    /// Runs `leaf` on each range of `from..to`, halving down to the chunk.
    fn each(&self, (from, to): (usize, usize), leaf: &(dyn Fn(usize, usize, i64) -> i64 + Sync)) {
        if from >= to || self.skips((from, to)) {
            return;
        }
        if to - from <= self.chunk {
            let code = self.ran((from, to), || leaf(from, to, i64::from(super::threads::index())));
            if code != 0 {
                self.failed(from, code);
            }
            return;
        }
        let middle = from + (to - from) / 2;
        self.each((from, middle), leaf);
        self.each((middle, to), leaf);
    }

    /// Starts a loop's or a comprehension's ranges on the pool, from the calling thread.
    fn each_on_start(self: &Arc<Self>, pool: &'static Pool, length: usize, leaf: Leaf) {
        let run = Arc::clone(self);
        pool.push(Box::new(move || run.each_on(pool, (0, length), leaf)));
    }

    /// Runs one range of a call on the pool: halving it as `each` does, it hands each right half
    /// to the pool and goes on with the left, so the ranges are the same as on one thread.
    fn each_on(self: &Arc<Self>, pool: &'static Pool, (from, mut to): (usize, usize), leaf: Leaf) {
        while to - from > self.chunk && !self.skips((from, to)) {
            let middle = from + (to - from) / 2;
            self.done.more();
            let run = Arc::clone(self);
            pool.push(Box::new(move || run.each_on(pool, (middle, to), leaf)));
            to = middle;
        }
        if self.waiting {
            // A body that may wait runs as a parked task, which ends the range when it's done.
            let run = Arc::clone(self);
            pool::as_task(move || {
                let before = pool::name_range(run.range_named(from));
                run.leaf_on((from, to), leaf);
                pool::name_range(before);
                run.done.finished();
                0
            });
            return;
        }
        self.leaf_on((from, to), leaf);
        self.done.finished();
    }

    /// Runs one range of a loop or a comprehension on this thread, as it was split.
    fn leaf_on(&self, (from, to): (usize, usize), leaf: Leaf) {
        if from < to && !self.skips((from, to)) {
            let code = self.ran((from, to), || leaf.run(from, to, i64::from(super::threads::index())));
            if code != 0 {
                self.failed(from, code);
            }
        }
    }

    /// Runs one range of a reduction on the pool: halving it as `reduce` does, it hands each
    /// right half to the pool and goes on with the left. Each half's partial goes to its range
    /// above, and whichever half finishes second combines them, so they combine in the same tree
    /// as on one thread.
    fn reduce_on(
        self: &Arc<Self>,
        pool: &'static Pool,
        (from, mut to): (usize, usize),
        mut up: Option<(Arc<Node>, usize)>,
        steps: (ReduceBody, Combine, i64),
    ) {
        while to - from > self.chunk && !self.skips((from, to)) {
            let middle = from + (to - from) / 2;
            let node = Arc::new(Node { from, to, halves: Mutex::new([Half::Running; 2]), up: up.take() });
            let (run, right) = (Arc::clone(self), Some((Arc::clone(&node), 1)));
            pool.push(Box::new(move || run.reduce_on(pool, (middle, to), right, steps)));
            up = Some((node, 0));
            to = middle;
        }
        if self.waiting {
            let run = Arc::clone(self);
            pool::as_task(move || {
                let before = pool::name_range(run.range_named(from));
                let partial = run.reduce((from, to), steps);
                pool::name_range(before);
                run.reduced_up(up, partial, steps);
                0
            });
            return;
        }
        let partial = self.reduce((from, to), steps);
        self.reduced_up(up, partial, steps);
    }

    /// Passes a range's partial up: combined with its other half's once that's in too, all the
    /// way to the whole, which ends the call.
    fn reduced_up(
        &self,
        mut up: Option<(Arc<Node>, usize)>,
        mut partial: Option<i64>,
        steps: (ReduceBody, Combine, i64),
    ) {
        while let Some((node, side)) = up {
            let both = {
                let mut halves = node.halves.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(half) = halves.get_mut(side) {
                    *half = Half::Done(partial);
                }
                match *halves {
                    [Half::Done(left), Half::Done(right)] => Some((left, right)),
                    _ => None,
                }
            };
            // The other half isn't in yet, and its thread goes on up from here when it is.
            let Some((left, right)) = both else { return };
            partial = self.combined((node.from, node.to), left, right, steps);
            up = node.up.as_ref().map(|(above, side)| (Arc::clone(above), *side));
        }
        *self.reduced.lock().unwrap_or_else(PoisonError::into_inner) = partial;
        self.done.finished();
    }

    /// Two halves' partials combined, the earlier first: None if either failed, or this does.
    fn combined(
        &self,
        (from, to): (usize, usize),
        left: Option<i64>,
        right: Option<i64>,
        steps: (ReduceBody, Combine, i64),
    ) -> Option<i64> {
        let (_, combine, env) = steps;
        // A half that failed leaves nothing to combine, and no range is started for it: one
        // started and left unended would keep what was lent for the call on its thread.
        let (left, right) = (left?, right?);
        let mut combined = 0;
        let code = self.ran((from, to), || combine(env, left, right, &mut combined));
        if code != 0 {
            self.failed(from, code);
            return None;
        }
        Some(combined)
    }

    /// A reduction over `from..to`: each range's partial, combined with the next in order.
    fn reduce(&self, (from, to): (usize, usize), steps: (ReduceBody, Combine, i64)) -> Option<i64> {
        let (body, combine, env) = steps;
        if self.skips((from, to)) {
            return None;
        }
        let thread = i64::from(super::threads::index());
        if to - from <= self.chunk {
            let mut partial = 0;
            let code = self.ran((from, to), || body(env, number(from), number(to), thread, &mut partial));
            if code != 0 {
                self.failed(from, code);
                return None;
            }
            return Some(partial);
        }
        let middle = from + (to - from) / 2;
        let left = self.reduce((from, middle), steps);
        let right = self.reduce((middle, to), steps);
        self.combined((from, to), left, right, (body, combine, env))
    }
}

/// How many `parallel` calls native code has made, for `FENNEC_COUNTS`: so a test can tell a
/// loop ran through them rather than as an ordinary loop.
static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many `parallel` calls native code has made.
pub fn calls() -> u64 {
    CALLS.load(Ordering::Relaxed)
}

fn counted() {
    CALLS.fetch_add(1, Ordering::Relaxed);
}

/// `fennec_parallel_for(list, body, env)`: `for x in parallel(list)` and `for mut x in
/// parallel(list)`. Gives back 0, or 1 when the body failed.
pub extern "C" fn parallel_for(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, Weight::Light)
}

/// `fennec_parallel_for_heavy`: `parallel_for`, for a body with a loop or a call in it.
pub extern "C" fn parallel_for_heavy(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, Weight::Heavy)
}

fn run_for(list: i64, body: Body, env: i64, weight: Weight) -> i64 {
    counted();
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    let run = Run::new(weight.chunk(length, words)).waiting(weight == Weight::Waiting);
    run_each(run, length, words, Leaf::For(body, env), weight)
}

/// Runs a loop's or a comprehension's ranges, on the pool when it's worth it, then passes the
/// failure up: 1 if there was one, else 0. A body that may wait goes to the pool however few its
/// items, as what it waits for is what takes the time.
fn run_each(run: Run, length: usize, words: usize, leaf: Leaf, weight: Weight) -> i64 {
    let Some(pool) = weight.pool(length, words) else {
        run.each((0, length), &|from, to, thread| leaf.run(from, to, thread));
        return run.finish();
    };
    each_on_pool(run, pool, length, leaf).finish()
}

/// A loop's or a comprehension's ranges run on `pool`, with the call's ranges to finish.
fn each_on_pool(mut run: Run, pool: &'static Pool, length: usize, leaf: Leaf) -> Run {
    run.joins_on_a_pool();
    let run = Arc::new(run);
    pool::install(&run, || run.each_on_start(pool, length, leaf), &run.done);
    alone(run)
}

/// The call's ranges, once the last job has let go of them: a worker can still be dropping its
/// share for an instant after saying it finished.
fn alone(mut run: Arc<Run>) -> Run {
    loop {
        match Arc::try_unwrap(run) {
            Ok(run) => return run,
            Err(shared) => {
                run = shared;
                std::thread::yield_now();
            }
        }
    }
}

/// A reduction's ranges run on `pool`, combined in the tree `reduce` uses: what they come to,
/// with the call's ranges to finish.
fn reduce_on_pool(
    mut run: Run,
    pool: &'static Pool,
    length: usize,
    steps: (ReduceBody, Combine, i64),
) -> (Option<i64>, Run) {
    run.joins_on_a_pool();
    let run = Arc::new(run);
    let start = || {
        let started = Arc::clone(&run);
        pool.push(Box::new(move || started.reduce_on(pool, (0, length), None, steps)));
    };
    pool::install(&run, start, &run.done);
    let run = alone(run);
    let reduced = run.reduced.lock().unwrap_or_else(PoisonError::into_inner).take();
    (reduced, run)
}

/// `fennec_parallel_map(list, body, env, element) -> list`: `[f(x) for x in parallel(list)]`.
/// The result is a new list owned once of the element numbered `element`, with each item
/// at its position. On a failure it's 0.
pub extern "C" fn parallel_map(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    run_map(list, body, env, element, Weight::Light)
}

/// `fennec_parallel_map_heavy`: `parallel_map`, for a body with a loop or a call in it.
pub extern "C" fn parallel_map_heavy(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    run_map(list, body, env, element, Weight::Heavy)
}

fn run_map(list: i64, body: MapBody, env: i64, element: i64, weight: Weight) -> i64 {
    counted();
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    if let Some(made) = map_of_task((length, words), body, env, element, weight) {
        return made;
    }
    let out = super::lists::new(element, number(length));
    let run = Run::new(weight.chunk(length, words)).waiting(weight == Weight::Waiting);
    if run_each(run, length, words, Leaf::Map(body, env, out), weight) != 0 {
        super::heap::free(out);
        return 0;
    }
    out
}

/// A comprehension a launched task makes, whose body is heavy or may wait, of two items or more:
/// its ranges go to the task's pool, as they do from the program's own thread, and the task parks
/// until they've finished. `None` anywhere else, and where what the body captures can't be copied:
/// the ranges then run here, one after another.
///
/// A thread that waits lends its ranges all its blocks. A task can't: the other tasks on its
/// worker go on using them. So the ranges get a copy of what the body captures, in a table of its
/// own, with the list they write into made in it too. When they've finished, the table's blocks
/// are the task's, the list among them, and the copy is let go.
fn map_of_task(
    (length, words): (usize, usize),
    body: MapBody,
    env: i64,
    element: i64,
    weight: Weight,
) -> Option<i64> {
    // A light body stays here, as copying what it captures costs about what running it does: but
    // not under `FENNEC_PARALLEL_STRESS`, which sends every call to the pool, a task's too.
    if (weight == Weight::Light && !pool::stressed()) || length < 2 {
        return None;
    }
    let (here, stop) = pool::for_task()?;
    let (copy, table) = copied_to_lend(env)?;
    with(|state| state.heap.enter_table(table, super::guarded::LENT_OWNER << super::heap::OWNER_SHIFT));
    let out = super::lists::new(element, number(length));
    let lent = Arc::new(*with(|state| state.heap.leave_table()));
    let run = Run::new(weight.chunk(length, words))
        .waiting(weight == Weight::Waiting)
        .of_task(Arc::clone(&stop), Arc::clone(&lent));
    let run = Arc::new(run);
    pool::install_for_task(
        here,
        || run.each_on_start(here.0, length, Leaf::Map(body, copy, out)),
        &run.done,
        &stop,
    );
    if taken_back(alone(run), lent, copy) != 0 {
        super::heap::free(out);
        return Some(0);
    }
    Some(out)
}

/// A copy of the closure `env`, with all it captures, for a launched task's call to lend its
/// ranges: its address, and the table it's in. An `env` of 0 captures nothing, and gets a table
/// with nothing in it. `None` when something it captures can't be copied, as a generator can't.
fn copied_to_lend(env: i64) -> Option<(i64, Box<super::table::Table>)> {
    if env == 0 {
        return Some((0, Box::new(super::table::Table::hashed())));
    }
    let (copy, table) = with(|state| super::guarded::copied_for_task(state, env.cast_unsigned()).ok())?;
    Some((copy.cast_signed(), table))
}

/// Ends a launched task's call on the pool, its ranges all finished: what it lent them is the
/// task's own now, with what they made and how they changed its owners, and the copy of what they
/// captured is let go. Gives back what `Run::finish` does.
fn taken_back(run: Run, lent: Arc<super::table::Table>, copy: i64) -> i64 {
    run.lent.write().unwrap_or_else(PoisonError::into_inner).take();
    let table = sole(lent);
    with(|state| state.heap.adopt_made(table.into_blocks()));
    let code = run.finish();
    if copy != 0 {
        with(|state| super::heap::release(state, copy.cast_unsigned()));
    }
    code
}

/// What a call lent, once the last range has let go of it, as `alone` waits for the call itself.
fn sole(mut lent: Arc<super::table::Table>) -> super::table::Table {
    loop {
        match Arc::try_unwrap(lent) {
            Ok(table) => return table,
            Err(shared) => {
                lent = shared;
                std::thread::yield_now();
            }
        }
    }
}

/// `fennec_parallel_reduce(list, body, combine, env, result) -> i64`: a reduction like
/// `sum(f(x) for x in parallel(list))`. Writes what it comes to into `result`, each range's
/// partial combined with the next in a fixed order. Gives back 0, or 1 when it failed. A list
/// with nothing in it leaves `result` as it was.
pub extern "C" fn parallel_reduce(
    list: i64,
    body: ReduceBody,
    combine: Combine,
    env: i64,
    result: &mut i64,
) -> i64 {
    run_reduce(list, (body, combine, env), result, false)
}

fn run_reduce(
    list: i64,
    (body, combine, env): (ReduceBody, Combine, i64),
    result: &mut i64,
    waiting: bool,
) -> i64 {
    counted();
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    if length == 0 {
        return 0;
    }
    // A reduction's leaves depend on the length alone, not on its items' sizes, so it combines
    // in the same order whatever the items are.
    if (waiting || pool::stressed())
        && let Some(code) = reduce_of_task(length, (body, combine, env), result, waiting)
    {
        return code;
    }
    let run = Run::new(crate::builtins::REDUCE_LEAF).waiting(waiting);
    let steps = (body, combine, env);
    let pool = if waiting { pool::for_waiting() } else { pool::for_call(length * words) };
    let Some(pool) = pool else {
        if let Some(reduced) = run.reduce((0, length), steps) {
            *result = reduced;
        }
        return run.finish();
    };
    let (reduced, run) = reduce_on_pool(run, pool, length, steps);
    if let Some(reduced) = reduced {
        *result = reduced;
    }
    run.finish()
}

/// A reduction a launched task makes, whose body may wait, of more than one leaf: on the task's
/// pool, as `map_of_task` runs a comprehension, lending a copy of what the body captures. `None`
/// anywhere else, and the ranges then run here. Under `FENNEC_PARALLEL_STRESS` it's every
/// reduction of more than one leaf that a task makes, `waiting` or not.
fn reduce_of_task(
    length: usize,
    (body, combine, env): (ReduceBody, Combine, i64),
    result: &mut i64,
    waiting: bool,
) -> Option<i64> {
    // A reduction of one leaf runs on one thread wherever it is, so there's nothing to gain from
    // copying what it captures.
    if length <= crate::builtins::REDUCE_LEAF {
        return None;
    }
    let (here, stop) = pool::for_task()?;
    let (copy, table) = copied_to_lend(env)?;
    let lent = Arc::new(*table);
    let run =
        Run::new(crate::builtins::REDUCE_LEAF).waiting(waiting).of_task(Arc::clone(&stop), Arc::clone(&lent));
    let run = Arc::new(run);
    let start = || {
        let (pool, started) = (here.0, Arc::clone(&run));
        pool.push(Box::new(move || started.reduce_on(pool, (0, length), None, (body, combine, copy))));
    };
    pool::install_for_task(here, start, &run.done, &stop);
    let run = alone(run);
    if let Some(reduced) = run.reduced.lock().unwrap_or_else(PoisonError::into_inner).take() {
        *result = reduced;
    }
    Some(taken_back(run, lent, copy))
}

// The `_waiting` entry points are for a body that may wait: a network call, a lock, a sleep, a
// blocking read. On the pool, each range runs as a parked task on its worker, with accounts of its
// own (see `threads::Accounts`), so a wait can park it and let the worker run other ranges.
// A loop's or a comprehension's ranges are an item each, since waiting is what takes the time.
// A reduction keeps the leaves its length gives, so it combines as it does anywhere else.

/// `fennec_parallel_for_waiting`: `parallel_for`, for a body that may wait.
pub extern "C" fn parallel_for_waiting(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, Weight::Waiting)
}

/// `fennec_parallel_map_waiting`: `parallel_map`, for a body that may wait.
pub extern "C" fn parallel_map_waiting(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    run_map(list, body, env, element, Weight::Waiting)
}

/// `fennec_parallel_reduce_waiting`: `parallel_reduce`, for a body that may wait.
pub extern "C" fn parallel_reduce_waiting(
    list: i64,
    body: ReduceBody,
    combine: Combine,
    env: i64,
    result: &mut i64,
) -> i64 {
    run_reduce(list, (body, combine, env), result, true)
}

// The `_in_place` entry points are for `for mut x in parallel(list)`, which writes each item back
// into its list where it is. The compiler makes the list the loop's alone first (`parallel.own`).
// A plain `for` gives nothing back through what it captures, so its ranges could read a copy of
// it, as a launched task's comprehension's do; a `for mut`'s can't, and the two are told apart by
// which entry is called. Until the compiler calls these for `for mut`, both loops still come
// through `fennec_parallel_for`, so neither is given a copy yet: a launched task runs a loop's
// ranges itself, one after another, under either name (#1802).

/// `fennec_parallel_for_in_place(list, body, env)`: `for mut x in parallel(list)`. Gives back 0,
/// or 1 when the body failed.
pub extern "C" fn parallel_for_in_place(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, Weight::Light)
}

/// `fennec_parallel_for_in_place_heavy`: `parallel_for_in_place`, for a body with a loop or a call
/// in it.
pub extern "C" fn parallel_for_in_place_heavy(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, Weight::Heavy)
}

/// `fennec_parallel_for_in_place_waiting`: `parallel_for_in_place`, for a body that may wait.
pub extern "C" fn parallel_for_in_place_waiting(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, Weight::Waiting)
}

/// `fennec_parallel_stream_waiting`: `parallel_stream`, for a body that may wait.
pub extern "C" fn parallel_stream_waiting(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    parallel_stream(list, body, env, element)
}

#[cfg(not(any(test, feature = "threads")))]
mod pool {
    //! Without threads, every range runs on the calling thread, one after another.

    /// Without threads, no task parks.
    pub extern "C" fn on_task_resume(_set: extern "C" fn(i64)) {}

    /// Without threads, there are no workers to set up.
    pub fn set_worker_setup(_setup: Box<dyn Fn() + Send + Sync>) {}

    /// Without threads, there's no pool to test.
    /// Without threads, the calling thread is the only worker.
    pub(crate) fn workers_wanted() -> usize {
        1
    }

    pub(super) fn stressed() -> bool {
        false
    }

    /// Without threads, there's no pool to stress.
    pub(crate) fn stress_asked() -> bool {
        false
    }

    /// Without threads, no chunk runs ahead of a stream's reader.
    pub(super) fn streams_run_ahead() -> bool {
        false
    }

    /// Without threads, there are no workers.
    pub(super) fn on_worker() -> bool {
        false
    }

    /// Without threads, native code only runs on the program's thread, whose limit it has.
    pub fn stack_limit() -> usize {
        0
    }

    /// Without threads, there are no launched tasks, and every range runs on its caller's thread,
    /// so a call gives its ranges nothing to take with them.
    #[derive(Clone, Copy)]
    pub(super) struct Calling;

    impl Calling {
        pub(super) fn here() -> Calling {
            Calling
        }

        pub(super) fn of_task(self, _stop: std::sync::Arc<std::sync::atomic::AtomicU8>) -> Calling {
            self
        }

        #[expect(clippy::unused_self, reason = "it stands in for the threads build's")]
        pub(super) fn is_task_s(self) -> bool {
            false
        }
    }

    pub(super) struct BeforeRange;

    #[expect(clippy::trivially_copy_pass_by_ref, reason = "it stands in for the threads build's")]
    pub(super) fn range_off_its_caller_starts(_calling: &Calling) -> BeforeRange {
        BeforeRange
    }

    pub(super) fn range_off_its_caller_ends(_before: BeforeRange) {}

    /// Without threads, there are no launched tasks, so no range is a stopped one's.
    pub(super) fn cut_short() -> bool {
        false
    }

    /// Without threads, there are no launched tasks to hand ranges to a pool.
    pub(super) fn for_task() -> Option<((&'static Pool, usize), std::sync::Arc<std::sync::atomic::AtomicU8>)>
    {
        None
    }

    pub(super) fn install_for_task(
        _here: (&'static Pool, usize),
        start: impl FnOnce(),
        _done: &Latch,
        _stop: &std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) {
        start();
    }

    /// Without threads, nothing is lent, so ranges have nothing to read or hand back.
    pub(super) fn range_starts(_lent: Option<std::sync::Arc<crate::embed::table::Table>>) {}

    pub(super) fn range_ends() -> (Vec<(u64, i64)>, Vec<u8>) {
        (Vec::new(), Vec::new())
    }

    pub(super) fn settle(_changes: std::collections::HashMap<u64, i64>) {}

    /// Without threads, nothing waits for jobs, which run where they're pushed.
    #[derive(Default)]
    pub(super) struct Latch;

    impl Latch {
        pub(super) fn one() -> Latch {
            Latch
        }

        #[expect(clippy::unused_self, reason = "it stands in for the threads build's latch")]
        pub(super) fn more(&self) {}

        #[expect(clippy::unused_self, reason = "it stands in for the threads build's latch")]
        pub(super) fn finished(&self) {}

        #[expect(clippy::unused_self, reason = "it stands in for the threads build's latch")]
        pub(super) fn counts_joins(&self, _counted: bool, _replay: Option<std::sync::Arc<str>>) {}

        #[expect(clippy::unused_self, reason = "it stands in for the threads build's latch")]
        pub(super) fn range_failed(&self, _from: usize, _crash: Option<String>) {}
    }

    /// Without threads, there's no pool: `for_call` never gives one.
    pub(super) struct Pool;

    impl Pool {
        #[expect(clippy::unused_self, reason = "it stands in for the threads build's pool")]
        pub(super) fn push(&self, job: Box<dyn FnOnce() + Send>) {
            job();
        }
    }

    pub(super) fn for_call(_words: usize) -> Option<&'static Pool> {
        None
    }

    pub(super) fn for_waiting() -> Option<&'static Pool> {
        None
    }

    pub(super) fn for_heavy(_length: usize) -> Option<&'static Pool> {
        None
    }

    /// Without threads, a range runs where it's made.
    /// Without threads no range is named: there's no pool to run one.
    pub(super) fn name_range(
        _name: Option<crate::embed::deadlock::RangeName>,
    ) -> Option<crate::embed::deadlock::RangeName> {
        None
    }

    pub(super) fn as_task(range: impl FnOnce() -> i64 + 'static) {
        let _ = range();
    }

    pub(super) fn install(_call: &super::Run, start: impl FnOnce(), _done: &Latch) {
        start();
    }

    /// Without threads, a stream's reader runs every chunk itself.
    pub(super) fn window(_words: usize) -> usize {
        0
    }

    pub(super) fn spawn(task: impl FnOnce() + Send + 'static) {
        task();
    }
}

#[cfg(any(test, feature = "threads"))]
mod pool;

mod stream;

pub use stream::{parallel_stream, stream_end, stream_next};

#[cfg(test)]
pub(in crate::embed) use pool::with_setup;
#[cfg(not(any(test, feature = "threads")))]
use pool::{Latch, Pool};
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) use pool::{Latch, Pool, for_server, here};
#[cfg(any(test, feature = "threads"))]
pub use pool::{has_worker_setup, run_worker_setup};
pub use pool::{on_task_resume, set_worker_setup, stack_limit};
pub(crate) use pool::{stress_asked, workers_wanted};

/// The tables each test that runs on the pool or serves gives its workers: a string's shape, and
/// lists of ints as element 0 and of strings as element 1.
///
/// The workers' setup is the process's, kept from the first test to give one, so every such test
/// must give this same one, through [`set_test_worker_setup`]: when one test's workers had lists of
/// strings as element 0 and another's lists of ints, whichever test came first set up both
/// tests' workers, and the other's lists came out of the wrong kind (#2540).
#[cfg(test)]
pub(in crate::embed) fn test_worker_tables() -> super::Tables {
    use crate::packed::Element;
    super::Tables {
        shapes: vec![std::rc::Rc::new(Element::Str)],
        elements: vec![std::rc::Rc::new(Element::Int), std::rc::Rc::new(Element::Str)],
        ..super::Tables::default()
    }
}

/// Gives the workers' setup every test that runs on the pool or serves gives: a runtime started
/// with [`test_worker_tables`].
#[cfg(test)]
pub(in crate::embed) fn set_test_worker_setup() {
    set_worker_setup(Box::new(|| super::start(test_worker_tables(), crate::Inputs::default())));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    use fennec_syntax::{Diagnostic, Span};

    use super::super::{State, Tables, attempt, finish, heap, lists, start, strings, threads, with};
    use crate::value::Value;
    use std::sync::atomic::Ordering;

    fn tables() -> Tables {
        super::test_worker_tables()
    }

    /// The workers' setup is given by a program, once, and by tests only through
    /// [`super::set_test_worker_setup`]: as the process keeps the first one given, a test giving
    /// its own set up every other test's workers with its tables (#2540). Each call of
    /// `set_worker_setup` in the runtime's sources is one of those, or where it's defined.
    #[test]
    fn only_a_program_and_the_shared_test_setup_set_workers_up() {
        // Each file is named by its path from `src`, its parts joined by `/`, however the system
        // writes paths: Windows gave them whole, with `\\`, and none matched (#2546's follow-up).
        fn calls_in(sources: &std::path::Path, folder: &std::path::Path, found: &mut Vec<(String, usize)>) {
            for entry in std::fs::read_dir(folder).expect("the runtime's sources").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    calls_in(sources, &path, found);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let source = std::fs::read_to_string(&path).expect("a source file");
                    let calls = source.matches(["set_worker", "_setup("].concat().as_str()).count();
                    if calls > 0 {
                        let inside = path.strip_prefix(sources).expect("a file under the sources");
                        let parts: Vec<String> = inside
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy().into_owned())
                            .collect();
                        found.push((parts.join("/"), calls));
                    }
                }
            }
        }
        let mut found = Vec::new();
        let sources = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        calls_in(&sources, &sources, &mut found);
        found.sort();
        let wanted = [
            // A program's own setup, given as it starts.
            ("embed/aot.rs", 1),
            // Defined without threads, and the shared test setup's one call.
            ("embed/parallel.rs", 2),
            // Defined with them.
            ("embed/parallel/pool.rs", 1),
        ];
        let wanted: Vec<(String, usize)> =
            wanted.iter().map(|(name, calls)| ((*name).to_owned(), *calls)).collect();
        assert_eq!(found, wanted, "a test gives the workers a setup of its own, not set_test_worker_setup()");
    }

    /// A thread's call shares its ranges with a pool of two workers or more, and keeps them with a
    /// pool of one, whose worker only ran them one after another while the thread waited, then
    /// handed back each block they made: `game_tick`'s 4,096 result lists a tick, at one thread.
    /// Under `FENNEC_PARALLEL_STRESS` every call goes to the pool, so there's nothing to check.
    #[test]
    fn a_threads_call_shares_its_ranges_only_with_a_pool_of_more_than_one_worker() {
        if super::pool::stressed() {
            return;
        }
        assert!(super::pool::shares(super::pool::with_threads(1)).is_none(), "a pool of one took it");
        assert!(super::pool::shares(super::pool::with_threads(2)).is_some(), "a pool of two didn't");
    }

    fn begin() {
        super::set_test_worker_setup();
        start(tables(), crate::Inputs::default());
    }

    /// Enough items to run on the pool.
    const ITEMS: i64 = 100_000;

    extern "C" fn harmonic(_env: i64, from: i64, to: i64, _thread: i64, partial: &mut i64) -> i64 {
        let sum: f64 = (from..to).map(|n| 1.0 / crate::ops::to_float(n + 1)).sum();
        *partial = sum.to_bits().cast_signed();
        0
    }

    extern "C" fn add(_env: i64, a: i64, b: i64, out: &mut i64) -> i64 {
        let sum = f64::from_bits(a.cast_unsigned()) + f64::from_bits(b.cast_unsigned());
        *out = sum.to_bits().cast_signed();
        0
    }

    #[test]
    fn a_reduction_combines_in_an_order_that_depends_on_the_length_alone() {
        begin();
        let list = lists::new(0, ITEMS);
        let mut on_the_pool = 0;
        assert_eq!(super::parallel_reduce(list, harmonic, add, 0, &mut on_the_pool), 0);
        // The same ranges, combined the same way on this thread alone.
        let run = super::Run::new(crate::builtins::REDUCE_LEAF);
        let alone = run.reduce((0, usize::try_from(ITEMS).unwrap()), (harmonic, add, 0));
        assert_eq!(Some(on_the_pool), alone);
        heap::free(list);
        finish();
    }

    /// Fails for an empty range: a sum's range reads its first item before anything else.
    extern "C" fn not_empty(_env: i64, from: i64, to: i64, _thread: i64, partial: &mut i64) -> i64 {
        *partial = 0;
        i64::from(from >= to)
    }

    #[test]
    fn a_reduction_never_gives_a_range_no_positions() {
        let leaf = crate::builtins::REDUCE_LEAF;
        for length in [1, 2, 3, leaf - 1, leaf, leaf + 1, 2 * leaf + 1, 3 * leaf + 7, 100_000] {
            let run = super::Run::new(leaf);
            assert_eq!(run.reduce((0, length), (not_empty, add, 0)), Some(0), "{length} items");
        }
        // An empty list runs no range at all, leaving the start as it is.
        begin();
        let list = lists::new(0, 0);
        let mut result = 5;
        assert_eq!(super::parallel_reduce(list, not_empty, add, 0, &mut result), 0);
        assert_eq!(result, 5);
        heap::free(list);
        finish();
    }

    /// `sum(1.0 / float(n + 1) for n in parallel(range(ITEMS)))`'s bits, on a pool of `threads`.
    fn harmonic_on(threads: usize) -> u64 {
        let pool = super::pool::with_threads(threads);
        let run = super::Run::new(crate::builtins::REDUCE_LEAF);
        let (reduced, _) =
            super::reduce_on_pool(run, pool, usize::try_from(ITEMS).unwrap(), (harmonic, add, 0));
        reduced.unwrap().cast_unsigned()
    }

    #[test]
    fn a_float_sum_has_the_same_bits_on_any_number_of_threads() {
        begin();
        let bits = harmonic_on(1);
        assert_eq!(harmonic_on(2), bits);
        assert_eq!(harmonic_on(8), bits);
        // Summed left to right, it rounds differently: the tree is what makes them agree.
        let in_order: f64 = (0..ITEMS).map(|n| 1.0 / crate::ops::to_float(n + 1)).sum();
        assert_ne!(in_order.to_bits(), bits);
        finish();
    }

    extern "C" fn fails_twice(_env: i64, from: i64, to: i64, _thread: i64) -> i64 {
        let at = [90_000, 70_000].into_iter().find(|at| (from..to).contains(at));
        let Some(at) = at else { return 0 };
        attempt(|_: &mut State| -> Result<i64, Diagnostic> {
            Err(Diagnostic::new(format!("failed in the range from {from}, at {at}"), Span::default()))
        });
        1
    }

    #[test]
    fn of_several_failures_the_lowest_range_s_is_passed_up() {
        begin();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, fails_twice, 0), 1);
        heap::free(list);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message.ends_with("at 70000")));
    }

    /// Fails with a list as long as where it failed, for each of two places, as a body whose `?`
    /// passes a failure up does: handing the value over, then giving back 2.
    /// The places: 90000 and 70000 for `env` 0, 7 and 3 for `env` 1. A range fails at the lower of
    /// the two it holds.
    extern "C" fn fails_twice_with_values(env: i64, from: i64, to: i64, _thread: i64) -> i64 {
        let places = if env == 0 { [90_000, 70_000] } else { [7, 3] };
        let Some(at) = places.into_iter().filter(|at| (from..to).contains(at)).min() else { return 0 };
        crate::embed::serve::task_failed(lists::new(0, at));
        super::RANGE_FAILED
    }

    /// The length of the list a failure passed up, and the thread that owns its block.
    fn passed_up() -> (Option<usize>, u64) {
        let value = super::parallel_failure();
        let length = with(|state| super::extent(state, value).map(|(length, _)| length));
        let owner = with(|state| state.heap.block(value.cast_unsigned()).map_or(u64::MAX, |block| block[1]));
        heap::free(value);
        (length, owner >> heap::OWNER_SHIFT)
    }

    #[test]
    fn of_several_ranges_failing_with_values_the_lowest_one_s_passes_up_as_the_caller_s() {
        begin();
        let before = with(|state| state.heap.live());
        let list = lists::new(0, ITEMS);
        for waiting in [false, true] {
            let call = if waiting { super::parallel_for_waiting } else { super::parallel_for };
            assert_eq!(call(list, fails_twice_with_values, 0), super::RANGE_FAILED, "waiting: {waiting}");
            assert_eq!(passed_up(), (Some(70_000), 0), "waiting: {waiting}");
            assert!(!crate::embed::failed());
        }
        heap::free(list);
        assert_eq!(with(|state| state.heap.live()), before, "the failure the lower one beat was let go of");
        finish();
    }

    #[test]
    fn a_call_too_small_for_the_pool_passes_its_lowest_failure_up_too() {
        begin();
        let list = lists::new(0, 10);
        assert_eq!(super::parallel_for(list, fails_twice_with_values, 1), super::RANGE_FAILED);
        assert_eq!(passed_up(), (Some(3), 0));
        heap::free(list);
        finish();
    }

    /// What each range of the turn-taking loop made, by where it starts: a string, and the string
    /// joined to itself after other ranges had their turns.
    static TOOK_TURNS: Mutex<Vec<(i64, i64, i64)>> = Mutex::new(Vec::new());

    /// Runs as a parked task: makes a string, lets the other ranges on its worker run, and joins the
    /// string to itself, which reads it through this worker's runtime. That only works if another
    /// range ending meanwhile didn't hand this one's string over.
    extern "C" fn takes_turns(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        assert!(fennec_parked::stack_low().is_some(), "range {from} isn't a parked task");
        let made = with(|state| heap::text_address(state, &format!("range {from}")));
        assert!(super::super::serve::yield_task());
        let joined = strings::concat(made, made);
        assert!(super::super::serve::yield_task());
        TOOK_TURNS.lock().unwrap().push((from, made, joined));
        0
    }

    #[test]
    fn a_body_that_may_wait_runs_each_item_as_a_parked_task_keeping_what_it_made_its_own() {
        begin();
        let before = with(|state| state.heap.live());
        let list = lists::new(0, 64);
        assert_eq!(super::parallel_for_waiting(list, takes_turns, 0), 0);
        heap::free(list);
        let mut took = std::mem::take(&mut *TOOK_TURNS.lock().unwrap());
        took.sort_unstable();
        // An item each, every one a task, and all of them read back here, where they were adopted.
        assert_eq!(took.iter().map(|(from, ..)| *from).collect::<Vec<_>>(), (0..64).collect::<Vec<_>>());
        for (from, made, joined) in took {
            let text = format!("range {from}");
            assert_eq!(
                with(|state| Some(heap::read_block(state, made.cast_unsigned()))).unwrap(),
                Value::str(&text)
            );
            let twice = with(|state| Some(heap::read_block(state, joined.cast_unsigned()))).unwrap();
            assert_eq!(twice, Value::str(&text.repeat(2)));
            heap::free(made);
            heap::free(joined);
        }
        assert_eq!(with(|state| state.heap.live()), before);
        finish();
    }

    /// A reduction's leaf that fails where it holds position 70,000, and counts its positions
    /// anywhere else.
    extern "C" fn fails_in_a_leaf(_env: i64, from: i64, to: i64, _thread: i64, partial: &mut i64) -> i64 {
        if (from..to).contains(&70_000) {
            attempt(|_: &mut State| -> Result<i64, Diagnostic> {
                Err(Diagnostic::new("a leaf failed", Span::default()))
            });
            return 1;
        }
        *partial = to - from;
        0
    }

    /// A reduction on the pool in which a leaf fails still ends every range it started, so the
    /// calling thread has its blocks back, whole, when the call's over. The range that combines
    /// two halves once started and was left unended when a half had failed: its worker went on
    /// holding what was lent, and the calling thread could no longer take it back as its own.
    #[test]
    fn a_reduction_in_which_a_leaf_fails_gives_the_calling_thread_its_blocks_back() {
        begin();
        let list = lists::new(0, ITEMS);
        let mut sum = 0;
        assert_eq!(super::parallel_reduce(list, fails_in_a_leaf, add, 0, &mut sum), 1);
        let own = with(|state| state.heap.holds_block(usize::try_from(list).unwrap()));
        assert!(own, "the list isn't in its thread's own table after the call");
        heap::free(list);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message == "a leaf failed"));
    }

    /// How many positions the ranges of each `for mut` entry's loop covered.
    static IN_PLACE: [AtomicUsize; 3] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];

    extern "C" fn counts_positions(env: i64, from: i64, to: i64, _thread: i64) -> i64 {
        let covered = usize::try_from(to - from).unwrap_or(0);
        if let Some(count) = usize::try_from(env).ok().and_then(|entry| IN_PLACE.get(entry)) {
            count.fetch_add(covered, Ordering::SeqCst);
        }
        0
    }

    /// `for mut` has entries of its own, which run a loop as the plain ones do: every position
    /// once, whatever the body's weight.
    #[test]
    fn the_entries_for_a_loop_that_writes_back_cover_every_position_once() {
        begin();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for_in_place(list, counts_positions, 0), 0);
        assert_eq!(super::parallel_for_in_place_heavy(list, counts_positions, 1), 0);
        heap::free(list);
        let few = lists::new(0, 48);
        assert_eq!(super::parallel_for_in_place_waiting(few, counts_positions, 2), 0);
        heap::free(few);
        let covered: Vec<usize> = IN_PLACE.iter().map(|count| count.load(Ordering::SeqCst)).collect();
        let items = usize::try_from(ITEMS).unwrap();
        assert_eq!(covered, [items, items, 48]);
        finish();
    }

    /// A server that answers each connection with a line, 300 ms after it's made, as a slow web
    /// service would, and counts the most connections it held open at once.
    fn slow_server() -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let seen = Arc::clone(&most);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (open, most) = (Arc::clone(&open), Arc::clone(&most));
                std::thread::spawn(move || {
                    let now = open.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    open.fetch_sub(1, Ordering::SeqCst);
                    let _ = std::io::Write::write_all(&mut &stream, b"pong\n");
                });
            }
        });
        (port, seen)
    }

    static FETCHED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// Connects to the slow server on port `env` and reads its line, as a parked task.
    extern "C" fn fetches(env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let port = u16::try_from(env).unwrap();
        assert_eq!(crate::stdlib::net::fetch_line("127.0.0.1", port), Ok(b"pong\n".to_vec()));
        FETCHED.fetch_add(1, Ordering::Relaxed);
        0
    }

    /// More fetches than workers: each parks while it connects and while its line is on the way, so
    /// two workers have more than two of them waiting at once, which blocking waits never could.
    #[test]
    fn fetches_that_may_wait_all_wait_at_once_on_a_few_workers() {
        begin();
        let (port, most) = slow_server();
        let pool = super::pool::with_threads(2);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, 16, super::Leaf::For(fetches, i64::from(port)));
        assert_eq!(run.finish(), 0);
        assert_eq!(FETCHED.load(Ordering::Relaxed), 16);
        let most = most.load(Ordering::SeqCst);
        assert!(most > 2, "the server held at most {most} connections at once, from 2 workers");
        finish();
    }

    /// The connection the first of the two tasks below writes to, how its write ended, and whether
    /// the second ran while it waited for room: 1 for as it should have.
    static WRITTEN_TO: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static WROTE_IT_ALL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static RAN_MEANWHILE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// The first of two tasks keeps a connection to the server on port `env` and writes `MUCH` to
    /// it, more than its buffers hold, with nobody reading. The second waits until the first is
    /// waiting for room, which it can only see by running, and says it ran.
    extern "C" fn writes_while_another_runs(env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{kept, waiting_on, written_within};
        if from == 0 {
            let handle = kept("127.0.0.1", u16::try_from(env).unwrap()).unwrap();
            WRITTEN_TO.store(handle, Ordering::SeqCst);
            let wrote = written_within(handle, &vec![b'x'; MUCH], WITHIN);
            WROTE_IT_ALL.store(if wrote.is_ok() { 1 } else { 2 }, Ordering::SeqCst);
            assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
            return 0;
        }
        let started = std::time::Instant::now();
        let waits = || {
            let (on_the_socket, on_a_copy) = waiting_on(WRITTEN_TO.load(Ordering::SeqCst));
            on_the_socket + on_a_copy
        };
        while waits() == 0 && started.elapsed() < 2 * WITHIN {
            crate::embed::serve::yield_now();
        }
        RAN_MEANWHILE.store(if waits() > 0 { 1 } else { 2 }, Ordering::SeqCst);
        0
    }

    /// A server for one connection that reads nothing until the second task has said it ran, then
    /// reads all `MUCH` of it and keeps the connection until the write's over.
    fn reads_once_another_task_has_run() -> u16 {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(WITHIN)).unwrap();
            let started = std::time::Instant::now();
            while RAN_MEANWHILE.load(Ordering::SeqCst) == 0 && started.elapsed() < 3 * WITHIN {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let mut left = MUCH;
            let mut some = vec![0_u8; 1 << 16];
            while left > 0 {
                match stream.read(&mut some) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => left = left.saturating_sub(read),
                }
            }
            while WROTE_IT_ALL.load(Ordering::SeqCst) == 0 && started.elapsed() < 4 * WITHIN {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        port
    }

    /// A task writing to a connection whose other side isn't reading parks for room, and the
    /// other task on its worker runs meanwhile. It used to hold its worker in the kernel: only a
    /// read made a connection's socket not block, and only while it was parked, so a write with
    /// no read beside it waited as a thread does. Here the second task then never ran while the
    /// first waited, and the write ran out its ten seconds, as the server reads only once the
    /// second task has run.
    #[test]
    fn a_task_writing_to_a_full_connection_parks_and_its_workers_other_task_runs() {
        begin();
        let port = reads_once_another_task_has_run();
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let leaf = super::Leaf::For(writes_while_another_runs, i64::from(port));
        let ended = super::each_on_pool(run, pool, 2, leaf).finish();
        crate::stdlib::net::shut(WRITTEN_TO.load(Ordering::SeqCst));
        assert_eq!(ended, 0);
        assert_eq!(RAN_MEANWHILE.load(Ordering::SeqCst), 1, "the other task, while the write waited");
        assert_eq!(WROTE_IT_ALL.load(Ordering::SeqCst), 1, "the write");
        finish();
    }

    /// The TLS connection the two tasks below share, and how each one's part ended: 1 for as it
    /// should have.
    static OVER_TLS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static READ_OVER_TLS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static WROTE_OVER_TLS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    /// More than TLS holds at once, and than the socket's buffers hold.
    const MUCH_OVER_TLS: usize = 2 << 20;

    /// One of two tasks reads a line from the TLS connection, parked until the server answers.
    /// The other writes `MUCH_OVER_TLS` to it meanwhile.
    extern "C" fn reads_or_writes_over_tls(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{line_from_within, written_within};
        let handle = OVER_TLS.load(Ordering::SeqCst);
        if from == 0 {
            let read = line_from_within(handle, WITHIN);
            READ_OVER_TLS.store(if read == Ok(Some(b"done\n".to_vec())) { 1 } else { 2 }, Ordering::SeqCst);
        } else {
            let wrote = written_within(handle, &vec![b'y'; MUCH_OVER_TLS], WITHIN);
            WROTE_OVER_TLS.store(if wrote.is_ok() { 1 } else { 2 }, Ordering::SeqCst);
        }
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        0
    }

    /// A task writes more over TLS than TLS holds at once, to a server that's slow to read, while
    /// another task on its worker is parked reading the same connection. All of it arrives, and
    /// the reader gets the server's answer. The write used to hand TLS everything in one go, which
    /// on a socket that doesn't block gives up partway when there's no room, with part taken and
    /// the rest lost: the write failed, and the server never got its bytes.
    #[test]
    fn a_long_write_over_tls_from_a_task_parks_for_room_and_all_of_it_arrives() {
        begin();
        crate::stdlib::http::use_tls();
        let (handle, server) = crate::stdlib::net::tests::tls::connected_to_a_slow_reader(MUCH_OVER_TLS);
        OVER_TLS.store(handle, Ordering::SeqCst);
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let ended = super::each_on_pool(run, pool, 2, super::Leaf::For(reads_or_writes_over_tls, 0)).finish();
        let whole = server.join().unwrap();
        crate::stdlib::net::shut(handle);
        assert_eq!(ended, 0);
        assert_eq!(WROTE_OVER_TLS.load(Ordering::SeqCst), 1, "the write");
        assert!(whole, "what was written didn't all arrive");
        assert_eq!(READ_OVER_TLS.load(Ordering::SeqCst), 1, "the read");
        finish();
    }

    /// The server's connection the task below closes, and how long closing it took, in
    /// milliseconds.
    static CLOSED_BY_A_TASK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    /// When the task's close began and ended.
    static CLOSING: Mutex<Option<(std::time::Instant, std::time::Instant)>> = Mutex::new(None);

    /// Closes the server's connection, as a parked task.
    extern "C" fn closes_while_the_client_sends(_env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let started = std::time::Instant::now();
        crate::stdlib::net::shut(CLOSED_BY_A_TASK.load(Ordering::SeqCst));
        *CLOSING.lock().unwrap() = Some((started, std::time::Instant::now()));
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        0
    }

    /// A task closing a server's connection reads what the other side is still sending until
    /// it's done, parked between reads, so the client's sends all go and nothing's reset.
    #[test]
    fn a_task_closing_a_servers_connection_reads_what_is_still_sent_parked() {
        begin();
        let (handle, sending) = crate::stdlib::net::tests::served_while_the_client_sends();
        CLOSED_BY_A_TASK.store(handle, Ordering::SeqCst);
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let leaf = super::Leaf::For(closes_while_the_client_sends, 0);
        let ended = super::each_on_pool(run, pool, 1, leaf).finish();
        assert_eq!(ended, 0);
        let (sent, done) = sending.join().unwrap();
        assert!(sent, "a piece the client sent was refused");
        let (started, closed) = CLOSING.lock().unwrap().take().unwrap();
        assert!(closed >= done, "it closed before the client had sent everything");
        let took = closed.duration_since(started);
        assert!(took < crate::stdlib::net::LINGER, "closing took {took:?}");
        finish();
    }

    /// The listener the tasks below take a connection from by hand, the port it's on, whether the
    /// one that takes waits with a timeout, and how each one's part ended: 1 for as it should.
    static TAKEN_FROM: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static TAKEN_ON: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static TAKES_WITHIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static TOOK_ONE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static CONNECTED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// One of two tasks takes a connection from the listener by hand. The other connects to it.
    extern "C" fn takes_or_connects(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{accepted_within, kept, shut};
        if from == 0 {
            let limit = TAKES_WITHIN.load(Ordering::SeqCst).then_some(WITHIN);
            let taken = accepted_within(TAKEN_FROM.load(Ordering::SeqCst), limit);
            TOOK_ONE.store(if taken.is_ok() { 1 } else { 2 }, Ordering::SeqCst);
            shut(taken.unwrap_or(0));
        } else {
            let port = u16::try_from(TAKEN_ON.load(Ordering::SeqCst)).unwrap();
            let connected = kept("127.0.0.1", port);
            CONNECTED.store(if connected.is_ok() { 1 } else { 2 }, Ordering::SeqCst);
            shut(connected.unwrap_or(0));
        }
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        0
    }

    /// A task waiting in `server.accept()` parks, and the task on its worker that's about to
    /// connect runs: with a timeout and without. It used to hold its worker, asleep 5 ms at a time
    /// with a timeout and in the kernel without, so the task that would connect never ran: with a
    /// timeout the wait ran out, and without it the two waited on each other for good.
    #[test]
    fn a_task_taking_a_connection_by_hand_parks_and_the_task_that_connects_runs() {
        begin();
        for within in [true, false] {
            let (server, port) = crate::stdlib::net::listened();
            TAKEN_FROM.store(server, Ordering::SeqCst);
            TAKEN_ON.store(i64::from(port), Ordering::SeqCst);
            TAKES_WITHIN.store(within, Ordering::SeqCst);
            for slot in [&TOOK_ONE, &CONNECTED] {
                slot.store(0, Ordering::SeqCst);
            }
            let pool = super::pool::with_threads(1);
            let run = super::Run::new(1).waiting(true);
            let ended = super::each_on_pool(run, pool, 2, super::Leaf::For(takes_or_connects, 0)).finish();
            crate::stdlib::net::shut(server);
            assert_eq!(ended, 0);
            assert_eq!(TOOK_ONE.load(Ordering::SeqCst), 1, "the one that takes; with a timeout: {within}");
            assert_eq!(
                CONNECTED.load(Ordering::SeqCst),
                1,
                "the one that connects; with a timeout: {within}"
            );
        }
        finish();
    }

    /// The UDP sockets the tasks below use, the port the first is on, and how each part ended.
    static HEARS_ON: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static SAYS_FROM: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static HEARD_ON_PORT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static HEARD_IT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static SAID_IT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// One of two tasks waits for a packet. The other sends it one.
    extern "C" fn hears_or_says(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{received_within, sent_to_port};
        if from == 0 {
            let heard = received_within(HEARS_ON.load(Ordering::SeqCst), WITHIN);
            HEARD_IT.store(if heard == Ok(b"ping".to_vec()) { 1 } else { 2 }, Ordering::SeqCst);
        } else {
            let port = u16::try_from(HEARD_ON_PORT.load(Ordering::SeqCst)).unwrap();
            let said = sent_to_port(SAYS_FROM.load(Ordering::SeqCst), b"ping", port);
            SAID_IT.store(if said.is_ok() { 1 } else { 2 }, Ordering::SeqCst);
        }
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        0
    }

    /// A task waiting for a UDP packet parks, and the task on its worker that sends it runs. It
    /// used to hold its worker in the kernel, a tenth of a second at a time, so the sender never
    /// ran and the wait ran out.
    #[test]
    fn a_task_waiting_for_a_packet_parks_and_the_task_that_sends_it_runs() {
        begin();
        let (hears, port) = crate::stdlib::net::udp_bound();
        let (says, _) = crate::stdlib::net::udp_bound();
        HEARS_ON.store(hears, Ordering::SeqCst);
        SAYS_FROM.store(says, Ordering::SeqCst);
        HEARD_ON_PORT.store(i64::from(port), Ordering::SeqCst);
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let ended = super::each_on_pool(run, pool, 2, super::Leaf::For(hears_or_says, 0)).finish();
        for socket in [hears, says] {
            crate::stdlib::net::shut(socket);
        }
        assert_eq!(ended, 0);
        assert_eq!(SAID_IT.load(Ordering::SeqCst), 1, "the one that sends");
        assert_eq!(HEARD_IT.load(Ordering::SeqCst), 1, "the one that waits");
        finish();
    }

    /// The listener one of the tasks below waits on and the other closes, and how the wait ended.
    static CLOSED_UNDER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static ENDED_AS_CLOSED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// One of two tasks waits for a connection that never comes. The other closes the listener
    /// once the first is waiting.
    extern "C" fn waits_or_closes_the_listener(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{Failure, accepted_within, shut, waiting_on_a_listener};
        let server = CLOSED_UNDER.load(Ordering::SeqCst);
        if from == 0 {
            let ended = accepted_within(server, Some(WITHIN));
            let closed = matches!(ended, Err(Failure::Other(_)));
            ENDED_AS_CLOSED.store(if closed { 1 } else { 2 }, Ordering::SeqCst);
        } else {
            // On the one worker this runs only while the other task is parked, in its wait.
            let started = std::time::Instant::now();
            while waiting_on_a_listener(server) == 0 && started.elapsed() < WITHIN {
                crate::embed::serve::yield_now();
            }
            shut(server);
        }
        0
    }

    /// Closing a listener from one task ends a wait for a connection another task is parked in,
    /// at once and as closed, not when its time runs out.
    #[test]
    fn closing_a_listener_ends_a_task_parked_waiting_for_a_connection() {
        begin();
        let (server, _) = crate::stdlib::net::listened();
        CLOSED_UNDER.store(server, Ordering::SeqCst);
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let leaf = super::Leaf::For(waits_or_closes_the_listener, 0);
        let started = std::time::Instant::now();
        let ended = super::each_on_pool(run, pool, 2, leaf).finish();
        assert_eq!(ended, 0);
        assert_eq!(ENDED_AS_CLOSED.load(Ordering::SeqCst), 1, "the wait ended some other way");
        assert!(started.elapsed() < WITHIN / 2, "it took {:?}", started.elapsed());
        finish();
    }

    /// More requests than there'll ever be blocking threads.
    const ASKING: usize = 100;
    static ANSWERED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// An HTTP server that takes `ASKING` requests and answers none until all have come.
    fn answers_once_all_have_asked() -> u16 {
        use std::io::{BufRead as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            for stream in listener.incoming().flatten().take(ASKING) {
                let mut head = std::io::BufReader::new(&stream);
                let mut line = String::new();
                while head.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    line.clear();
                }
                asked.push(stream);
            }
            for mut stream in asked {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\npong");
            }
        });
        port
    }

    /// Makes a request of the server on port `env`, as a parked task.
    extern "C" fn asks(env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let url = format!("http://127.0.0.1:{env}/");
        let got = crate::stdlib::http::asked("GET", &url, Vec::new(), std::time::Duration::from_secs(20));
        assert_eq!(got, Ok((200, b"pong".to_vec())));
        // Woken by the poll for its answer, not by looking again on a timer.
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        ANSWERED.fetch_add(1, Ordering::Relaxed);
        0
    }

    /// A request made in a task holds a blocking thread only while its connection is opened, and
    /// parks on its socket for the answer: a hundred of them on one worker all wait at once on a
    /// server that answers none until every one has asked. Each used to hold a blocking thread
    /// for its whole exchange, so no more could be asked at once than there were such threads,
    /// and this many waited on each other until their time ran out.
    #[test]
    fn requests_made_in_tasks_wait_for_their_answers_without_a_blocking_thread_each() {
        begin();
        let port = answers_once_all_have_asked();
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, ASKING, super::Leaf::For(asks, i64::from(port)));
        assert_eq!(run.finish(), 0);
        assert_eq!(ANSWERED.load(Ordering::Relaxed), i64::try_from(ASKING).unwrap());
        finish();
    }

    static ECHOED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// Posts 2 MB to the HTTPS server on port `env` and reads it back, as a parked task.
    extern "C" fn posts_over_tls(env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let sent: Vec<u8> = (0..2_000_000_usize).map(|at| u8::try_from(at % 251).unwrap()).collect();
        let url = format!("https://localhost:{env}/");
        let got = crate::stdlib::http::asked("POST", &url, sent.clone(), std::time::Duration::from_secs(20));
        let got = got.map(|(status, body)| (status, body == sent));
        assert_eq!(got, Ok((200, true)));
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        ECHOED.fetch_add(1, Ordering::Relaxed);
        0
    }

    /// Two requests over TLS from tasks on one worker, each with a body longer than its socket
    /// takes at once, to a server that's slow to read and answers neither until both have come
    /// whole. Each task parks for room as it writes and for its answer as it reads, with TLS
    /// between it and a socket that doesn't block, and gets back every byte it sent.
    #[test]
    fn requests_over_tls_made_in_tasks_park_for_room_and_for_their_answers() {
        begin();
        crate::stdlib::http::use_tls();
        let (port, trusting) = crate::stdlib::net::tests::tls::https_echoing(2);
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, 2, super::Leaf::For(posts_over_tls, i64::from(port)));
        let ended = run.finish();
        drop(trusting);
        assert_eq!(ended, 0);
        assert_eq!(ECHOED.load(Ordering::Relaxed), 2);
        finish();
    }

    /// How many clients connect to a server with TLS and never start their handshake.
    const SLOW_CLIENTS: usize = 200;
    /// The connections a server took, for the tasks that do their handshakes, in the order they came.
    static TAKEN: Mutex<Vec<Option<std::net::TcpStream>>> = Mutex::new(Vec::new());
    static SERVER_TLS: std::sync::OnceLock<Arc<dyn crate::stdlib::net::TlsServer>> =
        std::sync::OnceLock::new();
    /// How many handshakes had failed, and how many of those when the one client that shook hands
    /// was answered: -1 until it was.
    static HANDSHAKES_FAILED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static FAILED_WHEN_ANSWERED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

    /// Does a server's half of the handshake on the connection at `from`, as `server.accept()` does
    /// in a task, and answers the client if the handshake's done. A blocking thread still does
    /// its own work for the one it answers.
    extern "C" fn shakes_hands_and_answers(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        let taken = usize::try_from(from).ok().and_then(|at| TAKEN.lock().unwrap().get_mut(at)?.take());
        let (Some(stream), Some(tls)) = (taken, SERVER_TLS.get()) else { return 0 };
        let here = std::thread::current().id();
        let (_, handed_over_before) = crate::embed::serve::blocking_jobs_of(here);
        match crate::stdlib::net::tests::tls::taken_by(tls, stream) {
            Ok(handle) => {
                // Its handshake's arithmetic was done on a blocking thread, not on this worker.
                assert!(crate::embed::serve::blocking_jobs_of(here).1 > handed_over_before);
                assert_eq!(crate::embed::serve::off_thread(|| 7), 7);
                // Read before it answers: its client lets the slow ones go once it has its answer.
                FAILED_WHEN_ANSWERED.store(HANDSHAKES_FAILED.load(Ordering::SeqCst), Ordering::SeqCst);
                assert!(crate::stdlib::net::tests::tls::answered(handle));
            }
            Err(_) => {
                HANDSHAKES_FAILED.fetch_add(1, Ordering::SeqCst);
            }
        }
        // Each wait was the poll's, not a look again on a timer.
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        0
    }

    /// Two hundred clients connect to a server with TLS and never start their handshake, as a
    /// stranger can. Each of the server's handshakes, done by a task as `server.accept()` does it,
    /// parks on its socket and holds no thread: a client that does shake hands is answered while
    /// all two hundred are still waiting, and blocking threads do work for it meanwhile, its
    /// handshake's arithmetic among it.
    ///
    /// Each handshake once ran whole on one of the few blocking threads, for up to ten seconds, so
    /// as many slow clients as there were threads held them all, and the client that shook hands,
    /// and every file read and database call in the program, waited behind them.
    #[test]
    fn slow_handshakes_hold_no_thread_and_a_client_that_shakes_hands_is_answered_meanwhile() {
        begin();
        crate::stdlib::http::use_tls();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut slow = Vec::new();
        let mut taken = Vec::new();
        for _ in 0..SLOW_CLIENTS {
            slow.push(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
            taken.push(Some(listener.accept().unwrap().0));
        }
        // The client that shakes hands holds the slow ones' sockets, and lets go of them once it's
        // been answered or has given up, which ends their handshakes and so the test.
        let client = std::thread::spawn(move || {
            let heard = crate::stdlib::net::tests::tls::pinged(port);
            drop(slow);
            heard
        });
        taken.push(Some(listener.accept().unwrap().0));
        *TAKEN.lock().unwrap() = taken;
        let _ = SERVER_TLS.set(crate::stdlib::net::tests::tls::server_tls());
        let pool = super::pool::with_threads(2);
        let run = super::Run::new(1).waiting(true);
        let serving = super::Leaf::For(shakes_hands_and_answers, 0);
        let run = super::each_on_pool(run, pool, SLOW_CLIENTS + 1, serving);
        assert_eq!(run.finish(), 0);
        assert_eq!(client.join().unwrap(), Ok(b"pong\n".to_vec()));
        let failed_by_then = FAILED_WHEN_ANSWERED.load(Ordering::SeqCst);
        assert_eq!(
            failed_by_then, 0,
            "slow handshakes had ended before the client that shook hands was answered"
        );
        assert_eq!(HANDSHAKES_FAILED.load(Ordering::SeqCst), i64::try_from(SLOW_CLIENTS).unwrap());
        finish();
    }

    /// The thread of the task that's connecting, in each of the two tests below, for its server
    /// to ask about.
    static CONNECTING_FROM: [Mutex<Option<std::thread::ThreadId>>; 2] = [Mutex::new(None), Mutex::new(None)];
    static CONNECTED_AND_HEARD: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// How many pieces of work the connecting task's thread has on the blocking threads.
    fn connecting_task_s_blocking_work(test: usize) -> i64 {
        let from = *CONNECTING_FROM[test].lock().unwrap();
        from.map_or(-1, |from| crate::embed::serve::blocking_jobs_of(from).0)
    }

    fn plain_connector_s_blocking_work() -> i64 {
        connecting_task_s_blocking_work(0)
    }

    fn secure_connector_s_blocking_work() -> i64 {
        connecting_task_s_blocking_work(1)
    }

    /// Connects to the server on port `env` as `net.connect` does, by address, or by name with
    /// TLS for the second test, and talks to it, as a parked task.
    extern "C" fn connects_and_talks(env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        let (test, secure) = (usize::from(from != 0), from != 0);
        let port = u16::try_from(env).unwrap();
        let here = std::thread::current().id();
        *CONNECTING_FROM[test].lock().unwrap() = Some(here);
        let host = if secure { "localhost" } else { "127.0.0.1" };
        let (_, handed_over_before) = crate::embed::serve::blocking_jobs_of(here);
        let heard = crate::stdlib::net::tests::tls::connected_and_pinged(host, port, secure);
        assert_eq!(heard, Ok(b"pong\n".to_vec()));
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        // By address and with no TLS, nothing at all is handed to a blocking thread. By name with
        // TLS, the name's lookup is, and the handshake's arithmetic, in a piece or more.
        let (_, handed_over) = crate::embed::serve::blocking_jobs_of(here);
        if secure {
            assert!(handed_over - handed_over_before >= 2);
        } else {
            assert_eq!(handed_over - handed_over_before, 0);
        }
        CONNECTED_AND_HEARD.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// A task that connects by address hands nothing to a blocking thread: it starts the connect
    /// and parks until the socket says how it went. The connect once ran whole on a blocking
    /// thread, for as long as the network took.
    #[test]
    fn a_task_s_connect_holds_no_blocking_thread() {
        begin();
        let (port, server, _trusting) =
            crate::stdlib::net::tests::tls::answering_and_looking(false, plain_connector_s_blocking_work);
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        // The range from 0 connects by address.
        let run = super::each_on_pool(run, pool, 1, super::Leaf::For(connects_and_talks, i64::from(port)));
        assert_eq!(run.finish(), 0);
        assert_eq!(server.join().unwrap(), [0], "blocking work its task had as the connection came");
        finish();
    }

    /// The same with TLS, by name. The name's lookup is done on a blocking thread, and it's over
    /// before the connect starts. So is the handshake's arithmetic, each time the server has sent
    /// something to work through, and never while the task waits for the server. The server finds
    /// the task with no work on a blocking thread as the connection comes, when the task is
    /// waiting for it, and again once it has the last of the client's handshake. The lookup, the
    /// connect and the handshake once ran as one piece of work on a blocking thread, for as long
    /// as the network and the server took.
    #[test]
    fn a_task_s_handshake_as_a_client_holds_no_blocking_thread() {
        begin();
        crate::stdlib::http::use_tls();
        let (port, server, trusting) =
            crate::stdlib::net::tests::tls::answering_and_looking(true, secure_connector_s_blocking_work);
        let pool = super::pool::with_threads(1);
        // A list of two with its ranges an item each, of which only the second is run here: the
        // range from 1 connects by name, with TLS.
        let run = super::Run::new(1).waiting(true);
        let connecting = super::Leaf::For(second_connects_and_talks, i64::from(port));
        let run = super::each_on_pool(run, pool, 2, connecting);
        let ended = run.finish();
        drop(trusting);
        assert_eq!(ended, 0);
        assert_eq!(server.join().unwrap(), [0, 0], "blocking work its task had, at each look");
        finish();
    }

    /// `connects_and_talks`, for the range from 1 alone: the one from 0 has nothing to do.
    extern "C" fn second_connects_and_talks(env: i64, from: i64, to: i64, thread: i64) -> i64 {
        if from == 0 { 0 } else { connects_and_talks(env, from, to, thread) }
    }

    /// How long the two tests below give a name's lookup, as a connect's timeout.
    const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

    /// What the task connecting to a name that never answers found, in each of the two tests below:
    /// whether it failed as timed out, and how long it took, in milliseconds, or -1 until it ended.
    static TIMED_OUT: [std::sync::atomic::AtomicBool; 2] =
        [std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false)];
    static TOOK_MS: [std::sync::atomic::AtomicI64; 2] =
        [std::sync::atomic::AtomicI64::new(-1), std::sync::atomic::AtomicI64::new(-1)];

    /// Connects to `name`, whose name server never answers, as `net.connect` does in a task, and
    /// notes what it found, for the test `test`.
    fn connects_to_a_name_that_never_answers(name: &str, test: usize) {
        let started = std::time::Instant::now();
        let made = crate::stdlib::net::connected_parked(name, 80, LOOKUP_TIMEOUT, None);
        let took = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        TIMED_OUT[test]
            .store(matches!(made, Err(crate::stdlib::net::Failure::TimedOut(_))), Ordering::SeqCst);
        TOOK_MS[test].store(took, Ordering::SeqCst);
    }

    extern "C" fn connects_to_a_stalled_name(_env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        connects_to_a_name_that_never_answers("never.stalls.test", 0);
        0
    }

    /// A task connecting to a name whose name server never answers fails as timed out once the
    /// connect's timeout has passed, as a connect does: it waited for the lookup as long as the
    /// system's resolver took, holding a thread every other file's read and database call might
    /// need, however short its timeout (#1826). The lookup's thread is let go at the end, and its
    /// answer, which comes after the task gave up, is dropped.
    #[test]
    fn a_name_whose_lookup_never_answers_times_out_with_the_connect() {
        let _alone = crate::stdlib::net::tests::stalls::one_at_a_time();
        begin();
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, 1, super::Leaf::For(connects_to_a_stalled_name, 0));
        assert_eq!(run.finish(), 0);
        crate::stdlib::net::tests::stalls::let_go("never.stalls.test");
        assert!(TIMED_OUT[0].load(Ordering::SeqCst), "the connect didn't fail as timed out");
        let took = TOOK_MS[0].load(Ordering::SeqCst);
        let timeout = i64::try_from(LOOKUP_TIMEOUT.as_millis()).unwrap_or(i64::MAX);
        assert!(took >= timeout, "it gave up after {took} ms, before its timeout");
        // Well past its timeout is still well short of the lookup's, which never ends by itself.
        assert!(took < timeout * 25, "it gave up only after {took} ms");
        finish();
    }

    /// Sleeps for ten seconds, as a launched task that lives long does.
    extern "C" fn sleeps_beside_the_call(_env: i64, _thread: i64) -> i64 {
        super::super::capabilities::clock_sleep(10_000_000_000);
        0
    }

    static RANGES_RAN: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn notes_its_range(_env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        RANGES_RAN.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// A range whose body may wait, on a worker that can't get it a stack, runs where it is when no
    /// other range there could end and free one, though a launched task lives there. It waited for
    /// a task there to end, and a launched one that lives long held the call for as long (#1817).
    #[test]
    fn a_range_that_cant_get_a_stack_runs_where_it_is_beside_a_task_that_lives_long() {
        use std::time::{Duration, Instant};
        begin();
        let pool = super::pool::with_threads(1);
        let task = super::super::serve::launch_on(pool, sleeps_beside_the_call, 0);
        assert_ne!(task, 0);
        // From now on its worker gets no stack, as when the system refuses the memory.
        pool.run_on(0, Box::new(|_| super::super::serve::refuse_stacks_here(true)));
        // If the range never starts, the worker gets stacks again after ten seconds, so the test
        // fails rather than holding on.
        let over = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watching = Arc::clone(&over);
        let releasing = std::thread::spawn(move || {
            let started = Instant::now();
            while !watching.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(10));
            }
            pool.run_on(0, Box::new(|_| super::super::serve::refuse_stacks_here(false)));
        });
        let started = Instant::now();
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, 1, super::Leaf::For(notes_its_range, 0));
        assert_eq!(run.finish(), 0);
        let took = started.elapsed();
        over.store(true, Ordering::SeqCst);
        releasing.join().unwrap();
        assert_eq!(RANGES_RAN.load(Ordering::SeqCst), 1);
        assert!(took < Duration::from_secs(5), "the range waited {took:?} for a stack");
        let mut failure = 0;
        super::super::serve::launch_end(task, 1_000_000, 0, &mut failure);
        finish();
    }

    /// Whether the wait after a lookup that was given up on, parked on the lookup's token, was woken
    /// by anything before its own deadline.
    static WOKEN_EARLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    static ANSWERED_LATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Gives up on a lookup, then has the lookup answer while this task waits on the token the
    /// lookup parked on, as a wait would once the tokens had come round again.
    extern "C" fn gives_up_then_waits_on_the_same_token(
        _env: i64,
        _from: i64,
        _to: i64,
        _thread: i64,
    ) -> i64 {
        let name = "late.stalls.test";
        connects_to_a_name_that_never_answers(name, 1);
        crate::embed::serve::reuse_the_last_lookup_s_token();
        crate::stdlib::net::tests::stalls::let_go(name);
        // The lookup's answer is in by the time this parks, and a wake for its token with it, if
        // one was sent: the worker reads its mail only once this has parked.
        let answered =
            crate::stdlib::net::tests::stalls::answered_within(name, std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(50));
        ANSWERED_LATE.store(answered, Ordering::SeqCst);
        let token = crate::embed::serve::next_token();
        let parked = crate::embed::serve::park_on(token, Some(std::time::Instant::now() + LOOKUP_TIMEOUT));
        WOKEN_EARLY.store(!matches!(parked, crate::embed::serve::Parked::TimedOut), Ordering::SeqCst);
        0
    }

    /// A lookup's answer that comes after its task gave up wakes nothing, even a wait that parked on
    /// the same token since, as one would once the tokens had come round again: it'd be woken as
    /// if what it waited for had come.
    #[test]
    fn a_late_lookup_wakes_no_later_wait_on_its_token() {
        let _alone = crate::stdlib::net::tests::stalls::one_at_a_time();
        begin();
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let leaf = super::Leaf::For(gives_up_then_waits_on_the_same_token, 0);
        let run = super::each_on_pool(run, pool, 1, leaf);
        assert_eq!(run.finish(), 0);
        assert!(TIMED_OUT[1].load(Ordering::SeqCst), "the connect didn't fail as timed out");
        assert!(ANSWERED_LATE.load(Ordering::SeqCst), "the lookup never answered");
        assert!(!WOKEN_EARLY.load(Ordering::SeqCst), "the late lookup woke the wait on its token");
        finish();
    }

    /// How many of the tasks connecting at once to a name that never answers failed as timed out,
    /// and the thread they ran on.
    static CROWD_TIMED_OUT: AtomicUsize = AtomicUsize::new(0);
    static CROWD_THREAD: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

    extern "C" fn connects_with_a_crowd(_env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let made = crate::stdlib::net::connected_parked("crowd.stalls.test", 80, LOOKUP_TIMEOUT, None);
        if matches!(made, Err(crate::stdlib::net::Failure::TimedOut(_))) {
            CROWD_TIMED_OUT.fetch_add(1, Ordering::SeqCst);
        }
        *CROWD_THREAD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(std::thread::current().id());
        0
    }

    /// More tasks than the lookups' pool has threads, connecting at once to a name that never
    /// answers, each fail as timed out, holding no more than the pool's threads: those past it
    /// wait their turn parked. Once the stalled lookups end, a lookup whose task gave up before
    /// its turn came is dropped unmade, and every lookup the tasks handed over is counted done.
    #[test]
    fn more_lookups_than_the_pool_has_threads_wait_their_turn_and_give_up_on_time() {
        let _alone = crate::stdlib::net::tests::stalls::one_at_a_time();
        begin();
        let most = super::super::blocking::MOST_LOOKUPS;
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, most + 4, super::Leaf::For(connects_with_a_crowd, 0));
        assert_eq!(run.finish(), 0);
        let held = super::super::blocking::lookup_threads();
        crate::stdlib::net::tests::stalls::let_go("crowd.stalls.test");
        assert_eq!(CROWD_TIMED_OUT.load(Ordering::SeqCst), most + 4, "every task timed out");
        assert_eq!(held, most, "the stalled lookups held {held} threads, not the pool's {most}");
        let thread =
            CROWD_THREAD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expect("a task ran");
        let by = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while crate::embed::serve::blocking_jobs_of(thread).0 != 0 && std::time::Instant::now() < by {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let (left, handed) = crate::embed::serve::blocking_jobs_of(thread);
        assert_eq!(left, 0, "{left} of the {handed} lookups handed over are still counted");
        finish();
    }

    /// What a run of [`a_stalled_lookup_recorded_or_replayed`] is: `record` or `replay`, the log,
    /// and the file it says what it saw in.
    const LOOKUP_ROLE: &str = "FENNEC_TEST_LOOKUP_ROLE";
    const LOOKUP_LOG: &str = "FENNEC_TEST_LOOKUP_LOG";
    const LOOKUP_SAW: &str = "FENNEC_TEST_LOOKUP_SAW";

    /// What the task connecting through `Net`, recorded or replayed, was given, and its thread.
    static REPLAYED_CONNECT: Mutex<Option<(String, std::thread::ThreadId)>> = Mutex::new(None);

    /// `net.connect` to a name that never answers, as a program's task calls it through `Net`,
    /// recorded or replayed as the capability's call is.
    extern "C" fn connects_through_net(_env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let at = Span::default();
        let args = [Value::str("replayed.stalls.test"), Value::Int(80)];
        let made = crate::replay::value(format_args!("Net._connect"), at, || {
            crate::stdlib::net::real_net("_connect", &args, Some(LOOKUP_TIMEOUT), at)
        });
        *REPLAYED_CONNECT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((format!("{made:?}"), std::thread::current().id()));
        0
    }

    /// A recording or a replay of a task connecting to a name that never answers, as the
    /// environment says, saying what it saw into a file: what the connect gave, how many lookups
    /// its thread handed over, and any divergence. Run by
    /// [`a_lookup_that_timed_out_replays_as_timed_out_with_no_lookup`], each in a process of its
    /// own; on its own, it does nothing.
    #[test]
    #[ignore = "a recording or a replay needs a process of its own"]
    fn a_stalled_lookup_recorded_or_replayed() {
        let (Some(role), Some(log), Some(saw_into)) = (
            std::env::var_os(LOOKUP_ROLE),
            std::env::var_os(LOOKUP_LOG).map(std::path::PathBuf::from),
            std::env::var_os(LOOKUP_SAW),
        ) else {
            return;
        };
        let recording = role == "record";
        if recording {
            crate::replay::start_log(&log, b"lookup").expect("a log to record into");
            crate::replay::record_to(&log).expect("a recording");
        } else {
            crate::replay::replay_from(&log).expect("a replay");
        }
        let _alone = crate::stdlib::net::tests::stalls::one_at_a_time();
        begin();
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, 1, super::Leaf::For(connects_through_net, 0));
        assert_eq!(run.finish(), 0);
        crate::stdlib::net::tests::stalls::let_go("replayed.stalls.test");
        if recording {
            crate::replay::ended();
        }
        let (made, thread) = REPLAYED_CONNECT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("the task connected");
        let mut said = vec![made, format!("handed {}", crate::embed::serve::blocking_jobs_of(thread).1)];
        if let Some(why) = crate::replay::divergence() {
            said.push(format!("diverged: {why}"));
        }
        if let Some(why) = crate::replay::unfinished().filter(|_| !recording) {
            said.push(format!("unfinished: {why}"));
        }
        finish();
        std::fs::write(saw_into, said.join("\n")).expect("somewhere to say what it saw");
    }

    /// Runs [`a_stalled_lookup_recorded_or_replayed`] in a process of its own, on one worker, as
    /// `role`: what it saw. It's given 60 seconds.
    fn lookup_ran(role: &str, log: &std::path::Path) -> String {
        let saw = log.with_extension(format!("{role}-saw"));
        let _ = std::fs::remove_file(&saw);
        let mut child = std::process::Command::new(std::env::current_exe().expect("this test's binary"))
            .args([
                "--exact",
                "embed::parallel::tests::a_stalled_lookup_recorded_or_replayed",
                "--ignored",
                "--quiet",
            ])
            .env(LOOKUP_ROLE, role)
            .env(LOOKUP_LOG, log)
            .env(LOOKUP_SAW, &saw)
            .env("FENNEC_THREADS", "1")
            .spawn()
            .expect("this test's binary runs");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().expect("the run can be waited for") {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the {role} was still running after 60 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(status.success(), "the {role} failed: {status}");
        let said = std::fs::read_to_string(&saw).unwrap_or_default();
        let _ = std::fs::remove_file(&saw);
        said
    }

    /// Whether a name's lookup was given up on depends on time, so it's an outside input: a
    /// recording logs the connect it was in as `Net`'s call, timed out, and a replay gives that
    /// back without connecting, so it never hands a lookup over, nor waits for one.
    #[test]
    fn a_lookup_that_timed_out_replays_as_timed_out_with_no_lookup() {
        let folder = std::env::temp_dir().join(format!("fennec-lookup-replay-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("a folder for the log");
        let log = folder.join("lookup.log");
        let recorded = lookup_ran("record", &log);
        let replayed = lookup_ran("replay", &log);
        let _ = std::fs::remove_dir_all(&folder);
        let (Some((recorded_made, recorded_rest)), Some((replayed_made, replayed_rest))) =
            (recorded.split_once('\n'), replayed.split_once('\n'))
        else {
            panic!("the runs said too little:\n{recorded}\n--\n{replayed}");
        };
        assert!(recorded_made.contains("TimedOut"), "the recorded connect gave {recorded_made}");
        assert_eq!(replayed_made, recorded_made, "what the replayed connect gave");
        assert_eq!(recorded_rest, "handed 1", "the recording's lookup, and nothing wrong");
        assert_eq!(replayed_rest, "handed 0", "the replay's lookups, and nothing wrong");
    }

    /// A server that takes two connections and answers neither until both have sent a line.
    fn answers_once_both_have_spoken() -> u16 {
        use std::io::{BufRead as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let both: Vec<std::net::TcpStream> = listener.incoming().flatten().take(2).collect();
            for stream in &both {
                let mut line = String::new();
                let _ = std::io::BufReader::new(stream).read_line(&mut line);
            }
            for mut stream in &both {
                let _ = stream.write_all(b"pong\n");
            }
        });
        port
    }

    static BOTH_HEARD: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// Sends the server on port `env` a line and reads its answer, as a parked task.
    extern "C" fn says_and_hears(env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        let port = u16::try_from(env).unwrap();
        let heard = crate::stdlib::net::said_and_heard("127.0.0.1", port, b"ping\n");
        assert_eq!(heard, Ok(b"pong\n".to_vec()));
        // Woken by the poll, not by looking again on a timer, which a wait the poll won't take does.
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0);
        BOTH_HEARD.fetch_add(1, Ordering::Relaxed);
        0
    }

    /// A task waiting to read from a socket through a copy of it, as on Windows, parks: on one
    /// worker, a second task gets to send what the first one's answer waits for. A read there
    /// blocked its worker, so the second never ran, and neither was ever answered.
    #[test]
    fn a_read_that_waits_through_a_copy_of_its_socket_lets_its_workers_other_task_run() {
        begin();
        let port = answers_once_both_have_spoken();
        let before = crate::embed::serve::PARKED_ON_COPIES.load(Ordering::Relaxed);
        let on_copies = crate::stdlib::net::OnCopies::begin();
        let pool = super::pool::with_threads(1);
        let run = super::Run::new(1).waiting(true);
        let run = super::each_on_pool(run, pool, 2, super::Leaf::For(says_and_hears, i64::from(port)));
        let ended = run.finish();
        drop(on_copies);
        assert_eq!(ended, 0);
        assert_eq!(BOTH_HEARD.load(Ordering::Relaxed), 2);
        assert!(crate::embed::serve::PARKED_ON_COPIES.load(Ordering::Relaxed) > before);
        finish();
    }

    /// A server that takes connections and keeps them open, sending nothing.
    fn silent_server() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _kept: Vec<std::net::TcpStream> = listener.incoming().flatten().collect();
        });
        port
    }

    static KEPT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static ENDED_BY_A_CLOSE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// The first of two tasks keeps a connection to the silent server on port `env` and reads a
    /// line from it, which never comes. The second closes that connection once the first waits.
    extern "C" fn reads_or_closes(env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{Failure, kept, line_from, shut};
        if from == 0 {
            let handle = kept("127.0.0.1", u16::try_from(env).unwrap()).unwrap();
            KEPT.store(handle, Ordering::SeqCst);
            let read = line_from(handle);
            let ended = matches!(read, Ok(None) | Err(Failure::Closed));
            ENDED_BY_A_CLOSE.store(if ended { 1 } else { 2 }, Ordering::SeqCst);
        } else {
            // On the one worker this runs only while the other task is parked, and once the
            // connection's kept the next place that parks is its read.
            while KEPT.load(Ordering::SeqCst) == 0 {
                crate::embed::serve::yield_now();
            }
            shut(KEPT.load(Ordering::SeqCst));
        }
        0
    }

    /// Closing a connection from one task ends a read another task is parked in, when the other
    /// side neither sends nor closes: the socket's own way on Unix, and through a copy of it, as
    /// on Windows. There a worker's poll isn't told of a close made from this side, so the close
    /// wakes the parked task itself. Nothing tested this on any system.
    #[test]
    fn a_close_from_another_task_ends_a_read_parked_on_the_connection() {
        begin();
        let port = silent_server();
        for through_a_copy in [false, true] {
            KEPT.store(0, Ordering::SeqCst);
            ENDED_BY_A_CLOSE.store(0, Ordering::SeqCst);
            let on_copies = through_a_copy.then(crate::stdlib::net::OnCopies::begin);
            let pool = super::pool::with_threads(1);
            let run = super::Run::new(1).waiting(true);
            let run = super::each_on_pool(run, pool, 2, super::Leaf::For(reads_or_closes, i64::from(port)));
            let ended = run.finish();
            drop(on_copies);
            assert_eq!(ended, 0);
            assert_eq!(ENDED_BY_A_CLOSE.load(Ordering::SeqCst), 1, "through a copy: {through_a_copy}");
        }
        finish();
    }

    /// The connection the two tasks below share, and how each one's wait on it ended: 1 for as it
    /// should have.
    static SHARED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static READ_IT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static WROTE_IT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    /// Whether the first of the two tasks to wait is the one that writes.
    static WRITER_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    /// How many of the two tasks' waits their worker's poll didn't take, which then look again on
    /// a timer: none, or the test would pass without either being woken by what it waits for.
    static ON_A_TIMER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    /// More than a socket's buffers hold, so writing it waits for the other side to read.
    const MUCH: usize = 8 << 20;
    /// How long each wait has, so a wake that's lost fails the test rather than hang it.
    const WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

    /// The first of two tasks keeps a connection to the server on port `env`, and the second uses
    /// the same one. One reads a line from it and the other writes `MUCH` to it, each waiting on
    /// the connection for the server, which serves neither until both wait.
    extern "C" fn reads_while_another_writes(env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        use crate::stdlib::net::{kept, line_from_within, written_within};
        let first = from == 0;
        if first {
            SHARED.store(kept("127.0.0.1", u16::try_from(env).unwrap()).unwrap(), Ordering::SeqCst);
        }
        // On the one worker the second task runs only while the first is parked, and once the
        // connection's kept the next place the first parks is its wait on it.
        while SHARED.load(Ordering::SeqCst) == 0 {
            crate::embed::serve::yield_now();
        }
        let handle = SHARED.load(Ordering::SeqCst);
        if first == WRITER_FIRST.load(Ordering::SeqCst) {
            let wrote = written_within(handle, &vec![b'x'; MUCH], WITHIN);
            WROTE_IT.store(if wrote.is_ok() { 1 } else { 2 }, Ordering::SeqCst);
        } else {
            let read = line_from_within(handle, WITHIN);
            READ_IT.store(if read == Ok(Some(b"done\n".to_vec())) { 1 } else { 2 }, Ordering::SeqCst);
        }
        ON_A_TIMER.fetch_max(crate::embed::serve::waits_not_registered_here(), Ordering::SeqCst);
        0
    }

    /// A server for one connection. It does nothing until two tasks wait on the other end, and
    /// gives back how many of them had the socket itself and how many a copy. Then it serves the
    /// one that came second first, sending the reader its line and reading all the writer wrote:
    /// so the wait that ends first is the one with the copy, and ending must leave the other's
    /// place in its worker's poll alone.
    fn serves_the_second_wait_first(writer_first: bool) -> (u16, std::thread::JoinHandle<(usize, usize)>) {
        use std::io::{Read as _, Write as _};
        fn read_all_of_it(mut from: &std::net::TcpStream) {
            let mut left = MUCH;
            let mut some = vec![0_u8; 1 << 16];
            while left > 0 {
                match from.read(&mut some) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => left = left.saturating_sub(read),
                }
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(WITHIN)).unwrap();
            let started = std::time::Instant::now();
            let mut waiting = (0, 0);
            while waiting.0 + waiting.1 < 2 && started.elapsed() < WITHIN {
                std::thread::yield_now();
                waiting = crate::stdlib::net::waiting_on(SHARED.load(Ordering::SeqCst));
            }
            if writer_first {
                let _ = (&stream).write_all(b"done\n");
                read_all_of_it(&stream);
            } else {
                read_all_of_it(&stream);
                let _ = (&stream).write_all(b"done\n");
            }
            // The connection stays open until both tasks are done with it.
            let done = || READ_IT.load(Ordering::SeqCst) != 0 && WROTE_IT.load(Ordering::SeqCst) != 0;
            while !done() && started.elapsed() < 3 * WITHIN {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            waiting
        });
        (port, server)
    }

    /// Two tasks on one worker wait on one connection at once, one to read from it and one for
    /// room to write to it, as a chat's reader and the task that broadcasts to it do. Each is
    /// woken by what it waits for, whichever came first.
    ///
    /// Both used to hand the worker's poll the socket's own descriptor, which a poll takes once.
    /// On macOS the first wait to end took both off the poll and the other was never woken: this
    /// test then fails when that one's ten seconds run out. On Linux the second was refused and
    /// tried again at once for as long as it waited, its worker never polling: there it fails on
    /// the count, as nothing went through a copy.
    #[test]
    fn a_reader_and_a_writer_parked_on_one_connection_are_each_woken() {
        begin();
        for writer_first in [false, true] {
            for slot in [&SHARED, &READ_IT, &WROTE_IT] {
                slot.store(0, Ordering::SeqCst);
            }
            ON_A_TIMER.store(0, Ordering::SeqCst);
            WRITER_FIRST.store(writer_first, Ordering::SeqCst);
            let (port, server) = serves_the_second_wait_first(writer_first);
            let pool = super::pool::with_threads(1);
            let run = super::Run::new(1).waiting(true);
            let leaf = super::Leaf::For(reads_while_another_writes, i64::from(port));
            let ended = super::each_on_pool(run, pool, 2, leaf).finish();
            let waiting = server.join().unwrap();
            crate::stdlib::net::shut(SHARED.load(Ordering::SeqCst));
            assert_eq!(ended, 0);
            assert_eq!(WROTE_IT.load(Ordering::SeqCst), 1, "the write; writer first: {writer_first}");
            assert_eq!(READ_IT.load(Ordering::SeqCst), 1, "the read; writer first: {writer_first}");
            assert_eq!(waiting, (1, 1), "one has the socket and one a copy; writer first: {writer_first}");
            assert_eq!(
                ON_A_TIMER.load(Ordering::SeqCst),
                0,
                "a wait the poll didn't take; writer first: {writer_first}"
            );
        }
        finish();
    }

    #[test]
    fn a_reduction_that_may_wait_comes_to_the_same_bits() {
        begin();
        let list = lists::new(0, ITEMS);
        let (mut plain, mut waiting) = (0, 0);
        assert_eq!(super::parallel_reduce(list, harmonic, add, 0, &mut plain), 0);
        assert_eq!(super::parallel_reduce_waiting(list, harmonic, add, 0, &mut waiting), 0);
        assert_eq!(plain, waiting);
        heap::free(list);
        finish();
    }

    #[test]
    fn of_several_failures_in_a_loop_that_may_wait_the_lowest_range_s_is_passed_up() {
        begin();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for_waiting(list, fails_twice, 0), 1);
        heap::free(list);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message.ends_with("at 70000")));
    }

    static MADE: Mutex<Vec<(i64, i64, i64)>> = Mutex::new(Vec::new());

    extern "C" fn names(_env: i64, from: i64, to: i64, thread: i64) -> i64 {
        if from == 0 || to == ITEMS {
            let name = with(|state| heap::text_address(state, &format!("made for the range from {from}")));
            MADE.lock().unwrap().push((from, name, thread));
        }
        0
    }

    #[test]
    fn what_tasks_make_is_adopted_by_the_thread_that_ran_the_loop() {
        begin();
        let before = with(|state| state.heap.live());
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, names, 0), 0);
        heap::free(list);
        let made = std::mem::take(&mut *MADE.lock().unwrap());
        assert_eq!(made.len(), 2);
        // The ranges ran on the pool's workers, not on this thread.
        let here = i64::from(threads::index());
        assert!(made.iter().all(|(_, _, thread)| *thread != here), "{made:?}");
        for (from, name, _) in made {
            let read = with(|state| Some(heap::read_block(state, name.cast_unsigned()))).unwrap();
            assert_eq!(read, Value::str(&format!("made for the range from {from}")));
            heap::free(name);
        }
        assert_eq!(with(|state| state.heap.live()), before);
        finish();
    }

    /// The ranges the heavy-body test's calls ran: where each starts, how long it is, and the
    /// thread it ran on.
    static HEAVY: Mutex<Vec<(i64, i64, i64)>> = Mutex::new(Vec::new());

    extern "C" fn heavy(_env: i64, from: i64, to: i64, thread: i64) -> i64 {
        HEAVY.lock().unwrap().push((from, to - from, thread));
        0
    }

    fn heavy_ranges() -> Vec<(i64, i64, i64)> {
        let mut ranges = std::mem::take(&mut *HEAVY.lock().unwrap());
        ranges.sort_unstable();
        ranges
    }

    /// A heavy body's list is cut into at least 64 ranges however short it is, or an item each when
    /// it's shorter, cut the same way each time and shared among the pool's workers, where a light
    /// body's few items stay one range.
    #[test]
    fn a_heavy_body_s_short_list_is_cut_into_at_least_64_ranges_on_the_pool() {
        begin();
        let here = i64::from(threads::index());
        for length in [2_i64, 100, 640, 1000] {
            let list = lists::new(0, length);
            assert_eq!(super::parallel_for_heavy(list, heavy, 0), 0);
            let ranges = heavy_ranges();
            assert!(ranges.len() >= 64.min(usize::try_from(length).unwrap()), "{length}: {ranges:?}");
            let longest = (length + 63) / 64;
            assert!(ranges.iter().all(|(_, size, _)| (1..=longest).contains(size)), "{length}: {ranges:?}");
            // Together they cover the list once, in order.
            let mut next = 0;
            for (from, size, _) in &ranges {
                assert_eq!(*from, next, "{length}: {ranges:?}");
                next += size;
            }
            assert_eq!(next, length);
            assert!(ranges.iter().all(|(_, _, thread)| *thread != here), "{length}: ran here");
            // The same cut again.
            assert_eq!(super::parallel_for_heavy(list, heavy, 0), 0);
            let again: Vec<_> = heavy_ranges().iter().map(|(from, size, _)| (*from, *size)).collect();
            assert_eq!(again, ranges.iter().map(|(from, size, _)| (*from, *size)).collect::<Vec<_>>());
            assert_eq!(super::parallel_for(list, heavy, 0), 0);
            assert_eq!(heavy_ranges().len(), 1, "a light body's {length} items are one range");
            heap::free(list);
        }
        finish();
    }

    /// How many of the lending test's ranges ran.
    static RANGES: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// Runs on a worker: `env` is a string the calling thread made, read and joined to itself
    /// through the runtime, and given two owners more and one fewer, leaving one more.
    extern "C" fn uses_lent(env: i64, _from: i64, _to: i64, thread: i64) -> i64 {
        assert_ne!(thread, 0, "on a worker");
        let joined = strings::concat(env, env);
        let read = with(|state| Some(heap::read_block(state, joined.cast_unsigned()))).unwrap();
        assert_eq!(read, Value::str(&TEXT.repeat(2)));
        heap::free(joined);
        // It's copied to change it, as this thread doesn't hold it alone, whatever its count.
        with(|state| {
            let block = state.heap.block(env.cast_unsigned()).unwrap();
            assert!(!state.heap.is_alone(env.cast_unsigned(), block));
        });
        threads::retain_shared(env, 0);
        threads::retain_shared(env, 0);
        threads::release_shared(env, 0);
        RANGES.fetch_add(1, Ordering::Relaxed);
        0
    }

    const TEXT: &str = "a string long enough to be held in a block of its own";

    fn string_block() -> i64 {
        with(|state| heap::text_address(state, TEXT))
    }

    /// The calling thread's table holds its blocks again, and a block's count is right.
    #[test]
    fn ranges_read_lent_blocks_and_their_owner_changes_are_applied_when_the_call_ends() {
        begin();
        let text = string_block();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, uses_lent, text), 0);
        let ranges = u64::try_from(RANGES.load(Ordering::Relaxed)).unwrap();
        assert!(ranges > 1);
        let count =
            with(|state| state.heap.block(text.cast_unsigned()).and_then(|block| block.first().copied()));
        assert_eq!(count, Some(1 + ranges));
        // It's this thread's again: what the runtime holds of it, and that it's its alone.
        for _ in 0..ranges {
            with(|state| heap::release(state, text.cast_unsigned()));
        }
        with(|state| {
            let block = state.heap.block(text.cast_unsigned()).unwrap();
            assert!(state.heap.is_alone(text.cast_unsigned(), block));
        });
        heap::free(text);
        heap::free(list);
        finish();
    }

    /// Takes the one owner `env`'s block has, on the worker running the range at position 0.
    extern "C" fn lets_go(env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        if from == 0 {
            threads::release_shared(env, 0);
        }
        0
    }

    #[test]
    fn a_block_ranges_take_the_last_owner_of_is_freed_when_the_call_ends() {
        begin();
        let before = with(|state| state.heap.live());
        let text = string_block();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, lets_go, text), 0);
        assert!(with(|state| state.heap.block(text.cast_unsigned()).is_none()));
        heap::free(list);
        assert_eq!(with(|state| state.heap.live()), before);
        finish();
    }

    /// How many of the printing test's ranges ran, kept apart as tests run at the same time.
    static PRINTING_RANGES: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    extern "C" fn prints(env: i64, _from: i64, _to: i64, _thread: i64) -> i64 {
        strings::print(env);
        PRINTING_RANGES.fetch_add(1, Ordering::Relaxed);
        0
    }

    #[test]
    fn what_a_range_prints_on_a_worker_reaches_the_calling_thread() {
        begin();
        let text = string_block();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, prints, text), 0);
        let ranges = usize::try_from(PRINTING_RANGES.load(Ordering::Relaxed)).unwrap();
        heap::free(list);
        heap::free(text);
        let (output, _, _) = finish();
        assert_eq!(String::from_utf8(output).unwrap(), TEXT.repeat(ranges));
    }

    extern "C" fn prints_where_it_starts(_env: i64, from: i64, _to: i64, _thread: i64) -> i64 {
        let text = with(|state| heap::text_address(state, &format!("{from},")));
        strings::print(text);
        heap::free(text);
        0
    }

    /// What ranges print on workers comes out in the order they start in the list, whatever order
    /// they finish in, so a call prints the same every run.
    #[test]
    fn what_ranges_print_comes_out_in_their_order() {
        begin();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, prints_where_it_starts, 0), 0);
        heap::free(list);
        let (output, _, _) = finish();
        let starts: Vec<usize> =
            String::from_utf8(output).unwrap().split_terminator(',').map(|n| n.parse().unwrap()).collect();
        assert!(starts.len() > 1, "the call ran as one range: {starts:?}");
        assert_eq!(starts.first(), Some(&0));
        assert!(starts.is_sorted(), "{starts:?}");
    }

    #[test]
    fn what_a_thread_kept_comes_out_when_it_panics() {
        crate::embed::write_kept_output_on_panic();
        let mark = crate::embed::written_mark();
        let panicked = std::thread::spawn(|| {
            let inputs = crate::Inputs { live: true, ..crate::Inputs::default() };
            start(Tables::default(), inputs);
            with(|state| {
                let _ = crate::embed::io::write(state, "printed just before panicking\n");
            });
            std::panic::panic_any("panicking on purpose");
        })
        .join();
        assert!(panicked.is_err());
        assert!(crate::embed::written_since(mark).contains("printed just before panicking\n"));
    }
}
