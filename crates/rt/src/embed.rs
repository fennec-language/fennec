//! The runtime that compiled native code calls into. Native code keeps numbers and bools in
//! registers, and strings, lists, dicts and structs in blocks it reads and changes itself (see
//! `heap`). What it hands the runtime to format, compare or pass to the standard library becomes a
//! `Value` here, which native code refers to by a handle: a plain number.
//!
//! Handles index a stack of temporaries. A function's temporaries are dropped when it
//! returns, and a statement's when it finishes, so the stack stays small.
//!
//! Every function here takes and returns plain numbers. A failure is recorded, not
//! returned: native code checks the failure flag after any call that can fail.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span, Trace};

pub mod aot;
pub mod bytes;
pub mod capabilities;
pub mod channels;
pub mod checks;
pub mod cleanup;
pub mod codec;
pub mod dicts;
pub mod guarded;
pub mod heap;
pub mod layout;
pub mod lists;
#[cfg(any(test, feature = "threads"))]
pub(crate) mod live;
pub mod notes;
pub mod numbers;
pub mod objects;
pub mod parallel;
pub(crate) use parallel::workers_wanted;
mod plans;
pub mod regex;
#[cfg(any(test, feature = "threads"))]
pub mod serve;

/// Without the `threads` feature, a program has no tasks, so a server takes its connections one
/// at a time, as looping over it does.
#[cfg(not(any(test, feature = "threads")))]
pub mod serve {
    use fennec_syntax::{Diagnostic, Span};

    use crate::value::Value;

    /// What runs each served connection (see the `threads` version).
    pub type ConnBody = extern "C" fn(env: i64, conn: i64, thread: i64) -> i64;

    thread_local! {
        /// The failure's value a round handed over, then the one the loop passed up.
        static FAILED_WITH: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
    }

    /// Each round in turn: 1 is a crash, ending only itself; 2 a failure, passing up out of the
    /// loop; 3 a `break`.
    /// A server told to `stop_on` signals takes no more once one comes: the connection being
    /// served finishes, and the loop ends.
    pub extern "C" fn net_serve(server: i64, body: ConnBody, env: i64) -> i64 {
        super::write_kept_output();
        let draining = crate::stdlib::net::drain_on_stop(server);
        let stopped = || draining.is_some() && crate::signals::stopping();
        let mut how = 0;
        while !stopped() {
            let Ok(conn) = crate::stdlib::net::accepted(server) else { break };
            // What woke the `accept` may be the stop's own connection.
            if stopped() {
                crate::stdlib::net::closed(conn);
                break;
            }
            crate::stdlib::net::serving(Some(conn));
            let round = body(env, conn, 0);
            crate::stdlib::net::serving(None);
            super::write_kept_output();
            crate::stdlib::net::closed(conn);
            match round {
                0 => {}
                2 => {
                    how = 2;
                    break;
                }
                3 => break,
                _ => super::report_failure(),
            }
        }
        if let Some(listening) = draining {
            crate::signals::forget(listening);
        }
        how
    }

    /// Without tasks, nothing makes way.
    pub extern "C" fn task_tick() -> i64 {
        0
    }

    pub extern "C" fn net_serve_failure() -> i64 {
        FAILED_WITH.take()
    }

    /// A launched task's function (see the `threads` version).
    pub type TaskBody = extern "C" fn(env: i64, thread: i64) -> i64;

    /// `with launch(...)` needs tasks, so threads.
    pub extern "C" fn launch(_body: TaskBody, _env: i64) -> i64 {
        super::fail_with(Diagnostic::new("`launch` needs a program built with threads", Span::default()));
        0
    }

    pub extern "C" fn launch_end(_task: i64, _stop_after_ns: i64, _failed: i64, _failure: &mut i64) -> i64 {
        0
    }

    /// Only a server's round or a `parallel` call's range, as there are no tasks.
    pub extern "C" fn task_failed(failure: i64) {
        FAILED_WITH.set(failure);
    }

    /// The failure's value a range handed over, which it hasn't passed up yet.
    pub(in crate::embed) fn take_failed_with() -> Option<u64> {
        u64::try_from(FAILED_WITH.take()).ok().filter(|failure| *failure != 0)
    }

    pub(super) fn served_call(
        _handle: i64,
        _method: &str,
        _args: &[Value],
        _timeout: Option<std::time::Duration>,
        _at: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        None
    }
}

pub mod strings;
mod table;
mod tests;
pub mod text;
pub mod threads;

use std::collections::HashMap;

use crate::format;
use crate::value::{Capability, Value};

pub use heap::{Counts, count_allocations};
pub use tests::next_test;

/// What the compiler registers before a program runs: the names, text and layouts its native
/// code refers to by number.
#[derive(Default)]
pub struct Tables {
    pub names: Vec<String>,
    pub strings: Vec<String>,
    /// The kinds of items in the lists native code packs.
    pub elements: Vec<Rc<crate::packed::Element>>,
    /// The shapes of boxed values' blocks, by their number.
    pub shapes: Vec<Rc<crate::packed::Element>>,
    /// The numbers of the shapes of `shared` values, which are the same only if they're one
    /// value.
    pub cells: Vec<u64>,
    /// The kinds of block a dict's values can be, by the number `dicts::Word::Block` has.
    pub blocks: Vec<Rc<crate::packed::Element>>,
    /// The shape number strings' blocks have.
    pub str_shape: u64,
    /// Blocks that live for the whole run, like string literals, made while compiling so
    /// native code can refer to them by address.
    pub statics: Vec<Box<[u64]>>,
    /// The addresses of the one-character strings of ASCII, by character, once made.
    pub ascii: Box<[u64]>,
    /// The address of each string literal's block, by its text.
    pub literals: HashMap<String, i64>,
    /// What closes what's open in a generator that's let go partway, by its block's shape.
    pub abandons: HashMap<u64, extern "C" fn(i64)>,
    /// Whether native code holds a `str` as its 24-byte header (see `text`), so a list of
    /// strings the runtime makes holds headers.
    pub text_headers: bool,
}

impl Tables {
    /// The tables an executable carries: everything but the blocks made while compiling,
    /// which it makes again when it starts.
    #[must_use]
    pub fn clone_for_image(&self) -> Tables {
        Tables {
            names: self.names.clone(),
            strings: self.strings.clone(),
            elements: self.elements.clone(),
            shapes: self.shapes.clone(),
            cells: self.cells.clone(),
            str_shape: self.str_shape,
            ..Tables::default()
        }
    }
}

/// Collects what the program prints.
#[derive(Clone, Default)]
struct Output(Rc<RefCell<Vec<u8>>>, bool);

/// Whether the program writes its output as it goes, rather than keeping it for a host.
static PROGRAM_WRITES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// Whether what's running here is a launched task of a program that writes as it goes. A
    /// `parallel` range's output is kept to come out whole, but tasks talk to each other, and what
    /// one printed before it told another comes first.
    static TASK_WRITES: Cell<bool> = const { Cell::new(false) };
}

/// Whether a launched task's printing is written as it goes.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn program_writes() -> bool {
    PROGRAM_WRITES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Says whether what's about to run here writes as it goes, giving back what was.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn swap_task_writes(writes: bool) -> bool {
    TASK_WRITES.replace(writes)
}

/// How much output is kept before a live run writes it.
const LIVE_OUTPUT: usize = 1 << 13;

impl Output {
    /// Writes the first `length` bytes kept, in one go, so nothing another thread writes lands
    /// among them.
    fn write_out(&self, length: usize) -> std::io::Result<()> {
        let mut kept = self.0.borrow_mut();
        write_whole(kept.get(..length).unwrap_or_default())?;
        kept.drain(..length);
        Ok(())
    }
}

/// Where a live run's output goes: the process's standard output, one call at a time.
#[cfg(not(test))]
fn write_whole(bytes: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}

/// What live runs have written, in tests, in the order they wrote it.
#[cfg(test)]
static WRITTEN: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
#[expect(clippy::unnecessary_wraps, reason = "it stands for the real one, which can fail")]
fn write_whole(bytes: &[u8]) -> std::io::Result<()> {
    WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(bytes);
    Ok(())
}

impl Write for Output {
    /// Keeps what's printed, and once a live run has kept enough, writes it up to the last whole
    /// line, as one print comes here in pieces and threads share the output.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        if self.1 && self.0.borrow().len() >= LIVE_OUTPUT {
            let lines = self.0.borrow().iter().rposition(|byte| *byte == b'\n');
            if let Some(last) = lines {
                self.write_out(last + 1)?;
            }
        }
        Ok(bytes.len())
    }

    /// Writes what a live run has kept. Any other run's output stays for its host.
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.1 && !TASK_WRITES.get() {
            return Ok(());
        }
        let length = self.0.borrow().len();
        // Nothing kept, as after most of a `parallel` call's ranges: standard output, which every
        // thread shares, isn't touched.
        if length == 0 {
            return Ok(());
        }
        self.write_out(length)
    }
}

thread_local! {
    /// This thread's runtime's output, for writing it before the thread waits, whatever else of
    /// the runtime is borrowed then.
    static OUTPUT: RefCell<Option<Output>> = const { RefCell::new(None) };
}

/// Lets others run for a moment, for a replay's task waiting its turn: a task makes way for the
/// others on its thread, and a thread that isn't one sleeps briefly.
pub(crate) fn make_way() {
    #[cfg(any(test, feature = "threads"))]
    if serve::can_park() && serve::yield_task_always() {
        return;
    }
    std::thread::sleep(std::time::Duration::from_micros(50));
}

/// A parked task's own temporaries and inputs, kept apart from the thread's while it's parked.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
pub(in crate::embed) struct Temporaries {
    temps: Vec<Value>,
    inputs: Vec<Value>,
    /// Which task it is, in a recording or a replay: a server's round, named by its connection.
    replay: Option<crate::replay::Task>,
    /// For a server's round, what its server lent, when `round` is set.
    lent: Option<std::sync::Arc<table::Table>>,
    round: bool,
    /// Its failure, if it parked with one: it's the task's, not whatever its thread runs next.
    failure: Option<Diagnostic>,
    failed: u8,
}

#[cfg(any(test, feature = "threads"))]
impl Temporaries {
    pub(in crate::embed) fn is_empty(&self) -> bool {
        self.temps.is_empty()
            && self.inputs.is_empty()
            && self.replay.is_none()
            && self.failure.is_none()
            && self.failed == 0
            && !self.round
    }

    /// A server's round's own: named in a recording or a replay, and reading what its server lent.
    pub(in crate::embed) fn of_round(
        named: Option<crate::replay::Task>,
        lent: Option<std::sync::Arc<table::Table>>,
    ) -> Temporaries {
        Temporaries { replay: named, lent, round: true, ..Temporaries::default() }
    }

    /// A task's own, named `task` in a recording or a replay.
    pub(in crate::embed) fn of_task(task: crate::replay::Task) -> Temporaries {
        Temporaries { replay: Some(task), ..Temporaries::default() }
    }
}

/// Swaps the runtime's temporaries and inputs on this thread with `own`: once as a task is resumed,
/// and again as it parks or ends.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn swap_temporaries(own: &mut Temporaries) {
    STATE.with_borrow_mut(|state| {
        if let Some(state) = state.as_mut() {
            std::mem::swap(&mut state.temps, &mut own.temps);
            std::mem::swap(&mut state.inputs, &mut own.inputs);
            std::mem::swap(&mut state.failure, &mut own.failure);
            if own.round {
                let lent = &mut own.lent;
                let same = match (&state.heap.lent, &*lent) {
                    (Some(here), Some(there)) => std::sync::Arc::ptr_eq(here, there),
                    (None, None) => true,
                    _ => false,
                };
                std::mem::swap(&mut state.heap.lent, lent);
                // What the runtime kept of the lent strings goes when the lent blocks change.
                if !same {
                    state.heap.texts.get_mut().clear();
                    state.heap.set_cursor(heap::Cursor::default());
                }
            }
        }
    });
    own.failed = FLAGS.with(|flags| flags.failed.replace(own.failed));
    crate::replay::swap(&mut own.replay);
}

/// Puts a task's failure back in its thread's runtime, as it ends with one: what's run it says what
/// happens next.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn take_failure_of(own: &mut Temporaries) {
    if let Some(failure) = own.failure.take() {
        with(|state| state.failure = Some(failure));
    }
    if own.failed != 0 {
        set_failed(std::mem::take(&mut own.failed));
    }
}

/// Writes what this thread has printed and kept. Output comes out as it's printed, and is only
/// kept to save writing it line by line, so this is called before anything that can wait, and
/// before anything else on another thread is to come after it. A failure to write stays for the
/// next print to report, as there's nothing here to pass it to.
pub(crate) fn write_kept_output() {
    OUTPUT.with_borrow_mut(|output| {
        if let Some(output) = output {
            let _ = output.flush();
        }
    });
}

/// Has a panic write what the panicking thread printed and kept before it takes the program down:
/// printing just before a crash is how people find one. Only that thread's can be reached. What
/// other threads have kept goes with the program, as all of it does on a signal.
fn write_kept_output_on_panic() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            // A panic while the output's in use leaves it for now: it can't be read here.
            let free = OUTPUT
                .try_with(|output| {
                    output
                        .try_borrow()
                        .is_ok_and(|kept| kept.as_ref().is_none_or(|kept| kept.0.try_borrow_mut().is_ok()))
                })
                .unwrap_or(false);
            if free {
                write_kept_output();
            }
            earlier(panic);
        }));
    });
}

struct State {
    output: Output,
    regexes: crate::stdlib::re::Regexes,
    tables: Tables,
    temps: Vec<Value>,
    /// The inputs of the next expression or statement native code hands over.
    inputs: Vec<Value>,
    /// Each name's number in the name table, for telling types apart.
    #[cfg(any(test, feature = "threads"))]
    name_ids: HashMap<String, i64>,
    heap: heap::Heap,
    /// The block of each plain capability, by its kind, once it's been asked for.
    capabilities: Vec<i64>,
    /// A test's fake capabilities, while one that takes them runs.
    world: Option<crate::fakes::TestWorld>,
    /// The program's name, as `args.program`.
    program: String,
    /// What it was given on its command line, as `args`.
    arguments: Vec<String>,
    failure: Option<Diagnostic>,
    /// For each of the program's shapes, whether a plain key of it is hashed by what it holds,
    /// as its floats and blocks, rather than by its words: what a dict asks of every key.
    keys_by_value: Box<[bool]>,
    /// The calls the failure has left so far on its way out, innermost first: where each called
    /// what it came out of. Started again by each new failure.
    trace: Trace,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static FLAGS: Flags =
        const { Flags { failed: Cell::new(0), lock_held: Cell::new(0), replaying: Cell::new(0) } };
}

/// What native code reads and writes on this thread without a call, through the one address.
#[repr(C)]
struct Flags {
    /// Set when the program fails. Native code reads it directly after each call that can
    /// fail, and returns early when it's set.
    failed: Cell<u8>,
    /// The word of the `shared` value's lock this thread holds, if any, at 8 bytes in. Others held
    /// beside it are in `threads`.
    lock_held: Cell<u64>,
    /// 1 while this run is recorded or replayed, at 16 bytes in, and 0 otherwise: native code
    /// taking a `shared` value's lock reads it, and calls `fennec_replay_lock` and
    /// `fennec_replay_locked` only when it's 1, so a normal run pays one load.
    replaying: Cell<u64>,
}

fn set_failed(failed: u8) {
    FLAGS.with(|flags| flags.failed.set(failed));
}

/// Whether native code says this thread holds a `shared` value's lock.
fn lock_word_held() -> bool {
    FLAGS.with(|flags| flags.lock_held.get() != 0)
}

/// Where this thread's failure flag lives, for native code to read. Code compiled for one
/// thread must run on that thread.
pub(crate) fn failed() -> bool {
    FLAGS.with(|flags| flags.failed.get() != 0)
}

/// Where the failure flag is. Native code asks on each thread before it reads it, so this is
/// where the thread's word saying whether the run is recorded or replayed is set.
pub fn failed_flag() -> usize {
    FLAGS.with(|flags| {
        flags.replaying.set(u64::from(crate::replay::active()));
        std::ptr::from_ref(flags).addr()
    })
}

/// The capability native code means by `kind`: 0 for `Stdout`, 1 for `Fs`, 2 for `Clock`,
/// 3 for `Args`, 4 for `Stdin`, 5 for `Process`, 6 for `Entropy`, 7 for `Net`, 8 for `Env`, 9 for
/// `Signals` and 10 for `Stderr`.
fn capability(kind: i64) -> Capability {
    match kind {
        0 => Capability::Stdout,
        1 => Capability::Fs,
        3 => Capability::Args,
        4 => Capability::Stdin,
        5 => Capability::Process,
        6 => Capability::Entropy,
        7 => Capability::Net,
        8 => Capability::Env,
        9 => Capability::Signals,
        10 => Capability::Stderr,
        _ => Capability::Clock,
    }
}

/// Sets up the runtime to run a program with `tables` and `inputs`, replacing any earlier run's.
pub fn start(mut tables: Tables, inputs: crate::Inputs) {
    let crate::Inputs { program, arguments, live, .. } = inputs;
    let output = Output(Rc::default(), live);
    if live {
        PROGRAM_WRITES.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    OUTPUT.set(Some(output.clone()));
    #[cfg(any(test, feature = "threads"))]
    let name_ids = tables.names.iter().zip(0_i64..).map(|(name, id)| (name.clone(), id)).collect();
    set_failed(0);
    let keys_by_value = tables.shapes.iter().map(|shape| dicts::by_what_it_holds(shape)).collect();
    STATE.set(Some(State {
        output,
        regexes: crate::stdlib::re::Regexes::default(),
        temps: Vec::new(),
        inputs: Vec::new(),
        #[cfg(any(test, feature = "threads"))]
        name_ids,
        heap: heap::Heap::new(std::mem::take(&mut tables.statics)).planned(&tables.shapes, &tables.elements),
        capabilities: Vec::new(),
        world: None,
        program,
        arguments,
        tables,
        failure: None,
        keys_by_value,
        trace: Trace::default(),
    }));
}

/// What a run left in the runtime. A run that succeeds leaves nothing: a check on the
/// bookkeeping native code does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leftovers {
    pub temps: usize,
    /// Boxed values' blocks no one released.
    pub boxes: usize,
    /// What the run allocated and copied.
    pub counts: Counts,
}

/// Ends the run: what it printed, why it failed if it did, and what it left behind.
/// The exit code `main` gave back, as an int or in an `Ok`: the process exits with it, once
/// nothing failed.
static EXIT_CODE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Keeps the int `main` gave back as the program's exit code.
pub fn main_exit(code: i64) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
}

/// The exit code `main` gave back, 0 if it gave none, as the process gets it: its lowest 8 bits,
/// as Unix takes them, on every system. Taking it starts the next run at 0.
pub fn take_exit_code() -> u8 {
    let code = EXIT_CODE.swap(0, std::sync::atomic::Ordering::Relaxed);
    u8::try_from(code & 0xff).unwrap_or(0)
}

pub fn finish() -> (Vec<u8>, Option<Diagnostic>, Leftovers) {
    finish_run(false)
}

/// `finish`, for a process about to exit: the runtime's state, its heap and every block in it,
/// is left for the exit to give back. Freeing them one at a time was most of what an executable
/// did after its `main`.
pub(crate) fn finish_for_exit() -> (Vec<u8>, Option<Diagnostic>, Leftovers) {
    finish_run(true)
}

fn finish_run(leave: bool) -> (Vec<u8>, Option<Diagnostic>, Leftovers) {
    heap::run_abandons();
    let state = STATE.take();
    set_failed(0);
    match state {
        Some(mut state) => {
            state.temps.clear();
            state.let_go();
            aot::report_sites(&state.heap.sites);
            let leftovers =
                Leftovers { temps: state.temps.len(), boxes: state.heap.live(), counts: state.heap.counts };
            let finished = (state.output.0.take(), state.failure.take(), leftovers);
            LAST_TRACE.set(std::mem::take(&mut state.trace));
            if leave {
                // Kept until the process ends, which gives back all of it at once.
                Box::leak(Box::new(state));
            }
            finished
        }
        None => (Vec::new(), None, Leftovers::default()),
    }
}

/// Records a failure from compiled code's own checks, such as an overflow.
pub fn fail_with(diagnostic: Diagnostic) {
    with(|state| {
        if state.failure.is_none() {
            state.failure = Some(diagnostic);
            state.trace = Trace::default();
        }
    });
    set_failed(1);
}

/// `fennec_trace_frame(start, end)`: a failure leaves the call at this span on its way out, which
/// goes in its call chain. Native code only says so on that way out, so it costs nothing else.
pub extern "C" fn trace_frame(start: i64, end: i64) {
    let span = Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) };
    with(|state| {
        if state.failure.is_some() {
            state.trace.push(span, 1);
        }
    });
}

thread_local! {
    /// The call chain of the failure the last run on this thread ended with.
    static LAST_TRACE: RefCell<Trace> = RefCell::new(Trace::default());
}

/// Keeps a failure's call chain for [`take_trace`].
pub(crate) fn keep_trace(trace: Trace) {
    LAST_TRACE.set(trace);
}

/// The call chain of the failure the last run on this thread ended with, innermost first.
pub(crate) fn take_trace() -> Trace {
    LAST_TRACE.take()
}

/// Sets the call chain of the failure this thread has now.
pub(crate) fn set_trace(trace: Trace) {
    with(|state| state.trace = trace);
}

/// The failure this thread has now and its call chain, taken aside, for a crash's cleanup.
pub(crate) fn take_failure() -> (Option<Diagnostic>, Trace) {
    with(|state| (state.failure.take(), std::mem::take(&mut state.trace)))
}

/// Puts a failure taken aside back, with its call chain.
pub(crate) fn put_failure((failure, trace): (Option<Diagnostic>, Trace)) {
    with(|state| {
        state.failure = failure;
        state.trace = trace;
    });
}

/// Whether this thread's runtime state is borrowed, as it is all through a call into the
/// runtime: a task mustn't park then, or the next task on the thread would
/// find it taken.
#[cfg(any(test, feature = "threads"))]
fn state_is_borrowed() -> bool {
    STATE.with(|state| state.try_borrow_mut().is_err())
}

/// Says what this thread's runtime holds as a failure and forgets it, as a server does for a
/// connection's task that failed, which ends only that task.
/// Forgets this thread's failure, for a task whose failure is no one's to report.
#[cfg(any(test, feature = "threads"))]
fn clear_failure() {
    with(|state| state.failure = None);
    set_failed(0);
}

fn report_failure() {
    let failure = with(|state| state.failure.take());
    set_failed(0);
    if let Some(failure) = failure {
        write_kept_output();
        let _ = writeln!(std::io::stderr(), "error: {}", failure.message);
    }
}

fn with<T: Default>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with_borrow_mut(|state| state.as_mut().map(f)).unwrap_or_default()
}

/// Runs an operation that can fail, recording the failure for native code to notice.
fn attempt<T: Default>(f: impl FnOnce(&mut State) -> Result<T, Diagnostic>) -> T {
    let result = STATE.with_borrow_mut(|state| state.as_mut().map(f));
    match result {
        Some(Ok(value)) => value,
        Some(Err(diagnostic)) => {
            fail_with(diagnostic);
            T::default()
        }
        None => T::default(),
    }
}

fn span(start: i64, end: i64) -> Span {
    Span { start: u32::try_from(start).unwrap_or(0), end: u32::try_from(end).unwrap_or(0) }
}

fn handle(at: usize) -> i64 {
    i64::try_from(at).unwrap_or(-1)
}

impl State {
    fn push(&mut self, value: Value) -> i64 {
        self.temps.push(value);
        handle(self.temps.len() - 1)
    }

    /// A copy of a temporary, as a `Value`.
    fn temp(&self, at: i64) -> Value {
        self.expanded(self.raw(at)).unpacked()
    }

    /// A copy of a temporary as it is, for what doesn't need to look inside it.
    fn raw(&self, at: i64) -> Value {
        usize::try_from(at).ok().and_then(|at| self.temps.get(at)).cloned().unwrap_or(Value::Unit)
    }

    /// A value with any native value at its top turned into a `Value`.
    fn expanded(&self, value: Value) -> Value {
        match value {
            Value::Lent(lent) => heap::read_block(self, lent.address),
            Value::Ok(inner) if matches!(*inner, Value::Lent(_)) => {
                Value::Ok(Rc::new(self.expanded(inner.as_ref().clone()).unpacked()))
            }
            other => other,
        }
    }

    /// Lets go of the blocks whose last lent copy has been dropped.
    fn let_go(&mut self) {
        loop {
            let blocks = crate::value::blocks_let_go();
            if blocks.is_empty() {
                return;
            }
            for block in blocks {
                heap::release(self, block);
            }
        }
    }

    /// Moves a temporary out, leaving nothing behind, so changing it can't copy it. It comes
    /// back unpacked.
    fn take(&mut self, at: i64) -> Value {
        let value = self.take_raw(at);
        self.expanded(value).unpacked()
    }

    /// Moves a temporary out as it is, packed or not, for a name to hold.
    fn take_raw(&mut self, at: i64) -> Value {
        usize::try_from(at)
            .ok()
            .and_then(|at| self.temps.get_mut(at))
            .map_or(Value::Unit, |slot| std::mem::replace(slot, Value::Unit))
    }
}

/// Temporaries: making them, and dropping them when they're done with.
pub mod temps {
    use super::{State, Value, attempt, span, with};
    use fennec_syntax::Diagnostic;

    /// How many temporaries there are, to drop back to later.
    pub extern "C" fn mark() -> i64 {
        with(|state| super::handle(state.temps.len()))
    }

    pub extern "C" fn truncate(mark: i64) {
        with(|state| {
            state.temps.truncate(usize::try_from(mark).unwrap_or(0));
            state.let_go();
        });
    }

    /// Drops a returning function's temporaries except the value it returns.
    pub extern "C" fn keep(mark: i64, value: i64) -> i64 {
        with(|state| {
            let value = if value >= mark { state.take_raw(value) } else { state.raw(value) };
            state.temps.truncate(usize::try_from(mark).unwrap_or(0));
            state.let_go();
            state.push(value)
        })
    }

    pub extern "C" fn int(n: i64) -> i64 {
        with(|state| state.push(Value::Int(n)))
    }

    pub extern "C" fn float(x: f64) -> i64 {
        with(|state| state.push(Value::Float(x)))
    }

    pub extern "C" fn bool(b: i8) -> i64 {
        with(|state| state.push(Value::Bool(b != 0)))
    }

    /// A capability as a value, for code that passes one on, like `args.len()`: a block or a
    /// kind, as for `declare_capability`.
    pub extern "C" fn capability(kind: i64) -> i64 {
        with(|state| {
            let capability = super::capabilities::value_of(state, kind);
            state.push(capability)
        })
    }

    /// Nothing: what a function without a value gives back.
    pub extern "C" fn unit() -> i64 {
        with(|state| state.push(Value::Unit))
    }

    pub extern "C" fn none() -> i64 {
        with(|state| state.push(Value::None))
    }

    pub extern "C" fn str(index: i64) -> i64 {
        with(|state| {
            let text = usize::try_from(index).ok().and_then(|at| state.tables.strings.get(at)).cloned();
            state.push(Value::str(text.as_deref().unwrap_or_default()))
        })
    }

    fn mismatch(value: &Value, wanted: &str, start: i64, end: i64) -> Diagnostic {
        Diagnostic::new(
            format!("expected {wanted} here, but this is a {}", value.type_name()),
            span(start, end),
        )
    }

    pub extern "C" fn to_int(value: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| match state.temp(value) {
            Value::Int(n) => Ok(n),
            other => Err(mismatch(&other, "an int", start, end)),
        })
    }

    pub extern "C" fn to_float(value: i64, start: i64, end: i64) -> f64 {
        attempt(|state: &mut State| match state.temp(value) {
            Value::Float(x) => Ok(x),
            other => Err(mismatch(&other, "a float", start, end)),
        })
    }

    pub extern "C" fn to_bool(value: i64, start: i64, end: i64) -> i8 {
        attempt(|state: &mut State| match state.temp(value) {
            Value::Bool(b) => Ok(i8::from(b)),
            other => Err(mismatch(&other, "a bool", start, end)),
        })
    }

    /// `fennec_main_exit(code)`: keeps the int `main` gave back as the program's exit code.
    pub extern "C" fn main_exit(code: i64) {
        super::main_exit(code);
    }

    /// Fails the program with `main`'s failure when it gives back one, and keeps the int it
    /// gives back otherwise as the exit code. The result is the last temporary, so it's
    /// dropped, leaving none behind.
    pub extern "C" fn main_result(value: i64, start: i64, end: i64) {
        attempt(|state: &mut State| {
            let result = state.take(value);
            if let Ok(at) = usize::try_from(value) {
                state.temps.truncate(at);
            }
            match result {
                Value::Err(error) => Err(Diagnostic::new(
                    format!("`main` failed: {}", crate::format::failure(&error)),
                    span(start, end),
                )),
                Value::Ok(given) => {
                    if let Value::Int(code) = given.as_ref() {
                        super::main_exit(*code);
                    }
                    Ok(())
                }
                Value::Int(code) => {
                    super::main_exit(code);
                    Ok(())
                }
                _ => Ok(()),
            }
        });
    }

    /// Whether a value is a failed result.
    pub extern "C" fn is_err(value: i64) -> i8 {
        with(|state| i8::from(state.raw(value).is_failure()))
    }

    pub extern "C" fn is_none(value: i64) -> i8 {
        with(|state| i8::from(matches!(state.raw(value), Value::None)))
    }

    /// The value inside a successful result, or the value itself when it isn't a result.
    /// The `at`th of `count` names a tuple is unpacked into. Fails when the value can't be
    /// unpacked into that many names.
    pub extern "C" fn unpack(value: i64, at: i64, count: i64, for_loop: i8, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let value = state.temp(value);
            let Value::Tuple(items) = &value else {
                let into = if for_loop != 0 { format!(" into {count} names") } else { String::new() };
                return Err(Diagnostic::new(
                    format!("can't unpack a {}{into}", value.type_name()),
                    span(start, end),
                ));
            };
            if super::handle(items.len()) != count {
                return Err(Diagnostic::new(
                    format!("can't unpack {} values into {count} names", items.len()),
                    span(start, end),
                ));
            }
            let item = usize::try_from(at).ok().and_then(|at| items.get(at)).cloned().unwrap_or(Value::Unit);
            Ok(state.push(item))
        })
    }

    /// A copy of a temporary. Copies share memory until one changes.
    pub extern "C" fn copy(value: i64) -> i64 {
        with(|state| {
            let value = state.raw(value);
            state.push(value)
        })
    }

    pub extern "C" fn unwrap(value: i64) -> i64 {
        with(|state| {
            let inner = match state.raw(value) {
                Value::Ok(inner) => inner.as_ref().clone(),
                other => other,
            };
            state.push(inner)
        })
    }

    /// The error inside a failed result, as a new temporary, or the value itself when it
    /// isn't one: `unwrap`'s other side.
    pub extern "C" fn error_of(value: i64) -> i64 {
        with(|state| {
            let inner = match state.raw(value) {
                Value::Err(inner) => inner.as_ref().clone(),
                other => other,
            };
            state.push(inner)
        })
    }

    /// The failed result `fail error` gives back.
    pub extern "C" fn err(error: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let error = state.take(error);
            super::check_error(&error, span(start, end))?;
            Ok(state.push(Value::Err(std::rc::Rc::new(error))))
        })
    }

    /// Whether `?` passes a value up: a failed result, or None.
    pub extern "C" fn passes(value: i64) -> i8 {
        with(|state| i8::from(matches!(state.raw(value), Value::Err(_) | Value::None)))
    }

    /// `value? from wrapper`: the wrapper error, caused by the one passing up.
    pub extern "C" fn with_cause(wrapper: i64, failed: i64) -> i64 {
        with(|state| {
            let mut wrapper = state.take(wrapper);
            if let (Value::Instance(instance), Value::Err(cause)) = (&mut wrapper, state.temp(failed)) {
                std::rc::Rc::make_mut(instance).cause = Some(cause.as_ref().clone());
            }
            state.push(Value::Err(std::rc::Rc::new(wrapper)))
        })
    }

    /// A successful result holding a value, unless it already is a result.
    pub extern "C" fn ok(value: i64) -> i64 {
        with(|state| {
            let wrapped = match state.take_raw(value) {
                result @ (Value::Ok(_) | Value::Err(_)) => result,
                other => Value::Ok(std::rc::Rc::new(other)),
            };
            state.push(wrapped)
        })
    }
}

/// Inputs native code hands over, failures at a call into the standard library, and a `match`
/// that no case matches.
pub mod eval {
    use super::{State, attempt, span, with};
    use fennec_syntax::Diagnostic;

    /// Reports a failure inside the standard library where the program called it.
    pub extern "C" fn at_call(start: i64, end: i64) {
        with(|state| {
            if let Some(failure) = state.failure.take() {
                state.failure = Some(super::at_failed_call(failure, span(start, end)));
            }
        });
    }

    /// `fennec_trace_frame`, where native code calls it from.
    pub extern "C" fn trace_frame(start: i64, end: i64) {
        super::trace_frame(start, end);
    }

    /// The hidden name native code passes its `index`th input under.
    pub fn input_name(index: usize) -> String {
        format!("\0in{index}")
    }

    /// Hands a temporary to the next expression or statement as its next input.
    pub extern "C" fn input(value: i64) {
        with(|state| {
            let value = state.take(value);
            state.inputs.push(value);
        });
    }

    /// Fails because no case of a `match` matches its value.
    pub extern "C" fn no_match(value: i64, start: i64, end: i64) {
        attempt(|state: &mut State| -> Result<(), Diagnostic> {
            let value = state.temp(value);
            Err(Diagnostic::new(format!("no case matches {}", crate::format::repr(&value)), span(start, end))
                .with_help("add a case for it, or `case _:` for everything else"))
        });
    }
}

/// Writes what `parallel`'s ranges printed on workers, on this thread.
fn print_handed_over(printed: &[u8]) {
    if printed.is_empty() {
        return;
    }
    attempt(|state: &mut State| io::write(state, &String::from_utf8_lossy(printed)));
}

/// Printing, and the loops native code runs over `Value`s.
pub mod io {
    use super::{Span, State, Value, attempt, format, with};
    use fennec_syntax::Diagnostic;
    use std::io::Write;

    pub(super) fn write(state: &mut State, text: &str) -> Result<(), Diagnostic> {
        // A test's `Stdout` keeps what it printed, for `stdout.captured()`, and a fake one prints
        // nothing more.
        if let Some(world) = state.world.as_mut() {
            world.print(text, "");
            if !world.real() {
                return Ok(());
            }
        }
        state
            .output
            .write_all(text.as_bytes())
            .map_err(|err| Diagnostic::new(format!("couldn't write output: {err}"), Span::default()))
    }

    pub extern "C" fn print_int(n: i64) {
        attempt(|state: &mut State| write(state, &n.to_string()));
    }

    pub extern "C" fn print_float(x: f64) {
        attempt(|state: &mut State| write(state, &format::float(x)));
    }

    /// `print` of a `u64`, its bits in `n`: unsigned.
    pub extern "C" fn print_u64(n: i64) {
        attempt(|state: &mut State| write(state, &n.cast_unsigned().to_string()));
    }

    /// `print` of an `f32`, held as the `f64` that's exactly it.
    pub extern "C" fn print_f32(x: f64) {
        attempt(|state: &mut State| write(state, &format::float32(x)));
    }

    pub extern "C" fn print_bool(b: i8) {
        attempt(|state: &mut State| write(state, if b != 0 { "True" } else { "False" }));
    }

    pub extern "C" fn print_str(index: i64) {
        attempt(|state: &mut State| {
            let text = usize::try_from(index).ok().and_then(|at| state.tables.strings.get(at)).cloned();
            write(state, text.as_deref().unwrap_or_default())
        });
    }

    /// Prints the value in a block, read from the block.
    pub extern "C" fn print_block(address: i64) {
        let shown = super::heap::shown::shown(u64::try_from(address).unwrap_or(0));
        attempt(|state: &mut State| write(state, &shown));
    }

    /// How many items a list from `items` has.
    pub extern "C" fn count(list: i64) -> i64 {
        with(|state| match state.temp(list) {
            Value::List(items) => super::handle(items.len()),
            _ => 0,
        })
    }

    /// The `at`th item of a list from `items`.
    pub extern "C" fn item(list: i64, at: i64) -> i64 {
        with(|state| {
            let item = match state.temp(list) {
                Value::List(items) => usize::try_from(at).ok().and_then(|at| items.get(at)).cloned(),
                _ => None,
            };
            state.push(item.unwrap_or(Value::Unit))
        })
    }
}

/// A failure inside the standard library is reported where the program called it.
fn at_failed_call(mut diagnostic: Diagnostic, call: Span) -> Diagnostic {
    if diagnostic.span.in_prelude() && !call.in_prelude() {
        diagnostic.span = call;
    }
    diagnostic
}

/// Checks a value can be failed with: it must be declared with `error`.
fn check_error(value: &Value, span: Span) -> Result<(), Diagnostic> {
    match value {
        Value::Instance(instance) if instance.is_error => Ok(()),
        other => {
            Err(Diagnostic::new(format!("`fail` needs an error, but this is a {}", other.type_name()), span)
                .with_help("declare error types with `error`, like `error NotFound:`"))
        }
    }
}
