//! What the runtime needs when native code runs on more than one thread. Each thread has its
//! own runtime: its own heap and table of the blocks it made, which only it frees. A block's
//! shape word holds the number of the thread that made it (see `heap::OWNER_SHIFT`), 0 for the
//! main thread.
//!
//! Counts are biased. The thread that made a block counts it in its count word with plain
//! arithmetic. Any other thread counts it here, in a shared count kept beside it, so the two
//! never race on one word. When the owner lets go of its last count while others still hold
//! the block, the block waits; when the last of those lets go, the block goes on its owner's
//! queue, and the owner frees it the next time it's in the runtime. No thread ever frees a
//! block another made, and no table is shared.
//!
//! A `parallel` call works differently, since the thread that made the call only waits for it.
//! For the length of the call it lends its table of blocks (`lend`), which nothing can change
//! while it waits, so each range can read its blocks. Nothing writes a lent block: `alone` is
//! false for one, so it's copied rather than changed in place. Each range keeps how it changed
//! lent blocks' owners to itself, with no lock, and hands that over with what it made. When
//! the call ends, the calling thread takes its table back, adopts what the ranges made, and
//! applies those changes (`settle`), freeing what they took the last owner from.

use std::collections::HashMap;
use std::io::Write as _;
#[cfg(any(test, feature = "threads"))]
use std::sync::Arc;
#[cfg(any(test, feature = "threads"))]
use std::sync::atomic::AtomicU8;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

#[cfg(any(test, feature = "threads"))]
use super::table::Table;

/// Whether this runtime is built for native code on several threads. Without it, the checks
/// on the paths every block takes are compiled away.
pub(super) const ON: bool = cfg!(any(test, feature = "threads"));

/// The next number a thread taking one gets. The main thread is 0.
static NEXT: AtomicU16 = AtomicU16::new(1);

thread_local! {
    static INDEX: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
    /// The run this thread's runtime is, among all started in the process: threads with the same
    /// number, as every test's own thread is, can each run a runtime, and a block waits for the one
    /// that made it.
    static RUN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The next run a runtime starting gets.
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// A runtime starts on this thread: it's a run of its own, with a queue of its own.
pub(super) fn run_starts() {
    RUN.set(NEXT_RUN.fetch_add(1, Ordering::Relaxed));
}

/// The queue the runtime on this thread frees from: its thread's number, and its run.
type Queue = (u16, u64);

fn own_queue() -> Queue {
    (index(), RUN.get())
}

#[cfg(any(test, feature = "threads"))]
thread_local! {
    /// How far the launched task running on this thread has been told to stop, if one is: see
    /// `STOPPING` and `STOPPED_HARD`.
    static STOP: std::cell::RefCell<Option<Arc<AtomicU8>>> = const { std::cell::RefCell::new(None) };
    /// How many crashes' cleanups the task running on this thread is in.
    static CLEANING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// What this thread, or the task running on it, waits in as the code that launched tasks: so a
    /// task of its that crashes can cut those waits short (see [`Launcher`]). Made the first time
    /// it launches one.
    static LAUNCHER: std::cell::RefCell<Option<Arc<Launcher>>> = const { std::cell::RefCell::new(None) };
    /// Whether what's running on this thread is a range of a launched task's `parallel` call, on
    /// the pool: it stops as the task does, and its own calls run their ranges here, as any
    /// range's do.
    static RANGE_OF_TASK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// For a range of a `parallel` call on the pool, what the code that made the call waits in as
    /// one that launched tasks, if it has: the range is that code's, so a crash of one of those
    /// tasks cuts the range's waits short too. Not the range's own, for the tasks it launches
    /// itself, which is `LAUNCHER`.
    static CALLER_S_LAUNCHER: std::cell::RefCell<Option<Arc<Launcher>>> =
        const { std::cell::RefCell::new(None) };
    /// For a range of a `parallel` call whose body may wait, how a deadlock's report names it, as
    /// "range 3 of main's `parallel` call". Set as the range starts, and swapped with its task's
    /// accounts as it parks and resumes.
    static RANGE_NAME: std::cell::RefCell<Option<super::deadlock::RangeName>> =
        const { std::cell::RefCell::new(None) };
    /// Whether what runs here is a range of a waiting `parallel` call run as a parked task of its
    /// own, with accounts of its own: its list of what it made may be let go, as a launched task's
    /// may, and what it holds when it ends is found by its mark (#1842).
    static OWN_RANGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The name of the range of a waiting `parallel` call running here, if one is (see `RANGE_NAME`).
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_named() -> Option<super::deadlock::RangeName> {
    RANGE_NAME.with_borrow(Clone::clone)
}

/// Names the range running here `name`, giving back the name before, to put back as it ends.
#[cfg(any(test, feature = "threads"))]
pub(super) fn name_range(name: Option<super::deadlock::RangeName>) -> Option<super::deadlock::RangeName> {
    RANGE_NAME.replace(name)
}

/// The number the next wait said to a launcher gets.
#[cfg(any(test, feature = "threads"))]
static NEXT_WAIT: AtomicU64 = AtomicU64::new(1);

/// How to wake a wait.
#[cfg(any(test, feature = "threads"))]
type Waking = Arc<dyn Fn() + Send + Sync>;

/// A wait said to the launchers whose tasks' crashes cut it short, for `waited_here` to take back:
/// its number, or 0 when there was no launcher to say it to.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) struct Waiting(u64);

/// The code that launched tasks, as they see it: how many of them crashed or failed with their
/// `with launch` block yet to say so, and how to wake what it's waiting in now. While any has, each
/// wait it's in, or comes to, is cut short, as a stop cuts a task's short: it would otherwise wait
/// for good on a task that's gone, never reaching the block's end, which says `Crashed`.
///
/// What it waits in may be a `parallel` call, whose ranges wait on other threads: they're its
/// code, so their waits are cut short the same way, each one said here by its number.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
pub(in crate::embed) struct Launcher {
    crashed: std::sync::atomic::AtomicUsize,
    waking: Mutex<Vec<(u64, Waking)>>,
}

#[cfg(any(test, feature = "threads"))]
impl Launcher {
    /// This thread's, or the task's running on it, made the first time it's asked for.
    pub(in crate::embed) fn here() -> Arc<Launcher> {
        LAUNCHER.with_borrow_mut(|launcher| Arc::clone(launcher.get_or_insert_with(Arc::default)))
    }

    /// Whether a task this thread or task launched has crashed or failed, unreported: what it
    /// waits in ends.
    pub(in crate::embed) fn cut_short_here() -> bool {
        let cut_short = |launcher: &Option<Arc<Launcher>>| launcher.as_ref().is_some_and(|it| it.cut_short());
        LAUNCHER.with_borrow(cut_short) || CALLER_S_LAUNCHER.with_borrow(cut_short)
    }

    /// Does `each` with this thread's or task's own, and with its call's caller's when it's a range
    /// of a call on the pool: whichever there are.
    fn each_here(mut each: impl FnMut(&Launcher)) {
        LAUNCHER.with_borrow(|launcher| launcher.as_deref().map(&mut each));
        CALLER_S_LAUNCHER.with_borrow(|launcher| launcher.as_deref().map(&mut each));
    }

    pub(in crate::embed) fn cut_short(&self) -> bool {
        self.crashed.load(Ordering::SeqCst) > 0
    }

    /// The launchers a crash of whose tasks would cut short a wait here: this thread's or task's
    /// own, and its call's caller's, whichever there are. For the deadlock check: a crash wakes a
    /// wait that blocks its thread without changing what it waits on.
    pub(in crate::embed) fn those_here() -> [Option<Arc<Launcher>>; 2] {
        [LAUNCHER.with_borrow(Clone::clone), CALLER_S_LAUNCHER.with_borrow(Clone::clone)]
    }

    /// Says how to wake what this thread or task is about to wait in, for a crash to cut it short.
    /// Said before the wait's last look, so a crash between the two is never missed. Nothing's said
    /// when neither it nor the caller of the call it's a range of launched anything.
    pub(in crate::embed) fn waiting_here(waking: impl Fn() + Send + Sync + 'static) -> Waiting {
        let waking: Waking = Arc::new(waking);
        let mut wait = 0;
        Launcher::each_here(|launcher| {
            if wait == 0 {
                wait = NEXT_WAIT.fetch_add(1, Ordering::Relaxed);
            }
            launcher.waking.lock().unwrap_or_else(PoisonError::into_inner).push((wait, Arc::clone(&waking)));
        });
        Waiting(wait)
    }

    /// The wait's over: it's not to be woken.
    pub(in crate::embed) fn waited_here(Waiting(wait): Waiting) {
        if wait == 0 {
            return;
        }
        Launcher::each_here(|launcher| {
            launcher.waking.lock().unwrap_or_else(PoisonError::into_inner).retain(|(said, _)| *said != wait);
        });
    }

    /// A task it launched crashed or failed: its waits end from now on, and each one it's in wakes,
    /// its own and those of the ranges of a call it's waiting for, in the order they were said.
    pub(in crate::embed) fn task_crashed(&self) {
        self.crashed.fetch_add(1, Ordering::SeqCst);
        let waking = std::mem::take(&mut *self.waking.lock().unwrap_or_else(PoisonError::into_inner));
        for (_, wake) in waking {
            wake();
        }
    }

    /// The block that launched a task that crashed or failed has ended, saying so.
    pub(in crate::embed) fn crash_said(&self) {
        let _ = self.crashed.try_update(Ordering::SeqCst, Ordering::SeqCst, |count| count.checked_sub(1));
    }
}

/// A launched task's stop: each of its waits is cut short, unless it's cleaning up after a crash.
#[cfg(any(test, feature = "threads"))]
pub(crate) const STOPPING: u8 = 1;
/// A launched task's stop once its cleanup has had its grace: every wait is cut short.
#[cfg(any(test, feature = "threads"))]
pub(crate) const STOPPED_HARD: u8 = 2;

/// Whether the task running on this thread was launched and is to stop now: its waits are cut short.
/// A cleanup after a crash may still wait, until it's stopped hard.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn stopped() -> bool {
    let level = STOP.with_borrow(|stop| stop.as_ref().map_or(0, |stop| stop.load(Ordering::Acquire)));
    level >= STOPPED_HARD || (level >= STOPPING && CLEANING.get() == 0)
}

/// Whether a launched task is what's running on this thread, rather than `main` or a range's job.
/// A range of a launched task's own call is a range like any other here: it only stops as its task
/// does.
pub(crate) fn in_a_task() -> bool {
    #[cfg(any(test, feature = "threads"))]
    return STOP.with_borrow(Option::is_some) && !RANGE_OF_TASK.get();
    #[cfg(not(any(test, feature = "threads")))]
    false
}

/// How far the launched task running on this thread has been told to stop, shared with whatever
/// stops it, if a launched task is what's running.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn task_stop() -> Option<Arc<AtomicU8>> {
    STOP.with_borrow(Clone::clone)
}

/// What the ranges of a `parallel` call on the pool take with them from the code that made it,
/// whichever thread each runs on: they're that code's, so what cuts its waits short cuts theirs.
#[cfg(any(test, feature = "threads"))]
#[derive(Clone, Default)]
pub(super) struct Calling {
    /// A launched task's stop, when one made the call.
    stop: Option<Arc<AtomicU8>>,
    /// How many crashes' cleanups that task was in as it made the call: a cleanup may still wait
    /// after a stop, until it's stopped hard, and so may the ranges of a call it makes.
    cleaning: usize,
    /// What the caller waits in as the code that launched tasks, if it has launched any.
    launcher: Option<Arc<Launcher>>,
}

#[cfg(any(test, feature = "threads"))]
impl Calling {
    /// What a call made here, now, gives its ranges.
    pub(super) fn here() -> Calling {
        Calling { stop: None, cleaning: 0, launcher: LAUNCHER.with_borrow(Clone::clone) }
    }

    /// The same, for a call made by the launched task told to stop with `stop`.
    pub(super) fn of_task(mut self, stop: Arc<AtomicU8>) -> Calling {
        self.stop = Some(stop);
        self.cleaning = CLEANING.get();
        self
    }

    /// Whether a launched task made the call, for the pool.
    pub(super) fn is_task_s(&self) -> bool {
        self.stop.is_some()
    }
}

/// What was running on a thread before a range of a call made elsewhere started there, for
/// `range_off_its_caller_ends` to put back.
#[cfg(any(test, feature = "threads"))]
pub(super) struct BeforeRange {
    /// The stop, whether a task's range ran, and the cleanups it was in, when the range is a
    /// launched task's.
    task_s: Option<(Option<Arc<AtomicU8>>, bool, usize)>,
    caller_s_launcher: Option<Arc<Launcher>>,
}

/// Starts a range on a thread other than its call's caller waits on, or as a task of its own: a
/// crash of a task the caller launched cuts its waits short, and when the caller is a launched
/// task it's told to stop with the task's stop, so a stop cuts them short too, as late as it would
/// cut the task's own: a cleanup's ranges have the cleanup's grace.
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_off_its_caller_starts(calling: &Calling) -> BeforeRange {
    BeforeRange {
        task_s: calling.stop.clone().map(|stop| {
            (STOP.replace(Some(stop)), RANGE_OF_TASK.replace(true), CLEANING.replace(calling.cleaning))
        }),
        caller_s_launcher: CALLER_S_LAUNCHER.replace(calling.launcher.clone()),
    }
}

/// Ends such a range on this thread.
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_off_its_caller_ends(BeforeRange { task_s, caller_s_launcher }: BeforeRange) {
    if let Some((stop, range, cleaning)) = task_s {
        STOP.set(stop);
        RANGE_OF_TASK.set(range);
        CLEANING.set(cleaning);
    }
    CALLER_S_LAUNCHER.set(caller_s_launcher);
}

/// Whether what's running on this thread is a range of a launched task's `parallel` call.
#[cfg(any(test, feature = "threads"))]
pub(super) fn in_range_of_task() -> bool {
    RANGE_OF_TASK.get()
}

/// Whether the task running on this thread is cleaning up after a crash.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn cleaning_up() -> bool {
    CLEANING.get() > 0
}

/// The task running on this thread starts cleaning up after a crash, or ends doing so.
pub(crate) fn cleaning(starts: bool) {
    #[cfg(any(test, feature = "threads"))]
    CLEANING.set(if starts { CLEANING.get() + 1 } else { CLEANING.get().saturating_sub(1) });
    #[cfg(not(any(test, feature = "threads")))]
    let _ = starts;
}

thread_local! {
    /// The `shared` values' lock words this thread holds beside the one native code remembers.
    static LOCKS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Notes that this thread is taking the lock at `word`: 1 if it holds it already, which would
/// wait forever, and 0 otherwise.
pub extern "C" fn lock_note(word: u64) -> i64 {
    LOCKS.with_borrow_mut(|held| {
        if held.contains(&word) {
            return 1;
        }
        held.push(word);
        0
    })
}

/// Whether this thread holds any `shared` value's lock.
pub(super) fn holds_locks() -> bool {
    super::lock_word_held() || LOCKS.with_borrow(|held| !held.is_empty())
}

/// Notes that this thread has given back the lock at `word`.
pub extern "C" fn lock_forget(word: u64) {
    LOCKS.with_borrow_mut(|held| {
        if let Some(at) = held.iter().rposition(|taken| *taken == word) {
            held.swap_remove(at);
        }
    });
}

/// This thread's number: 0 for the main thread, until it takes another.
pub fn index() -> u16 {
    INDEX.get()
}

/// The last number a thread can take: those above it mark blocks in a copy lent to other threads,
/// blocks that are never freed, a `shared` value's own block, and blocks in a `shared` value's
/// table, and 0 is the main thread's.
const LAST_INDEX: u16 = 0xFFFB;

/// The next number from `next`, or `None` once they're all taken: it never comes round again to
/// 0 or to the numbers that mark blocks.
fn next_index(next: &AtomicU16) -> Option<u16> {
    next.try_update(Ordering::Relaxed, Ordering::Relaxed, |index| (index <= LAST_INDEX).then(|| index + 1))
        .ok()
}

/// Gives this thread a number of its own, for a worker before its runtime starts. A process
/// starts a worker once for each of its pool's threads, so running out is a bug in Fennec, and the
/// program stops rather than give two threads one number.
pub fn take_index() -> u16 {
    let Some(index) = next_index(&NEXT) else {
        super::write_kept_output();
        let _ = writeln!(
            std::io::stderr(),
            "error: more than {LAST_INDEX} threads asked for a number of their own\n  \
             = help: this is a bug in Fennec; please report it"
        );
        super::aot::stop_now(1)
    };
    INDEX.set(index);
    index
}

/// A block another thread holds: how many counts the other threads hold, whether its owner has
/// let go of all of its own, and, once it has, the queue it waits on: its owner's run's.
#[derive(Default)]
struct Shared {
    count: u64,
    owner_done: bool,
    owner: u16,
    run: u64,
}

/// Whether any block has ever been counted across threads, so a program with none never
/// looks in the table.
static ANY_SHARED: AtomicBool = AtomicBool::new(false);

/// Blocks counted across threads, by address.
static SHARED: Mutex<Option<HashMap<u64, Shared>>> = Mutex::new(None);

/// Blocks waiting for the runtime that made them to free them, by its thread's number and run.
static REMOTE: Mutex<Option<HashMap<Queue, Vec<u64>>>> = Mutex::new(None);

/// Whether any thread has blocks waiting on its queue.
static ANY_REMOTE: AtomicBool = AtomicBool::new(false);

fn shared<T>(f: impl FnOnce(&mut HashMap<u64, Shared>) -> T) -> T {
    let mut table = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    f(table.get_or_insert_with(HashMap::new))
}

fn remote<T>(f: impl FnOnce(&mut HashMap<Queue, Vec<u64>>) -> T) -> T {
    let mut queues = REMOTE.lock().unwrap_or_else(PoisonError::into_inner);
    f(queues.get_or_insert_with(HashMap::new))
}

/// Lends this thread's blocks to the ranges of the call it's about to wait for.
#[cfg(any(test, feature = "threads"))]
pub(super) fn lend() -> Arc<Table> {
    super::with(|state| Arc::new(std::mem::take(&mut state.heap.blocks)))
}

/// Takes this thread's blocks back, the call over. Every range has let go of them by now; if
/// one somehow hadn't, they're kept where this thread can read them, as native code still
/// points at them.
#[cfg(any(test, feature = "threads"))]
pub(super) fn take_back(lent: Arc<Table>) {
    super::with(|state| match Arc::try_unwrap(lent) {
        Ok(table) => state.heap.blocks = table,
        Err(table) => state.heap.lent = Some(table),
    });
}

/// Starts a range on a worker: it can read the blocks lent for the call.
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_starts(lent: Option<Arc<Table>>) {
    super::with(|state| state.heap.lent = lent);
}

/// Starts a server's I/O worker: it can read what's lent, and its tasks' blocks end with them, so
/// nothing's kept to hand over, however long it serves.
#[cfg(any(test, feature = "threads"))]
pub(super) fn serving_starts(lent: Option<Arc<Table>>) {
    super::with(|state| {
        state.heap.lent = lent;
        state.heap.serving = true;
    });
}

/// A server's state on a worker, set aside while the worker runs a job from the pool.
#[cfg(any(test, feature = "threads"))]
pub(super) struct Serving {
    lent: Option<Arc<Table>>,
}

/// Starts a job from the pool on a worker. While the worker serves, the job hands what it makes
/// over as any range's does, and lends and takes back blocks of its own, so the server's are set
/// aside until it ends.
#[cfg(any(test, feature = "threads"))]
pub(super) fn job_starts() -> Option<Serving> {
    super::with(|state| {
        let serving = std::mem::replace(&mut state.heap.serving, false);
        serving.then(|| Serving { lent: state.heap.lent.take() })
    })
}

/// Ends a job from the pool on a worker, giving a server's state back.
#[cfg(any(test, feature = "threads"))]
pub(super) fn job_ends(serving: Option<Serving>) {
    if let Some(Serving { lent }) = serving {
        super::with(|state| {
            state.heap.serving = true;
            state.heap.lent = lent;
        });
    }
}

/// What a range running as a parked task keeps of its own in its thread's runtime: swapped in each
/// time it's resumed, and out each time it parks or ends. Tasks taking turns on one thread then
/// never mix what they made, the blocks lent to them, how they changed those blocks' owners, or
/// what they printed. Without it, one range ending could hand over blocks another range on the
/// same thread was still using.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a fact of its own about the task, swapped in and out with a thread-local of its own"
)]
pub(crate) struct Accounts {
    made: Vec<u64>,
    /// Whether it let its list of what it made go, and how many ranges or chunks have it marked.
    made_listing: super::heap::Listing,
    lent: Option<Arc<Table>>,
    lent_owners: HashMap<u64, i64, std::hash::BuildHasherDefault<super::heap::AddressHasher>>,
    printed: Vec<u8>,
    /// Whether the heap serves, which a range never does: a range parked on a worker that serves
    /// hands what it makes over, as one that isn't parked does.
    serving: bool,
    /// For a launched task, how far it's been told to stop: each wait it's in, or comes to, is cut
    /// short.
    stop: Option<Arc<AtomicU8>>,
    /// How many crashes' cleanups it's in.
    cleaning: usize,
    /// Which task it is, in a recording or a replay, for naming its entries.
    replay: Option<crate::replay::Task>,
    /// Whether what it prints is written as it goes.
    writes: bool,
    /// What it waits in as the code that launched tasks, once it has.
    launcher: Option<Arc<Launcher>>,
    /// The mark in the blocks it makes, so that when it ends it takes only its own.
    task: u64,
    /// The capture of the test that launched it, if a test did, for what it prints.
    capture: Option<Arc<crate::fakes::Capture>>,
    /// Whose it is.
    of: Of,
    /// Whether it's a range of a launched task's `parallel` call.
    range_of_task: bool,
    /// For a range of a call on the pool, what its caller waits in as the code that launched tasks.
    caller_s_launcher: Option<Arc<Launcher>>,
    /// For a range of a waiting call, how a deadlock's report names it (see `RANGE_NAME`).
    range_name: Option<super::deadlock::RangeName>,
    /// Whether it's a range's, run as its own task (see `OWN_RANGE`).
    own_range: bool,
    /// The generators it let go of partway that wait to be closed: its own, so that closing one,
    /// which can fail, is never done in another task's free, and its failure never said there.
    abandoning: Vec<u64>,
}

/// Whose accounts they are: a launched task's, or those of a range of a `parallel` call whose body
/// may wait.
#[cfg(any(test, feature = "threads"))]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Of {
    #[default]
    Task,
    Range,
}

#[cfg(any(test, feature = "threads"))]
impl Accounts {
    /// A range's, of a `parallel` call whose body may wait: it has nothing until its call gives it
    /// what to read.
    pub(in crate::embed) fn for_range() -> Accounts {
        Accounts { of: Of::Range, own_range: true, ..Accounts::default() }
    }

    /// Whether it's a range's, rather than a launched task's.
    pub(in crate::embed) fn is_range_s(&self) -> bool {
        self.of == Of::Range
    }

    /// A launched task's, reading what's `lent` to the code that launched it, if anything, and
    /// stopping when `stop` is set.
    pub(in crate::embed) fn for_task(lent: Option<Arc<Table>>, stop: Arc<AtomicU8>) -> Accounts {
        let writes = super::program_writes();
        let capture = super::capture_here();
        Accounts {
            lent,
            stop: Some(stop),
            replay: crate::replay::child(),
            writes,
            capture,
            ..Accounts::default()
        }
    }

    /// Its name in a recording or a replay, if it's a launched task's.
    pub(in crate::embed) fn replay_name(&self) -> Option<Arc<str>> {
        self.replay.as_ref().map(crate::replay::Task::name)
    }

    /// Marks what it makes from now on as the blocks of the task numbered `task` on this thread.
    pub(in crate::embed) fn set_task(&mut self, task: u64) {
        self.task = super::heap::task_mark(task);
    }

    /// How many blocks it has listed as made, to hand over when it ends.
    pub(in crate::embed) fn listed(&self) -> usize {
        self.made.len()
    }

    /// Whether its mark is the one the task numbered `task` on this thread would have.
    pub(in crate::embed) fn marked_as(&self, task: u64) -> bool {
        self.task == super::heap::task_mark(task)
    }

    /// Which task it is, in a recording or a replay, for something else to say from here on.
    pub(in crate::embed) fn take_replay(&mut self) -> Option<crate::replay::Task> {
        self.replay.take()
    }

    /// Whether it's a range of the call of the task told to stop with `stop`.
    pub(in crate::embed) fn is_range_stopped_with(&self, stop: &Arc<AtomicU8>) -> bool {
        self.range_of_task && self.stop.as_ref().is_some_and(|own| Arc::ptr_eq(own, stop))
    }

    /// Whether nothing's left in them: a range that ended has handed everything over.
    pub(crate) fn is_empty(&self) -> bool {
        self.made.is_empty()
            && self.lent.is_none()
            && self.lent_owners.is_empty()
            && self.printed.is_empty()
            && self.abandoning.is_empty()
    }
}

/// Swaps a parked range's accounts with this thread's runtime's: in before it's resumed, back out
/// after. What the runtime keeps of lent strings is let go when the lent blocks change, as at a
/// range's end, since the same addresses may hold other strings.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn swap_accounts(accounts: &mut Accounts) {
    super::with(|state| {
        state.heap.swap_made(&mut accounts.made, &mut accounts.made_listing, &mut accounts.task);
        let same = match (&state.heap.lent, &accounts.lent) {
            (Some(here), Some(there)) => Arc::ptr_eq(here, there),
            (None, None) => true,
            _ => false,
        };
        std::mem::swap(&mut state.heap.lent, &mut accounts.lent);
        std::mem::swap(&mut state.heap.lent_owners, &mut accounts.lent_owners);
        std::mem::swap(&mut state.heap.serving, &mut accounts.serving);
        state.heap.swap_abandoning(&mut accounts.abandoning);
        std::mem::swap(&mut *state.output.0.borrow_mut(), &mut accounts.printed);
        STOP.with_borrow_mut(|stop| std::mem::swap(stop, &mut accounts.stop));
        LAUNCHER.with_borrow_mut(|launcher| std::mem::swap(launcher, &mut accounts.launcher));
        accounts.writes = super::swap_task_writes(accounts.writes);
        super::swap_capture(&mut accounts.capture);
        accounts.cleaning = CLEANING.replace(accounts.cleaning);
        accounts.range_of_task = RANGE_OF_TASK.replace(accounts.range_of_task);
        CALLER_S_LAUNCHER
            .with_borrow_mut(|launcher| std::mem::swap(launcher, &mut accounts.caller_s_launcher));
        RANGE_NAME.with_borrow_mut(|name| std::mem::swap(name, &mut accounts.range_name));
        accounts.own_range = OWN_RANGE.replace(accounts.own_range);
        crate::replay::swap(&mut accounts.replay);
        if !same {
            state.heap.texts.get_mut().clear();
            state.heap.set_cursor(super::heap::Cursor::default());
        }
    });
}

/// Ends a server on a worker that served it: as a range's end, and what its tasks make is handed
/// over again from here on.
#[cfg(any(test, feature = "threads"))]
pub(super) fn serving_ends() -> (Vec<(u64, i64)>, Vec<u8>) {
    super::with(|state| state.heap.serving = false);
    range_ends()
}

/// Ends a range on a worker: it lets go of the blocks lent for the call, and of the text it
/// kept of them, as the same addresses may hold other strings by its next range. Writes what it
/// printed, so it all comes before what the calling thread prints next. Gives back how it changed
/// their owners, and what it printed if this worker's runtime doesn't write it.
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_ends() -> (Vec<(u64, i64)>, Vec<u8>) {
    super::write_kept_output();
    super::with(|state| {
        if state.heap.lent.take().is_some() {
            state.heap.texts.get_mut().clear();
            state.heap.set_cursor(super::heap::Cursor::default());
        }
        (state.heap.lent_owners.drain().collect(), state.output.0.take())
    })
}

/// Applies the changes the ranges of a call made to this thread's blocks' owners, freeing those
/// left with none. A block that's never freed has none: a worker leaves its count alone
/// (`heap::uncounted`), and so doesn't note a change to one.
#[cfg(any(test, feature = "threads"))]
pub(super) fn settle(changes: HashMap<u64, i64>) {
    let gone: Vec<u64> = super::with(|state| {
        let mut gone = Vec::new();
        for (address, by) in changes {
            if let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut()) {
                *count = count.saturating_add_signed(by);
                if *count == 0 {
                    gone.push(address);
                }
            }
        }
        gone
    });
    for address in gone {
        super::heap::free(i64::try_from(address).unwrap_or(0));
    }
}

/// `fennec_retain_shared(address, owner)`: one more count of a block, from a thread that
/// didn't make it. `owner` is the number in the block's shape word, which native code has
/// just read to see the block isn't its own.
pub extern "C" fn retain_shared(address: i64, owner: i64) {
    if owner.cast_unsigned() == super::guarded::HANDLE_OWNER {
        super::guarded::count_handle(address.cast_unsigned());
        return;
    }
    if super::with(|state| state.heap.change_lent_owners(address.cast_unsigned(), 1)) {
        return;
    }
    if counted_behind(address.cast_unsigned(), 1).is_some() {
        return;
    }
    ANY_SHARED.store(true, Ordering::Relaxed);
    let owner = u16::try_from(owner).unwrap_or(0);
    shared(|table| {
        let entry =
            table.entry(address.cast_unsigned()).or_insert_with(|| Shared { owner, ..Shared::default() });
        entry.count += 1;
    });
}

/// `fennec_release_shared(address, owner)`: one count fewer of a block, from a thread that
/// didn't make it. When that was the last count anyone held, the block goes on its owner's
/// queue.
pub extern "C" fn release_shared(address: i64, owner: i64) {
    let address = address.cast_unsigned();
    if owner.cast_unsigned() == super::guarded::HANDLE_OWNER {
        super::with(|state| super::guarded::release_handle(state, address));
        return;
    }
    if super::with(|state| state.heap.change_lent_owners(address, -1)) {
        return;
    }
    match counted_behind(address, -1) {
        Some(true) => return super::heap::free(address.cast_signed()),
        Some(false) => return,
        None => {}
    }
    let freed = shared(|table| {
        let entry = table.get_mut(&address)?;
        entry.count = entry.count.saturating_sub(1);
        if entry.count > 0 || !entry.owner_done {
            return None;
        }
        table.remove(&address).map(|entry| (entry.owner, entry.run))
    });
    if let Some(queue) = freed {
        remote(|queues| queues.entry(queue).or_default().push(address));
        ANY_REMOTE.store(true, Ordering::Release);
    }
}

/// A count of this thread's on one of its own blocks, in a table behind a write lock it holds: under
/// the lock, `fennec_thread` is the lock's owner, so native code took the block for another
/// thread's. It's this thread's to count, as it would be outside the lock, so its own count changes
/// by `by` (#2456). Counted in the shared table, a release found nothing there and the block was
/// left at the end, and a retain made an entry that kept it from ever being freed. Whether the
/// count reached 0, or `None`, having changed nothing, for any other block.
fn counted_behind(address: u64, by: i64) -> Option<bool> {
    super::with(|state| {
        state.heap.behind_holding(usize::try_from(address).ok()?)?;
        let count = state.heap.block_mut(address)?.first_mut()?;
        *count = count.saturating_add_signed(by);
        Some(*count == 0)
    })
}

/// Whether a block whose owner has just let go of its last count is still held by another
/// thread, in which case it waits for that thread to let go, rather than being freed now.
#[inline]
/// Whether any block may be counted by threads that didn't make it, so freeing one has to ask
/// `held_elsewhere` first.
pub(super) fn counted_elsewhere() -> bool {
    ON && ANY_SHARED.load(Ordering::Relaxed)
}

pub(super) fn held_elsewhere(address: u64) -> bool {
    ON && ANY_SHARED.load(Ordering::Relaxed) && held_elsewhere_now(address)
}

/// `held_elsewhere`, once some block has been counted across threads.
#[cold]
#[inline(never)]
fn held_elsewhere_now(address: u64) -> bool {
    shared(|table| match table.get_mut(&address) {
        Some(entry) if entry.count > 0 => {
            entry.owner_done = true;
            entry.run = RUN.get();
            true
        }
        Some(_) => {
            table.remove(&address);
            false
        }
        None => false,
    })
}

/// Whether any thread has blocks waiting on its queue: one load, for every `free` to ask.
#[inline]
pub(super) fn any_waiting() -> bool {
    ON && ANY_REMOTE.load(Ordering::Acquire)
}

/// Forgets what's kept for the blocks of a run ending with this heap: their counts by other
/// threads, and its queue. The blocks go with the heap, and their memory can come back as another
/// runtime's, which a count or a queue left behind would then reach.
pub(super) fn forget_ending(heap: &super::heap::Heap) {
    if !ON {
        return;
    }
    if ANY_SHARED.load(Ordering::Relaxed) {
        shared(|table| {
            table.retain(|address, _| !usize::try_from(*address).is_ok_and(|at| heap.holds_block(at)));
        });
    }
    if ANY_REMOTE.load(Ordering::Acquire) {
        remote(|queues| queues.remove(&own_queue()));
    }
}

/// Whether anything waits on the queue of the run this thread's runtime is or last was, for tests.
/// Only that run's blocks ever go there, so no other test's can, whatever addresses come back.
#[cfg(test)]
pub(super) fn waits_here() -> bool {
    remote(|queues| queues.get(&own_queue()).is_some_and(|queue| !queue.is_empty()))
}

/// Puts `address` on the queue this thread's runtime frees from, as a block let go of elsewhere
/// would be, for tests.
#[cfg(test)]
pub(super) fn wait_here(address: u64) {
    remote(|queues| queues.entry(own_queue()).or_default().push(address));
    ANY_REMOTE.store(true, Ordering::Release);
}

/// The blocks waiting on this thread's queue, for it to free.
#[cold]
#[inline(never)]
pub(super) fn waiting() -> Vec<u64> {
    remote(|queues| queues.remove(&own_queue()).unwrap_or_default())
}

/// How many blocks this thread's runtime has listed as made since it last handed them over.
pub fn made_so_far() -> usize {
    super::with(|state| state.heap.made_so_far())
}

/// Where this thread's list of what it's made is, as a range or chunk starts here: it hands over
/// only what's listed after, with `hand_over`, and says `made_scope_ends` when it's done, handing
/// over or not. Work a worker steals while a range waits starts and ends inside it, and hands its
/// own over first.
pub fn made_scope_starts() -> usize {
    super::with(|state| state.heap.made_scope_starts())
}

/// A range or chunk that marked this thread's list has ended. With `kept`, it ran on the thread
/// that called it, and nothing it made is handed over.
pub fn made_scope_ends(kept: bool) {
    super::with(|state| state.heap.made_scope_ends(kept));
}

/// What this thread's runtime made since `since` that outlives the task it ran, handed over
/// as blocks for the thread that started the task to adopt when the task ends.
pub fn hand_over(since: usize) -> Vec<Box<[u64]>> {
    super::with(|state| state.heap.hand_over(since))
}

/// Everything the task running here made and still holds, as `hand_over` does, with what it no
/// longer lists found by its mark: for a range run as a task of its own, as it ends.
#[cfg(any(test, feature = "threads"))]
pub fn hand_over_all() -> Vec<Box<[u64]>> {
    super::with(|state| state.heap.hand_over_all())
}

/// Whether the range starting here runs as a task of its own, with its own accounts and mark,
/// taken so that a range a call in it runs on this thread isn't: `put_own_range` gives it back
/// as the range ends. A range run here for want of a stack has its own accounts but no mark, so
/// it keeps the list the way to find what it made.
#[cfg(any(test, feature = "threads"))]
pub(super) fn take_own_range() -> (bool, bool) {
    let flag = OWN_RANGE.replace(false);
    (flag, flag && super::with(|state| state.heap.has_task_mark()))
}

#[cfg(any(test, feature = "threads"))]
pub(super) fn put_own_range(flag: bool) {
    OWN_RANGE.set(flag);
}

/// Without threads no range runs as a task.
#[cfg(not(any(test, feature = "threads")))]
pub(super) fn take_own_range() -> (bool, bool) {
    (false, false)
}

#[cfg(not(any(test, feature = "threads")))]
pub(super) fn put_own_range(_flag: bool) {}

#[cfg(not(any(test, feature = "threads")))]
pub fn hand_over_all() -> Vec<Box<[u64]>> {
    hand_over(0)
}

/// Takes over blocks a task's thread handed over, as this thread's own.
pub fn adopt(blocks: Vec<Box<[u64]>>) {
    super::with(|state| state.heap.adopt_blocks(blocks));
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, with};
    use crate::packed::Element;
    use crate::value::Value;

    fn begin() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
    }

    fn live() -> usize {
        with(|state| state.heap.live())
    }

    fn text_of(address: i64) -> Value {
        with(|state| Some(heap::read_block(state, address.cast_unsigned()))).unwrap()
    }

    fn shape_word(address: i64) -> u64 {
        with(|state| state.heap.block(address.cast_unsigned())?.get(1).copied()).unwrap_or(0)
    }

    #[test]
    fn a_task_s_blocks_are_its_own_until_the_thread_that_started_it_adopts_them() {
        begin();
        let before = live();
        let (owner, made) = std::thread::spawn(|| {
            let owner = super::take_index();
            begin();
            let names: Vec<i64> = ["a fox's name, longer than a word", "a fennec's"]
                .iter()
                .map(|name| with(|state| heap::text_address(state, name)))
                .collect();
            // Its own runtime reads its own blocks, whatever their owner bits say.
            assert_eq!(text_of(names[0]), Value::str("a fox's name, longer than a word"));
            let marked = names.iter().all(|name| shape_word(*name) >> heap::OWNER_SHIFT == u64::from(owner));
            assert!(marked);
            let handed = super::hand_over(0);
            finish();
            (owner, (names, handed))
        })
        .join()
        .unwrap();
        assert_ne!(owner, 0);
        let (names, handed) = made;
        super::adopt(handed);
        // Adopted, they're this thread's ordinary blocks.
        assert_eq!(shape_word(names[1]) >> heap::OWNER_SHIFT, 0);
        assert_eq!(text_of(names[1]), Value::str("a fennec's"));
        for name in names {
            heap::free(name);
        }
        assert_eq!(live(), before);
        finish();
    }

    /// What a thread kept of a string's text goes with the string when the string's handed over: the
    /// same address can come back holding another.
    #[test]
    fn handing_a_string_over_forgets_its_text() {
        let kept = std::thread::spawn(|| {
            let _ = super::take_index();
            begin();
            let name = with(|state| heap::text_address(state, "a string long enough for a block"));
            assert_eq!(text_of(name), Value::str("a string long enough for a block"));
            let at = usize::try_from(name).unwrap();
            assert!(with(|state| state.heap.texts.borrow().contains_key(&at)));
            let handed = super::hand_over(0);
            let kept = with(|state| state.heap.texts.borrow().contains_key(&at));
            drop(handed);
            finish();
            kept
        })
        .join()
        .unwrap();
        assert!(!kept);
    }

    #[test]
    fn a_block_another_thread_still_holds_waits_and_its_owner_frees_it_after() {
        let owner = super::take_index();
        begin();
        let before = live();
        let kept = with(|state| heap::text_address(state, "kept by another thread for a while"));
        let other = with(|state| heap::text_address(state, "something else to free"));
        std::thread::spawn(move || super::retain_shared(kept, i64::from(owner))).join().unwrap();
        // The owner lets go of its count, but the other thread still holds the block.
        heap::free(kept);
        assert_eq!(text_of(kept), Value::str("kept by another thread for a while"));
        std::thread::spawn(move || super::release_shared(kept, i64::from(owner))).join().unwrap();
        // The next time the owner frees anything, it frees what waited on its queue too.
        heap::free(other);
        assert_eq!(live(), before);
        finish();
    }

    /// Runtimes on threads with the same number, as every test's own thread is, each free only
    /// what waits for them: a block another thread let go of last waits for the runtime that made
    /// it, and one on another thread with the same number freeing something doesn't take it.
    #[test]
    fn a_block_waits_for_its_own_runtime_and_not_another_on_a_thread_with_its_number() {
        std::thread::spawn(|| {
            begin();
            assert_eq!(super::index(), 0);
            let before = live();
            let kept = with(|state| heap::text_address(state, "kept by another thread for a while"));
            let other = with(|state| heap::text_address(state, "something else to free"));
            std::thread::spawn(move || super::retain_shared(kept, 0)).join().unwrap();
            heap::free(kept);
            std::thread::spawn(move || super::release_shared(kept, 0)).join().unwrap();
            // Another runtime, on another thread numbered 0 too, frees something of its own.
            std::thread::spawn(|| {
                begin();
                let its_own = with(|state| heap::text_address(state, "another runtime's"));
                heap::free(its_own);
                finish();
            })
            .join()
            .unwrap();
            heap::free(other);
            assert_eq!(live(), before, "what waited for this runtime was freed by it");
            finish();
        })
        .join()
        .unwrap();
    }

    /// Thread numbers run out rather than come round again to 0 or to the numbers that mark
    /// blocks: the last one taken is the last before those.
    #[test]
    fn thread_numbers_stop_before_the_ones_that_mark_blocks() {
        use std::sync::atomic::AtomicU16;
        let next = AtomicU16::new(super::LAST_INDEX - 1);
        assert_eq!(super::next_index(&next), Some(super::LAST_INDEX - 1));
        assert_eq!(super::next_index(&next), Some(super::LAST_INDEX));
        assert_eq!(super::next_index(&next), None);
        assert_eq!(super::next_index(&next), None);
        let reserved = [
            super::super::guarded::LENT_OWNER,
            heap::NEVER_FREED_OWNER,
            super::super::guarded::HANDLE_OWNER,
            super::super::guarded::GUARDED_OWNER,
        ];
        assert!(reserved.iter().all(|owner| *owner > u64::from(super::LAST_INDEX)));
    }

    /// A run that ends while another thread still counts one of its blocks leaves nothing behind
    /// for that block: its memory goes with the run, and can come back as another runtime's. Asked
    /// of the run's own queue, as the block's address can be another test's by then.
    #[test]
    fn a_run_that_ends_leaves_nothing_waiting_for_its_blocks() {
        std::thread::spawn(|| {
            begin();
            let kept = with(|state| heap::text_address(state, "counted elsewhere when the run ends"));
            std::thread::spawn(move || super::retain_shared(kept, 0)).join().unwrap();
            heap::free(kept);
            finish();
            std::thread::spawn(move || super::release_shared(kept, 0)).join().unwrap();
            assert!(!super::waits_here(), "nothing waits for a run that's ended");
        })
        .join()
        .unwrap();
    }
}
