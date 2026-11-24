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
pub(crate) mod deadlock;
pub mod dicts;
pub mod guarded;
pub mod heap;
pub mod layout;
pub mod lists;
#[cfg(any(test, feature = "threads"))]
pub(crate) mod live;

/// Without threads, a runtime counts nothing for `FENNEC_LIVE_REPORT`: its one line is whether a
/// server has taken a stop the program was asked for (see `live` in the threads build).
#[cfg(not(any(test, feature = "threads")))]
pub(crate) mod live {
    /// A server has taken a stop the program was asked for, and is draining: written to the report,
    /// if it's asked for, as `stopping 1`, once.
    pub(crate) fn stop_taken() {
        let Some(path) = std::env::var_os("FENNEC_LIVE_REPORT").map(std::path::PathBuf::from) else { return };
        let writing = path.with_extension("writing");
        if std::fs::write(&writing, "stopping 1\n").is_ok() {
            let _ = std::fs::rename(&writing, &path);
        }
    }
}
pub mod conn_calls;
pub mod notes;
pub mod numbers;
pub mod objects;
pub mod parallel;
pub(crate) use parallel::{stress_asked, workers_wanted};
mod plans;
pub mod regex;
#[cfg(any(test, feature = "threads"))]
pub mod serve;

/// Without the `threads` feature, a program has no tasks, so a server takes its connections one
/// at a time, as looping over it does.
#[cfg(not(any(test, feature = "threads")))]
pub mod serve {
    use std::time::Duration;

    use fennec_syntax::{Diagnostic, Span};

    use crate::stdlib::net::Failure;
    use crate::stdlib::net::head::{Head, Limits};

    /// Without threads there are no blocking threads, so none is ever busy (see the `threads`
    /// version, for the deadlock check).
    pub(crate) fn blocking_idle() -> bool {
        true
    }

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
        _timeout: Option<Duration>,
        _at: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        None
    }

    // Without threads no worker serves a connection: each is a real one, which the typed calls
    // (`embed::conn_calls`) reach by value, as `served_call` sends it.
    pub(in crate::embed) fn served_write(
        _: i64,
        _: &[u8],
        _: Option<Duration>,
    ) -> Option<Result<(), Failure>> {
        None
    }

    pub(in crate::embed) fn served_reads_within(_: i64, _: Duration) -> Option<()> {
        None
    }

    pub(in crate::embed) fn served_draining(_: i64) -> Option<bool> {
        None
    }

    pub(in crate::embed) fn served_room_for_body(_: i64, _: usize) -> Option<bool> {
        None
    }

    pub(in crate::embed) fn served_body(
        _: i64,
        _: usize,
        _: Option<Duration>,
    ) -> Option<Result<Vec<u8>, Failure>> {
        None
    }

    pub(in crate::embed) fn served_head(
        _: i64,
        _: Limits,
        _: Option<Duration>,
    ) -> Option<Result<Head, Failure>> {
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

pub use heap::{Counts, RanOn, count_allocations};
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
    /// The kinds of item the runtime last asked the number of, with each one's number in
    /// `elements`, which never changes once given: a list of strings `split` makes finds its
    /// number here, however many kinds a program describes before strings.
    pub found_elements: Vec<(Rc<crate::packed::Element>, usize)>,
    /// The number in `elements` of the kind of item a list of strings the runtime makes holds
    /// (see `lists::list_of_texts`), once found: `split` makes one a call.
    pub texts_element: Option<usize>,
    /// The kinds of item the runtime numbered itself, past `elements`, as this thread has read them
    /// (see `lists::AddedKinds`).
    pub added_elements: lists::AddedKinds,
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

/// Collects what the program prints: what's kept, how it's written (`Writes`), and since when.
#[derive(Clone, Default)]
struct Output(Rc<RefCell<Vec<u8>>>, Writes, Rc<Cell<Since>>);

/// How a run writes what it prints, settled as it starts.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Writes {
    /// It's kept for the run's host, as a test's is, or a `parallel` range's, to come out whole.
    #[default]
    Kept,
    /// A line at a time, to a terminal, where a person waits on each.
    Lines,
    /// To a pipe or a file: at 8 KB, or at a line once `OUTPUT_AGE` has gone since the last write.
    InTime,
    /// At 8 KB only, in a recording or a replay: see `Output::due`.
    Sized,
}

/// When a run last wrote what it printed, if it has, and how many lines it has kept since.
#[derive(Clone, Copy, Default)]
struct Since {
    written: Option<std::time::Instant>,
    lines: u32,
}

thread_local! {
    /// Whether what's running here is a launched task of a program that writes as it goes. A
    /// `parallel` range's output is kept to come out whole, but tasks talk to each other, and what
    /// one printed before it told another comes first.
    static TASK_WRITES: Cell<bool> = const { Cell::new(false) };
}

/// Whether a task launched from here writes as it goes: as what launches it does, whether that's a
/// runtime writing its output live or a task that was launched to. A run's own, so one run writing
/// live doesn't change how another's tasks print, as tests running one after another in a process
/// would see.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn program_writes() -> bool {
    TASK_WRITES.get()
        || OUTPUT.with_borrow(|output| output.as_ref().is_some_and(|output| output.1 != Writes::Kept))
}

/// Says whether what's about to run here writes as it goes, giving back what was.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn swap_task_writes(writes: bool) -> bool {
    TASK_WRITES.replace(writes)
}

/// How much output is kept before a live run writes it.
const LIVE_OUTPUT: usize = 1 << 13;

/// How long after a live run last wrote its output a line printed to a pipe or a file is written,
/// however little is kept, so a program killed loses only its last moment's lines.
const OUTPUT_AGE: std::time::Duration = std::time::Duration::from_millis(100);

/// How a run that writes as it goes writes: a line at a time to a terminal, and in time to a pipe or
/// a file. A recording and its replay break their output only at waits and at 8 KB, as before lines
/// went out in time, whatever the clock or the terminal: a replay writes the bursts its recording
/// logged, and one the clock broke off there would wait for bytes the replay keeps.
fn live_writes() -> Writes {
    if crate::replay::active() {
        Writes::Sized
    } else if to_terminal() {
        Writes::Lines
    } else {
        Writes::InTime
    }
}

/// Whether standard output is a terminal.
#[cfg(not(test))]
fn to_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Tests write to `WRITTEN`, not to a terminal.
#[cfg(test)]
const fn to_terminal() -> bool {
    false
}

/// Whether a run that `writes` so writes the lines it has kept, now that a piece ending one has
/// come, the `lines`th since the last write. To a pipe or a file, that's once `LIVE_OUTPUT` bytes
/// are kept, or once `OUTPUT_AGE` has gone since the last write. The clock is read only when 8 KB
/// hasn't said yes, and only at the 1st, 2nd, 4th, 8th and so on line since the last write: read at
/// every line, it cost a program printing a million short lines a fifth more instructions on macOS,
/// and this way one printing fast reads it about ten times in 8 KB.
fn due(
    writes: Writes,
    kept: usize,
    lines: u32,
    since_written: impl FnOnce() -> Option<std::time::Duration>,
) -> bool {
    match writes {
        Writes::Kept => false,
        Writes::Lines => true,
        Writes::Sized => kept >= LIVE_OUTPUT,
        Writes::InTime => {
            kept >= LIVE_OUTPUT
                || (lines.is_power_of_two() && since_written().is_none_or(|since| since >= OUTPUT_AGE))
        }
    }
}

impl Output {
    /// Writes the first `length` bytes kept, in one go, so nothing another thread writes lands
    /// among them.
    fn write_out(&self, length: usize) -> std::io::Result<()> {
        let mut kept = self.0.borrow_mut();
        write_whole(kept.get(..length).unwrap_or_default())?;
        kept.drain(..length);
        self.2.set(Since { written: Some(std::time::Instant::now()), lines: 0 });
        Ok(())
    }

    /// Whether this run writes the `kept` bytes' lines now (`due`). A launched task of a run that
    /// writes as it goes writes as the run does, but as before in a recording: only at waits.
    fn due(&self, kept: usize) -> bool {
        let writes = match self.1 {
            Writes::Kept if TASK_WRITES.get() && !crate::replay::active() => live_writes(),
            writes => writes,
        };
        let mut since = self.2.get();
        since.lines = since.lines.saturating_add(1);
        self.2.set(since);
        due(writes, kept, since.lines, || since.written.map(|written| written.elapsed()))
    }
}

/// Where a live run's output goes: the process's standard output, one call at a time.
#[cfg(not(test))]
fn write_whole(bytes: &[u8]) -> std::io::Result<()> {
    crate::replay::writing(bytes, |bytes| {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    })
}

/// What live runs have written, in tests, in the order they wrote it. Every test's runs write here,
/// so a test reads only what was written since it started (`written_since`).
#[cfg(test)]
static WRITTEN: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

/// Where what live runs write has got to, for a test to read only what's written after it
/// (`written_since`).
#[cfg(test)]
pub(in crate::embed) fn written_mark() -> usize {
    WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
}

/// What live runs have written since `mark`, as text: a test's, along with whatever other tests
/// running at the same time wrote, which it tells apart by what it looks for.
#[cfg(test)]
pub(in crate::embed) fn written_since(mark: usize) -> String {
    let written = WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    String::from_utf8_lossy(written.get(mark..).unwrap_or_default()).into_owned()
}

#[cfg(test)]
fn write_whole(bytes: &[u8]) -> std::io::Result<()> {
    crate::replay::writing(bytes, |bytes| {
        WRITTEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(bytes);
        Ok(())
    })
}

impl Write for Output {
    /// Keeps what's printed, and once a run that writes as it goes is due to (`due`), writes it up
    /// to the last whole line, as one print comes here in pieces and threads share the output.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        let kept = self.0.borrow().len();
        // A print's last piece ends its line, so whether to write is asked once a line, not once a
        // piece, and only of its last byte. 8 KB kept writes the lines before it as well.
        let may_write = kept >= LIVE_OUTPUT || bytes.last() == Some(&b'\n');
        if may_write && self.due(kept) {
            let lines = self.0.borrow().iter().rposition(|byte| *byte == b'\n');
            if let Some(last) = lines {
                self.write_out(last + 1)?;
            }
        }
        Ok(bytes.len())
    }

    /// Writes what a live run has kept. Any other run's output stays for its host.
    fn flush(&mut self) -> std::io::Result<()> {
        if self.1 == Writes::Kept && !TASK_WRITES.get() {
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
            write_kept_output_if_free();
            earlier(panic);
        }));
    });
}

/// Writes what this thread printed and kept, unless the program stopped while the output was in
/// use, when it can't be read: for a stop that can't wait for the program's own end, a panic or
/// running out of memory.
pub(crate) fn write_kept_output_if_free() {
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
    /// A test's fake capabilities, while one that takes them runs. Behind a box whose type is
    /// known only where one is made, so a program that isn't a test links none of what a test's
    /// world does, like cleaning up the files its fakes made.
    world: Option<Box<dyn std::any::Any>>,
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
    /// The words of a dict's value held in place, on its way in or out (see
    /// `dicts::value_buffer_of`): made the first time one's wanted, and this run's own.
    dict_value: std::cell::OnceCell<Box<[Cell<u64>]>>,
    /// How many words each kind of item this run has takes held in place, worked out the first
    /// time a dict of values in place asks: its width is a walk over the kind's type, which every
    /// call for such a dict would otherwise make.
    value_widths: std::cell::OnceCell<Box<[usize]>>,
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
    /// `fennec_replay_locked` only when it's 1, and giving it back `fennec_replay_unlocked`, so a
    /// normal run pays one load.
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

/// Whether this thread's failure flag is set, for the runtime library's check, in a debug build,
/// that a function it says never fails didn't.
pub fn failure_flag_set() -> bool {
    failed()
}

/// Stops the program: `symbol`, which the runtime library says never fails, set the failure flag.
/// Generated code skips the check after such a call, so it would have gone on past the failure.
#[cold]
#[inline(never)]
pub fn failed_though_never_fails(symbol: &str) -> ! {
    let message = with(|state| state.failure.as_ref().map(|failure| failure.message.clone()));
    let _ = writeln!(
        std::io::stderr(),
        "fennec runtime: `{symbol}` is listed in NEVER_FAIL but failed: {}",
        message.unwrap_or_default()
    );
    std::process::abort();
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
    let writes = if live { live_writes() } else { Writes::Kept };
    let output = Output(Rc::default(), writes, Rc::default());
    OUTPUT.set(Some(output.clone()));
    #[cfg(any(test, feature = "threads"))]
    let name_ids = tables.names.iter().zip(0_i64..).map(|(name, id)| (name.clone(), id)).collect();
    set_failed(0);
    threads::run_starts();
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
        dict_value: std::cell::OnceCell::new(),
        value_widths: std::cell::OnceCell::new(),
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
            // Where native code has no frames left, so every owner is one the heap can show.
            #[cfg(feature = "audit")]
            if leave {
                aot::audit_at_the_end(&mut state);
            }
            if !leave {
                guarded::forget_ending(&state.heap);
                threads::forget_ending(&state.heap);
            }
            // Every thread's, as a server's cost is on its workers and a program's in its tasks.
            aot::report_sites(&heap::every_threads_sites(&state.heap.sites));
            let leftovers = Leftovers {
                temps: state.temps.len(),
                boxes: state.heap.leftover(),
                counts: state.heap.counts,
            };
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

/// What a task fails with when a stop, or a cancel of its round, cuts it short.
pub(crate) const STOPPED_FAILURE: &str = "this task was stopped";

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
    // A replay that couldn't follow its recording says where it first couldn't, once, as it ends:
    // a task's failure after that comes of it, as another task's patience running out with its own.
    let followed = crate::replay::divergence().is_none();
    if let Some(failure) = failure.filter(|_| followed) {
        write_kept_output();
        // Where it happened, as `main`'s failure and a range's say it (#2045).
        let said = aot::rendered_here(&failure).unwrap_or_else(|| format!("error: {}\n", failure.message));
        let _ = std::io::stderr().write_all(said.as_bytes());
    }
}

/// A block a runtime function gives native code, which uses it at once with no check. 0, no block
/// at all, only a bug in Fennec gives: an input generated code shouldn't pass, or no runtime on the
/// thread. So it stops the program saying so, rather than giving native code address 0 to read.
pub(in crate::embed) fn made(what: &str, address: i64) -> i64 {
    if address == 0 && !made_up_inputs() {
        heap::runtime_bug(&format!("`{what}` made no block"));
    }
    address
}

#[cfg(feature = "made-up-inputs")]
thread_local! {
    /// Whether what calls the runtime on this thread passes it made-up values on purpose, as the
    /// fuzzer does to show they can't make it follow an address it didn't give out: then a block
    /// it can't make for one is given back as 0, which the caller drops, rather than stopping the
    /// process it runs in. Only a runtime built with the `made-up-inputs` feature, as the fuzzer's
    /// is, has this: native code never passes a made-up value, and can't set it.
    static MADE_UP_INPUTS: Cell<bool> = const { Cell::new(false) };
}

/// Says that what calls the runtime on this thread passes it made-up values on purpose, as a
/// fuzzer does (see `MADE_UP_INPUTS`).
#[cfg(feature = "made-up-inputs")]
pub fn takes_made_up_inputs(on: bool) {
    MADE_UP_INPUTS.set(on);
}

/// Whether what calls the runtime here passes it made-up values on purpose: never, in a runtime
/// built without the `made-up-inputs` feature, as one that ships is.
fn made_up_inputs() -> bool {
    #[cfg(feature = "made-up-inputs")]
    return MADE_UP_INPUTS.get();
    #[cfg(not(feature = "made-up-inputs"))]
    false
}

/// [`with`], for a runtime function that gives native code a block: see [`made`].
pub(in crate::embed) fn made_with(what: &str, make: impl FnOnce(&mut State) -> i64) -> i64 {
    made(what, with(make))
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
    /// The fakes of the test running, if one is.
    fn world(&self) -> Option<&crate::fakes::TestWorld> {
        self.world.as_ref().and_then(|world| world.downcast_ref())
    }

    /// `world`, to change.
    fn world_mut(&mut self) -> Option<&mut crate::fakes::TestWorld> {
        self.world.as_mut().and_then(|world| world.downcast_mut())
    }

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

    /// A capability as a value, for code that passes one on, like `len(args)`: a block or a
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
        main_result_shown(value, 0, start, end);
    }

    /// `main_result`, with how its failure is shown, when that's given: `shown`, the block of a
    /// string the runtime takes over, holds its words with the failure as it is on the next line.
    pub extern "C" fn main_result_shown(value: i64, shown: i64, start: i64, end: i64) {
        // The failure's call chain, which native code keeps among the error's notes.
        let mut chain = Vec::new();
        attempt(|state: &mut State| {
            let shown = u64::try_from(shown).ok().filter(|address| *address != 0).and_then(|address| {
                let text = super::heap::read_block(state, address);
                super::heap::release(state, address);
                if let Value::Str(text) = text { Some(text.to_string()) } else { None }
            });
            let result = state.take(value);
            if let Ok(at) = usize::try_from(value) {
                state.temps.truncate(at);
            }
            match result {
                Value::Err(error) => {
                    if let Value::Instance(instance) = error.as_ref()
                        && let Some(notes) = &instance.notes
                    {
                        chain = super::notes::chain_in(notes);
                    }
                    Err(Diagnostic::new(
                        format!("`main` failed: {}", crate::format::failure(&error, shown)),
                        span(start, end),
                    ))
                }
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
        // Said where it was raised, with its call chain under it, as a `main` that gives nothing
        // back says its failure: with no chain, where `main` is.
        super::notes::said_where_raised(&chain);
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

thread_local! {
    /// The capture of the test the task or range running here was started by, which what it
    /// prints and writes to standard error goes into, as its thread has no test world of its own.
    static CAPTURE: RefCell<Option<std::sync::Arc<crate::fakes::Capture>>> = const { RefCell::new(None) };
}

/// The capture what's started from here prints into: the test's own on the thread running it, or
/// the one this task or range was given. `None` outside a test, and in `fennec test --real`.
pub(in crate::embed) fn capture_here() -> Option<std::sync::Arc<crate::fakes::Capture>> {
    CAPTURE.with_borrow(Option::clone).or_else(|| {
        with(|state| state.world().filter(|world| !world.real()).map(crate::fakes::TestWorld::capture))
    })
}

/// Makes `capture` the one what runs here prints into, giving back the one before.
pub(in crate::embed) fn swap_capture(capture: &mut Option<std::sync::Arc<crate::fakes::Capture>>) {
    // What's held of an unfinished line goes to the capture it was printed for, before another's
    // comes in.
    flush_held_line();
    CAPTURE.with_borrow_mut(|here| std::mem::swap(here, capture));
}

thread_local! {
    /// What this thread has printed into a test's capture of a line not ended yet, and the capture
    /// it's for. A `print` comes in pieces, and its pieces go into the capture together, once its
    /// line ends, so other threads' prints never land inside it.
    static HELD_LINE: RefCell<Option<(std::sync::Arc<crate::fakes::Capture>, String)>> =
        const { RefCell::new(None) };
}

/// Puts what this thread printed into `capture`, a whole line at a time: the end of a line it
/// holds back until the rest of it comes.
fn capture_lines(capture: &std::sync::Arc<crate::fakes::Capture>, text: &str) {
    let held = HELD_LINE.take();
    let mut held = match held {
        Some((was, held)) if std::sync::Arc::ptr_eq(&was, capture) => held,
        Some((was, held)) => {
            was.print(&held);
            String::new()
        }
        None => String::new(),
    };
    held.push_str(text);
    if let Some(end) = held.rfind('\n') {
        let rest = held.split_off(end + 1);
        capture.print(&held);
        held = rest;
    }
    if !held.is_empty() {
        HELD_LINE.set(Some((std::sync::Arc::clone(capture), held)));
    }
}

/// Puts what this thread holds of an unfinished line into the capture it was printed for, as
/// what's captured is about to be read or this thread is to print elsewhere.
pub(crate) fn flush_held_line() {
    if let Some((capture, held)) = HELD_LINE.take() {
        capture.print(&held);
    }
}

/// The capture this thread prints into, when what it runs belongs to a test and the thread has no
/// world of its own.
fn capture_given() -> Option<std::sync::Arc<crate::fakes::Capture>> {
    CAPTURE.with_borrow(Option::clone)
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
        if let Some(world) = state.world_mut() {
            // Each line whole, as a task the test started may be printing at the same time.
            super::capture_lines(&world.capture(), text);
            if !world.real() {
                return Ok(());
            }
        } else if let Some(capture) = super::capture_given() {
            // A task or range a test started, on a thread of its own: what it prints is the test's,
            // each line whole.
            super::capture_lines(&capture, text);
            return Ok(());
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
