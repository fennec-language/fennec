//! Channels on one thread, then between launched tasks and the code that launched them, the way
//! generated code will use them. A task's body can't assert, as a panic in it would end the test
//! run, so it notes what it saw for the test to check.

use std::rc::Rc;
use std::sync::atomic::AtomicI64;

use super::*;
use crate::embed::parallel::Pool;
use crate::embed::serve::{launch_end, launch_on};
use crate::embed::{Tables, finish, lists, start};
use crate::packed::Element;

/// The kind of item of a list of strings.
const LINES: i64 = 0;
const FOREVER: i64 = -1;
const MILLISECOND: i64 = 1_000_000;
/// How long the tests' tasks wait for each other at most, so a wake that's lost fails a test rather
/// than hang it.
const PATIENCE: i64 = 10_000 * MILLISECOND;

fn tables() -> Tables {
    let strings = Rc::new(Element::Str);
    Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![strings], str_shape: 0, ..Tables::default() }
}

fn begin() {
    start(&fennec_syntax::Module { items: Vec::new() }, tables(), crate::Inputs::default()).unwrap();
}

/// These tests' own pool, whose workers' runtimes have these tests' tables.
fn pool() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            let _ = start(&fennec_syntax::Module { items: Vec::new() }, tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(2, &setup)
    })
}

fn launch(body: crate::embed::serve::TaskBody) -> i64 {
    launch_on(pool(), body, 0)
}

/// Waits for a task to end, without stopping it.
fn end(task: i64) -> i64 {
    let mut failure = 0;
    launch_end(task, FOREVER, 0, &mut failure)
}

fn list(items: &[&str]) -> i64 {
    with(|state| lists::list_of_strings(state, LINES, items))
}

fn lines(list: i64) -> Vec<String> {
    let read = with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()))).unwrap();
    let crate::value::Value::List(items) = read else { panic!("not a list") };
    items
        .iter()
        .map(|item| match item {
            crate::value::Value::Str(text) => text.to_string(),
            other => panic!("not a string: {other:?}"),
        })
        .collect()
}

fn blocks() -> usize {
    with(|state| state.heap.blocks.len())
}

fn receive(channel: i64, timeout_ns: i64) -> (i64, i64) {
    let mut value = 0;
    let got = channel_receive(channel, timeout_ns, &mut value);
    (got, value)
}

#[test]
fn messages_come_out_in_order_and_a_full_or_empty_channel_times_out() {
    begin();
    let channel = channel_new(2);
    assert_eq!(channel_send(channel, 1, 0, FOREVER), DONE);
    assert_eq!(channel_send(channel, 2, 0, FOREVER), DONE);
    assert_eq!(channel_send(channel, 3, 0, MILLISECOND), TIMED_OUT, "it's full");
    assert_eq!(receive(channel, FOREVER), (DONE, 1));
    assert_eq!(receive(channel, FOREVER), (DONE, 2));
    assert_eq!(receive(channel, MILLISECOND), (TIMED_OUT, 0), "it's empty");
    channel_free(channel);
    finish();
}

#[test]
fn a_closed_channel_is_drained_before_it_says_so_and_takes_no_more() {
    begin();
    let channel = channel_new(4);
    channel_send(channel, 7, 0, FOREVER);
    channel_send(channel, 8, 0, FOREVER);
    channel_close(channel);
    assert_eq!(channel_send(channel, 9, 0, FOREVER), CLOSED, "it doesn't wait");
    assert_eq!(receive(channel, FOREVER), (DONE, 7));
    assert_eq!(receive(channel, FOREVER), (DONE, 8));
    assert_eq!(receive(channel, FOREVER), (CLOSED, 0));
    channel_free(channel);
    assert_eq!(receive(channel, FOREVER), (CLOSED, 0), "a freed channel acts as a closed one");
    assert_eq!(channel_send(channel, 1, 0, FOREVER), CLOSED);
    finish();
}

#[test]
fn a_message_holding_blocks_arrives_as_the_receiver_s_own_copy() {
    begin();
    let before = blocks();
    let channel = channel_new(1);
    let sent = list(&["a message", "of two lines"]);
    assert_eq!(channel_send(channel, sent, 1, FOREVER), DONE);
    // The sender keeps its own, and lets go of it.
    heap::free(sent);
    let (got, received) = receive(channel, FOREVER);
    assert_eq!(got, DONE);
    assert_ne!(received, sent);
    assert_eq!(lines(received), ["a message", "of two lines"]);
    with(|state| heap::release(state, u64::try_from(received).unwrap()));
    channel_free(channel);
    assert_eq!(blocks(), before);
    finish();
}

/// The copy waits in a table of its own, so freeing the channel takes it into this thread's and
/// lets go of it there, as anything it holds, a `shared` value's handle among them, needs.
#[test]
fn freeing_a_channel_with_a_message_left_in_it_leaves_nothing_behind() {
    begin();
    let before = blocks();
    let channel = channel_new(2);
    let sent = list(&["never received"]);
    channel_send(channel, sent, 1, FOREVER);
    heap::free(sent);
    channel_free(channel);
    assert_eq!(blocks(), before);
    assert_eq!(receive(channel, FOREVER), (CLOSED, 0));
    finish();
}

/// The channel the tests' tasks use, set before each launches.
static INBOX: AtomicI64 = AtomicI64::new(0);
static SUM: AtomicI64 = AtomicI64::new(0);
static COUNT: AtomicI64 = AtomicI64::new(0);

/// Drains its inbox: receives until it's closed, adding up what it gets.
extern "C" fn drains(_env: i64, _thread: i64) -> i64 {
    let inbox = INBOX.load(Ordering::SeqCst);
    let mut value = 0;
    while channel_receive(inbox, PATIENCE, &mut value) == DONE {
        SUM.fetch_add(value, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
    }
    0
}

#[test]
fn a_task_drains_its_inbox_through_a_small_channel_and_ends_once_it_s_closed() {
    begin();
    let inbox = channel_new(4);
    INBOX.store(inbox, Ordering::SeqCst);
    let task = launch(drains);
    // Far more than it holds: this thread waits on a full channel, the task parks on an empty one.
    for n in 0..200 {
        assert_eq!(channel_send(inbox, n, 0, PATIENCE), DONE);
    }
    channel_close(inbox);
    assert_eq!(end(task), 0);
    channel_free(inbox);
    assert_eq!(COUNT.load(Ordering::SeqCst), 200);
    assert_eq!(SUM.load(Ordering::SeqCst), (0..200).sum::<i64>());
    finish();
}

static OUTBOX: AtomicI64 = AtomicI64::new(0);
static SENT: AtomicI64 = AtomicI64::new(0);

/// Sends 100 numbers into a channel that holds one, parking each time it's full.
extern "C" fn sends(_env: i64, _thread: i64) -> i64 {
    let outbox = OUTBOX.load(Ordering::SeqCst);
    for n in 0..100 {
        if channel_send(outbox, n, 0, PATIENCE) == DONE {
            SENT.fetch_add(1, Ordering::SeqCst);
        }
    }
    channel_close(outbox);
    0
}

#[test]
fn a_task_sending_into_a_full_channel_parks_until_there_s_room() {
    begin();
    let outbox = channel_new(1);
    OUTBOX.store(outbox, Ordering::SeqCst);
    let task = launch(sends);
    let mut got = Vec::new();
    while let (DONE, value) = receive(outbox, PATIENCE) {
        got.push(value);
    }
    assert_eq!(end(task), 0);
    channel_free(outbox);
    assert_eq!(got, (0..100).collect::<Vec<_>>());
    assert_eq!(SENT.load(Ordering::SeqCst), 100);
    finish();
}

static QUIET: AtomicI64 = AtomicI64::new(0);
static STOPPED: AtomicI64 = AtomicI64::new(0);
static TIMED: AtomicI64 = AtomicI64::new(0);

/// Waits for a message that never comes: first for a millisecond, then for as long as it takes.
extern "C" fn waits_for_nothing(_env: i64, _thread: i64) -> i64 {
    let quiet = QUIET.load(Ordering::SeqCst);
    let mut value = 0;
    TIMED.store(channel_receive(quiet, MILLISECOND, &mut value), Ordering::SeqCst);
    channel_receive(quiet, FOREVER, &mut value);
    STOPPED.store(if crate::embed::failed() { 1 } else { 2 }, Ordering::SeqCst);
    0
}

#[test]
fn a_parked_receive_times_out_and_a_stop_cuts_one_short() {
    begin();
    let quiet = channel_new(1);
    QUIET.store(quiet, Ordering::SeqCst);
    let task = launch(waits_for_nothing);
    let mut failure = 0;
    assert_eq!(launch_end(task, 20 * MILLISECOND, 0, &mut failure), 0, "stopped, quietly");
    channel_free(quiet);
    assert_eq!(TIMED.load(Ordering::SeqCst), TIMED_OUT);
    assert_eq!(STOPPED.load(Ordering::SeqCst), 1, "the stop cut the wait short");
    finish();
}
