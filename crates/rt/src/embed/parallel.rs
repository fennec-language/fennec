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
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// The thread that started the call, whose runtime adopts what the ranges make.
    caller: u16,
    /// Where the lowest range that failed so far starts. Ranges after it don't start, and
    /// ranges before it still run, since one of them failing is the failure passed up.
    stop: AtomicUsize,
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
    halves: Mutex<[Half; 2]>,
    up: Option<(Arc<Node>, usize)>,
}

/// Where one half of a split reduction is.
/// A range that's started on a thread: where what it makes starts, what was running before it, in a
/// recording or a replay, and where it starts.
struct Started {
    since: usize,
    was: Option<crate::replay::Task>,
    from: usize,
    /// The capture what was running before printed into, put back as the range ends.
    capture: Option<Arc<crate::fakes::Capture>>,
}

#[derive(Clone, Copy)]
enum Half {
    Running,
    /// Its partial, or None if it failed.
    Done(Option<i64>),
}

impl Run {
    fn new(chunk: usize) -> Run {
        Run {
            chunk,
            caller: super::threads::index(),
            task: crate::replay::child(),
            capture: super::capture_here(),
            stop: AtomicUsize::new(usize::MAX),
            failure: Mutex::new(None),
            beaten: Mutex::new(Vec::new()),
            lent: RwLock::new(None),
            made: Mutex::new(Vec::new()),
            owners: Mutex::new(HashMap::new()),
            printed: Mutex::new(Vec::new()),
            done: Latch::one(),
            reduced: Mutex::new(None),
            waiting: false,
        }
    }

    /// The same call, for a body that may wait or not. A loop's or a comprehension's ranges are
    /// then an item each; a reduction's leaves stay as its length gives them.
    fn waiting(mut self, waiting: bool) -> Run {
        self.waiting = waiting;
        self
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
    fn range_starts(&self, from: usize) -> Started {
        if super::threads::index() != self.caller {
            pool::range_starts(self.lent.read().unwrap_or_else(PoisonError::into_inner).clone());
        }
        let mut was = crate::replay::range(self.task.as_ref(), from);
        crate::replay::swap(&mut was);
        let mut capture = self.capture.clone();
        super::swap_capture(&mut capture);
        Started { since: super::threads::made_so_far(), was, from, capture }
    }

    /// Keeps what the range just run on this thread made since `since`, and how it changed the
    /// owners of the calling thread's blocks, when this thread is a worker.
    fn keep_made(&self, Started { since, mut was, from, mut capture }: Started) {
        crate::replay::swap(&mut was);
        super::swap_capture(&mut capture);
        if super::threads::index() != self.caller {
            // Most ranges make nothing that outlives them, and every worker shares these locks.
            let made = super::threads::hand_over(since);
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

    /// Passes the failure up, in the calling thread's runtime, after adopting what the ranges
    /// made: 1 if there was one, else 0.
    fn finish(self) -> i64 {
        super::threads::adopt(self.made.into_inner().unwrap_or_else(PoisonError::into_inner));
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
        if from > self.stop.load(Ordering::Relaxed) || from >= to {
            return;
        }
        if to - from <= self.chunk {
            let since = self.range_starts(from);
            let code = leaf(from, to, i64::from(super::threads::index()));
            self.keep_made(since);
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
        while to - from > self.chunk && from <= self.stop.load(Ordering::Relaxed) {
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
                run.leaf_on((from, to), leaf);
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
        if from <= self.stop.load(Ordering::Relaxed) && from < to {
            let since = self.range_starts(from);
            let code = leaf.run(from, to, i64::from(super::threads::index()));
            self.keep_made(since);
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
        while to - from > self.chunk && from <= self.stop.load(Ordering::Relaxed) {
            let middle = from + (to - from) / 2;
            let node = Arc::new(Node { from, halves: Mutex::new([Half::Running; 2]), up: up.take() });
            let (run, right) = (Arc::clone(self), Some((Arc::clone(&node), 1)));
            pool.push(Box::new(move || run.reduce_on(pool, (middle, to), right, steps)));
            up = Some((node, 0));
            to = middle;
        }
        if self.waiting {
            let run = Arc::clone(self);
            pool::as_task(move || {
                let partial = run.reduce((from, to), steps);
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
            partial = self.combined(node.from, left, right, steps);
            up = node.up.as_ref().map(|(above, side)| (Arc::clone(above), *side));
        }
        *self.reduced.lock().unwrap_or_else(PoisonError::into_inner) = partial;
        self.done.finished();
    }

    /// Two halves' partials combined, the earlier first: None if either failed, or this does.
    fn combined(
        &self,
        from: usize,
        left: Option<i64>,
        right: Option<i64>,
        steps: (ReduceBody, Combine, i64),
    ) -> Option<i64> {
        let (_, combine, env) = steps;
        let mut combined = 0;
        let since = self.range_starts(from);
        let code = combine(env, left?, right?, &mut combined);
        self.keep_made(since);
        if code != 0 {
            self.failed(from, code);
            return None;
        }
        Some(combined)
    }

    /// A reduction over `from..to`: each range's partial, combined with the next in order.
    fn reduce(&self, (from, to): (usize, usize), steps: (ReduceBody, Combine, i64)) -> Option<i64> {
        let (body, combine, env) = steps;
        if from > self.stop.load(Ordering::Relaxed) {
            return None;
        }
        let thread = i64::from(super::threads::index());
        if to - from <= self.chunk {
            let mut partial = 0;
            let since = self.range_starts(from);
            let code = body(env, number(from), number(to), thread, &mut partial);
            self.keep_made(since);
            if code != 0 {
                self.failed(from, code);
                return None;
            }
            return Some(partial);
        }
        let middle = from + (to - from) / 2;
        let left = self.reduce((from, middle), steps);
        let right = self.reduce((middle, to), steps);
        self.combined(from, left, right, (body, combine, env))
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
    run_for(list, body, env, false)
}

fn run_for(list: i64, body: Body, env: i64, waiting: bool) -> i64 {
    counted();
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    let run = Run::new(if waiting { 1 } else { chunk(words) }).waiting(waiting);
    run_each(run, length, words, Leaf::For(body, env))
}

/// Runs a loop's or a comprehension's ranges, on the pool when it's worth it, then passes the
/// failure up: 1 if there was one, else 0. A body that may wait goes to the pool however few its
/// items, as what it waits for is what takes the time.
fn run_each(run: Run, length: usize, words: usize, leaf: Leaf) -> i64 {
    let pool = if run.waiting { pool::for_waiting() } else { pool::for_call(length * words) };
    let Some(pool) = pool else {
        run.each((0, length), &|from, to, thread| leaf.run(from, to, thread));
        return run.finish();
    };
    each_on_pool(run, pool, length, leaf).finish()
}

/// A loop's or a comprehension's ranges run on `pool`, with the call's ranges to finish.
fn each_on_pool(run: Run, pool: &'static Pool, length: usize, leaf: Leaf) -> Run {
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
    run: Run,
    pool: &'static Pool,
    length: usize,
    steps: (ReduceBody, Combine, i64),
) -> (Option<i64>, Run) {
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
    run_map(list, body, env, element, false)
}

fn run_map(list: i64, body: MapBody, env: i64, element: i64, waiting: bool) -> i64 {
    counted();
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    let out = super::lists::new(element, number(length));
    let run = Run::new(if waiting { 1 } else { chunk(words) }).waiting(waiting);
    if run_each(run, length, words, Leaf::Map(body, env, out)) != 0 {
        super::heap::free(out);
        return 0;
    }
    out
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

// The `_waiting` entry points are for a body that may wait: a network call, a lock, a sleep, a
// blocking read. On the pool, each range runs as a parked task on its worker, with accounts of its
// own (see `threads::Accounts`), so a wait can park it and let the worker run other ranges.
// A loop's or a comprehension's ranges are an item each, since waiting is what takes the time.
// A reduction keeps the leaves its length gives, so it combines as it does anywhere else.

/// `fennec_parallel_for_waiting`: `parallel_for`, for a body that may wait.
pub extern "C" fn parallel_for_waiting(list: i64, body: Body, env: i64) -> i64 {
    run_for(list, body, env, true)
}

/// `fennec_parallel_map_waiting`: `parallel_map`, for a body that may wait.
pub extern "C" fn parallel_map_waiting(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    run_map(list, body, env, element, true)
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

    /// Without threads, native code only runs on the program's thread, whose limit it has.
    pub fn stack_limit() -> usize {
        0
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

    /// Without threads, a range runs where it's made.
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
pub(crate) use pool::workers_wanted;
#[cfg(not(any(test, feature = "threads")))]
use pool::{Latch, Pool};
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) use pool::{Latch, Pool, for_server, here};
#[cfg(any(test, feature = "threads"))]
pub use pool::{has_worker_setup, run_worker_setup};
pub use pool::{on_task_resume, set_worker_setup, stack_limit};

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    use fennec_syntax::{Diagnostic, Span};

    use super::super::{State, Tables, attempt, finish, heap, lists, start, strings, threads, with};
    use crate::packed::Element;
    use crate::value::Value;
    use std::sync::atomic::Ordering;

    fn tables() -> Tables {
        Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![Rc::new(Element::Int)],
            ..Tables::default()
        }
    }

    fn begin() {
        super::set_worker_setup(Box::new(|| {
            start(tables(), crate::Inputs::default());
        }));
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
        let written = crate::embed::WRITTEN.lock().unwrap();
        assert!(String::from_utf8_lossy(&written).contains("printed just before panicking\n"));
    }
}
