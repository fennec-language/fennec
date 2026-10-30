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

pub mod heap;
pub mod lists;
pub mod objects;

use std::collections::HashMap;

use crate::format;
use crate::interp::Interp;
use crate::place::Place;
use crate::scope::Frame;
use crate::value::{Capability, Value};

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
}

/// Collects what the program prints.
#[derive(Clone, Default)]
struct Output(Rc<RefCell<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct State {
    interp: Interp<'static>,
    output: Output,
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
    heap: heap::Heap,
    failure: Option<Diagnostic>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Set when the program fails. Native code reads it directly after each call that can
    /// fail, and returns early when it's set.
    static FAILED: Cell<u8> = const { Cell::new(0) };
}

/// Where this thread's failure flag lives, for native code to read. Code compiled for one
/// thread must run on that thread.
pub fn failed_flag() -> usize {
    FAILED.with(|flag| flag.as_ptr().addr())
}

/// Sets up the runtime to run `module` with `tables`, replacing any earlier run's.
pub fn start(module: &Module, tables: Tables) -> Result<(), Diagnostic> {
    let output = Output::default();
    let mut interp = Interp::with_output(Box::new(output.clone()))?;
    interp.load(module)?;
    let name_ids = tables.names.iter().zip(0_i64..).map(|(name, id)| (name.clone(), id)).collect();
    FAILED.set(0);
    STATE.set(Some(State {
        interp,
        output,
        tables,
        temps: Vec::new(),
        inputs: Vec::new(),
        bindings: Vec::new(),
        leaving: None,
        name_ids,
        path: None,
        callers: Vec::new(),
        heap: heap::Heap::default(),
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
}

/// Ends the run: what it printed, why it failed if it did, and what it left behind.
pub fn finish() -> (Vec<u8>, Option<Diagnostic>, Leftovers) {
    let state = STATE.take();
    FAILED.set(0);
    match state {
        Some(state) => {
            let leftovers =
                Leftovers { temps: state.temps.len(), frames: state.callers.len(), boxes: state.heap.live() };
            (state.output.0.take(), state.failure, leftovers)
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
    FAILED.set(1);
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

    /// A copy of a temporary, unpacked for the interpreter.
    fn temp(&self, at: i64) -> Value {
        usize::try_from(at).ok().and_then(|at| self.temps.get(at)).cloned().unwrap_or(Value::Unit).unpacked()
    }

    /// Moves a temporary out, leaving nothing behind, so changing it can't copy it. It's
    /// unpacked for the interpreter.
    fn take(&mut self, at: i64) -> Value {
        self.take_raw(at).unpacked()
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

    fn name(&self, id: i64) -> String {
        usize::try_from(id).ok().and_then(|id| self.tables.names.get(id)).cloned().unwrap_or_default()
    }
}

/// Values of the names a native function holds.
pub mod names {
    use super::{Frame, State, Value, with};

    /// Starts a native function's frame.
    pub extern "C" fn enter() {
        with(|state| {
            let frame = std::mem::replace(&mut state.interp.frame, Frame::new());
            state.callers.push(frame);
        });
    }

    pub extern "C" fn leave() {
        with(|state| {
            if let Some(frame) = state.callers.pop() {
                state.interp.frame = frame;
            }
        });
    }

    pub extern "C" fn push_scope() {
        with(|state| state.interp.frame.push());
    }

    pub extern "C" fn pop_scope() {
        with(|state| state.interp.frame.pop());
    }

    /// Creates a name holding a temporary's value.
    pub extern "C" fn declare(name: i64, value: i64, is_mut: i8) {
        with(|state| {
            let value = state.take_raw(value);
            let name = State::name_ref(&state.tables, name);
            state.interp.frame.declare(name, value, is_mut != 0);
        });
    }

    /// Changes what a name holds.
    pub extern "C" fn assign(name: i64, value: i64) {
        with(|state| {
            let value = state.take_raw(value);
            let name = State::name_ref(&state.tables, name);
            if let Some(binding) = state.interp.frame.get_mut(name) {
                binding.value = value;
            }
        });
    }

    /// A temporary with a copy of a name's value. Copies share memory until one changes.
    pub extern "C" fn get(name: i64) -> i64 {
        with(|state| {
            let name = State::name_ref(&state.tables, name);
            let value = state.interp.frame.get(name).map_or(Value::Unit, |binding| binding.value.clone());
            state.push(value)
        })
    }

    /// Moves a name's value into a temporary, for a `mut` argument that's put back after.
    pub extern "C" fn take(name: i64) -> i64 {
        with(|state| {
            let name = State::name_ref(&state.tables, name);
            let value = state
                .interp
                .frame
                .get_mut(name)
                .map_or(Value::Unit, |binding| std::mem::replace(&mut binding.value, Value::Unit));
            state.push(value)
        })
    }

    /// Copies a `mut` parameter's final value back into the name the caller passed, in the
    /// caller's frame. Runs just before the function leaves.
    pub extern "C" fn put_back(caller_name: i64, param: i64) {
        with(|state| {
            let param = State::name_ref(&state.tables, param);
            let caller_name = State::name_ref(&state.tables, caller_name);
            let value = state
                .interp
                .frame
                .get_mut(param)
                .map_or(Value::Unit, |binding| std::mem::replace(&mut binding.value, Value::Unit));
            if let Some(binding) = state.callers.last_mut().and_then(|frame| frame.get_mut(caller_name)) {
                binding.value = value;
            }
        });
    }

    /// Declares a capability `main` was given, so interpreted code can use it: 0 for
    /// `Stdout`, 1 for `Fs`.
    pub extern "C" fn declare_capability(name: i64, kind: i64) {
        with(|state| {
            let name = state.name(name);
            let capability = if kind == 0 { super::Capability::Stdout } else { super::Capability::Fs };
            state.interp.frame.declare(&name, Value::Capability(capability), false);
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
        with(|state| state.temps.truncate(usize::try_from(mark).unwrap_or(0)));
    }

    /// Drops a returning function's temporaries except the value it returns.
    pub extern "C" fn keep(mark: i64, value: i64) -> i64 {
        with(|state| {
            let value = if value >= mark { state.take_raw(value) } else { state.temp(value) };
            state.temps.truncate(usize::try_from(mark).unwrap_or(0));
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
        with(|state| i8::from(state.temp(value).is_failure()))
    }

    pub extern "C" fn is_none(value: i64) -> i8 {
        with(|state| i8::from(matches!(state.temp(value), Value::None)))
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
            let value = state.temp(value);
            state.push(value)
        })
    }

    pub extern "C" fn unwrap(value: i64) -> i64 {
        with(|state| {
            let inner = match state.temp(value) {
                Value::Ok(inner) => inner.as_ref().clone(),
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
        with(|state| i8::from(matches!(state.temp(value), Value::Err(_) | Value::None)))
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
            let wrapped = match state.take(value) {
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
    use crate::exec::Flow;
    use fennec_syntax::Diagnostic;

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

    fn bind_inputs(state: &mut State) {
        state.interp.frame.push();
        for (index, value) in std::mem::take(&mut state.inputs).into_iter().enumerate() {
            state.interp.frame.declare(&input_name(index), value, true);
        }
    }

    /// Evaluates a registered expression with the inputs handed over.
    pub extern "C" fn expr(id: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let expr = usize::try_from(id).ok().and_then(|id| state.tables.exprs.get(id)).cloned();
            let expr = expr.ok_or_else(|| Diagnostic::new("the JIT lost an expression", span(start, end)))?;
            bind_inputs(state);
            let result = state.interp.eval(&expr);
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
    pub extern "C" fn stmt(id: i64, start: i64, end: i64) {
        attempt(|state: &mut State| {
            let stmt = usize::try_from(id).ok().and_then(|id| state.tables.stmts.get(id)).cloned();
            let stmt = stmt.ok_or_else(|| Diagnostic::new("the JIT lost a statement", span(start, end)))?;
            bind_inputs(state);
            let result = state.interp.exec(&stmt);
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
                Ok(Flow::Break | Flow::Continue) => {
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
            None | Some(Flow::Normal(_) | Flow::Break | Flow::Continue) => 0,
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

/// Printing, and the loops native code runs over interpreter values.
pub mod io {
    use super::{Span, State, Value, attempt, format, with};
    use fennec_syntax::Diagnostic;

    fn write(state: &mut State, text: &str) -> Result<(), Diagnostic> {
        state
            .interp
            .out
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
    pub extern "C" fn print_value(value: i64) {
        attempt(|state: &mut State| {
            let text = format::display(&state.temp(value));
            write(state, &text)
        });
    }

    /// Prints a value with an f-string format spec, like `.1f` or `,`.
    pub extern "C" fn print_spec(value: i64, spec: i64, start: i64, end: i64) {
        attempt(|state: &mut State| {
            let spec = usize::try_from(spec).ok().and_then(|at| state.tables.strings.get(at)).cloned();
            let text = format::with_spec(&state.temp(value), spec.as_deref().unwrap_or_default())
                .map_err(|message| Diagnostic::new(message, super::span(start, end)))?;
            write(state, &text)
        });
    }

    /// The items a `for` loop walks through, as a list.
    pub extern "C" fn items(value: i64, start: i64, end: i64) -> i64 {
        attempt(|state: &mut State| {
            let value = state.temp(value);
            let items = state.interp.iterate(value, super::span(start, end))?;
            Ok(state.push(Value::list(items)))
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
