//! `with launch(...)`: a task that runs alongside the code that launched it, until that code's block
//! ends. A task is a parked task on one of the pool's workers, so each wait it's in parks it rather
//! than hold the thread, and a stop can cut that wait short.
//!
//! - **Starting:** the code that launches keeps running, so a task can't read its blocks. What the
//!   task's given is copied for it then, as a value crossing a lock's edge is, into a table of its
//!   own that it takes as its own. Only `shared` handles, and the string literals every thread can
//!   read, are kept as they are. A task launched from a range of a `parallel` call can read what's
//!   lent to that range too, as the range ends after the task does.
//! - **Running:** a task launched from another task on a worker runs on that worker, beside it.
//!   One launched from anywhere else runs on the workers in turn.
//! - **Ending:** what the task made that's still alive when it ends, its failure's value among it,
//!   goes to the code that launched it, which takes it as its own, as a `parallel` range's is.
//! - **Stopping:** a stop cuts short every wait the task is in, or comes to. The task then fails as a
//!   crash does, so it ends with its locks put back and let go. Being stopped, its end is quiet.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_parked::TaskId;
use fennec_syntax::{Diagnostic, Span};
use mio::Token;

use super::super::parallel::Pool;
use super::super::{State, attempt, guarded, heap, threads, with};
use super::{Io, Parked};

/// A launched task's function: given its own copy of what it was given, and its thread's number for
/// native code to hold. 0 when it ended normally, 1 when it crashed, with the runtime's failure set,
/// and 2 when it failed, having handed its failure's value over with `fennec_task_failed`.
pub type TaskBody = extern "C" fn(env: i64, thread: i64) -> i64;

/// How a task ended.
const ENDED: i64 = 0;
const CRASHED: i64 = 1;
const FAILED: i64 = 2;

/// What a task's launcher and the worker it runs on share.
struct Launched {
    /// How far it's been told to stop: `threads::STOPPING`, then `threads::STOPPED_HARD`.
    stop: Arc<AtomicU8>,
    pool: &'static Pool,
    worker: usize,
    /// Its number on its worker, once it's started there.
    task: Mutex<Option<TaskId>>,
    end: Mutex<End>,
    ended: Condvar,
}

enum End {
    /// Still running, with a launcher that's parked waiting for it, if one is: its worker and token.
    Running(Option<(usize, Token)>),
    Ended(Outcome),
    /// Its launcher has taken what it left.
    Taken,
}

/// What a task left when it ended.
struct Outcome {
    how: i64,
    failure: u64,
    blocks: Vec<Box<[u64]>>,
    /// How it changed the owners of what was lent to it, to go on its launcher's.
    lent_owners: Vec<(u64, i64)>,
    /// What it printed that its worker didn't write, for its launcher to.
    printed: Vec<u8>,
}

impl Outcome {
    /// What a launcher that couldn't wait for its task gets.
    fn left_behind() -> Outcome {
        Outcome { how: ENDED, failure: 0, blocks: Vec::new(), lent_owners: Vec::new(), printed: Vec::new() }
    }
}

/// Launched tasks still to end, by the number their launcher holds.
static TASKS: Mutex<Option<HashMap<i64, Arc<Launched>>>> = Mutex::new(None);
static NEXT_TASK: AtomicI64 = AtomicI64::new(1);
/// The worker the next task launched from off the pool runs on.
static NEXT_WORKER: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The failure's value a task's function is about to fail with.
    static FAILED_WITH: Cell<Option<u64>> = const { Cell::new(None) };
}

fn tasks<T>(use_them: impl FnOnce(&mut HashMap<i64, Arc<Launched>>) -> T) -> T {
    use_them(TASKS.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new))
}

/// `fennec_launch(body, env)`: starts a task running `body` with a copy of `env`, a block or 0, and
/// gives back the number `fennec_launch_end` takes. Fails only when there's no pool to run it on.
pub extern "C" fn launch(body: TaskBody, env: i64) -> i64 {
    let Some(pool) = super::super::parallel::for_server() else {
        super::super::fail_with(Diagnostic::new("`launch` has no thread to run a task on", Span::default()));
        return 0;
    };
    launch_on(pool, body, env)
}

/// `launch`, on `pool` unless this is a task on a worker, which launches beside itself.
pub(in crate::embed) fn launch_on(pool: &'static Pool, body: TaskBody, env: i64) -> i64 {
    let here = super::super::parallel::here();
    let beside = here.filter(|_| super::can_park());
    let pool = beside.map_or(pool, |(pool, _)| pool);
    let worker = beside.map_or_else(
        || {
            let threads = pool.threads().max(1);
            let next = NEXT_WORKER.fetch_add(1, Ordering::Relaxed) % threads;
            // A job on a worker blocks it while it waits, so its task runs on another.
            match here {
                Some((_, at)) if next == at && threads > 1 => (next + 1) % threads,
                _ => next,
            }
        },
        |(_, at)| at,
    );
    let given = attempt(|state: &mut State| {
        if threads::index() == 0 {
            heap::share_statics(state);
        }
        let lent = state.heap.lent.clone();
        let copy = match u64::try_from(env).unwrap_or(0) {
            0 => None,
            env => Some(guarded::copied_for_task(state, env)?),
        };
        Ok(Some((copy, lent)))
    });
    let Some((copy, lent)) = given else { return 0 };
    let launched = Arc::new(Launched {
        stop: Arc::new(AtomicU8::new(0)),
        pool,
        worker,
        task: Mutex::new(None),
        end: Mutex::new(End::Running(None)),
        ended: Condvar::new(),
    });
    let number = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    tasks(|tasks| tasks.insert(number, Arc::clone(&launched)));
    let accounts = threads::Accounts::for_task(lent, Arc::clone(&launched.stop));
    pool.run_on(
        worker,
        Box::new(move |io: &mut Io| {
            let running = Arc::clone(&launched);
            let told = Box::new(move |task| {
                *launched.task.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
            });
            io.start_task(Box::new(move || run(&running, body, copy)), accounts, told);
        }),
    );
    number
}

/// Runs a task, on its worker with its own accounts in the runtime, and hands over what it left.
fn run(launched: &Launched, body: TaskBody, copy: Option<(u64, Box<crate::embed::table::Table>)>) -> i64 {
    let env = match copy {
        Some((env, table)) => {
            with(|state| state.heap.adopt_made(table.into_blocks()));
            env
        }
        None => 0,
    };
    let thread = with(|state| state.heap.owner_number());
    FAILED_WITH.set(None);
    let how = body(i64::try_from(env).unwrap_or(0), thread);
    let failure = FAILED_WITH.take().unwrap_or(0);
    let how = match how {
        ENDED if super::super::failed() => CRASHED,
        FAILED if failure == 0 => CRASHED,
        ENDED | FAILED => how,
        _ => CRASHED,
    };
    let how = if how == CRASHED { crashed(launched) } else { how };
    if env != 0 {
        attempt(|state: &mut State| {
            // What it was given was made on the thread that launched it.
            let was = state.heap.freeing(heap::Freed::GivenBack);
            heap::release(state, env);
            state.heap.freeing(was);
            Ok(())
        });
    }
    let blocks = with(|state| state.heap.hand_over(0));
    let (lent_owners, printed) = threads::range_ends();
    let outcome = Outcome { how, failure, blocks, lent_owners, printed };
    let mut end = launched.end.lock().unwrap_or_else(PoisonError::into_inner);
    let waiting = match std::mem::replace(&mut *end, End::Ended(outcome)) {
        End::Running(waiting) => waiting,
        End::Ended(_) | End::Taken => None,
    };
    drop(end);
    launched.ended.notify_all();
    if let Some((at, token)) = waiting {
        launched.pool.run_on(at, Box::new(move |io: &mut Io| io.wake_token(token)));
    }
    0
}

/// How a task that failed as a crash does ends. Stopped, it ends quietly. Otherwise its failure is
/// reported, and it's a crash. A crash holding a lock leaves what it guards half changed, until
/// rollback is emitted, so the program ends.
fn crashed(launched: &Launched) -> i64 {
    if launched.stop.load(Ordering::Acquire) > 0 {
        with(|state| state.failure = None);
        super::super::set_failed(0);
        return ENDED;
    }
    if threads::holds_locks() {
        super::super::aot::stop_for_failure();
    }
    super::super::report_failure();
    CRASHED
}

/// The failure's value a task's function, a server's round, or a `parallel` call's range, handed over
/// before giving back 2.
pub(in crate::embed) fn take_failed_with() -> Option<u64> {
    FAILED_WITH.take()
}

/// `fennec_task_failed(failure)`: the task's function is about to give back 2, failing with this
/// value, which its launcher takes.
pub extern "C" fn task_failed(failure: i64) {
    FAILED_WITH.set(u64::try_from(failure).ok().filter(|failure| *failure != 0));
}

/// `fennec_launch_end(task, stop_after_ns, failed, failure)`: the block that launched `task` ends.
/// With `failed` 1 it failed, and the task's stopped at once. Otherwise the task has `stop_after_ns`
/// to end before it's stopped, or as long as it takes when that's negative. Gives back 0 when it
/// ended normally or was stopped, 1 when it crashed, and 2 when it failed, with `failure` its value,
/// now the caller's own.
pub extern "C" fn launch_end(task: i64, stop_after_ns: i64, failed: i64, failure: &mut i64) -> i64 {
    let Some(launched) = tasks(|tasks| tasks.remove(&task)) else { return 0 };
    // A failure this thread has, as when a stop ended the block, waits aside: the flag's the
    // thread's, and other tasks run on it while this one waits.
    let held = with(|state| state.failure.take());
    super::super::set_failed(0);
    if failed != 0 || threads::stopped() {
        stop(&launched, threads::STOPPING);
    }
    let deadline = u64::try_from(stop_after_ns)
        .ok()
        .filter(|_| failed == 0)
        .map(|nanos| Instant::now() + Duration::from_nanos(nanos));
    let outcome = wait_for(&launched, deadline);
    match held {
        Some(held) => super::super::fail_with(held),
        // Stopped while it waited: it's cut short now its task has ended.
        None => drop(super::cut_short()),
    }
    with(|state| {
        state.output.0.borrow_mut().extend(outcome.printed);
        state.heap.adopt_made(outcome.blocks);
        for (address, by) in outcome.lent_owners {
            if !state.heap.change_lent_owners(address, by) {
                // Lent to the task by its launcher, which only reads it: the owner's count it is.
                if let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut()) {
                    *count = count.saturating_add_signed(by);
                }
            }
        }
    });
    *failure = i64::try_from(outcome.failure).unwrap_or(0);
    outcome.how
}

/// Cuts short every wait the task's in, and each it comes to: at `threads::STOPPING`, unless it's
/// cleaning up after a crash, and at `threads::STOPPED_HARD`, whatever it's doing.
fn stop(launched: &Arc<Launched>, level: u8) {
    launched.stop.fetch_max(level, Ordering::AcqRel);
    // A read blocking the task's thread, as on Windows, hears of nothing run there: it's ended
    // from here, by shutting its socket.
    crate::stdlib::net::cut_blocked_reads(&launched.stop);
    let stopping = Arc::clone(launched);
    launched.pool.run_on(
        launched.worker,
        Box::new(move |io: &mut Io| {
            if let Some(task) = *stopping.task.lock().unwrap_or_else(PoisonError::into_inner) {
                io.cancel(task);
            }
        }),
    );
}

/// How long a stopped task's cleanup after a crash may still wait, before it's stopped hard: a
/// rollback over the network, say. A cleanup can't hold a program's end up for longer.
const CLEANUP_GRACE: Duration = Duration::from_secs(1);

/// Waits for the task to end, stopping it at `deadline`, and hard once its grace is up: parked, in a
/// task that can, and otherwise blocking. A launcher that's stopped itself stops its task at once.
fn wait_for(launched: &Arc<Launched>, mut deadline: Option<Instant>) -> Outcome {
    if launched.stop.load(Ordering::Acquire) > 0 {
        deadline = Some(Instant::now() + CLEANUP_GRACE);
    }
    loop {
        let mut end = launched.end.lock().unwrap_or_else(PoisonError::into_inner);
        if let End::Ended(_) = &*end {
            let End::Ended(outcome) = std::mem::replace(&mut *end, End::Taken) else { continue };
            return outcome;
        }
        let parked = if let (true, Some((_, at))) = (super::can_park(), super::super::parallel::here()) {
            let token = super::next_token();
            *end = End::Running(Some((at, token)));
            drop(end);
            Some(super::park_on_token(token, deadline, false))
        } else if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            let waited = launched.ended.wait_timeout(end, left).unwrap_or_else(PoisonError::into_inner);
            waited.1.timed_out().then_some(Parked::TimedOut)
        } else {
            drop(launched.ended.wait(end).unwrap_or_else(PoisonError::into_inner));
            None
        };
        // A cancelled launcher can't wait, and its task may need this very worker to run. So the task's
        // stopped hard and left to end by itself, with nothing of it coming back here.
        if fennec_parked::is_cancelled() {
            *launched.end.lock().unwrap_or_else(PoisonError::into_inner) = End::Running(None);
            stop(launched, threads::STOPPED_HARD);
            return Outcome::left_behind();
        }
        // Its time's up, or the launcher's been stopped itself: the task's stopped, and waited for,
        // then stopped hard once its grace is up.
        let late = deadline.is_some_and(|deadline| deadline <= Instant::now());
        if matches!(parked, Some(Parked::TimedOut | Parked::Cancelled)) || late {
            if launched.stop.load(Ordering::Acquire) == 0 {
                stop(launched, threads::STOPPING);
                deadline = Some(Instant::now() + CLEANUP_GRACE);
            } else {
                stop(launched, threads::STOPPED_HARD);
                deadline = None;
            }
        }
    }
}

#[cfg(test)]
mod tests;
