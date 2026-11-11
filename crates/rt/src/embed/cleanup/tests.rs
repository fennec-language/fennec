//! A crash held aside around its cleanup, as native code will on its way out of a function: on this
//! thread, then in launched tasks that are stopped while they clean up. A task's body can't
//! assert, as a panic in it would end the test run, so it notes what it saw for the test to check.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use fennec_syntax::Span;

use super::*;
use crate::embed::capabilities::clock_sleep;
use crate::embed::parallel::Pool;
use crate::embed::serve::{launch_end, launch_on};
use crate::embed::{Tables, fail_with, failed, finish, start};
use crate::packed::Element;

const SECONDS: i64 = 1_000_000_000;

fn tables() -> Tables {
    Tables { shapes: vec![Rc::new(Element::Str)], str_shape: 0, ..Tables::default() }
}

fn begin() {
    start(tables(), crate::Inputs::default());
}

/// These tests' own pool, whose workers' runtimes have these tests' tables.
fn pool() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            start(tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(2, &setup)
    })
}

fn crash(message: &str) {
    fail_with(Diagnostic::new(message, Span::default()));
}

fn failure() -> Option<String> {
    with(|state| state.failure.as_ref().map(|failure| failure.message.clone()))
}

#[test]
fn a_crash_is_held_aside_while_its_cleanup_runs_and_goes_on_after() {
    begin();
    crash("the block crashed");
    let held = crash_hold();
    assert!(!failed(), "the cleanup's calls run as normal");
    assert_eq!(failure(), None);
    crash_resume(held);
    assert!(failed());
    assert_eq!(failure().as_deref(), Some("the block crashed"));
    finish();
}

#[test]
fn a_cleanup_that_crashes_too_leaves_the_first_crash_going_on() {
    begin();
    crash("the block crashed");
    let held = crash_hold();
    crash("so did its cleanup");
    crash_resume(held);
    assert!(failed());
    assert_eq!(failure().as_deref(), Some("the block crashed"));
    finish();
}

#[test]
fn cleanups_nest_innermost_first_each_holding_its_own_crash() {
    begin();
    crash("the block crashed");
    let outer = crash_hold();
    crash("the inner cleanup's block crashed");
    let inner = crash_hold();
    crash_resume(inner);
    assert_eq!(failure().as_deref(), Some("the inner cleanup's block crashed"));
    crash_resume(outer);
    assert_eq!(failure().as_deref(), Some("the block crashed"));
    finish();
}

static IN_GRACE: AtomicI64 = AtomicI64::new(0);

/// Its long sleep is cut short by a stop, and its cleanup then sleeps a little, which the stop's
/// grace lets it.
extern "C" fn cleans_up_after_a_stop(_env: i64, _thread: i64) -> i64 {
    clock_sleep(10 * SECONDS);
    let cut = failed();
    let held = crash_hold();
    clock_sleep(SECONDS / 20);
    let cleaned = !failed();
    crash_resume(held);
    IN_GRACE.store(if cut && cleaned { 1 } else { 2 }, Ordering::SeqCst);
    0
}

#[test]
fn a_stopped_task_s_cleanup_may_still_wait_within_its_grace() {
    begin();
    let task = launch_on(pool(), cleans_up_after_a_stop, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, SECONDS / 100, 0, &mut failure), 0, "it ends quietly");
    assert_eq!(IN_GRACE.load(Ordering::SeqCst), 1);
    finish();
}

static HARD: AtomicI64 = AtomicI64::new(0);

/// A cleanup that would wait ten seconds, longer than the grace.
extern "C" fn cleans_up_too_long(_env: i64, _thread: i64) -> i64 {
    crash("the block crashed");
    let held = crash_hold();
    clock_sleep(10 * SECONDS);
    HARD.store(if failed() { 1 } else { 2 }, Ordering::SeqCst);
    crash_resume(held);
    0
}

#[test]
fn a_cleanup_that_outlasts_the_grace_is_stopped_hard() {
    begin();
    let task = launch_on(pool(), cleans_up_too_long, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, SECONDS / 100, 0, &mut failure), 0);
    assert_eq!(HARD.load(Ordering::SeqCst), 1, "its wait was cut short after the grace");
    finish();
}

/// A pool of one worker, for tasks that must share it.
fn one_worker() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            start(tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(1, &setup)
    })
}

/// The order things happened in, across tasks.
static SEQUENCE: AtomicI64 = AtomicI64::new(1);
static CLEANUP_ENDED_AT: AtomicI64 = AtomicI64::new(0);
static OTHER_CUT_AT: AtomicI64 = AtomicI64::new(0);

/// Cleans up for 300 ms within its grace.
extern "C" fn cleans_up_a_while(_env: i64, _thread: i64) -> i64 {
    crash("the block crashed");
    let held = crash_hold();
    clock_sleep(3 * SECONDS / 10);
    CLEANUP_ENDED_AT.store(SEQUENCE.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
    crash_resume(held);
    0
}

extern "C" fn sleeps_until_cut(_env: i64, _thread: i64) -> i64 {
    clock_sleep(10 * SECONDS);
    OTHER_CUT_AT.store(SEQUENCE.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
    0
}

/// A cleanup waiting within its grace parks, rather than spin, so another task on its worker that's
/// stopped meanwhile is cut short while the cleanup still waits.
#[test]
fn a_cleanup_waiting_in_its_grace_leaves_its_worker_to_other_tasks() {
    begin();
    let cleaning = launch_on(one_worker(), cleans_up_a_while, 0);
    let other = launch_on(one_worker(), sleeps_until_cut, 0);
    // The cleaning task's stopped first, from another thread, so its cleanup's in its grace when
    // the other's stopped.
    let first = std::thread::spawn(move || {
        let mut failure = 0;
        launch_end(cleaning, SECONDS / 100, 0, &mut failure)
    });
    let mut failure = 0;
    assert_eq!(launch_end(other, SECONDS / 20, 0, &mut failure), 0);
    assert_eq!(first.join().unwrap(), 0);
    let (cut, ended) = (OTHER_CUT_AT.load(Ordering::SeqCst), CLEANUP_ENDED_AT.load(Ordering::SeqCst));
    assert!(cut > 0 && ended > 0);
    assert!(cut < ended, "the other task was cut short while the cleanup still waited");
    finish();
}

/// What a crash's cleanup gives back, how it failed or nothing, is let go of, and a failure is a
/// note under the crash's message, which comes back otherwise as it was.
#[test]
fn a_cleanup_s_failure_is_noted_on_the_crash_and_let_go_of() {
    begin();
    for (said, message) in [
        ("Refused(code=4)", "the first crash\nnote: the cleanup failed: Refused(code=4)"),
        ("", "the first crash"),
    ] {
        crash("the first crash");
        let number = crash_hold();
        let text = with(|state| crate::embed::heap::text_address(state, said));
        let at = u64::try_from(text).unwrap();
        crash_note(number, text);
        assert!(with(|state| state.heap.block(at).is_none()), "{said:?} let go of");
        crash_resume(number);
        assert!(failed());
        assert_eq!(failure().as_deref(), Some(message));
        let _ = crate::embed::take_failure();
    }
    finish();
}
