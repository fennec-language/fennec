//! A launched task's `parallel` calls. Their ranges go to the task's pool, as they do from the
//! program's own thread, while the task parks. A task can't lend its worker's blocks, which the
//! other tasks there go on using, so its ranges read a copy of what the body captures.
//!
//! Each test has a pool of its own: its task counts the blocks its worker holds.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::*;
use crate::embed::parallel::{parallel_map_heavy, parallel_map_waiting, parallel_reduce_waiting};
use crate::embed::threads;

/// A pool of `workers` of its own, for one test.
fn own_pool(made: &'static OnceLock<&'static Pool>, workers: usize) -> &'static Pool {
    made.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            start(tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(workers, &setup)
    })
}

/// How long a range waits for another before it gives up, so a test fails rather than hangs.
const GIVES_UP: Duration = Duration::from_secs(20);

fn live() -> usize {
    with(|state| state.heap.live())
}

/// Whether a block is this thread's own, in its own table.
fn is_own(block: i64) -> bool {
    let here = with(|state| state.heap.owner_number()).cast_unsigned();
    block > 0
        && with(|state| state.heap.holds_block(usize::try_from(block).unwrap_or(0)))
        && owner_of(block) == here
}

static WAITING_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// The address of what the task was given, which its ranges mustn't be given.
static TASK_S_OWN: AtomicI64 = AtomicI64::new(0);
static SECOND_RAN: AtomicBool = AtomicBool::new(false);
/// 1 once the first range saw the second had run while it waited, and 2 if it gave up.
static FIRST_WAITED: AtomicI64 = AtomicI64::new(0);
/// How many ranges read a copy of what the body captures, and not the task's own.
static READ_A_COPY: AtomicI64 = AtomicI64::new(0);
/// How many ranges were noted as a task's ranges, stopped as it is.
static WERE_THE_TASK_S: AtomicI64 = AtomicI64::new(0);
/// What the task found once its call was over: 1 for each thing as it should be.
static CALL_LEFT: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];

/// A range of a comprehension whose body may wait. The first waits, parked, until the second has
/// run: run one after another, as they were before, it would wait for good.
extern "C" fn first_waits_for_second(env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    if env != TASK_S_OWN.load(Ordering::SeqCst) && lines(env) == ["captured", "by the body"] {
        READ_A_COPY.fetch_add(1, Ordering::SeqCst);
    }
    if threads::in_range_of_task() && threads::task_stop().is_some() {
        WERE_THE_TASK_S.fetch_add(1, Ordering::SeqCst);
    }
    if from != 0 {
        SECOND_RAN.store(true, Ordering::SeqCst);
        return 0;
    }
    let started = Instant::now();
    while !SECOND_RAN.load(Ordering::SeqCst) && started.elapsed() < GIVES_UP {
        clock_sleep(SECONDS / 1000);
    }
    FIRST_WAITED.store(if SECOND_RAN.load(Ordering::SeqCst) { 1 } else { 2 }, Ordering::SeqCst);
    0
}

/// A task that makes a comprehension of two items, whose body may wait and captures what the task
/// was given.
extern "C" fn maps_while_it_waits(env: i64, _thread: i64) -> i64 {
    TASK_S_OWN.store(env, Ordering::SeqCst);
    let before = live();
    let items = list(&["one", "two"]);
    let made = parallel_map_waiting(items, first_waits_for_second, env, LINES);
    // The list the ranges wrote into is the task's own block now, in its worker's table.
    CALL_LEFT[0].store(i64::from(is_own(made)), Ordering::SeqCst);
    // What it captured is still its own, as it was: nothing was written back.
    CALL_LEFT[1].store(i64::from(lines(env) == ["captured", "by the body"]), Ordering::SeqCst);
    heap::free(made);
    heap::free(items);
    // The copy lent to the ranges has gone, with nothing of it left.
    CALL_LEFT[2].store(i64::from(live() == before), Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_s_comprehension_runs_its_ranges_at_once_on_a_copy_of_what_they_capture() {
    begin();
    let given = list(&["captured", "by the body"]);
    let task = launch_on(own_pool(&WAITING_POOL, 2), maps_while_it_waits, given);
    heap::free(given);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(FIRST_WAITED.load(Ordering::SeqCst), 1, "the first range waited for the second in vain");
    assert_eq!(READ_A_COPY.load(Ordering::SeqCst), 2, "each range reads a copy of what's captured");
    assert_eq!(WERE_THE_TASK_S.load(Ordering::SeqCst), 2, "each range is noted as its task's");
    let left: Vec<i64> = CALL_LEFT.iter().map(|left| left.load(Ordering::SeqCst)).collect();
    assert_eq!(left, [1, 1, 1], "the list is the task's, what it captured is as it was, the copy has gone");
    finish();
}

static ONE_WORKER_POOL: OnceLock<&'static Pool> = OnceLock::new();
static ON_ONE_WORKER: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];

/// The same two ranges on a pool of one worker, with marks of their own to note in.
extern "C" fn first_waits_on_one_worker(_env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    if from != 0 {
        ON_ONE_WORKER[1].store(1, Ordering::SeqCst);
        return 0;
    }
    let started = Instant::now();
    while ON_ONE_WORKER[1].load(Ordering::SeqCst) == 0 && started.elapsed() < GIVES_UP {
        clock_sleep(SECONDS / 1000);
    }
    ON_ONE_WORKER[0].store(ON_ONE_WORKER[1].load(Ordering::SeqCst) + 1, Ordering::SeqCst);
    0
}

extern "C" fn maps_on_one_worker(_env: i64, _thread: i64) -> i64 {
    let items = list(&["one", "two"]);
    heap::free(parallel_map_waiting(items, first_waits_on_one_worker, 0, LINES));
    heap::free(items);
    ENDED
}

/// The task's worker is the pool's only one: it runs the ranges itself while the task is parked,
/// each a task that parks too, so the first can still wait for the second.
#[test]
fn a_task_s_ranges_run_on_its_own_worker_while_it_s_parked_when_that_s_the_only_one() {
    begin();
    let task = launch_on(own_pool(&ONE_WORKER_POOL, 1), maps_on_one_worker, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(ON_ONE_WORKER[0].load(Ordering::SeqCst), 2, "the first range waited for the second in vain");
    finish();
}

static HEAVY_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// The thread the first range other than the first ran on, plus one: the one that let the first
/// stop waiting. Only that one is kept. Kept for whichever ran last, under load it could be the
/// first's own thread, which ran the rest once it stopped waiting while the other worker was slow
/// to, and the ranges seemed to have run on one thread (2 runs in 150 beside 12 busy loops).
static ANOTHER_RAN_ON: AtomicI64 = AtomicI64::new(0);
/// The thread the first range ran on, plus one, once another had run elsewhere: -1 if none did.
static FIRST_RAN_ON: AtomicI64 = AtomicI64::new(0);
static HEAVY_LEFT: AtomicI64 = AtomicI64::new(0);

/// A range of a comprehension whose body is heavy, which holds its thread. The first waits for
/// any other to have run, which only another worker can do meanwhile.
extern "C" fn first_waits_for_another_thread(_env: i64, from: i64, _to: i64, thread: i64, _out: i64) -> i64 {
    if from != 0 {
        let _ = ANOTHER_RAN_ON.compare_exchange(0, thread + 1, Ordering::SeqCst, Ordering::SeqCst);
        return 0;
    }
    let started = Instant::now();
    while ANOTHER_RAN_ON.load(Ordering::SeqCst) == 0 && started.elapsed() < GIVES_UP {
        std::thread::sleep(Duration::from_millis(1));
    }
    let another = ANOTHER_RAN_ON.load(Ordering::SeqCst);
    FIRST_RAN_ON.store(if another == 0 { -1 } else { thread + 1 }, Ordering::SeqCst);
    0
}

extern "C" fn maps_a_heavy_body(_env: i64, _thread: i64) -> i64 {
    let before = live();
    let items = list(&["one", "two", "three", "four", "five", "six", "seven", "eight"]);
    let made = parallel_map_heavy(items, first_waits_for_another_thread, 0, LINES);
    let own = is_own(made);
    heap::free(made);
    heap::free(items);
    HEAVY_LEFT.store(i64::from(own && live() == before), Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_s_heavy_comprehension_shares_its_ranges_among_the_workers() {
    begin();
    let task = launch_on(own_pool(&HEAVY_POOL, 2), maps_a_heavy_body, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    let (first, another) = (FIRST_RAN_ON.load(Ordering::SeqCst), ANOTHER_RAN_ON.load(Ordering::SeqCst));
    assert!(first > 0, "no other range ran while the first held its thread");
    assert_ne!(first, another, "the ranges ran on one thread");
    assert_eq!(HEAVY_LEFT.load(Ordering::SeqCst), 1, "the list is the task's and nothing else is left");
    finish();
}

static CROWDED_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// As many launched tasks as a worker runs ranges at once, all alive on the one worker.
const CROWD: i64 = 1024;
static CROWD_STARTED: AtomicI64 = AtomicI64::new(0);
static CROWD_ENDED: AtomicI64 = AtomicI64::new(0);
static CROWDED: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
/// How many of the crowd had ended when the comprehension among them had.
static ENDED_BY_THEN: AtomicI64 = AtomicI64::new(-1);

/// One of the crowd: parked for as long as its test lets it be.
extern "C" fn sleeps_in_a_crowd(_env: i64, _thread: i64) -> i64 {
    CROWD_STARTED.fetch_add(1, Ordering::SeqCst);
    clock_sleep(60 * SECONDS);
    CROWD_ENDED.fetch_add(1, Ordering::SeqCst);
    ENDED
}

extern "C" fn first_waits_in_a_crowd(_env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    if from != 0 {
        CROWDED[1].store(1, Ordering::SeqCst);
        return 0;
    }
    let started = Instant::now();
    while CROWDED[1].load(Ordering::SeqCst) == 0 && started.elapsed() < GIVES_UP {
        clock_sleep(SECONDS / 1000);
    }
    CROWDED[0].store(CROWDED[1].load(Ordering::SeqCst) + 1, Ordering::SeqCst);
    0
}

extern "C" fn maps_in_a_crowd(_env: i64, _thread: i64) -> i64 {
    let items = list(&["one", "two"]);
    heap::free(parallel_map_waiting(items, first_waits_in_a_crowd, 0, LINES));
    heap::free(items);
    ENDED_BY_THEN.store(CROWD_ENDED.load(Ordering::SeqCst), Ordering::SeqCst);
    ENDED
}

/// A worker runs only so many ranges at once, each on a stack of its own. Launched tasks are on
/// such stacks too, and once counted against that: a worker holding that many long-lived tasks
/// never started a range, so a task there that handed its comprehension's ranges to it waited
/// until enough of the others had ended, which may be never. Only ranges count now.
#[test]
fn a_task_s_ranges_start_on_a_worker_crowded_with_long_lived_tasks() {
    begin();
    let pool = own_pool(&CROWDED_POOL, 1);
    let crowd: Vec<i64> = (0..CROWD).map(|_| launch_on(pool, sleeps_in_a_crowd, 0)).collect();
    let started = Instant::now();
    while CROWD_STARTED.load(Ordering::SeqCst) < CROWD && started.elapsed() < GIVES_UP {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(CROWD_STARTED.load(Ordering::SeqCst), CROWD, "the crowd didn't all start");
    let task = launch_on(pool, maps_in_a_crowd, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(CROWDED[0].load(Ordering::SeqCst), 2, "the first range waited for the second in vain");
    assert_eq!(ENDED_BY_THEN.load(Ordering::SeqCst), 0, "the ranges only started once others had ended");
    // Each of the crowd is stopped, as a block that failed stops its task.
    for sleeper in crowd {
        assert_eq!(launch_end(sleeper, -1, 1, &mut failure), ENDED);
    }
    finish();
}

static CLEANING_POOL: OnceLock<&'static Pool> = OnceLock::new();
static CLEANERS_SLEEPING: AtomicI64 = AtomicI64::new(0);
/// How many of a cleanup's ranges slept their sleep out, though their task had been stopped.
static CLEANERS_SLEPT: AtomicI64 = AtomicI64::new(0);

/// Whether the task whose code is running here has been told to stop.
fn told_to_stop() -> bool {
    threads::task_stop().is_some_and(|stop| stop.load(Ordering::Acquire) > 0)
}

/// A range of a comprehension a crash's cleanup makes: it waits, a millisecond at a time, as a
/// rollback over the network would, until its task has been told to stop, and then once more. A
/// wait the stop cut short leaves the range failed.
extern "C" fn waits_in_a_cleanup(_env: i64, _from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    CLEANERS_SLEEPING.fetch_add(1, Ordering::SeqCst);
    let started = Instant::now();
    while !told_to_stop() && started.elapsed() < GIVES_UP {
        clock_sleep(SECONDS / 1000);
    }
    clock_sleep(SECONDS / 1000);
    if super::super::super::super::failed() || !told_to_stop() {
        return CRASHED;
    }
    CLEANERS_SLEPT.fetch_add(1, Ordering::SeqCst);
    0
}

extern "C" fn cleans_up_with_a_comprehension(_env: i64, _thread: i64) -> i64 {
    threads::cleaning(true);
    let items = list(&["one", "two"]);
    let made = parallel_map_waiting(items, waits_in_a_cleanup, 0, LINES);
    heap::free(made);
    heap::free(items);
    threads::cleaning(false);
    ENDED
}

/// A stopped task's cleanup after a crash may still wait, until it's stopped hard a second later.
/// The ranges of a comprehension it makes are the cleanup's code, so they have that grace too:
/// they once ran in the task and had it, and on other threads would have lost it. Each range here
/// is waiting when the stop comes, and waits once more after it.
#[test]
fn a_cleanup_s_ranges_may_wait_as_the_cleanup_may_after_a_stop() {
    begin();
    let task = launch_on(own_pool(&CLEANING_POOL, 2), cleans_up_with_a_comprehension, 0);
    let started = Instant::now();
    while CLEANERS_SLEEPING.load(Ordering::SeqCst) < 2 && started.elapsed() < GIVES_UP {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(CLEANERS_SLEEPING.load(Ordering::SeqCst), 2, "the ranges didn't both start");
    let mut failure = 0;
    // The block failed, so its task is stopped at once, while its ranges sleep.
    assert_eq!(launch_end(task, 10 * SECONDS, 1, &mut failure), ENDED);
    assert_eq!(CLEANERS_SLEPT.load(Ordering::SeqCst), 2, "a cleanup's range was cut short by the stop");
    finish();
}

static STOPPED_SUM_POOL: OnceLock<&'static Pool> = OnceLock::new();
static SUMMING: AtomicI64 = AtomicI64::new(0);
/// What the stopped task's sum gave back, plus one.
static STOPPED_SUM_GAVE: AtomicI64 = AtomicI64::new(0);

/// A leaf of a sum whose body may wait: it sleeps for a minute unless its task is stopped.
extern "C" fn sums_until_stopped(_env: i64, from: i64, to: i64, _thread: i64, partial: &mut i64) -> i64 {
    SUMMING.fetch_add(1, Ordering::SeqCst);
    clock_sleep(60 * SECONDS);
    if super::super::super::super::failed() {
        return CRASHED;
    }
    *partial = (from..to).sum();
    0
}

extern "C" fn sums_until_it_s_stopped(_env: i64, _thread: i64) -> i64 {
    let before = live();
    let items = lists::new(LINES, SUMMED);
    let mut sum = 0;
    let how = parallel_reduce_waiting(items, sums_until_stopped, adds, 0, &mut sum);
    heap::free(items);
    // Its worker has every block back that the call lent, and nothing more.
    STOPPED_SUM_GAVE.store(if live() == before { how + 1 } else { -1 }, Ordering::SeqCst);
    if how == 0 { ENDED } else { CRASHED }
}

/// A stopped task's sum: each leaf's sleep is cut short, so every leaf fails, and nothing's left to
/// combine. Each range still ends where it started. A combining range once started and then left,
/// unended, when a half had failed, keeping what was lent for the call on its thread.
#[test]
fn a_stopped_task_s_sum_ends_every_range_it_started() {
    begin();
    let task = launch_on(own_pool(&STOPPED_SUM_POOL, 2), sums_until_it_s_stopped, 0);
    let started = Instant::now();
    while SUMMING.load(Ordering::SeqCst) < 4 && started.elapsed() < GIVES_UP {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(SUMMING.load(Ordering::SeqCst), 4, "the leaves didn't all start");
    let mut failure = 0;
    assert_eq!(launch_end(task, 10 * SECONDS, 1, &mut failure), ENDED);
    assert_eq!(
        STOPPED_SUM_GAVE.load(Ordering::SeqCst),
        2,
        "the sum failed, and left its worker as it found it"
    );
    finish();
}

static UNBEGUN_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// How many ranges' bodies ran, for a body that may wait and for one that never does.
static BEGAN: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
static FIRST_HOLDS_ITS_THREAD: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
/// What each call gave back once its task had been stopped, plus one.
static STOPPED_EARLY_GAVE: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];

/// Counts itself, and the first holds the pool's one thread until its task has been told to stop,
/// so the rest haven't begun by then.
fn begins(kind: usize, from: i64) -> i64 {
    BEGAN[kind].fetch_add(1, Ordering::SeqCst);
    if from == 0 {
        FIRST_HOLDS_ITS_THREAD[kind].store(true, Ordering::SeqCst);
        let started = Instant::now();
        while !told_to_stop() && started.elapsed() < GIVES_UP {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    0
}

extern "C" fn begins_and_may_wait(_env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    begins(0, from)
}

extern "C" fn begins_and_never_waits(_env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    begins(1, from)
}

fn maps_sixteen(kind: usize) -> i64 {
    let items = lists::new(LINES, 16);
    let made = if kind == 0 {
        parallel_map_waiting(items, begins_and_may_wait, 0, LINES)
    } else {
        parallel_map_heavy(items, begins_and_never_waits, 0, LINES)
    };
    STOPPED_EARLY_GAVE[kind].store(made.min(1) + 1, Ordering::SeqCst);
    heap::free(made);
    heap::free(items);
    if made == 0 { CRASHED } else { ENDED }
}

extern "C" fn maps_a_body_that_may_wait_until_stopped(_env: i64, _thread: i64) -> i64 {
    maps_sixteen(0)
}

extern "C" fn maps_a_body_that_never_waits_through_a_stop(_env: i64, _thread: i64) -> i64 {
    maps_sixteen(1)
}

/// A task stopped while it's parked in a comprehension whose body may wait is in a wait, and the
/// stop cuts it short: the ranges that haven't begun don't, and the call fails as stopped. A
/// comprehension whose body never waits is no wait: every range runs and the call gives its list,
/// as the task's own code would have run on to its next wait.
#[test]
fn a_stop_ends_a_waiting_call_before_its_other_ranges_begin_and_lets_a_heavy_one_finish() {
    begin();
    let pool = own_pool(&UNBEGUN_POOL, 1);
    let bodies: [TaskBody; 2] =
        [maps_a_body_that_may_wait_until_stopped, maps_a_body_that_never_waits_through_a_stop];
    for (kind, body) in bodies.into_iter().enumerate() {
        let task = launch_on(pool, body, 0);
        let started = Instant::now();
        while !FIRST_HOLDS_ITS_THREAD[kind].load(Ordering::SeqCst) && started.elapsed() < GIVES_UP {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(FIRST_HOLDS_ITS_THREAD[kind].load(Ordering::SeqCst), "the first range didn't start");
        let mut failure = 0;
        // The block failed, so its task is stopped while the first range holds the only thread.
        assert_eq!(launch_end(task, 10 * SECONDS, 1, &mut failure), ENDED);
    }
    let began: Vec<i64> = BEGAN.iter().map(|began| began.load(Ordering::SeqCst)).collect();
    assert_eq!(began, [1, 16], "ranges begun: of the body that may wait, and of the one that never does");
    let gave: Vec<i64> = STOPPED_EARLY_GAVE.iter().map(|gave| gave.load(Ordering::SeqCst)).collect();
    assert_eq!(gave, [1, 2], "the waiting call failed, and the heavy one gave its list");
    assert!(!super::super::super::super::failed(), "the launcher hasn't failed");
    finish();
}

static CUTTING_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// The ranges a heavy comprehension over 200 items was cut into, made by a task and made by the
/// test's own thread.
static CUT_FOR_A_TASK: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());
static CUT_FOR_A_THREAD: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());

extern "C" fn notes_a_task_s_range(_env: i64, from: i64, to: i64, _thread: i64, _out: i64) -> i64 {
    CUT_FOR_A_TASK.lock().unwrap_or_else(PoisonError::into_inner).push((from, to));
    0
}

extern "C" fn notes_a_thread_s_range(_env: i64, from: i64, to: i64, _thread: i64, _out: i64) -> i64 {
    CUT_FOR_A_THREAD.lock().unwrap_or_else(PoisonError::into_inner).push((from, to));
    0
}

extern "C" fn maps_two_hundred(_env: i64, _thread: i64) -> i64 {
    let items = lists::new(LINES, 200);
    heap::free(parallel_map_heavy(items, notes_a_task_s_range, 0, LINES));
    heap::free(items);
    ENDED
}

/// How a call's list is cut depends on the list and the body alone, so the same call is cut into
/// the same ranges whether a task makes it, for the pool, or a thread does.
#[test]
fn a_task_s_call_is_cut_into_the_ranges_a_thread_s_is() {
    begin();
    let task = launch_on(own_pool(&CUTTING_POOL, 2), maps_two_hundred, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    let items = lists::new(LINES, 200);
    heap::free(parallel_map_heavy(items, notes_a_thread_s_range, 0, LINES));
    heap::free(items);
    let sorted = |cut: &Mutex<Vec<(i64, i64)>>| {
        let mut ranges = std::mem::take(&mut *cut.lock().unwrap_or_else(PoisonError::into_inner));
        ranges.sort_unstable();
        ranges
    };
    let (for_a_task, for_a_thread) = (sorted(&CUT_FOR_A_TASK), sorted(&CUT_FOR_A_THREAD));
    assert_eq!(for_a_task.len(), 64, "a heavy body's 200 items are cut into 64 ranges: {for_a_task:?}");
    assert_eq!(for_a_task, for_a_thread);
    finish();
}

static MAKING_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// The string each range made.
static MADE: [AtomicI64; 4] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
/// How many of them the task could read as its own once the call was over.
static READ_AS_ITS_OWN: AtomicI64 = AtomicI64::new(0);
/// The one the task kept until it ended.
static KEPT: AtomicI64 = AtomicI64::new(0);

/// Makes a string on whichever worker it runs on, and waits a moment, so the ranges take turns.
extern "C" fn makes_a_string(_env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    let made = with(|state| heap::text_address(state, &format!("made by range {from}")));
    clock_sleep(SECONDS / 1000);
    if let Some(slot) = usize::try_from(from).ok().and_then(|from| MADE.get(from)) {
        slot.store(made, Ordering::SeqCst);
    }
    0
}

fn text(block: i64) -> Option<String> {
    let address = u64::try_from(block).ok()?;
    with(|state| state.heap.block(address).is_some().then(|| heap::read_block(state, address))).and_then(
        |read| match read {
            crate::value::Value::Str(text) => Some(text.to_string()),
            _ => None,
        },
    )
}

extern "C" fn maps_a_body_that_makes_blocks(_env: i64, _thread: i64) -> i64 {
    let items = list(&["one", "two", "three", "four"]);
    heap::free(parallel_map_waiting(items, makes_a_string, 0, LINES));
    heap::free(items);
    for (from, made) in MADE.iter().enumerate() {
        let made = made.load(Ordering::SeqCst);
        if is_own(made) && text(made) == Some(format!("made by range {from}")) {
            READ_AS_ITS_OWN.fetch_add(1, Ordering::SeqCst);
        }
        // The last is kept: the task's own, it goes to the launcher when the task ends.
        if from == 3 {
            KEPT.store(made, Ordering::SeqCst);
        } else {
            heap::free(made);
        }
    }
    ENDED
}

#[test]
fn what_a_task_s_ranges_make_is_the_task_s_own_and_goes_on_with_it_when_it_ends() {
    begin();
    let before = live();
    let task = launch_on(own_pool(&MAKING_POOL, 2), maps_a_body_that_makes_blocks, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(READ_AS_ITS_OWN.load(Ordering::SeqCst), 4, "the task reads what each range made as its own");
    let kept = KEPT.load(Ordering::SeqCst);
    assert_eq!(text(kept).as_deref(), Some("made by range 3"), "what the task kept came with it");
    assert_eq!(owner_of(kept), 0, "and is the launcher's own now");
    heap::free(kept);
    assert_eq!(live(), before);
    finish();
}

static STOPPING_POOL: OnceLock<&'static Pool> = OnceLock::new();
static SLEEPING: AtomicI64 = AtomicI64::new(0);
static CUT_SHORT: AtomicI64 = AtomicI64::new(0);
/// What the stopped task's call gave back, plus one.
static STOPPED_CALL_GAVE: AtomicI64 = AtomicI64::new(0);

/// Sleeps for a minute, on any worker, unless its task is stopped.
extern "C" fn sleeps_until_its_task_is_stopped(
    _env: i64,
    _from: i64,
    _to: i64,
    _thread: i64,
    _out: i64,
) -> i64 {
    SLEEPING.fetch_add(1, Ordering::SeqCst);
    clock_sleep(60 * SECONDS);
    if !super::super::super::super::failed() {
        return 0;
    }
    CUT_SHORT.fetch_add(1, Ordering::SeqCst);
    CRASHED
}

extern "C" fn maps_until_it_s_stopped(_env: i64, _thread: i64) -> i64 {
    let items = list(&["one", "two", "three", "four"]);
    let made = parallel_map_waiting(items, sleeps_until_its_task_is_stopped, 0, LINES);
    STOPPED_CALL_GAVE.store(made + 1, Ordering::SeqCst);
    heap::free(items);
    if made == 0 { CRASHED } else { ENDED }
}

/// The task is parked waiting for its ranges, which sleep on both workers. Its stop wakes it, it
/// passes the stop on, and each range's sleep is cut short, so the call ends and the task with it.
/// A stop that only reached the task would leave it waiting out the ranges' minute.
#[test]
fn a_stop_reaches_the_ranges_a_parked_task_waits_for() {
    begin();
    let task = launch_on(own_pool(&STOPPING_POOL, 2), maps_until_it_s_stopped, 0);
    let started = Instant::now();
    while SLEEPING.load(Ordering::SeqCst) < 4 && started.elapsed() < GIVES_UP {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(SLEEPING.load(Ordering::SeqCst), 4, "the ranges didn't all start");
    let mut failure = 0;
    // The block failed, so its task is stopped at once.
    assert_eq!(launch_end(task, 10 * SECONDS, 1, &mut failure), ENDED);
    assert_eq!(CUT_SHORT.load(Ordering::SeqCst), 4, "each range's sleep was cut short");
    assert_eq!(STOPPED_CALL_GAVE.load(Ordering::SeqCst), 1, "the call failed, as its ranges did");
    assert!(!super::super::super::super::failed(), "the launcher hasn't failed");
    finish();
}

static REDUCING_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// How many positions the reduction is over: four leaves.
const SUMMED: i64 = 4096;
static SUM: AtomicI64 = AtomicI64::new(0);
static LEAVES_AT_ONCE: AtomicI64 = AtomicI64::new(0);
static LEAVES_RUNNING: AtomicI64 = AtomicI64::new(0);
/// How many leaves the reduction has, and how many have begun.
const LEAVES: i64 = 4;
static LEAVES_BEGUN: AtomicI64 = AtomicI64::new(0);

/// The sum of a range's positions, after a wait in which the other leaves get their turns: it waits
/// until every leaf has begun, so all are running at once however slowly they start, as under
/// `ThreadSanitizer` (#2519). A leaf that only runs once another has ended waits ten seconds for
/// it, a millisecond at a time, and gives up, and fewer were running at once.
extern "C" fn sums_positions(_env: i64, from: i64, to: i64, _thread: i64, partial: &mut i64) -> i64 {
    let running = LEAVES_RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
    LEAVES_AT_ONCE.fetch_max(running, Ordering::SeqCst);
    LEAVES_BEGUN.fetch_add(1, Ordering::SeqCst);
    for _ in 0..10_000 {
        if LEAVES_BEGUN.load(Ordering::SeqCst) >= LEAVES {
            break;
        }
        clock_sleep(SECONDS / 1000);
    }
    LEAVES_RUNNING.fetch_sub(1, Ordering::SeqCst);
    *partial = (from..to).sum();
    0
}

extern "C" fn adds(_env: i64, a: i64, b: i64, out: &mut i64) -> i64 {
    *out = a + b;
    0
}

extern "C" fn reduces_while_it_waits(_env: i64, _thread: i64) -> i64 {
    let items = lists::new(LINES, SUMMED);
    let mut sum = 0;
    let how = parallel_reduce_waiting(items, sums_positions, adds, 0, &mut sum);
    heap::free(items);
    SUM.store(if how == 0 { sum } else { -1 }, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_s_reduction_whose_body_may_wait_runs_its_leaves_at_once() {
    begin();
    let task = launch_on(own_pool(&REDUCING_POOL, 2), reduces_while_it_waits, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(SUM.load(Ordering::SeqCst), (0..SUMMED).sum::<i64>());
    // Each leaf is a task that parks while it waits, so they all wait at once, on two workers.
    assert_eq!(LEAVES_AT_ONCE.load(Ordering::SeqCst), LEAVES, "the leaves waited one after another");
    finish();
}

static LONG_RANGE_POOL: OnceLock<&'static Pool> = OnceLock::new();
/// What the long range kept, from its start and from its end.
static LONG_RANGE_KEPT: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
/// The longest its list of what it had made was, each time it looked, and the longest its heap
/// said the list could get then.
static LONG_RANGE_LISTED: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
/// Whether it had let its list go by the time it stopped.
static LONG_RANGE_LET_GO: AtomicBool = AtomicBool::new(false);
/// What the task found of what the range kept, once its call was over: 1 for each that was its own
/// now, and whole.
static LONG_RANGE_FOUND: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];

/// A range of a waiting call, run as a task of its own: the first makes a list and lets it go over
/// and over, as a piece serving a session for an hour does, keeping one list from its start and one
/// from its end. The other ends at once.
extern "C" fn one_range_runs_long(_env: i64, from: i64, _to: i64, _thread: i64, _out: i64) -> i64 {
    if from != 0 {
        return 0;
    }
    LONG_RANGE_KEPT[0].store(list(&["kept from the start"]), Ordering::SeqCst);
    let mut after_letting_go = 0;
    for round in 0..2_000_000 {
        heap::free(list(&["made and let go of"]));
        if round % 1000 == 0 {
            let (listed, allowed, let_go) =
                with(|state| (state.heap.made_so_far(), state.heap.most_listed(), state.heap.made_let_go()));
            LONG_RANGE_LISTED[0].fetch_max(i64::try_from(listed).unwrap_or(i64::MAX), Ordering::SeqCst);
            LONG_RANGE_LISTED[1].fetch_max(i64::try_from(allowed).unwrap_or(i64::MAX), Ordering::SeqCst);
            LONG_RANGE_LET_GO.store(let_go, Ordering::SeqCst);
            after_letting_go += usize::from(let_go);
        }
        if after_letting_go > 100 {
            break;
        }
    }
    LONG_RANGE_KEPT[1].store(list(&["kept from the end"]), Ordering::SeqCst);
    0
}

/// A task whose waiting call has one long range, which then reads what that range kept.
extern "C" fn calls_with_a_long_range(_env: i64, _thread: i64) -> i64 {
    let items = list(&["one", "two"]);
    heap::free(parallel_map_waiting(items, one_range_runs_long, 0, LINES));
    heap::free(items);
    for (at, text) in [(0, "kept from the start"), (1, "kept from the end")] {
        let kept = LONG_RANGE_KEPT[at].load(Ordering::SeqCst);
        LONG_RANGE_FOUND[at].store(i64::from(is_own(kept) && lines(kept) == [text]), Ordering::SeqCst);
        heap::free(kept);
    }
    ENDED
}

/// A range of a waiting call that makes blocks for a long time keeps a short list, as a launched
/// task does. Run as a task of its own, it marked its thread's list as it started and handed over
/// by where the list was then, so the list was kept whole for as long as it ran: a word for every
/// block it made, freed or not, without end in a piece that lives as long as its program (#1842).
/// Now it lists as a launched task does, letting the list go once it's as long as a look through
/// its thread's table, and what it holds when it ends is found by its mark and handed to its
/// caller all the same.
#[test]
fn a_range_that_makes_blocks_for_a_long_time_keeps_a_short_list_and_hands_over_what_it_holds() {
    begin();
    let task = launch_on(own_pool(&LONG_RANGE_POOL, 2), calls_with_a_long_range, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    let (most, allowed) =
        (LONG_RANGE_LISTED[0].load(Ordering::SeqCst), LONG_RANGE_LISTED[1].load(Ordering::SeqCst));
    assert!(most <= allowed, "its list reached {most} blocks while it ran, past the {allowed} it may");
    assert!(LONG_RANGE_LET_GO.load(Ordering::SeqCst), "it never let its list go, having listed {most}");
    for (at, text) in [(0, "kept from the start"), (1, "kept from the end")] {
        assert_eq!(
            LONG_RANGE_FOUND[at].load(Ordering::SeqCst),
            1,
            "what it kept isn't the task's whole: {text}"
        );
    }
    finish();
}
