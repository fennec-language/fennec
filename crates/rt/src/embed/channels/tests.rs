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

fn failure() -> Option<String> {
    with(|state| state.failure.take().map(|failure| failure.message))
}

#[test]
fn every_listener_gets_every_message_sent_after_it_started_listening() {
    begin();
    let channel = channel_new_every(8);
    channel_send(channel, 1, 0, FOREVER);
    let (first, second) = (channel_listen(channel, 0), channel_listen(channel, 0));
    assert_ne!(first, second);
    for word in [2, 3] {
        assert_eq!(channel_send(channel, word, 0, FOREVER), DONE);
    }
    for listener in [first, second] {
        assert_eq!(receive(listener, FOREVER), (DONE, 2));
        assert_eq!(receive(listener, FOREVER), (DONE, 3));
        assert_eq!(receive(listener, MILLISECOND), (TIMED_OUT, 0));
    }
    channel_free(channel);
    finish();
}

#[test]
fn sending_never_waits_and_a_listener_that_falls_behind_fails_loudly() {
    begin();
    let channel = channel_new_every(2);
    let behind = channel_listen(channel, 0);
    for word in 1..=5 {
        assert_eq!(channel_send(channel, word, 0, FOREVER), DONE, "full, it still doesn't wait");
    }
    assert!(!crate::embed::failed());
    receive(behind, FOREVER);
    assert!(crate::embed::failed());
    let said = failure().unwrap_or_default();
    assert!(said.contains("missing 3 messages") && said.contains("when_behind=.DropOldest"), "{said}");
    crate::embed::clear_failure();
    channel_free(channel);
    finish();
}

#[test]
fn a_listener_that_drops_the_oldest_goes_on_from_what_is_kept() {
    begin();
    let channel = channel_new_every(2);
    let skipping = channel_listen(channel, 1);
    for word in 1..=5 {
        channel_send(channel, word, 0, FOREVER);
    }
    assert_eq!(receive(skipping, FOREVER), (DONE, 4));
    assert_eq!(receive(skipping, FOREVER), (DONE, 5));
    assert!(!crate::embed::failed());
    channel_free(channel);
    finish();
}

#[test]
fn each_listener_gets_a_copy_of_its_own_of_a_message_holding_blocks() {
    begin();
    let before = blocks();
    let channel = channel_new_every(4);
    let (first, second) = (channel_listen(channel, 0), channel_listen(channel, 0));
    let sent = list(&["for", "every listener"]);
    channel_send(channel, sent, 1, FOREVER);
    heap::free(sent);
    let (_, one) = receive(first, FOREVER);
    let (_, other) = receive(second, FOREVER);
    assert_ne!(one, other);
    assert_eq!(lines(one), ["for", "every listener"]);
    assert_eq!(lines(other), ["for", "every listener"]);
    for received in [one, other] {
        with(|state| heap::release(state, u64::try_from(received).unwrap()));
    }
    channel_free(channel);
    assert_eq!(blocks(), before, "nothing's left behind, the copy the channel kept included");
    finish();
}

#[test]
fn a_closed_channel_to_every_lets_each_listener_drain_then_says_so() {
    begin();
    let channel = channel_new_every(4);
    let listener = channel_listen(channel, 0);
    channel_send(channel, 7, 0, FOREVER);
    channel_close(channel);
    assert_eq!(channel_send(channel, 8, 0, FOREVER), CLOSED);
    assert_eq!(receive(listener, FOREVER), (DONE, 7));
    assert_eq!(receive(listener, FOREVER), (CLOSED, 0));
    channel_free(channel);
    assert_eq!(receive(listener, FOREVER), (CLOSED, 0), "a freed channel's listeners act as closed");
    finish();
}

#[test]
fn listening_to_a_work_queue_is_receiving_from_it() {
    begin();
    let channel = channel_new(2);
    assert_eq!(channel_listen(channel, 0), channel);
    channel_free(channel);
    finish();
}

static LISTENERS: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
static HEARD: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];

extern "C" fn first_listens(_env: i64, _thread: i64) -> i64 {
    listens(0)
}

extern "C" fn second_listens(_env: i64, _thread: i64) -> i64 {
    listens(1)
}

/// Adds up all it hears, parked while it waits, until the channel's closed.
fn listens(which: usize) -> i64 {
    let listener = LISTENERS[which].load(Ordering::SeqCst);
    let mut value = 0;
    while channel_receive(listener, PATIENCE, &mut value) == DONE {
        HEARD[which].fetch_add(value, Ordering::SeqCst);
    }
    0
}

#[test]
fn two_tasks_listening_each_hear_everything_sent() {
    begin();
    let channel = channel_new_every(128);
    for (which, slot) in LISTENERS.iter().enumerate() {
        slot.store(channel_listen(channel, 0), Ordering::SeqCst);
        HEARD[which].store(0, Ordering::SeqCst);
    }
    let tasks = [launch(first_listens), launch(second_listens)];
    for word in 1..=100 {
        assert_eq!(channel_send(channel, word, 0, FOREVER), DONE);
    }
    channel_close(channel);
    for task in tasks {
        assert_eq!(end(task), 0);
    }
    channel_free(channel);
    for heard in &HEARD {
        assert_eq!(heard.load(Ordering::SeqCst), (1..=100).sum::<i64>());
    }
    finish();
}

/// A channel this thread found, and keeps to find again without the lock every thread takes, is
/// gone once it's freed, as it is to a thread that never found it: a send says it's closed, and a
/// receive that there's nothing more.
#[test]
fn a_channel_found_and_kept_is_gone_once_it_s_freed() {
    begin();
    let channel = channel_new(2);
    assert_eq!(channel_send(channel, 7, 0, FOREVER), DONE);
    let mut got = 0;
    assert_eq!(channel_receive(channel, FOREVER, &mut got), DONE);
    assert_eq!(got, 7);
    assert!(FOUND.with_borrow(|found| found.contains_key(&channel)), "kept to find again");
    channel_free(channel);
    assert_eq!(channel_send(channel, 8, 0, FOREVER), CLOSED);
    assert_eq!(channel_receive(channel, FOREVER, &mut got), CLOSED);
    finish();
}

/// The channels the freed-while-kept test's task uses: the one it finds and keeps, one to say it
/// has, and one to be told it's been freed. Then what a send on the kept one came to.
static KEPT: AtomicI64 = AtomicI64::new(0);
static READY: AtomicI64 = AtomicI64::new(0);
static GO: AtomicI64 = AtomicI64::new(0);
static AFTER_FREE: AtomicI64 = AtomicI64::new(-1);

/// Finds and keeps a channel by receiving from it, says so, waits to be told it's been freed on
/// another thread, then sends on it.
extern "C" fn keeps_then_sends(_env: i64, _thread: i64) -> i64 {
    let (kept, ready, go) =
        (KEPT.load(Ordering::SeqCst), READY.load(Ordering::SeqCst), GO.load(Ordering::SeqCst));
    let mut value = 0;
    if channel_receive(kept, PATIENCE, &mut value) != DONE || channel_send(ready, 1, 0, PATIENCE) != DONE {
        return 0;
    }
    if channel_receive(go, PATIENCE, &mut value) == DONE {
        AFTER_FREE.store(channel_send(kept, 9, 0, PATIENCE), Ordering::SeqCst);
    }
    0
}

/// A channel another thread found and keeps to find again, freed on this one, is closed to that
/// thread's next send: it never reaches the freed channel.
#[test]
fn a_channel_freed_while_another_thread_keeps_it_is_closed_to_that_thread() {
    begin();
    let (kept, ready, go) = (channel_new(2), channel_new(2), channel_new(2));
    KEPT.store(kept, Ordering::SeqCst);
    READY.store(ready, Ordering::SeqCst);
    GO.store(go, Ordering::SeqCst);
    let task = launch(keeps_then_sends);
    assert_eq!(channel_send(kept, 1, 0, PATIENCE), DONE);
    let mut value = 0;
    assert_eq!(channel_receive(ready, PATIENCE, &mut value), DONE, "the task found it");
    channel_free(kept);
    assert_eq!(channel_send(go, 1, 0, PATIENCE), DONE);
    assert_eq!(end(task), 0);
    assert_eq!(AFTER_FREE.load(Ordering::SeqCst), CLOSED);
    channel_free(ready);
    channel_free(go);
    finish();
}
