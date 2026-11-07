//! The runtime that native code compiled by `fennec-jit` calls into. Native code keeps ints,
//! floats and bools in machine registers; every other value lives here, as an interpreter
//! value, and native code refers to it by a handle: a plain number. Names that hold such
//! values live in an interpreter frame, exactly as when the interpreter runs the program, so
//! the two engines share one meaning for every operation.
//!
//! Handles index a stack of temporaries. A function's temporaries are dropped when it
//! returns, and a statement's when it finishes, so the stack stays small.
//!
//! Every function here takes and returns plain numbers. A failure is recorded, not
//! returned: native code checks the failure flag after any call that can fail.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Expr, Module, Pattern, Span, Stmt};

pub mod aot;
pub mod bytes;
pub mod capabilities;
pub mod channels;
pub mod checks;
pub mod codec;
pub mod dicts;
pub mod guarded;
pub mod heap;
pub mod json;
pub mod layout;
pub mod lists;
pub mod numbers;
pub mod objects;
pub mod parallel;
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

    pub extern "C" fn net_serve(server: i64, body: ConnBody, env: i64) -> i64 {
        super::write_kept_output();
        while let Ok(conn) = crate::stdlib::net::accepted(server) {
            if body(env, conn, 0) != 0 {
                super::report_failure();
            }
            super::write_kept_output();
            crate::stdlib::net::closed(conn);
        }
        0
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

    pub extern "C" fn task_failed(_failure: i64) {}

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
#[cfg(not(feature = "lean"))]
use crate::interp::Interp;
use crate::place::Place;
use crate::scope::Frame;
use crate::value::{Capability, Value};

pub use heap::{Counts, count_allocations};
#[cfg(not(feature = "lean"))]
pub use tests::{next_test, ran_in_interpreter};

/// What the compiler registers before a program runs: the names, text and syntax its
/// native code refers to by number.
#[derive(Default)]
pub struct Tables {
    pub names: Vec<String>,
    pub strings: Vec<String>,
    pub exprs: Vec<Expr>,
    pub stmts: Vec<Stmt>,
    pub patterns: Vec<Pattern>,
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
            exprs: self.exprs.clone(),
            stmts: self.stmts.clone(),
            patterns: self.patterns.clone(),
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
        if !self.1 {
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
    #[cfg(not(feature = "lean"))]
    interp: Interp<'static>,
    output: Output,
    regexes: crate::stdlib::re::Regexes,
    tables: Tables,
    temps: Vec<Value>,
    /// The inputs of the next expression or statement native code hands over.
    inputs: Vec<Value>,
    /// The names the last pattern that matched binds.
    bindings: Vec<(String, Value)>,
    /// Each name's number in the name table, for telling types apart.
    name_ids: HashMap<String, i64>,
    /// The path `objects::put` changes, built a step at a time.
    path: Option<Place>,
    /// A `return`, or a failure `?` or `fail` passes up, from code the interpreter ran,
    /// which native code must now carry out.
    leaving: Option<crate::exec::Flow>,
    /// The frames of the native functions that called the running one.
    callers: Vec<Frame>,
    /// Emptied frames, kept to reuse for later calls.
    spare_frames: Vec<Frame>,
    /// Names' text by their number, shared by every frame that declares them.
    shared_names: Vec<Option<Rc<str>>>,
    heap: heap::Heap,
    /// The block of each plain capability, by its kind, once it's been asked for.
    capabilities: Vec<i64>,
    /// The program's name, as `args.program`.
    program: String,
    /// What it was given on its command line, as `args`.
    arguments: Vec<String>,
    failure: Option<Diagnostic>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static FLAGS: Flags = const { Flags { failed: Cell::new(0), lock_held: Cell::new(0) } };
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

/// Where the failure flag is.
pub fn failed_flag() -> usize {
    FLAGS.with(|flags| std::ptr::from_ref(flags).addr())
}

/// The capability native code means by `kind`: 0 for `Stdout`, 1 for `Fs`, 2 for `Clock`,
/// 3 for `Args`, 4 for `Stdin`, 5 for `Process`, 6 for `Entropy`, 7 for `Net` and 8 for `Env`.
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
        _ => Capability::Clock,
    }
}

/// The runtime's exports only the one with the interpreter has: native code calling any of these
/// can't link against the lean runtime. The runtime's own tests check it's exactly those.
pub const INTERPRETER_EXPORTS: &[&str] = &[
    "fennec_enter",
    "fennec_leave",
    "fennec_push_scope",
    "fennec_pop_scope",
    "fennec_declare",
    "fennec_assign",
    "fennec_get",
    "fennec_take",
    "fennec_put_back",
    "fennec_declare_capability",
    "fennec_expr",
    "fennec_stmt",
    "fennec_matches",
    "fennec_bind",
    "fennec_print_value",
    "fennec_print_spec",
    "fennec_items",
    "fennec_next",
    "fennec_binary",
    "fennec_field",
    "fennec_index",
    "fennec_path_get",
    "fennec_put",
    "fennec_put_item",
    "fennec_entries",
    "fennec_make_instance",
    "fennec_value_text",
    "fennec_spec_text",
    "fennec_make_variant",
    "fennec_case_of",
    "fennec_json_dumps",
    "fennec_json_loads",
    "fennec_capability_call",
    "fennec_std_call",
    "fennec_toml_dumps",
    "fennec_toml_loads",
    "fennec_aot_test_done",
    "fennec_aot_test_world",
];

/// Sets up the runtime to run `module` with `tables` and `inputs`, replacing any earlier
/// run's.
pub fn start(module: &Module, mut tables: Tables, inputs: crate::Inputs) -> Result<(), Diagnostic> {
    let output = Output(Rc::default(), inputs.live);
    OUTPUT.set(Some(output.clone()));
    let (program, arguments) = (inputs.program.clone(), inputs.arguments.clone());
    #[cfg(not(feature = "lean"))]
    let interp = {
        let mut interp = Interp::with_output(Box::new(output.clone()))?;
        interp.give(inputs);
        interp.load(module)?;
        interp
    };
    #[cfg(feature = "lean")]
    let _ = (module, inputs);
    let name_ids = tables.names.iter().zip(0_i64..).map(|(name, id)| (name.clone(), id)).collect();
    set_failed(0);
    STATE.set(Some(State {
        #[cfg(not(feature = "lean"))]
        interp,
        output,
        regexes: crate::stdlib::re::Regexes::default(),
        temps: Vec::new(),
        inputs: Vec::new(),
        bindings: Vec::new(),
        leaving: None,
        name_ids,
        path: None,
        callers: Vec::new(),
        spare_frames: Vec::new(),
        shared_names: Vec::new(),
        heap: heap::Heap::new(std::mem::take(&mut tables.statics)).planned(&tables.shapes, &tables.elements),
        capabilities: Vec::new(),
        program,
        arguments,
        tables,
        failure: None,
    }));
    Ok(())
}

/// What a run left in the runtime. A run that succeeds leaves nothing: a check on the
/// bookkeeping native code does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leftovers {
    pub temps: usize,
    pub frames: usize,
    /// Boxed values' blocks no one released.
    pub boxes: usize,
    /// What the run allocated and copied.
    pub counts: Counts,
}

/// Ends the run: what it printed, why it failed if it did, and what it left behind.
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
            let leftovers = Leftovers {
                temps: state.temps.len(),
                frames: state.callers.len(),
                boxes: state.heap.live(),
                counts: state.heap.counts,
            };
            let finished = (state.output.0.take(), state.failure.take(), leftovers);
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
        }
    });
    set_failed(1);
}

/// Whether this thread's runtime state is borrowed, as it is all through a call into the
/// runtime or the interpreter: a task mustn't park then, or the next task on the thread would
/// find it taken.
#[cfg(any(test, feature = "threads"))]
fn state_is_borrowed() -> bool {
    STATE.with(|state| state.try_borrow_mut().is_err())
}

/// Says what this thread's runtime holds as a failure and forgets it, as a server does for a
/// connection's task that failed, which ends only that task.
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

    /// A copy of a temporary, as the interpreter holds values.
    fn temp(&self, at: i64) -> Value {
        self.expanded(self.raw(at)).unpacked()
    }

    /// A copy of a temporary as it is, for what doesn't need to look inside it.
    fn raw(&self, at: i64) -> Value {
        usize::try_from(at).ok().and_then(|at| self.temps.get(at)).cloned().unwrap_or(Value::Unit)
    }

    /// A value with any native value at its top turned into what the interpreter holds.
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
        #[cfg(not(feature = "lean"))]
        if let Err(failure) = self.interp.clean_up_abandoned() {
            self.failure.get_or_insert(failure);
            set_failed(1);
        }
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

    /// Moves a temporary out, leaving nothing behind, so changing it can't copy it. It's
    /// unpacked for the interpreter.
    fn take(&mut self, at: i64) -> Value {
        let value = self.take_raw(at);
        self.expanded(value).unpacked()
    }

    /// Moves a temporary out for a name to hold, which may be packed but not lent.
    fn take_held(&mut self, at: i64) -> Value {
        let value = self.take_raw(at);
        self.expanded(value)
    }

    /// Moves a temporary out as it is, packed or not, for a name to hold.
    fn take_raw(&mut self, at: i64) -> Value {
        usize::try_from(at)
            .ok()
            .and_then(|at| self.temps.get_mut(at))
            .map_or(Value::Unit, |slot| std::mem::replace(slot, Value::Unit))
    }

    /// A name from the table, without copying it.
    fn name_ref(tables: &Tables, id: i64) -> &str {
        usize::try_from(id).ok().and_then(|id| tables.names.get(id)).map_or("", String::as_str)
    }

    /// A name's text from the table, shared rather than copied.
    fn shared_name(&mut self, id: i64) -> Rc<str> {
        let at = usize::try_from(id).unwrap_or(0);
        if let Some(Some(name)) = self.shared_names.get(at) {
            return Rc::clone(name);
        }
        let name: Rc<str> = Rc::from(Self::name_ref(&self.tables, id));
        if self.shared_names.len() <= at {
            self.shared_names.resize(at + 1, None);
        }
        if let Some(slot) = self.shared_names.get_mut(at) {
            *slot = Some(Rc::clone(&name));
        }
        name
    }

    fn name(&self, id: i64) -> String {
        usize::try_from(id).ok().and_then(|id| self.tables.names.get(id)).cloned().unwrap_or_default()
    }
}

/// Values of the names a native function holds.
pub mod names {
    use super::{Frame, Value, with};

    /// Starts a native function's frame.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn enter() {
        with(|state| {
            let fresh = state.spare_frames.pop().unwrap_or_else(Frame::new);
            let frame = std::mem::replace(&mut state.interp.frame, fresh);
            state.callers.push(frame);
        });
    }

    #[cfg(not(feature = "lean"))]
    pub extern "C" fn leave() {
        with(|state| {
            if let Some(frame) = state.callers.pop() {
                let mut done = std::mem::replace(&mut state.interp.frame, frame);
                if state.spare_frames.len() < SPARE_FRAMES {
                    done.reset();
                    state.spare_frames.push(done);
                }
            }
        });
    }

    /// How many emptied frames are kept for reuse.
    const SPARE_FRAMES: usize = 64;

    #[cfg(not(feature = "lean"))]
    pub extern "C" fn push_scope() {
        with(|state| state.interp.frame.push());
    }

    #[cfg(not(feature = "lean"))]
    pub extern "C" fn pop_scope() {
        with(|state| state.interp.frame.pop());
    }

    /// Creates a name holding a temporary's value.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn declare(name: i64, value: i64, is_mut: i8) {
        with(|state| {
            let value = state.take_held(value);
            let name = state.shared_name(name);
            state.interp.frame.declare_shared(name, value, is_mut != 0);
        });
    }

    /// Changes what a name holds.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn assign(name: i64, value: i64) {
        with(|state| {
            let value = state.take_held(value);
            let name = state.shared_name(name);
            if let Some(binding) = state.interp.frame.get_mut(&name) {
                binding.value = value;
            }
        });
    }

    /// A temporary with a copy of a name's value. Copies share memory until one changes.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn get(name: i64) -> i64 {
        with(|state| {
            let name = state.shared_name(name);
            let value = state.interp.frame.get(&name).map_or(Value::Unit, |binding| binding.value.clone());
            state.push(value)
        })
    }

    /// Moves a name's value into a temporary, for a `mut` argument that's put back after.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn take(name: i64) -> i64 {
        with(|state| {
            let name = state.shared_name(name);
            let value = state
                .interp
                .frame
                .get_mut(&name)
                .map_or(Value::Unit, |binding| std::mem::replace(&mut binding.value, Value::Unit));
            state.push(value)
        })
    }

    /// Copies a `mut` parameter's final value back into the name the caller passed, in the
    /// caller's frame. Runs just before the function leaves.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn put_back(caller_name: i64, param: i64) {
        with(|state| {
            let param = state.shared_name(param);
            let caller_name = state.shared_name(caller_name);
            let value = state
                .interp
                .frame
                .get_mut(&param)
                .map_or(Value::Unit, |binding| std::mem::replace(&mut binding.value, Value::Unit));
            if let Some(binding) = state.callers.last_mut().and_then(|frame| frame.get_mut(&caller_name)) {
                binding.value = value;
            }
        });
    }

    /// Declares a capability `main` was given, so interpreted code can use it: a capability's
    /// block, or a kind, 0 for `Stdout`, 1 for `Fs`, 2 for `Clock`, 3 for `Args`, 4 for
    /// `Stdin`, 5 for `Process`, 6 for `Entropy`, 7 for `Net` and 8 for `Env`.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn declare_capability(name: i64, kind: i64) {
        with(|state| {
            let name = state.name(name);
            let capability = super::capabilities::value_of(state, kind);
            state.interp.frame.declare(&name, capability, false);
        });
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

    /// Fails the program the way the interpreter does when `main` gives back a failure. The
    /// result is the last temporary, so it's dropped, leaving none behind.
    pub extern "C" fn main_result(value: i64, start: i64, end: i64) {
        attempt(|state: &mut State| {
            let result = state.take(value);
            if let Ok(at) = usize::try_from(value) {
                state.temps.truncate(at);
            }
            match result {
                Value::Err(error) => Err(Diagnostic::new(
                    format!("`main` failed: {}", crate::format::repr(&error)),
                    span(start, end),
                )),
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
    /// The `at`th of `count` names a tuple is unpacked into, failing like the interpreter
    /// when the value can't be unpacked into that many names.
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
            crate::exec::check_error(&error, span(start, end))?;
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

/// Running the parts of a program that native code hands to the interpreter.
pub mod eval {
    use super::{State, Value, attempt, span, with};
    use crate::call::at_call as repoint;
    use crate::exec::Flow;
    use fennec_syntax::Diagnostic;

    /// Reports a failure inside the standard library where the program called it.
    pub extern "C" fn at_call(start: i64, end: i64) {
        with(|state| {
            if let Some(failure) = state.failure.take() {
                state.failure = Some(repoint(failure, span(start, end)));
            }
        });
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

    #[cfg(not(feature = "lean"))]
    fn bind_inputs(state: &mut State) {
        state.interp.frame.push();
        for (index, value) in std::mem::take(&mut state.inputs).into_iter().enumerate() {
            state.interp.frame.declare(&input_name(index), value, true);
        }
    }

    /// Evaluates a registered expression with the inputs handed over.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn expr(id: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let expr = usize::try_from(id).ok().and_then(|id| state.tables.exprs.get(id)).cloned();
            let expr = expr.ok_or_else(|| Diagnostic::new("the JIT lost an expression", span(start, end)))?;
            bind_inputs(state);
            let result = state.interp.eval(&expr).map_err(|failure| repoint(failure, span(start, end)));
            state.interp.frame.pop();
            let value = match result {
                Err(diagnostic) if diagnostic.message == crate::interp::PASSING_UP => {
                    state.leaving = state.interp.passing_up.take();
                    Value::Unit
                }
                other => other?,
            };
            Ok(state.push(value))
        })
    }

    /// Runs a registered statement with the inputs handed over.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn stmt(id: i64, start: i64, end: i64) {
        attempt(|state: &mut State| {
            let stmt = usize::try_from(id).ok().and_then(|id| state.tables.stmts.get(id)).cloned();
            let stmt = stmt.ok_or_else(|| Diagnostic::new("the JIT lost a statement", span(start, end)))?;
            bind_inputs(state);
            let result = state.interp.exec(&stmt).map_err(|failure| repoint(failure, span(start, end)));
            state.interp.frame.pop();
            match result {
                Err(diagnostic) if diagnostic.message == crate::interp::PASSING_UP => {
                    state.leaving = state.interp.passing_up.take();
                    Ok(())
                }
                Ok(Flow::Normal(_)) => Ok(()),
                Ok(flow @ (Flow::Return(_) | Flow::PassUp(_))) => {
                    state.leaving = Some(flow);
                    Ok(())
                }
                Ok(Flow::Break | Flow::Continue | Flow::Yield(_)) => {
                    Err(Diagnostic::new("the JIT can't run this statement here", span(start, end)))
                }
                Err(diagnostic) => Err(diagnostic),
            }
        });
    }

    /// Whether the code the interpreter just ran is leaving the function: 0 when it isn't,
    /// 1 for a `return`, 2 for a failure passing up.
    pub extern "C" fn leaving() -> i8 {
        with(|state| match state.leaving {
            None | Some(Flow::Normal(_) | Flow::Break | Flow::Continue | Flow::Yield(_)) => 0,
            Some(Flow::Return(_)) => 1,
            Some(Flow::PassUp(_)) => 2,
        })
    }

    /// The value the function is leaving with.
    pub extern "C" fn left() -> i64 {
        with(|state| {
            let value = match state.leaving.take() {
                Some(Flow::Return(value) | Flow::PassUp(value) | Flow::Normal(value)) => value,
                _ => Value::Unit,
            };
            state.push(value)
        })
    }

    /// Whether a value matches a registered pattern. When it does, its bindings wait for
    /// `bind`.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn matches(value: i64, pattern: i64, start: i64, end: i64) -> i8 {
        attempt(|state: &mut State| {
            let pattern = usize::try_from(pattern).ok().and_then(|id| state.tables.patterns.get(id)).cloned();
            let pattern =
                pattern.ok_or_else(|| Diagnostic::new("the JIT lost a pattern", span(start, end)))?;
            let value = state.temp(value);
            Ok(match state.interp.matches(&pattern, &value, span(start, end))? {
                Some(bindings) => {
                    state.bindings = bindings;
                    1
                }
                None => 0,
            })
        })
    }

    /// Declares the names the last pattern that matched binds.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn bind() {
        with(|state| {
            for (name, value) in std::mem::take(&mut state.bindings) {
                state.interp.frame.declare(&name, value, false);
            }
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
    attempt(|state: &mut State| {
        state
            .output
            .write_all(printed)
            .map_err(|err| Diagnostic::new(format!("couldn't write output: {err}"), Span::default()))
    });
}

/// Printing, and the loops native code runs over interpreter values.
pub mod io {
    use super::{Span, State, Value, attempt, format, with};
    use fennec_syntax::Diagnostic;
    use std::io::Write;

    pub(super) fn write(state: &mut State, text: &str) -> Result<(), Diagnostic> {
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

    pub extern "C" fn print_bool(b: i8) {
        attempt(|state: &mut State| write(state, if b != 0 { "True" } else { "False" }));
    }

    pub extern "C" fn print_str(index: i64) {
        attempt(|state: &mut State| {
            let text = usize::try_from(index).ok().and_then(|at| state.tables.strings.get(at)).cloned();
            write(state, text.as_deref().unwrap_or_default())
        });
    }

    /// Prints a value as `stdout.print` shows it.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn print_value(value: i64) {
        attempt(|state: &mut State| {
            let value = state.temp(value);
            let text = state.interp.shown(&value, Span::default())?;
            write(state, &text)
        });
    }

    /// Prints the value in a block, read from the block.
    pub extern "C" fn print_block(address: i64) {
        let shown = super::heap::shown::shown(u64::try_from(address).unwrap_or(0));
        attempt(|state: &mut State| write(state, &shown?));
    }

    /// Prints a value with an f-string format spec, like `.1f` or `,`.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn print_spec(value: i64, spec: i64, start: i64, end: i64) {
        attempt(|state: &mut State| {
            let spec = usize::try_from(spec).ok().and_then(|at| state.tables.strings.get(at)).cloned();
            let value = state.temp(value);
            let text = state.interp.shown_spec(
                &value,
                spec.as_deref().unwrap_or_default(),
                super::span(start, end),
            )?;
            write(state, &text)
        });
    }

    /// The items a `for` loop walks through, as a list.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn items(value: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let value = state.temp(value);
            let span = super::span(start, end);
            if let Value::Generator(generator) = &value {
                crate::generators::start(generator, span)?;
                return Ok(state.push(value));
            }
            let items = state.interp.iterate(value, span)?;
            Ok(state.push(Value::list(items)))
        })
    }

    /// The next item of what `items` gave: -1 at the end, the item's temporary doubled, or
    /// doubled plus one for a generator's failure, for native code to pass up.
    #[cfg(not(feature = "lean"))]
    pub extern "C" fn next(items: i64, at: i64, can_pass: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let span = super::span(start, end);
            match state.temp(items) {
                Value::List(items) => Ok(usize::try_from(at)
                    .ok()
                    .and_then(|at| items.get(at))
                    .cloned()
                    .map_or(-1, |item| state.push(item) << 1)),
                Value::Generator(generator) => match state.interp.pull(&generator, span)? {
                    crate::generators::Pulled::Item(item) => Ok(state.push(item) << 1),
                    crate::generators::Pulled::Done => Ok(-1),
                    crate::generators::Pulled::Failed(failure) if can_pass != 0 => {
                        Ok((state.push(failure) << 1) | 1)
                    }
                    crate::generators::Pulled::Failed(failure) => {
                        Err(crate::generators::nowhere_to_pass(&failure, span))
                    }
                },
                _ => Ok(-1),
            }
        })
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
