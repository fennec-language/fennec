//! Starting a compiled executable. Its C `main` hands over the program it carries, a word
//! at a time, then calls `run`, which runs the program's native entry point on a thread
//! with a big stack and reports what happened the way `fennec run` does.

use std::cell::RefCell;
use std::io::Write;

use fennec_syntax::Sources;

use super::codec::Image;
use super::{State, heap, with};

/// The stack an executable's program runs on. It's only reserved, not used, until calls
/// nest deeply.
const STACK_BYTES: usize = 1 << 30;

/// How much of the stack native calls may use, leaving the rest for the runtime and the
/// interpreter.
const NATIVE_STACK_BYTES: usize = 1 << 28;

/// What ends the process at once, which the runtime library provides.
static STOP: std::sync::OnceLock<fn(i32)> = std::sync::OnceLock::new();

thread_local! {
    /// The program's bytes, as they're handed over.
    static IMAGE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// What renders a failure in the program running here.
    static SOURCES: RefCell<Option<Sources>> = const { RefCell::new(None) };
    /// The text of each string literal native code asks for by number.
    static LITERALS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The next eight bytes of the program the executable carries.
pub extern "C" fn image(word: i64) {
    IMAGE.with_borrow_mut(|bytes| bytes.extend_from_slice(&word.to_le_bytes()));
}

/// Runs the program handed over, `length` bytes of it, by calling `entry` with the lowest
/// address its stack may grow to. Prints what it printed, and the failure if it failed.
/// Gives back the process's exit code.
pub fn run(entry: extern "C" fn(i64), length: i64, stop: fn(i32)) -> i32 {
    let _ = STOP.set(stop);
    let mut bytes = IMAGE.take();
    bytes.truncate(usize::try_from(length).unwrap_or(0));
    let ran = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || execute(entry, &bytes))
            .map(std::thread::ScopedJoinHandle::join)
    });
    match ran {
        Ok(Ok((output, failure))) => report(&output, &failure),
        Ok(Err(_)) => report(b"", "error: the program stopped unexpectedly\n"),
        Err(err) => report(b"", &format!("error: couldn't start the program's thread: {err}\n")),
    }
}

/// Runs the program, on the current thread: what it printed, and its failure, rendered,
/// or nothing.
fn execute(entry: extern "C" fn(i64), bytes: &[u8]) -> (Vec<u8>, String) {
    let Some(Image { module, sources, tables, literals }) = Image::from_bytes(bytes) else {
        return (Vec::new(), "error: this executable's program is damaged\n".to_owned());
    };
    let mut arguments = std::env::args();
    let inputs = crate::Inputs {
        live: true,
        program: arguments.next().unwrap_or_default(),
        arguments: arguments.collect(),
        stdin: Box::new(std::io::BufReader::new(std::io::stdin())),
    };
    if let Err(failure) = super::start(&module, tables, inputs) {
        return (Vec::new(), sources.render(&failure));
    }
    LITERALS.set(literals);
    SOURCES.set(Some(sources.clone()));
    let here = 0_u8;
    let limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
    entry(i64::try_from(limit).unwrap_or(0));
    let (output, failure, _) = super::finish();
    (output, failure.map(|failure| sources.render(&failure)).unwrap_or_default())
}

/// Ends the program for a failure where nothing can pass it on, printing what it printed and
/// the failure, as the end of a run would. In anything but an executable, the failure just
/// stays for whoever finishes the run.
pub(super) fn stop_for_failure() {
    let Some(sources) = SOURCES.take() else { return };
    let (output, failure, _) = super::finish();
    let failure = failure.map(|failure| sources.render(&failure)).unwrap_or_default();
    let code = report(&output, &failure);
    if let Some(stop) = STOP.get() {
        stop(code);
    }
}

/// Prints a run's output and, if there is one, its failure; gives back the exit code.
fn report(output: &[u8], failure: &str) -> i32 {
    let printed = std::io::stdout().write_all(output).and_then(|()| std::io::stdout().flush());
    if failure.is_empty() && printed.is_ok() {
        return 0;
    }
    let _ = std::io::stderr().write_all(failure.as_bytes());
    1
}

/// Where the failure flag native code checks is.
pub extern "C" fn failed_flag() -> i64 {
    i64::try_from(super::failed_flag()).unwrap_or(0)
}

/// Where the table of one-character ASCII strings is.
pub extern "C" fn ascii() -> i64 {
    with(|state: &mut State| {
        let address = heap::ascii_table(&mut state.tables);
        state.heap.adopt(&mut state.tables.statics);
        address
    })
}

/// Where a string literal's block is, by its number.
pub extern "C" fn literal(at: i64) -> i64 {
    let text =
        LITERALS.with_borrow(|literals| usize::try_from(at).ok().and_then(|at| literals.get(at).cloned()));
    with(|state: &mut State| {
        let address = heap::literal(&mut state.tables, &text.unwrap_or_default());
        state.heap.adopt(&mut state.tables.statics);
        address
    })
}
