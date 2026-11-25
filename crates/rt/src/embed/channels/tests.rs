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

/// A message of more blocks than a copy keeps in a row, a list of forty strings each with a block
/// of its own, arrives whole and in order, and nothing of it is left once both sides let go.
#[test]
fn a_message_of_many_blocks_arrives_whole() {
    begin();
    let before = blocks();
    let channel = channel_new(1);
    let texts: Vec<String> =
        (0..40).map(|at| format!("line {at} of forty, long enough for a block of its own")).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    let sent = list(&texts);
    assert!(blocks() - before > 8, "it's more blocks than are kept in a row: {}", blocks() - before);
    assert_eq!(channel_send(channel, sent, 1, FOREVER), DONE);
    heap::free(sent);
    let (got, received) = receive(channel, FOREVER);
    assert_eq!(got, DONE);
    assert_eq!(lines(received), texts);
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
    assert_eq!(noted(quiet), (0, 0), "the wait that timed out and the one stopped each took its note back");
    channel_free(quiet);
    assert_eq!(TIMED.load(Ordering::SeqCst), TIMED_OUT);
    assert_eq!(STOPPED.load(Ordering::SeqCst), 1, "the stop cut the wait short");
    finish();
}

/// How many parked tasks have a note on `channel` to be woken through, waiting for room and for a
/// message.
fn noted(channel: i64) -> (usize, usize) {
    let found = find(channel).unwrap();
    let queue = found.queue();
    (queue.senders.len(), queue.receivers.len())
}

static FREED_UNDER: AtomicI64 = AtomicI64::new(0);
static FOUND_FREED: AtomicI64 = AtomicI64::new(-1);

/// Waits, parked, for a message on a channel that's freed while it waits.
extern "C" fn waits_while_freed(_env: i64, _thread: i64) -> i64 {
    let mut value = 0;
    FOUND_FREED
        .store(channel_receive(FREED_UNDER.load(Ordering::SeqCst), PATIENCE, &mut value), Ordering::SeqCst);
    0
}

/// Waits until `ready` says so, failing the test rather than hanging it.
fn until(what: &str, ready: impl Fn() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < Duration::from_secs(10), "never {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A channel freed while a task is parked on it, and while a thread waits on it, wakes both, and
/// each finds it closed at once. They waited on until their own time ran out, or their task was
/// stopped, as freeing told nobody.
#[test]
fn a_channel_freed_while_a_task_and_a_thread_wait_on_it_wakes_both_as_closed() {
    begin();
    let quiet = channel_new(1);
    FREED_UNDER.store(quiet, Ordering::SeqCst);
    let task = launch(waits_while_freed);
    until("parked", || noted(quiet).1 == 1);
    let thread = std::thread::spawn(move || {
        begin();
        let mut value = 0;
        let got = channel_receive(quiet, PATIENCE, &mut value);
        finish();
        got
    });
    until("asleep", || find(quiet).is_some_and(|found| found.queue().asleep.for_a_message == 1));
    let freed_at = Instant::now();
    channel_free(quiet);
    assert_eq!(thread.join().unwrap(), CLOSED, "the thread found it closed");
    assert_eq!(end(task), 0);
    assert_eq!(FOUND_FREED.load(Ordering::SeqCst), CLOSED, "the task found it closed");
    assert!(freed_at.elapsed() < Duration::from_secs(5), "they woke at once");
    finish();
}

/// A receiver on another thread, which keeps the channel to find again, sees its sender's end as
/// it always has: what was sent before the close, then `CLOSED` at once, and `CLOSED` again once
/// the channel's freed. What a thread keeps holds no say in when a channel closes: only `close`
/// and `free` do, and they're what's waited for here.
#[test]
fn a_receiver_that_kept_the_channel_sees_its_sender_close_it_as_before() {
    begin();
    let channel = channel_new(4);
    assert_eq!(channel_send(channel, 1, 0, FOREVER), DONE);
    let (took_one, close_now) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    let (took_one, said_took) = took_one;
    let (go_on, wait_for_close) = close_now;
    let receiving = std::thread::spawn(move || {
        begin();
        let first = receive(channel, PATIENCE);
        took_one.send(()).unwrap();
        wait_for_close.recv().unwrap();
        let after_close = [receive(channel, PATIENCE), receive(channel, PATIENCE)];
        took_one.send(()).unwrap();
        wait_for_close.recv().unwrap();
        let after_free = receive(channel, PATIENCE);
        finish();
        (first, after_close, after_free)
    });
    said_took.recv().unwrap();
    assert_eq!(channel_send(channel, 2, 0, FOREVER), DONE);
    channel_close(channel);
    go_on.send(()).unwrap();
    said_took.recv().unwrap();
    channel_free(channel);
    go_on.send(()).unwrap();
    let (first, after_close, after_free) = receiving.join().unwrap();
    assert_eq!(first, (DONE, 1));
    assert_eq!(after_close, [(DONE, 2), (CLOSED, 0)]);
    assert_eq!(after_free, (CLOSED, 0));
    finish();
}

/// A channel this thread keeps to find again, freed by another thread, is closed to this one at
/// once: what it keeps is held, so it stays to say it's freed. A thread that's found more channels
/// than it keeps still finds each one that isn't freed.
#[test]
fn a_channel_another_thread_freed_is_closed_though_this_one_kept_it() {
    begin();
    let made: Vec<i64> = (0..100).map(|_| channel_new(1)).collect();
    for channel in &made {
        assert_eq!(channel_send(*channel, 7, 0, FOREVER), DONE);
    }
    let freeing = made.clone();
    std::thread::spawn(move || {
        begin();
        for channel in freeing.iter().step_by(2) {
            channel_free(*channel);
        }
        finish();
    })
    .join()
    .unwrap();
    for (at, channel) in made.iter().enumerate() {
        let got = receive(*channel, MILLISECOND);
        if at % 2 == 0 {
            assert_eq!(got.0, CLOSED, "channel {at} was freed");
        } else {
            assert_eq!(got, (DONE, 7), "channel {at}");
        }
    }
    for channel in made.iter().skip(1).step_by(2) {
        channel_free(*channel);
    }
    finish();
}

/// How many times each of these tests' tasks waits and gives up.
const TRIES: i64 = 50;
/// What each test's task waits on: a quiet channel, a full one, and a listener nothing's sent to.
static GIVEN_UP_ON: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
static GAVE_UP: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];

/// Waits a millisecond for what never comes, `TRIES` times over: room to send if `sending`, and
/// otherwise a message.
fn gives_up(which: usize, sending: bool) -> i64 {
    let on = GIVEN_UP_ON[which].load(Ordering::SeqCst);
    let mut value = 0;
    for _ in 0..TRIES {
        let got = if sending {
            channel_send(on, 7, 0, MILLISECOND)
        } else {
            channel_receive(on, MILLISECOND, &mut value)
        };
        GAVE_UP[which].fetch_add(i64::from(got == TIMED_OUT), Ordering::SeqCst);
    }
    0
}

extern "C" fn gives_up_receiving(_env: i64, _thread: i64) -> i64 {
    gives_up(0, false)
}

extern "C" fn gives_up_sending(_env: i64, _thread: i64) -> i64 {
    gives_up(1, true)
}

extern "C" fn gives_up_listening(_env: i64, _thread: i64) -> i64 {
    gives_up(2, false)
}

/// A parked wait leaves a note on its channel to be woken through. One that runs out of time is
/// woken another way, and used to leave the note behind: a task waiting with a timeout in a loop
/// on a quiet channel added one each time, without end.
#[test]
fn a_parked_wait_that_runs_out_of_time_takes_its_note_back() {
    begin();
    let quiet = channel_new(1);
    let full = channel_new(1);
    assert_eq!(channel_send(full, 1, 0, FOREVER), DONE);
    let every = channel_new_every(4);
    let listener = channel_listen(every, 0);
    for (which, on) in [quiet, full, listener].into_iter().enumerate() {
        GIVEN_UP_ON[which].store(on, Ordering::SeqCst);
        GAVE_UP[which].store(0, Ordering::SeqCst);
    }
    for task in [launch(gives_up_receiving), launch(gives_up_sending), launch(gives_up_listening)] {
        assert_eq!(end(task), 0);
    }
    for gave_up in &GAVE_UP {
        assert_eq!(gave_up.load(Ordering::SeqCst), TRIES, "every wait ran out of time");
    }
    assert_eq!(noted(quiet), (0, 0), "a receive");
    assert_eq!(noted(full), (0, 0), "a send");
    assert_eq!(noted(every), (0, 0), "a listener's receive");
    for channel in [quiet, full, every] {
        channel_free(channel);
    }
    finish();
}

/// A wait takes back only its own note. A token is one worker's, so two tasks on two workers can
/// wait on one channel under the same token: the one whose time runs out leaves the other's note,
/// which the next send still wakes it through. And once a take has moved the count on, the wait's
/// note has gone with it, so it looks for nothing: a note left since under its name isn't its own.
#[test]
fn a_wait_takes_back_its_own_note_and_no_other_worker_s_under_the_same_token() {
    begin();
    let number = channel_new(1);
    let channel = find(number).unwrap();
    let token = mio::Token(5);
    let left_at = {
        let mut queue = channel.queue();
        queue.receivers.push(Waiter { pool: pool(), at: 0, token });
        queue.receivers.push(Waiter { pool: pool(), at: 1, token });
        channel.taken(Side::Receiving)
    };
    channel.take_back(Side::Receiving, (pool(), 0, token), left_at);
    let left: Vec<usize> = channel.queue().receivers.iter().map(|note| note.at).collect();
    assert_eq!(left, [1], "the note of the task on the other worker");
    let woken = {
        let mut queue = channel.queue();
        channel.take_notes(&mut queue, Side::Receiving)
    };
    assert_eq!(woken.len(), 1);
    channel.queue().receivers.push(Waiter { pool: pool(), at: 1, token });
    channel.take_back(Side::Receiving, (pool(), 1, token), left_at);
    assert_eq!(noted(number), (0, 1), "a note left after the take isn't the earlier wait's");
    channel.queue().receivers.clear();
    channel_free(number);
    finish();
}

static CUT: AtomicI64 = AtomicI64::new(0);
static CUT_SHORT: AtomicI64 = AtomicI64::new(-1);

extern "C" fn fails_at_once(_env: i64, _thread: i64) -> i64 {
    crate::embed::fail_with(Diagnostic::new("the task failed", Span::default()));
    0
}

/// Launches a task that fails, and waits for a message that never comes. The new task is put on
/// this one's worker, so it runs, and fails, only once this one is parked.
extern "C" fn waits_on_a_task_that_fails(_env: i64, _thread: i64) -> i64 {
    let task = launch(fails_at_once);
    let mut value = 0;
    CUT_SHORT.store(channel_receive(CUT.load(Ordering::SeqCst), PATIENCE, &mut value), Ordering::SeqCst);
    let mut failure = 0;
    launch_end(task, FOREVER, 0, &mut failure);
    0
}

/// A parked wait that's ended by a task it launched failing is woken by the failure, not through
/// its note, and takes the note back too.
#[test]
fn a_parked_wait_cut_short_by_a_task_that_failed_takes_its_note_back() {
    begin();
    let quiet = channel_new(1);
    CUT.store(quiet, Ordering::SeqCst);
    let task = launch(waits_on_a_task_that_fails);
    assert_eq!(end(task), 0);
    assert_eq!(CUT_SHORT.load(Ordering::SeqCst), CLOSED, "the failure ended the wait as a close would");
    assert_eq!(noted(quiet), (0, 0));
    channel_free(quiet);
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

/// A plain receive from a channel to every listener fails, saying what to do, and takes nothing:
/// there's no one next message, as each listener has a place of its own in what's kept. It used
/// to take the oldest kept message out from under them, after which a listener that was caught
/// up never saw the next message sent, and nothing said so.
#[test]
fn a_plain_receive_from_a_channel_to_every_listener_fails_and_takes_nothing() {
    begin();
    let channel = channel_new_every(8);
    let listener = channel_listen(channel, 0);
    assert_eq!(channel_send(channel, 1, 0, FOREVER), DONE);
    assert_eq!(receive(listener, FOREVER), (DONE, 1), "the listener's caught up");
    for timeout in [FOREVER, MILLISECOND] {
        assert!(!crate::embed::failed());
        assert_eq!(receive(channel, timeout), (DONE, 0));
        assert!(crate::embed::failed());
        let said = failure().unwrap_or_default();
        assert!(said.contains("to every listener") && said.contains("`inbox()`"), "{said}");
        crate::embed::clear_failure();
    }
    // Nothing was taken, so the listener hears what's sent next, which it used to miss.
    for word in [2, 3] {
        assert_eq!(channel_send(channel, word, 0, FOREVER), DONE);
    }
    assert_eq!(receive(listener, FOREVER), (DONE, 2));
    assert_eq!(receive(listener, FOREVER), (DONE, 3));
    // A work queue is received from as ever.
    let queue = channel_new(2);
    assert_eq!(channel_send(queue, 9, 0, FOREVER), DONE);
    assert_eq!(receive(queue, FOREVER), (DONE, 9));
    assert!(!crate::embed::failed());
    channel_free(queue);
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

/// How many threads are asleep on `channel`, waiting for room and for a message, and how many
/// times it's told them to look again.
fn asleep(channel: i64) -> (usize, usize, i64) {
    let found = find(channel).unwrap();
    let queue = found.queue();
    (queue.asleep.for_room, queue.asleep.for_a_message, found.told.load(Ordering::Relaxed))
}

/// Waits until `channel` has `room` threads asleep for room and `message` for a message, however
/// long the machine takes to get there. A count that never comes fails the test after a minute.
fn until_asleep(channel: i64, room: usize, message: usize) {
    let started = Instant::now();
    while asleep(channel).0 != room || asleep(channel).1 != message {
        let waited = started.elapsed() < Duration::from_secs(60);
        assert!(waited, "waited a minute for {room} and {message} asleep: {:?}", asleep(channel));
        std::thread::yield_now();
    }
}

/// A thread of its own with a runtime, as a thread that isn't a task has: its waits on a channel
/// block it.
fn thread<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> std::thread::JoinHandle<T> {
    std::thread::spawn(move || {
        begin();
        let done = body();
        finish();
        done
    })
}

/// A channel tells the threads asleep on it to look again only when there are some. It told them
/// at every send and every receive, asleep or not, which on Linux is a system call each time. The
/// count of who's asleep is kept under the queue's lock by the wait itself, so a thread that's
/// about to sleep is counted before the queue can change, and a change never finds the count
/// behind.
#[test]
fn a_channel_tells_the_threads_asleep_on_it_and_nobody_when_there_are_none() {
    begin();
    let channel = channel_new(8);
    for round in 0..1_000 {
        assert_eq!(channel_send(channel, round, 0, PATIENCE), DONE);
        assert_eq!(receive(channel, PATIENCE), (DONE, round));
    }
    assert_eq!(asleep(channel), (0, 0, 0), "nobody was asleep, so nobody was told");
    // A receive with nothing to take sleeps, and the send that comes tells it, once.
    let waiting = thread(move || receive(channel, PATIENCE));
    until_asleep(channel, 0, 1);
    assert_eq!(channel_send(channel, 7, 0, PATIENCE), DONE);
    assert_eq!(waiting.join().unwrap(), (DONE, 7), "it has the message well within its patience");
    // Told once. A thread may wake by itself, and one that had just then was looking again, not
    // asleep, when the send came: then it took the message untold.
    let (for_room, for_a_message, told) = asleep(channel);
    assert_eq!((for_room, for_a_message), (0, 0));
    assert!(told <= 1, "{told}");
    // A wait that runs out of time is no longer counted, and the send after it tells nobody.
    assert_eq!(receive(channel, MILLISECOND), (TIMED_OUT, 0));
    assert_eq!(channel_send(channel, 8, 0, PATIENCE), DONE);
    assert_eq!(asleep(channel), (0, 0, told));
    channel_free(channel);
    finish();
}

/// Eight threads send through a channel with room for one message, so nearly every send sleeps
/// until there's room, and none is left asleep: all 4,000 messages arrive, each once. A sender left
/// asleep at the end shows as a send or a receive that runs out of time, not as a test that hangs.
/// A wake lost along the way, and made up for by the next one, this can't see: that's what the test
/// of the first free slot is for.
#[test]
fn many_threads_sending_through_a_channel_of_one_are_each_woken_for_room() {
    const SENDERS: i64 = 8;
    const EACH: i64 = 500;
    begin();
    let channel = channel_new(1);
    let senders: Vec<_> = (0..SENDERS)
        .map(|sender| {
            thread(move || (0..EACH).all(|at| channel_send(channel, sender * EACH + at, 0, PATIENCE) == DONE))
        })
        .collect();
    let mut seen = vec![false; usize::try_from(SENDERS * EACH).unwrap()];
    for _ in 0..SENDERS * EACH {
        let (got, value) = receive(channel, PATIENCE);
        assert_eq!(got, DONE, "a message never came: {:?}", asleep(channel));
        let at = usize::try_from(value).unwrap();
        assert!(!std::mem::replace(&mut seen[at], true), "{value} came twice");
    }
    assert!(senders.into_iter().all(|sender| sender.join().unwrap()), "every send went");
    assert_eq!(receive(channel, MILLISECOND), (TIMED_OUT, 0), "and nothing more");
    let (for_room, for_a_message, told) = asleep(channel);
    assert_eq!((for_room, for_a_message), (0, 0));
    assert!(told > 0, "eight threads through room for one, and none ever slept");
    channel_free(channel);
    finish();
}

/// A thread asleep sending is woken at the first free slot, not once the channel has emptied
/// some way. With room for four and four waiting, one receive is enough for the fifth to go.
/// Waking it later would leave it asleep with room free, for ever if nothing more were received.
#[test]
fn a_thread_asleep_sending_is_woken_at_the_first_free_slot() {
    begin();
    let channel = channel_new(4);
    for word in 1..=4 {
        assert_eq!(channel_send(channel, word, 0, PATIENCE), DONE);
    }
    let fifth = thread(move || channel_send(channel, 5, 0, PATIENCE));
    until_asleep(channel, 1, 0);
    assert_eq!(receive(channel, PATIENCE), (DONE, 1));
    assert_eq!(fifth.join().unwrap(), DONE, "one free slot was enough");
    for word in 2..=5 {
        assert_eq!(receive(channel, PATIENCE), (DONE, word));
    }
    channel_free(channel);
    finish();
}

/// A thread asleep listening to a channel to every listener is woken by a send, as one asleep on
/// a work queue is: a listener that isn't a task sleeps on the same count.
#[test]
fn a_thread_asleep_listening_is_woken_by_a_send() {
    begin();
    let channel = channel_new_every(8);
    let listener = channel_listen(channel, 0);
    let listening = thread(move || receive(listener, PATIENCE));
    until_asleep(channel, 0, 1);
    assert_eq!(channel_send(channel, 9, 0, PATIENCE), DONE);
    assert_eq!(listening.join().unwrap(), (DONE, 9));
    assert_eq!((asleep(channel).0, asleep(channel).1), (0, 0));
    channel_free(channel);
    finish();
}

/// Closing a channel wakes a thread asleep sending to it, and one asleep receiving from it: each
/// is told it's closed, rather than left asleep.
#[test]
fn closing_a_channel_wakes_the_threads_asleep_sending_to_it_and_receiving_from_it() {
    begin();
    let full = channel_new(1);
    assert_eq!(channel_send(full, 1, 0, PATIENCE), DONE);
    let sending = thread(move || channel_send(full, 2, 0, PATIENCE));
    until_asleep(full, 1, 0);
    channel_close(full);
    assert_eq!(sending.join().unwrap(), CLOSED);
    assert_eq!(receive(full, PATIENCE), (DONE, 1), "what was sent before is still taken");
    assert_eq!(receive(full, PATIENCE), (CLOSED, 0));
    let empty = channel_new(1);
    let receiving = thread(move || receive(empty, PATIENCE));
    until_asleep(empty, 0, 1);
    channel_close(empty);
    assert_eq!(receiving.join().unwrap(), (CLOSED, 0));
    channel_free(full);
    channel_free(empty);
    finish();
}

/// Four threads send and four receive through a channel of one, with some of the sends and
/// receives giving up after a millisecond and trying again, so sleeping, being told, running out
/// of time and closing all cross each other. Every message sent arrives once, every thread ends,
/// and nobody's left counted asleep. Nothing waits for ever: a thread left asleep runs out of its
/// patience and fails the test.
#[test]
fn senders_and_receivers_crossing_on_a_channel_of_one_all_finish() {
    const SENDERS: i64 = 4;
    const EACH: i64 = 5_000;
    begin();
    let channel = channel_new(1);
    let senders: Vec<_> = (0..SENDERS)
        .map(|sender| {
            thread(move || {
                (0..EACH).all(|at| {
                    let value = sender * EACH + at;
                    // Every third send tries with a millisecond first, and goes on if it has to.
                    let quick =
                        if at % 3 == 0 { channel_send(channel, value, 0, MILLISECOND) } else { TIMED_OUT };
                    quick == DONE || channel_send(channel, value, 0, PATIENCE) == DONE
                })
            })
        })
        .collect();
    let receivers: Vec<_> = (0..4)
        .map(|receiver| {
            thread(move || {
                let (mut count, mut sum) = (0_i64, 0_i64);
                loop {
                    let patience = if (count + receiver) % 5 == 0 { MILLISECOND } else { PATIENCE };
                    match receive(channel, patience) {
                        (DONE, value) => {
                            count += 1;
                            sum += value;
                        }
                        (TIMED_OUT, _) if patience == MILLISECOND => {}
                        (how, _) => return (count, sum, how),
                    }
                }
            })
        })
        .collect();
    // Closed whether or not every send went, so the receivers end either way.
    let sent: Vec<bool> = senders.into_iter().map(|sender| sender.join().unwrap()).collect();
    channel_close(channel);
    let ended: Vec<(i64, i64, i64)> =
        receivers.into_iter().map(|receiver| receiver.join().unwrap()).collect();
    assert!(sent.iter().all(|went| *went), "every send went");
    let all = SENDERS * EACH;
    assert!(ended.iter().all(|(_, _, how)| *how == CLOSED), "each receiver ended at the close: {ended:?}");
    assert_eq!(ended.iter().map(|(count, _, _)| count).sum::<i64>(), all);
    assert_eq!(ended.iter().map(|(_, sum, _)| sum).sum::<i64>(), all * (all - 1) / 2);
    assert_eq!((asleep(channel).0, asleep(channel).1), (0, 0));
    channel_free(channel);
    finish();
}
