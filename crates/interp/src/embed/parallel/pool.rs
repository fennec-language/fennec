//! The work-stealing pool `parallel` runs on, with the `threads` feature: `rayon-core`'s, whose
//! deques are the well-tested Chase-Lev kind. Each worker takes a thread number and starts a
//! runtime of its own before its first range, through the setup the program gives
//! (`set_worker_setup`). Each range hands what it made over to its call, which the calling
//! thread adopts when the call ends.

use std::cell::Cell;
use std::sync::OnceLock;

/// Below this many words of items, a loop isn't worth threads, and its ranges run on the
/// calling thread.
const SEQUENTIAL_WORDS: usize = 4096;

/// What a worker does before its first range: start a runtime of its own, holding the
/// program. An executable gives it when it starts; with none, every loop runs on the calling
/// thread.
static SETUP: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

static POOL: OnceLock<Option<rayon_core::ThreadPool>> = OnceLock::new();

thread_local! {
    /// Whether this thread is one of the pool's workers.
    static WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Gives what each worker runs before its first range. The first one given is kept.
pub fn set_worker_setup(setup: Box<dyn Fn() + Send + Sync>) {
    let _ = SETUP.set(setup);
}

fn builder() -> rayon_core::ThreadPoolBuilder {
    rayon_core::ThreadPoolBuilder::new().thread_name(|at| format!("fennec-worker-{at}")).start_handler(|_| {
        super::super::threads::take_index();
        WORKER.set(true);
        if let Some(setup) = SETUP.get() {
            setup();
        }
    })
}

fn pool() -> Option<&'static rayon_core::ThreadPool> {
    POOL.get_or_init(|| builder().build().ok()).as_ref()
}

/// Runs `run` on a pool of its own with `threads` workers, set up as the shared pool's are.
#[cfg(test)]
pub(super) fn on_threads<R: Send>(threads: usize, run: impl FnOnce() -> R + Send) -> R {
    builder().num_threads(threads).build().unwrap().install(run)
}

/// Runs a call's ranges: on the pool when there are enough words of items to be worth it and
/// workers can have runtimes, and on the calling thread otherwise.
pub(super) fn install<R: Send>(words: usize, run: impl FnOnce() -> R + Send) -> R {
    let pool = (words >= SEQUENTIAL_WORDS && SETUP.get().is_some()).then(pool).flatten();
    let Some(pool) = pool else { return run() };
    pool.install(run)
}

/// Runs both, in parallel on the pool, or one after the other off it.
pub(super) fn join<A: Send, B: Send>(
    first: impl FnOnce() -> A + Send,
    second: impl FnOnce() -> B + Send,
) -> (A, B) {
    if WORKER.get() { rayon_core::join(first, second) } else { (first(), second()) }
}
