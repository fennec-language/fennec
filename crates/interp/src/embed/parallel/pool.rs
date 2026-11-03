//! The work-stealing pool `parallel` runs on, with the `threads` feature: `rayon-core`'s, whose
//! deques are the well-tested Chase-Lev kind. Each worker takes a thread number and starts a
//! runtime of its own before its first range, through the setup the program gives
//! (`set_worker_setup`). Each range hands what it made over to its call, which the calling
//! thread adopts when the call ends.

use std::cell::Cell;
use std::sync::{OnceLock, PoisonError};

/// Below this many words of items, a loop isn't worth threads, and its ranges run on the
/// calling thread.
const SEQUENTIAL_WORDS: usize = 4096;

/// Each worker's stack. Only what it touches is ever real memory.
const WORKER_STACK_BYTES: usize = 64 << 20;

/// How deep native code on a worker may go, below where the worker starts: the rest of its
/// stack is room for the runtime it calls.
const WORKER_NATIVE_BYTES: usize = 48 << 20;

/// What a worker does before its first range: start a runtime of its own, holding the
/// program. An executable gives it when it starts; with none, every loop runs on the calling
/// thread.
static SETUP: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

static POOL: OnceLock<Option<rayon_core::ThreadPool>> = OnceLock::new();

thread_local! {
    /// Whether this thread is one of the pool's workers.
    static WORKER: Cell<bool> = const { Cell::new(false) };
    /// The lowest address native code on this worker may grow its stack to, or 0 off the pool.
    static LIMIT: Cell<usize> = const { Cell::new(0) };
    /// Whether this worker is running a `parallel` call of its own, on itself alone.
    static NESTED: Cell<bool> = const { Cell::new(false) };
}

/// Gives what each worker runs before its first range. The first one given is kept.
pub fn set_worker_setup(setup: Box<dyn Fn() + Send + Sync>) {
    let _ = SETUP.set(setup);
}

fn builder() -> rayon_core::ThreadPoolBuilder {
    let builder = rayon_core::ThreadPoolBuilder::new().thread_name(|at| format!("fennec-worker-{at}"));
    builder.stack_size(WORKER_STACK_BYTES).start_handler(|_| {
        let here = 0_u8;
        LIMIT.set(std::ptr::from_ref(&here).addr().saturating_sub(WORKER_NATIVE_BYTES));
        super::super::threads::take_index();
        WORKER.set(true);
        if let Some(setup) = SETUP.get() {
            setup();
        }
    })
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
    LIMIT.get()
}

/// How many workers `FENNEC_THREADS` asks for, or 0 for as many as there are cores (or as
/// `RAYON_NUM_THREADS` says).
fn threads_wanted() -> usize {
    std::env::var("FENNEC_THREADS").ok().and_then(|threads| threads.trim().parse().ok()).unwrap_or(0)
}

fn pool() -> Option<&'static rayon_core::ThreadPool> {
    POOL.get_or_init(|| builder().num_threads(threads_wanted()).build().ok()).as_ref()
}

/// Whether `FENNEC_PARALLEL_STRESS` asks every loop to run on the pool, however few its items,
/// and loops and comprehensions to split down to one item a range, for testing what bodies do on
/// workers with programs small enough to test many of.
pub(super) fn stressed() -> bool {
    static STRESSED: OnceLock<bool> = OnceLock::new();
    *STRESSED.get_or_init(|| std::env::var_os("FENNEC_PARALLEL_STRESS").is_some())
}

/// Runs `run` on a pool of its own with `threads` workers, set up as the shared pool's are.
#[cfg(test)]
pub(super) fn on_threads<R: Send>(threads: usize, run: impl FnOnce() -> R + Send) -> R {
    builder().num_threads(threads).build().unwrap().install(run)
}

/// Runs a call's ranges: on the pool when there are enough words of items to be worth it and
/// workers can have runtimes, and on the calling thread otherwise. A worker's own call stays on
/// that worker, which can't lend its blocks while it keeps working.
pub(super) fn install<R: Send>(call: &super::Run, words: usize, run: impl FnOnce() -> R + Send) -> R {
    let pool = ((words >= SEQUENTIAL_WORDS || stressed()) && SETUP.get().is_some()).then(pool).flatten();
    let Some(pool) = pool else { return run() };
    if WORKER.get() {
        let outer = NESTED.replace(true);
        let result = run();
        NESTED.set(outer);
        return result;
    }
    // What this thread printed comes before anything the ranges print.
    super::super::write_kept_output();
    *call.lent.write().unwrap_or_else(PoisonError::into_inner) = Some(super::super::threads::lend());
    let result = pool.install(run);
    if let Some(lent) = call.lent.write().unwrap_or_else(PoisonError::into_inner).take() {
        super::super::threads::take_back(lent);
    }
    result
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
    pool.map_or(0, |pool| 2 * pool.current_num_threads())
}

/// Runs a task on the pool's workers, when they get to it.
pub(super) fn spawn(task: impl FnOnce() + Send + 'static) {
    match pool() {
        Some(pool) => pool.spawn(task),
        None => task(),
    }
}

/// Runs both, in parallel on the pool, or one after the other off it.
pub(super) fn join<A: Send, B: Send>(
    first: impl FnOnce() -> A + Send,
    second: impl FnOnce() -> B + Send,
) -> (A, B) {
    if WORKER.get() && !NESTED.get() { rayon_core::join(first, second) } else { (first(), second()) }
}
