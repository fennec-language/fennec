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
/// worker: `cargo test --release -p fennec-interp --lib tick_s_cost -- --ignored --nocapture`.
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
