//! Each test launches a task the way generated code will, with a Rust function for its body. A
//! body can't assert, as a panic in it would end the test run, so it notes what it saw for the
//! test to check.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

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
    start(tables(), crate::Inputs::default());
}

/// The tests' own pool, whose workers' runtimes have these tests' tables, as a program's threads all
/// have its own.
fn pool() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            start(tables(), crate::Inputs::default());
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

/// What each of the two tests' long tasks kept: a list from before it had made many blocks, and
/// one from after.
static KEPT_EARLY: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
static KEPT_LATE: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
/// The longest its list of what it had made was, each time it looked, and the longest its heap
/// said the list could get, then: as long as a look through its thread's table.
static MOST_LISTED: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
static MOST_ALLOWED: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
/// Whether it had let its list go by the time it had made them all.
static LET_GO: [AtomicBool; 3] = [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)];
/// How many times it makes a list and lets it go, at most: each is two blocks. It stops once its
/// list has been let go and it's made as many again.
const CHURNED: usize = 2_000_000;

/// Makes a list and lets it go, `CHURNED` times over, as a task that runs for long does. It keeps
/// one from the start and one from the end, and fails with a third.
fn makes_and_frees_many(test: usize) -> i64 {
    KEPT_EARLY[test].store(list(&["kept from the start"]), Ordering::SeqCst);
    let mut after_letting_go = 0;
    for round in 0..CHURNED {
        heap::free(list(&["made and let go of"]));
        if round % 1000 == 0 {
            let (listed, allowed, let_go) =
                with(|state| (state.heap.made_so_far(), state.heap.most_listed(), state.heap.made_let_go()));
            MOST_LISTED[test].fetch_max(i64::try_from(listed).unwrap_or(i64::MAX), Ordering::SeqCst);
            MOST_ALLOWED[test].fetch_max(i64::try_from(allowed).unwrap_or(i64::MAX), Ordering::SeqCst);
            LET_GO[test].store(let_go, Ordering::SeqCst);
            after_letting_go += usize::from(let_go);
        }
        if after_letting_go > 100 {
            break;
        }
    }
    KEPT_LATE[test].store(list(&["kept from the end"]), Ordering::SeqCst);
    task_failed(list(&["failed after it all"]));
    FAILED
}

extern "C" fn runs_long_alone(_env: i64, _thread: i64) -> i64 {
    makes_and_frees_many(0)
}

extern "C" fn runs_long_beside_another(_env: i64, _thread: i64) -> i64 {
    makes_and_frees_many(1)
}

/// A task that makes blocks for a long time keeps no list of them all. It used to list every
/// block it made, freed or not, until it ended: a word each, without end in a task that runs as
/// long as its program. Now its list is never longer than a look through its thread's table would
/// be, however many it makes: past that it lets the list go. What it still holds when it ends is
/// found by its mark and handed to its launcher all the same: what it made before it let the list
/// go, what it made after, and its failure.
#[test]
fn a_task_that_makes_blocks_for_a_long_time_keeps_a_short_list_and_hands_over_what_it_holds() {
    begin();
    let task = launch(runs_long_alone, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), FAILED);
    let (most, allowed) = (MOST_LISTED[0].load(Ordering::SeqCst), MOST_ALLOWED[0].load(Ordering::SeqCst));
    assert!(most <= allowed, "its list reached {most} blocks while it ran, past the {allowed} it may");
    assert!(LET_GO[0].load(Ordering::SeqCst), "it never let its list go, having listed {most}");
    for (kept, text) in [(&KEPT_EARLY[0], "kept from the start"), (&KEPT_LATE[0], "kept from the end")] {
        let kept = kept.load(Ordering::SeqCst);
        assert_eq!(lines(kept), [text]);
        assert_eq!(owner_of(kept), 0, "what it kept is the launcher's own now: {text}");
        heap::free(kept);
    }
    assert_eq!(lines(failure), ["failed after it all"]);
    assert_eq!(owner_of(failure), 0);
    heap::free(failure);
    finish();
}

/// What the task that makes its blocks in ranges kept from inside one, after its list was let go.
static KEPT_IN_A_RANGE: AtomicI64 = AtomicI64::new(0);
/// The longest its list was between two ranges once it had been let go.
static LISTED_ONCE_LET_GO: AtomicI64 = AtomicI64::new(0);
/// How many ranges it runs at most, each making a hundred lists: it stops once its list has been
/// let go and it's run a hundred more.
const RANGES: usize = 20_000;

/// Makes every block inside a range it runs itself, as a task does whose `parallel` call's pieces
/// run on its own thread: each range marks the list as it starts, and ends with what it made kept
/// here, none of it handed over. It keeps a list from before any range, one from inside a range
/// after its list was let go, and fails with a third.
extern "C" fn makes_blocks_in_its_own_ranges(_env: i64, _thread: i64) -> i64 {
    KEPT_EARLY[2].store(list(&["kept from the start"]), Ordering::SeqCst);
    let mut after_letting_go = 0;
    for _ in 0..RANGES {
        let _marked = threads::made_scope_starts();
        for _ in 0..100 {
            heap::free(list(&["made in a range and let go of"]));
        }
        if after_letting_go == 50 {
            KEPT_IN_A_RANGE.store(list(&["kept from inside a range"]), Ordering::SeqCst);
        }
        threads::made_scope_ends(true);
        let (listed, allowed, let_go) =
            with(|state| (state.heap.made_so_far(), state.heap.most_listed(), state.heap.made_let_go()));
        let listed = i64::try_from(listed).unwrap_or(i64::MAX);
        MOST_LISTED[2].fetch_max(listed, Ordering::SeqCst);
        MOST_ALLOWED[2].fetch_max(i64::try_from(allowed).unwrap_or(i64::MAX), Ordering::SeqCst);
        LET_GO[2].store(let_go, Ordering::SeqCst);
        if let_go {
            LISTED_ONCE_LET_GO.fetch_max(listed, Ordering::SeqCst);
            after_letting_go += 1;
        }
        if after_letting_go > 100 {
            break;
        }
    }
    KEPT_LATE[2].store(list(&["kept from the end"]), Ordering::SeqCst);
    task_failed(list(&["failed after it all"]));
    FAILED
}

/// A task whose blocks are all made in ranges it runs itself keeps a short list too. Its list
/// used to grow for as long as it ran, in two ways. A look at the list that came while a range
/// was running couldn't let it go, and every look did, as every block was made in a range. And a
/// list let go already was kept again for each range and never dropped after, so what each range
/// made stayed listed. Now a range that ends on the thread that called it has the list looked
/// at, and one let go is emptied: between ranges it's never longer than it may be, and holds
/// nothing once it's been let go. What the task still holds at its end, from inside a range as
/// from outside, is found by its mark and handed to its launcher.
#[test]
fn a_task_that_makes_its_blocks_in_ranges_it_runs_itself_keeps_a_short_list() {
    begin();
    let task = launch(makes_blocks_in_its_own_ranges, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), FAILED);
    let (most, allowed) = (MOST_LISTED[2].load(Ordering::SeqCst), MOST_ALLOWED[2].load(Ordering::SeqCst));
    assert!(most <= allowed, "its list reached {most} blocks between ranges, past the {allowed} it may");
    assert!(LET_GO[2].load(Ordering::SeqCst), "it never let its list go, having listed {most}");
    assert_eq!(LISTED_ONCE_LET_GO.load(Ordering::SeqCst), 0, "blocks stayed listed after it was let go");
    let kept = [
        (KEPT_EARLY[2].load(Ordering::SeqCst), "kept from the start"),
        (KEPT_IN_A_RANGE.load(Ordering::SeqCst), "kept from inside a range"),
        (KEPT_LATE[2].load(Ordering::SeqCst), "kept from the end"),
    ];
    for (kept, text) in kept {
        assert_eq!(lines(kept), [text]);
        assert_eq!(owner_of(kept), 0, "what it kept is the launcher's own now: {text}");
        heap::free(kept);
    }
    assert_eq!(lines(failure), ["failed after it all"]);
    assert_eq!(owner_of(failure), 0);
    heap::free(failure);
    finish();
}

static BESIDE_POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static LONG_ONE_ENDED: AtomicBool = AtomicBool::new(false);
/// What the task beside the long one read of its own list once the long one had ended: 1 if it
/// was all there.
static BESIDE_READ: AtomicI64 = AtomicI64::new(0);

/// Makes a list, sleeps until the long task on its worker has ended, and reads its list again.
extern "C" fn keeps_a_list_beside_a_long_task(_env: i64, _thread: i64) -> i64 {
    let mine = list(&["the other task's own"]);
    while !LONG_ONE_ENDED.load(Ordering::SeqCst) {
        clock_sleep(SECONDS / 1000);
    }
    let whole = lines(mine) == ["the other task's own"];
    BESIDE_READ.store(if whole { 1 } else { 2 }, Ordering::SeqCst);
    heap::free(mine);
    ENDED
}

/// A long task's end takes what has its mark and nothing else: another task on the same worker,
/// parked while the long one ran and ended, still has every block it made.
#[test]
fn a_long_tasks_end_leaves_the_blocks_of_the_task_beside_it() {
    begin();
    let pool = one_worker(&BESIDE_POOL);
    let beside = launch_on(pool, keeps_a_list_beside_a_long_task, 0);
    let long = launch_on(pool, runs_long_beside_another, 0);
    let mut failure = 0;
    assert_eq!(launch_end(long, -1, 0, &mut failure), FAILED);
    heap::free(failure);
    for kept in [&KEPT_EARLY[1], &KEPT_LATE[1]] {
        heap::free(kept.load(Ordering::SeqCst));
    }
    LONG_ONE_ENDED.store(true, Ordering::SeqCst);
    assert_eq!(launch_end(beside, -1, 0, &mut failure), ENDED);
    assert_eq!(BESIDE_READ.load(Ordering::SeqCst), 1, "the task beside it lost a block");
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

static STACK_SEEN: AtomicI64 = AtomicI64::new(0);

/// Notes how big its own stack is.
extern "C" fn notes_its_stack(_env: i64, _thread: i64) -> i64 {
    let size = fennec_parked::stack_size().and_then(|size| i64::try_from(size).ok()).unwrap_or(-1);
    STACK_SEEN.store(size, Ordering::SeqCst);
    ENDED
}

/// A server may launch a task for each connection, so a launched task's stack is a few megabytes,
/// not a range's 64, whose kernel tables alone cost 16 KB a task.
#[test]
fn a_launched_task_runs_on_a_stack_smaller_than_a_range_s() {
    begin();
    let task = launch(notes_its_stack, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    let seen = usize::try_from(STACK_SEEN.load(Ordering::SeqCst)).unwrap_or(0);
    assert!(
        (super::super::LAUNCHED_STACK..super::super::RANGE_STACK).contains(&seen),
        "a launched task's stack is {seen} bytes"
    );
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

/// A pool of one worker that gets no stack for a task, so each it starts runs where it is, as a
/// plain call, and can't park.
fn stackless() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            start(tables(), crate::Inputs::default());
            crate::embed::serve::refuse_stacks_here(true);
        });
        crate::embed::parallel::with_setup(1, &setup)
    })
}

/// What a stackless task's wait came to: 1 if it was stopped and failed as stopped, 2 if something
/// else ended it, 3 if it could park after all, which the test isn't about.
fn stackless_wait(ended: bool) -> i64 {
    if crate::embed::serve::can_park() {
        3
    } else if ended && super::super::super::failed() {
        1
    } else {
        2
    }
}

static STACKLESS_SERVER: AtomicI64 = AtomicI64::new(0);
static STACKLESS_ACCEPT: AtomicI64 = AtomicI64::new(0);

/// Waits as long as it takes for a connection, where it can't park.
extern "C" fn accepts_without_a_stack(_env: i64, _thread: i64) -> i64 {
    let accepted = crate::stdlib::net::accepted_within(STACKLESS_SERVER.load(Ordering::SeqCst), None);
    STACKLESS_ACCEPT.store(stackless_wait(accepted.is_err()), Ordering::SeqCst);
    ENDED
}

static STACKLESS_SOCKET: AtomicI64 = AtomicI64::new(0);
static STACKLESS_RECEIVE: AtomicI64 = AtomicI64::new(0);

/// Waits a minute for a packet, where it can't park.
extern "C" fn receives_without_a_stack(_env: i64, _thread: i64) -> i64 {
    let socket = STACKLESS_SOCKET.load(Ordering::SeqCst);
    let received = crate::stdlib::net::received_within(socket, Duration::from_secs(60));
    STACKLESS_RECEIVE.store(stackless_wait(received.is_err()), Ordering::SeqCst);
    ENDED
}

/// Ends the task's wait with `release` if its stop hasn't within ten seconds, so a stop that does
/// nothing fails the test rather than holding it for good.
fn stopped_or_released(task: i64, release: impl FnOnce() + Send + 'static) -> Duration {
    let over = Arc::new(AtomicBool::new(false));
    let watching = Arc::clone(&over);
    let releasing = std::thread::spawn(move || {
        let started = Instant::now();
        while !watching.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !watching.load(Ordering::SeqCst) {
            release();
        }
    });
    let started = Instant::now();
    let mut failure = 0;
    assert_eq!(launch_end(task, SECONDS / 20, 0, &mut failure), ENDED);
    let took = started.elapsed();
    over.store(true, Ordering::SeqCst);
    releasing.join().unwrap();
    took
}

/// A stop ends a wait for a connection by a task that can't park, which holds its thread while it
/// waits in that thread's own poll. Nothing woke that poll, so the task waited on until a
/// connection came, for good with no timeout (#1803).
#[test]
fn a_stop_ends_a_wait_for_a_connection_by_a_task_that_cant_park() {
    begin();
    let (server, port) = crate::stdlib::net::listened();
    STACKLESS_SERVER.store(server, Ordering::SeqCst);
    let task = launch_on(stackless(), accepts_without_a_stack, 0);
    assert_ne!(task, 0);
    let took = stopped_or_released(task, move || drop(std::net::TcpStream::connect(("127.0.0.1", port))));
    assert_eq!(STACKLESS_ACCEPT.load(Ordering::SeqCst), 1, "the stop didn't end the accept");
    assert!(took < Duration::from_secs(5), "the stop took {took:?}");
    finish();
}

/// A stop ends a wait for a packet by a task that can't park, as it ends a wait for a connection
/// (#1803).
#[test]
fn a_stop_ends_a_wait_for_a_packet_by_a_task_that_cant_park() {
    begin();
    let (socket, port) = crate::stdlib::net::udp_bound();
    STACKLESS_SOCKET.store(socket, Ordering::SeqCst);
    let task = launch_on(stackless(), receives_without_a_stack, 0);
    assert_ne!(task, 0);
    let took = stopped_or_released(task, move || {
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        drop(sender.send_to(b"late", ("127.0.0.1", port)));
    });
    assert_eq!(STACKLESS_RECEIVE.load(Ordering::SeqCst), 1, "the stop didn't end the receive");
    assert!(took < Duration::from_secs(5), "the stop took {took:?}");
    finish();
}

static STACKLESS_LOOKUP: AtomicI64 = AtomicI64::new(0);

/// Waits a minute for a name's lookup whose name server never answers, where it can't park.
extern "C" fn looks_up_without_a_stack(_env: i64, _thread: i64) -> i64 {
    let minute = Duration::from_secs(60);
    let looked = crate::stdlib::net::looked_up_by("stopped.stalls.test", 80, Instant::now() + minute, minute);
    STACKLESS_LOOKUP.store(stackless_wait(looked.is_err()), Ordering::SeqCst);
    ENDED
}

/// A stop ends a wait for a name's lookup by a task that can't park, which waits for the lookups'
/// pool to answer holding its thread. Nothing woke that wait, so the task waited out its
/// connect's timeout (#1803).
#[test]
fn a_stop_ends_a_wait_for_a_lookup_by_a_task_that_cant_park() {
    use crate::stdlib::net::tests::stalls;
    let _alone = stalls::one_at_a_time();
    begin();
    let task = launch_on(stackless(), looks_up_without_a_stack, 0);
    assert_ne!(task, 0);
    let took = stopped_or_released(task, || stalls::let_go("stopped.stalls.test"));
    // The lookup, if the lookups' pool began it before the task gave up, ends unread.
    stalls::let_go("stopped.stalls.test");
    assert_eq!(STACKLESS_LOOKUP.load(Ordering::SeqCst), 1, "the stop didn't end the lookup");
    assert!(took < Duration::from_secs(5), "the stop took {took:?}");
    finish();
}

static READ_THROUGH_A_COPY: AtomicI64 = AtomicI64::new(0);
static SILENT_TO_A_COPY: AtomicI64 = AtomicI64::new(0);

/// Reads a line from a server that never sends one, for the test that waits through a copy.
extern "C" fn reads_silence_through_a_copy(_env: i64, _thread: i64) -> i64 {
    let port = u16::try_from(SILENT_TO_A_COPY.load(Ordering::SeqCst)).unwrap_or(0);
    let read = crate::stdlib::net::fetch_line("127.0.0.1", port);
    let how = if read.is_err() && super::super::super::failed() { 1 } else { 2 };
    READ_THROUGH_A_COPY.store(how, Ordering::SeqCst);
    ENDED
}

/// A stop cuts short a read that waits through a copy of its socket, as every task's does on
/// Windows: the task fails as stopped, and goes on to close its connection, whose other side hears
/// the end. That a wait's copy is closed is the serve tests' to show, since closing a connection
/// shuts it down whatever copies are left.
#[test]
fn a_stop_cuts_short_a_socket_read_that_waits_through_a_copy() {
    use crate::embed::serve::PARKED_ON_COPIES;
    begin();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    SILENT_TO_A_COPY.store(i64::from(listener.local_addr().unwrap().port()), Ordering::SeqCst);
    let before = PARKED_ON_COPIES.load(Ordering::Relaxed);
    let on_copies = crate::stdlib::net::OnCopies::begin();
    let task = launch(reads_silence_through_a_copy, 0);
    // The stop comes once a wait has parked through a copy, so it lands in the wait and not
    // before it, however long the task takes to get there. Another test's wait can let it by
    // early: the stop is then tried before the wait, which ends the read the same way.
    let started = Instant::now();
    while PARKED_ON_COPIES.load(Ordering::Relaxed) == before && started.elapsed() < Duration::from_secs(30) {
        std::thread::yield_now();
    }
    let mut failure = 0;
    let ended = if task == 0 { 0 } else { launch_end(task, SECONDS / 20, 0, &mut failure) };
    drop(on_copies);
    assert_eq!(ended, ENDED);
    assert_eq!(READ_THROUGH_A_COPY.load(Ordering::SeqCst), 1, "the read was cut short");
    let (mut made, _) = listener.accept().unwrap();
    made.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut rest = Vec::new();
    assert_eq!(std::io::Read::read_to_end(&mut made, &mut rest).ok(), Some(0), "{rest:?}");
    drop(listener);
    finish();
}

static WRITE_ENDED: AtomicI64 = AtomicI64::new(0);
static NEVER_READ: AtomicI64 = AtomicI64::new(0);

/// Writes to a server that never reads, on a connection made not to block, until a write fails.
extern "C" fn writes_to_nobody(_env: i64, _thread: i64) -> i64 {
    use crate::stdlib::net::{Failure, kept, shut, written_until_it_fails};
    let port = u16::try_from(NEVER_READ.load(Ordering::SeqCst)).unwrap_or(0);
    let Ok(handle) = kept("127.0.0.1", port) else { return ENDED };
    let failed = written_until_it_fails(handle, &vec![7_u8; 64 * 1024]);
    WRITE_ENDED.store(if matches!(failed, Failure::TimedOut(_)) { 1 } else { 2 }, Ordering::SeqCst);
    shut(handle);
    ENDED
}

/// A stop ends a write that's waiting for room, as it ends a read. The wait for room threw away
/// what its park came to, so a stopped task's write tried again at once, for ever: it never
/// waited and never ended, and its block's end never came back.
#[test]
fn a_stop_ends_a_write_thats_waiting_for_room() {
    begin();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    NEVER_READ.store(i64::from(listener.local_addr().unwrap().port()), Ordering::SeqCst);
    let task = launch(writes_to_nobody, 0);
    assert_ne!(task, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, SECONDS / 20, 0, &mut failure), ENDED);
    assert_eq!(WRITE_ENDED.load(Ordering::SeqCst), 1, "the write gave up as a stopped read does");
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

/// A pool of one worker of its own, for tasks that must share a thread, made by `make`.
fn one_worker(made: &'static std::sync::OnceLock<&'static Pool>) -> &'static Pool {
    made.get_or_init(|| {
        let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            start(tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(1, &setup)
    })
}

static SHARED_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static LONE_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();

/// How many turns of a loop native code ticks after.
const TICKS: u64 = 4096;

/// Spins in an arithmetic loop, ticking as native code will, until `stop` is set or `limit` passes:
/// gives back how many ticks made way, and whether the stop was what ended it.
fn spin(stop: &AtomicBool, limit: Duration) -> (i64, bool) {
    let started = Instant::now();
    let (mut x, mut turns, mut made_way) = (1_u64, 0_u64, 0);
    loop {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        turns += 1;
        if turns.is_multiple_of(TICKS) {
            made_way += crate::embed::serve::task_tick();
            if stop.load(Ordering::SeqCst) {
                std::hint::black_box(x);
                return (made_way, true);
            }
            if started.elapsed() > limit {
                std::hint::black_box(x);
                return (made_way, false);
            }
        }
    }
}

static STOP_SPINNING: AtomicBool = AtomicBool::new(false);
static SPUN: AtomicI64 = AtomicI64::new(0);
static REQUESTS: AtomicI64 = AtomicI64::new(0);
static REPLIES: AtomicI64 = AtomicI64::new(0);

extern "C" fn spins_until_told(_env: i64, _thread: i64) -> i64 {
    let (made_way, stopped) = spin(&STOP_SPINNING, Duration::from_secs(30));
    SPUN.store(if stopped { made_way } else { -1 }, Ordering::SeqCst);
    ENDED
}

/// Answers each request with one more, until the requests close.
extern "C" fn answers(_env: i64, _thread: i64) -> i64 {
    use crate::embed::channels::{channel_receive, channel_send};
    let (requests, replies) = (REQUESTS.load(Ordering::SeqCst), REPLIES.load(Ordering::SeqCst));
    let mut asked = 0;
    while channel_receive(requests, 5 * SECONDS, &mut asked) == 0 {
        channel_send(replies, asked + 1, 0, 5 * SECONDS);
    }
    ENDED
}

#[test]
fn a_task_spinning_in_a_loop_makes_way_for_one_answering_requests_on_its_thread() {
    use crate::embed::channels::{channel_close, channel_free, channel_new, channel_receive, channel_send};
    begin();
    let (requests, replies) = (channel_new(1), channel_new(1));
    REQUESTS.store(requests, Ordering::SeqCst);
    REPLIES.store(replies, Ordering::SeqCst);
    let worker = one_worker(&SHARED_WORKER);
    let spinning = launch_on(worker, spins_until_told, 0);
    let answering = launch_on(worker, answers, 0);
    let mut waited = Vec::new();
    for asked in 0..20 {
        let sent = Instant::now();
        assert_eq!(channel_send(requests, asked, 0, 5 * SECONDS), 0);
        let mut got = 0;
        assert_eq!(channel_receive(replies, 5 * SECONDS, &mut got), 0, "request {asked} wasn't answered");
        assert_eq!(got, asked + 1);
        waited.push(sent.elapsed());
    }
    // Told to stop only now: every answer came while it spun.
    STOP_SPINNING.store(true, Ordering::SeqCst);
    channel_close(requests);
    let mut failure = 0;
    assert_eq!(launch_end(answering, -1, 0, &mut failure), ENDED);
    assert_eq!(launch_end(spinning, -1, 0, &mut failure), ENDED);
    channel_free(requests);
    channel_free(replies);
    let made_way = SPUN.load(Ordering::SeqCst);
    assert!(made_way > 0, "the spinning task never made way, or wasn't stopped: {made_way}");
    waited.sort();
    eprintln!("answers while a task spun beside them: median {:?}, slowest {:?}", waited[10], waited[19]);
    finish();
}

static ALONE_STOP: AtomicBool = AtomicBool::new(false);
static ALONE_MADE_WAY: AtomicI64 = AtomicI64::new(-1);

extern "C" fn spins_alone(_env: i64, _thread: i64) -> i64 {
    let (made_way, _) = spin(&ALONE_STOP, Duration::from_millis(30));
    ALONE_MADE_WAY.store(made_way, Ordering::SeqCst);
    ENDED
}

#[test]
fn a_task_alone_on_its_worker_never_makes_way_and_outside_a_task_a_tick_does_nothing() {
    begin();
    assert_eq!(crate::embed::serve::task_tick(), 0);
    let task = launch_on(one_worker(&LONE_WORKER), spins_alone, 0);
    let mut failure = 0;
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    assert_eq!(ALONE_MADE_WAY.load(Ordering::SeqCst), 0, "nothing else wanted its thread");
    finish();
}

static COST_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static COST: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The same arithmetic loop as `spin`, a fixed number of turns, with ticks or without, giving back
/// the nanoseconds a turn took.
fn turns_cost(ticking: bool) -> f64 {
    let turns = 1_u64 << 28;
    let started = Instant::now();
    let mut x = 1_u64;
    for turn in 1..=turns {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        if ticking && turn.is_multiple_of(TICKS) {
            x ^= u64::try_from(crate::embed::serve::task_tick()).unwrap_or(0);
        }
    }
    std::hint::black_box(x);
    started.elapsed().as_secs_f64() * 1e9 / f64::from(u32::try_from(turns >> 8).unwrap_or(1)) / 256.0
}

extern "C" fn measures_ticks(_env: i64, _thread: i64) -> i64 {
    let mut lines = Vec::new();
    // The best of nine of each, taken in turn, against the machine's noise.
    let (mut plain, mut ticking) = (f64::MAX, f64::MAX);
    for _ in 0..9 {
        plain = plain.min(turns_cost(false));
        ticking = ticking.min(turns_cost(true));
    }
    lines.push(format!(
        "a turn: {plain:.3} ns plain, {ticking:.3} ns ticking every {TICKS}: {:+.2}%",
        (ticking / plain - 1.0) * 100.0
    ));
    // The tick itself, after its slice is up, with nothing else wanting the thread.
    let calls = 1_u32 << 22;
    let started = Instant::now();
    for _ in 0..calls {
        std::hint::black_box(crate::embed::serve::task_tick());
    }
    let each = started.elapsed().as_secs_f64() * 1e9 / f64::from(calls);
    lines.push(format!(
        "a tick: {each:.1} ns, so {:.2}% of {TICKS} turns of {plain:.3} ns",
        each / (f64::from(u32::try_from(TICKS).unwrap_or(1)) * plain) * 100.0
    ));
    COST.lock().unwrap_or_else(PoisonError::into_inner).extend(lines);
    ENDED
}

/// What ticking costs a loop whose turn is a few arithmetic instructions, in a task alone on its
/// worker: `cargo test --release -p fennec-rt --lib tick_s_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_the_tick_s_cost() {
    begin();
    let task = launch_on(one_worker(&COST_WORKER), measures_ticks, 0);
    let mut failure = 0;
    launch_end(task, -1, 0, &mut failure);
    for line in COST.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        eprintln!("{line}");
    }
    finish();
}

static TEMPS_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static FIRST_PUSHED: AtomicBool = AtomicBool::new(false);
static SECOND_PUSHED: AtomicBool = AtomicBool::new(false);
static FIRST_DONE: AtomicBool = AtomicBool::new(false);
static SECOND_READ: AtomicI64 = AtomicI64::new(-1);
static LEFT_FOR_NEXT: AtomicI64 = AtomicI64::new(-1);

/// Makes way for the other tasks on this thread until `ready` is set, a bounded number of times.
fn until(ready: &AtomicBool) {
    for _ in 0..100_000 {
        if ready.load(Ordering::SeqCst) {
            return;
        }
        crate::embed::serve::yield_task();
    }
}

/// Holds a temporary across a wait, then truncates to where it started, as native code does.
extern "C" fn first_holds_a_temporary(_env: i64, _thread: i64) -> i64 {
    let mark = crate::embed::temps::mark();
    let _ = with(|state| Some(state.push(crate::value::Value::Int(1))));
    FIRST_PUSHED.store(true, Ordering::SeqCst);
    until(&SECOND_PUSHED);
    crate::embed::temps::truncate(mark);
    FIRST_DONE.store(true, Ordering::SeqCst);
    ENDED
}

/// Holds its own temporary across the first task's truncating, and reads it after.
extern "C" fn second_holds_a_temporary(_env: i64, _thread: i64) -> i64 {
    until(&FIRST_PUSHED);
    let mark = crate::embed::temps::mark();
    let held = with(|state| Some(state.push(crate::value::Value::Int(2)))).unwrap_or(-1);
    SECOND_PUSHED.store(true, Ordering::SeqCst);
    until(&FIRST_DONE);
    let read = with(|state| match usize::try_from(held).ok().and_then(|at| state.temps.get(at)) {
        Some(crate::value::Value::Int(n)) => Some(*n),
        _ => Some(0),
    });
    SECOND_READ.store(read.unwrap_or(0), Ordering::SeqCst);
    // Leaves without truncating, as native code leaving by a failure may.
    let _ = mark;
    ENDED
}

/// What a task finds in the runtime's temporaries when it starts.
extern "C" fn counts_what_it_finds(_env: i64, _thread: i64) -> i64 {
    let found = with(|state| Some(i64::try_from(state.temps.len() + state.inputs.len()).unwrap_or(-1)));
    LEFT_FOR_NEXT.store(found.unwrap_or(-1), Ordering::SeqCst);
    ENDED
}

/// Tasks taking turns on one thread each keep their own temporaries: one truncating to where it
/// started never cuts off another's, and what a task left when it ended goes with it.
#[test]
fn tasks_taking_turns_on_a_thread_keep_their_own_temporaries() {
    begin();
    let worker = one_worker(&TEMPS_WORKER);
    let first = launch_on(worker, first_holds_a_temporary, 0);
    let second = launch_on(worker, second_holds_a_temporary, 0);
    let mut failure = 0;
    assert_eq!(launch_end(first, -1, 0, &mut failure), ENDED);
    assert_eq!(launch_end(second, -1, 0, &mut failure), ENDED);
    assert_eq!(SECOND_READ.load(Ordering::SeqCst), 2, "the second task's temporary was cut off");
    let next = launch_on(worker, counts_what_it_finds, 0);
    assert_eq!(launch_end(next, -1, 0, &mut failure), ENDED);
    assert_eq!(LEFT_FOR_NEXT.load(Ordering::SeqCst), 0, "an ended task's temporaries stayed behind");
    finish();
}

static ENV_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static SPARE_ON_WORKER: AtomicI64 = AtomicI64::new(-1);

/// Ends at once, with the copy of its environment let go of as it ends.
extern "C" fn ends_with_its_environment(_env: i64, _thread: i64) -> i64 {
    ENDED
}

/// What its worker's runtime keeps of freed blocks for reuse.
extern "C" fn reports_what_its_worker_keeps(_env: i64, _thread: i64) -> i64 {
    let kept = with(|state| Some(i64::try_from(state.heap.spare_words()).unwrap_or(-1)));
    SPARE_ON_WORKER.store(kept.unwrap_or(-1), Ordering::SeqCst);
    ENDED
}

/// A launched task's environment was made on the thread that launched it, so when the task ends its
/// worker gives it back to the allocator rather than keeping it for reuse. Kept, every launch would
/// add to what the worker holds and never reuses, and hold memory from the launching thread.
#[test]
fn a_launched_task_s_environment_goes_back_to_the_allocator_when_it_ends() {
    begin();
    let worker = one_worker(&ENV_WORKER);
    let env =
        with(|state| heap::text_address(state, "an environment long enough to take a block of its own"));
    let mut failure = 0;
    for _ in 0..500 {
        let task = launch_on(worker, ends_with_its_environment, env);
        assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    }
    let task = launch_on(worker, reports_what_its_worker_keeps, 0);
    assert_eq!(launch_end(task, -1, 0, &mut failure), ENDED);
    let kept = SPARE_ON_WORKER.load(Ordering::SeqCst);
    assert!((0..200).contains(&kept), "the worker kept {kept} words of what 500 launches were given");
    heap::free(env);
    finish();
}

static TLS_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static TLS_BESIDE_RAN: AtomicBool = AtomicBool::new(false);
static TLS_HANDLE: AtomicI64 = AtomicI64::new(0);
static TLS_READ: AtomicI64 = AtomicI64::new(-1);

/// Reads a line from a TLS connection whose server only answers once the task beside it has run.
extern "C" fn reads_a_tls_line(_env: i64, _thread: i64) -> i64 {
    let handle = TLS_HANDLE.load(Ordering::SeqCst);
    let read = crate::stdlib::net::real_connection(
        handle,
        "read_line",
        &[],
        Some(Duration::from_secs(10)),
        Span::default(),
    );
    TLS_READ.store(i64::from(matches!(read, Ok(crate::value::Value::Ok(_)))), Ordering::SeqCst);
    ENDED
}

extern "C" fn runs_beside(_env: i64, _thread: i64) -> i64 {
    TLS_BESIDE_RAN.store(true, Ordering::SeqCst);
    ENDED
}

/// A read waiting on a TLS connection parks its task, so its thread runs others meanwhile: the
/// server here only answers once the task beside it has run on the same thread.
#[test]
fn a_task_waiting_to_read_a_tls_connection_lets_others_run_on_its_thread() {
    begin();
    let worker = one_worker(&TLS_WORKER);
    TLS_HANDLE.store(crate::stdlib::net::tests::tls::connected_once_ready(&TLS_BESIDE_RAN), Ordering::SeqCst);
    let reading = launch_on(worker, reads_a_tls_line, 0);
    let beside = launch_on(worker, runs_beside, 0);
    let mut failure = 0;
    assert_eq!(launch_end(beside, -1, 0, &mut failure), ENDED);
    assert_eq!(launch_end(reading, -1, 0, &mut failure), ENDED);
    assert_eq!(TLS_READ.load(Ordering::SeqCst), 1, "the read didn't get its line");
    finish();
}

static WRITE_WORKER: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
static WRITE_BESIDE_DONE: AtomicBool = AtomicBool::new(false);
static WRITE_HANDLE: AtomicI64 = AtomicI64::new(0);
static WRITE_READ: AtomicI64 = AtomicI64::new(-1);
static WRITE_WROTE: AtomicI64 = AtomicI64::new(-1);

extern "C" fn reads_while_another_writes(_env: i64, _thread: i64) -> i64 {
    let handle = WRITE_HANDLE.load(Ordering::SeqCst);
    let read = crate::stdlib::net::real_connection(
        handle,
        "read_line",
        &[],
        Some(Duration::from_secs(10)),
        Span::default(),
    );
    WRITE_READ.store(i64::from(matches!(read, Ok(crate::value::Value::Ok(_)))), Ordering::SeqCst);
    ENDED
}

extern "C" fn writes_while_another_reads(_env: i64, _thread: i64) -> i64 {
    let handle = WRITE_HANDLE.load(Ordering::SeqCst);
    let wrote = crate::stdlib::net::real_connection(
        handle,
        "write",
        &[crate::value::Value::str("more\n")],
        None,
        Span::default(),
    );
    WRITE_WROTE.store(i64::from(matches!(wrote, Ok(crate::value::Value::Ok(_)))), Ordering::SeqCst);
    WRITE_BESIDE_DONE.store(true, Ordering::SeqCst);
    ENDED
}

/// One task writes to a TLS connection while another on its thread is parked reading it. The write
/// once waited on the lock the parked read held, blocking the thread the read needed to go on.
#[test]
fn a_task_can_write_a_tls_connection_another_is_reading() {
    let worker = one_worker(&WRITE_WORKER);
    let handle = crate::stdlib::net::tests::tls::connected_once_ready(&WRITE_BESIDE_DONE);
    WRITE_HANDLE.store(handle, Ordering::SeqCst);
    let (done, ended) = std::sync::mpsc::channel();
    // On a thread of its own, so a test that hangs fails rather than waits forever.
    let running = std::thread::spawn(move || {
        begin();
        let reading = launch_on(worker, reads_while_another_writes, 0);
        let writing = launch_on(worker, writes_while_another_reads, 0);
        let mut failure = 0;
        let both = (launch_end(writing, -1, 0, &mut failure), launch_end(reading, -1, 0, &mut failure));
        finish();
        let _ = done.send(both);
    });
    let both = ended.recv_timeout(Duration::from_secs(30));
    assert_eq!(both, Ok((ENDED, ENDED)), "the tasks didn't end: the write waited on the read");
    assert_eq!(WRITE_WROTE.load(Ordering::SeqCst), 1, "the write failed");
    assert_eq!(WRITE_READ.load(Ordering::SeqCst), 1, "the read didn't get its line");
    drop(running.join());
}

mod spreads;
