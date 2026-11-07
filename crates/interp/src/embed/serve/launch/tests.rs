//! Each test launches a task the way generated code will, with a Rust function for its body. A
//! body can't assert, as a panic in it would end the test run, so it notes what it saw for the
//! test to check.

use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use crate::embed::capabilities::clock_sleep;
use crate::embed::{Tables, finish, lists, start};
use crate::packed::Element;

/// The kind of item of a list of strings.
const LINES: i64 = 0;

fn tables() -> Tables {
    let strings = Rc::new(Element::Str);
    Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![strings], str_shape: 0, ..Tables::default() }
}

fn begin() {
    start(&fennec_syntax::Module { items: Vec::new() }, tables(), crate::Inputs::default()).unwrap();
}

/// The tests' own pool, whose workers' runtimes have these tests' tables, as a program's threads all
/// have its own.
fn pool() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            let _ = start(&fennec_syntax::Module { items: Vec::new() }, tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(2, &setup)
    })
}

/// `fennec_launch`, on the tests' pool.
fn launch(body: TaskBody, env: i64) -> i64 {
    launch_on(pool(), body, env)
}

fn list(items: &[&str]) -> i64 {
    with(|state| lists::list_of_strings(state, LINES, items))
}

/// What a list of strings holds, or nothing if this thread can't read it.
fn lines(list: i64) -> Vec<String> {
    let Some(address) = u64::try_from(list).ok() else { return Vec::new() };
    if with(|state| state.heap.block(address).is_none()) {
        return Vec::new();
    }
    let read = with(|state| Some(heap::read_block(state, address))).unwrap();
    let crate::value::Value::List(items) = read else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| match item {
            crate::value::Value::Str(text) => Some(text.to_string()),
            _ => None,
        })
        .collect()
}

fn owner_of(block: i64) -> u64 {
    let shape = with(|state| state.heap.block(u64::try_from(block).unwrap()).map(|words| words[1]));
    shape.unwrap_or(0) >> heap::OWNER_SHIFT
}

const SECONDS: i64 = 1_000_000_000;

static SAW: AtomicI64 = AtomicI64::new(0);

/// Notes how many lines its own copy holds, and that it isn't the launcher's, then fails with a
/// list of its own.
extern "C" fn fails_with_a_list(env: i64, _thread: i64) -> i64 {
    let mine = lines(env) == ["given", "to the task"];
    SAW.store(if mine { env } else { -1 }, Ordering::SeqCst);
    task_failed(list(&["failed"]));
    FAILED
}

#[test]
fn a_task_gets_its_own_copy_and_its_failure_comes_out_where_its_block_ends() {
    begin();
    let given = list(&["given", "to the task"]);
    let task = launch(fails_with_a_list, given);
    // The launcher goes on, and lets go of what it gave: the task has its own.
    heap::free(given);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), FAILED);
    let copy = SAW.load(Ordering::SeqCst);
    assert!(copy > 0, "the task didn't find its copy");
    assert_ne!(copy, given);
    assert_eq!(lines(failure), ["failed"]);
    assert_eq!(owner_of(failure), 0, "the failure's value is the launcher's own now");
    finish();
}

extern "C" fn crashes(_env: i64, _thread: i64) -> i64 {
    super::super::super::fail_with(Diagnostic::new("the task crashed", Span::default()));
    CRASHED
}

#[test]
fn a_crash_ends_only_its_task_and_comes_out_as_crashed() {
    begin();
    let task = launch(crashes, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), CRASHED);
    assert_eq!(failure, 0);
    assert!(!super::super::super::failed(), "the launcher hasn't failed");
    finish();
}

static SLEPT: AtomicI64 = AtomicI64::new(0);

/// Sleeps for ten seconds, noting 1 if that's cut short and 2 if it isn't.
extern "C" fn sleeps_long(_env: i64, _thread: i64) -> i64 {
    clock_sleep(10 * SECONDS);
    SLEPT.store(if super::super::super::failed() { 1 } else { 2 }, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_still_running_after_stop_after_is_stopped_and_ends_quietly() {
    begin();
    let task = launch(sleeps_long, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, SECONDS / 20, 0, &mut failure), ENDED);
    assert_eq!(SLEPT.load(Ordering::SeqCst), 1, "the sleep was cut short");
    assert!(!super::super::super::failed());
    finish();
}

static STOPPED_AT_ONCE: AtomicI64 = AtomicI64::new(0);

extern "C" fn sleeps_until_stopped(_env: i64, _thread: i64) -> i64 {
    clock_sleep(10 * SECONDS);
    STOPPED_AT_ONCE.store(if super::super::super::failed() { 1 } else { 2 }, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_block_that_failed_stops_its_task_at_once() {
    begin();
    let task = launch(sleeps_until_stopped, 0);
    let mut failure = 0;
    // A `stop_after` it never gets, as the block failed.
    assert_eq!(launch_end(task, 10 * SECONDS, 1, &mut failure), ENDED);
    assert_eq!(STOPPED_AT_ONCE.load(Ordering::SeqCst), 1);
    finish();
}

static FINISHED: AtomicI64 = AtomicI64::new(0);

extern "C" fn sleeps_a_little(_env: i64, _thread: i64) -> i64 {
    clock_sleep(SECONDS / 50);
    FINISHED.store(if super::super::super::failed() { 1 } else { 2 }, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_that_ends_in_time_is_waited_for_and_not_stopped() {
    begin();
    let task = launch(sleeps_a_little, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, 10 * SECONDS, 0, &mut failure), ENDED);
    assert_eq!(FINISHED.load(Ordering::SeqCst), 2, "it slept its full time");
    finish();
}

static READ: AtomicI64 = AtomicI64::new(0);
static SILENT_PORT: AtomicI64 = AtomicI64::new(0);

/// Reads a line from a server that never sends one.
extern "C" fn reads_silence(_env: i64, _thread: i64) -> i64 {
    let port = u16::try_from(SILENT_PORT.load(Ordering::SeqCst)).unwrap_or(0);
    let read = crate::stdlib::net::fetch_line("127.0.0.1", port);
    READ.store(if read.is_err() && super::super::super::failed() { 1 } else { 2 }, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_stop_cuts_a_socket_read_short() {
    begin();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    SILENT_PORT.store(i64::from(listener.local_addr().unwrap().port()), Ordering::SeqCst);
    let task = launch(reads_silence, 0);
    assert_ne!(task, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, SECONDS / 20, 0, &mut failure), ENDED);
    assert_eq!(READ.load(Ordering::SeqCst), 1, "the read was cut short");
    drop(listener);
    finish();
}

static INNER: AtomicI64 = AtomicI64::new(0);

extern "C" fn notes_it_ran(_env: i64, _thread: i64) -> i64 {
    clock_sleep(SECONDS / 100);
    INNER.fetch_add(1, Ordering::SeqCst);
    ENDED
}

/// A task that launches one of its own, beside it on its worker, and parks until it's ended.
extern "C" fn launches_another(_env: i64, _thread: i64) -> i64 {
    let task = launch(notes_it_ran, 0);
    let mut failure = 0;
    launch_end(task, -1, 0, &mut failure)
}

#[test]
fn a_task_launched_from_a_task_runs_beside_it_and_is_waited_for_parked() {
    begin();
    let tasks: Vec<i64> = (0..4).map(|_| launch(launches_another, 0)).collect();
    let mut failure = 0;
    for task in tasks {
        assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    }
    assert_eq!(INNER.load(Ordering::SeqCst), 4);
    finish();
}

static LITERAL: AtomicI64 = AtomicI64::new(0);
static LITERAL_READ: AtomicI64 = AtomicI64::new(0);

/// Reads a string literal of the launcher's, as native code holding its address does to print it.
extern "C" fn reads_a_literal(_env: i64, _thread: i64) -> i64 {
    let address = u64::try_from(LITERAL.load(Ordering::SeqCst)).unwrap_or(0);
    let read = with(|state| state.heap.block(address).is_some().then(|| heap::read_block(state, address)));
    let found = matches!(read, Some(crate::value::Value::Str(text)) if &*text == "a literal the task prints");
    LITERAL_READ.store(if found { 1 } else { 2 }, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_on_another_thread_reads_its_launcher_s_string_literals() {
    begin();
    let literal = with(|state| {
        let address = heap::literal(&mut state.tables, "a literal the task prints");
        state.heap.adopt(&mut state.tables.statics);
        address
    });
    LITERAL.store(literal, Ordering::SeqCst);
    let task = launch(reads_a_literal, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(LITERAL_READ.load(Ordering::SeqCst), 1);
    finish();
}
