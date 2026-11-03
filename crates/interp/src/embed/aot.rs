//! Starting a compiled executable. Its C `main` hands over the program it carries, a word
//! at a time, then calls `run`, which runs the program's native entry point on a thread
//! with a big stack and reports what happened the way `fennec run` does.

use std::cell::RefCell;
use std::fmt::Write as _;
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
    let bytes = program_handed_over(length, stop);
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

/// The program handed over, `length` bytes of it, ready for workers to start from.
fn program_handed_over(length: i64, stop: fn(i32)) -> Vec<u8> {
    let _ = STOP.set(stop);
    let mut bytes = IMAGE.take();
    bytes.truncate(usize::try_from(length).unwrap_or(0));
    // Each worker `parallel` runs on starts a runtime of its own, holding the program.
    let program = std::sync::Arc::new(bytes.clone());
    super::parallel::set_worker_setup(Box::new(move || {
        if let Some(Image { module, tables, .. }) = Image::from_bytes(&program) {
            let _ = super::start(&module, tables, crate::Inputs::default());
        }
    }));
    bytes
}

/// Runs a test executable's tests: those whose numbers are its arguments, each by calling
/// `run_test` with its number, one after another. What each did goes to standard output as soon
/// as it's done, in a line `fennec test` reads, so a crash loses only the tests not yet run. Tests
/// here take nothing, so nothing else writes there.
pub fn run_tests(
    entry: extern "C" fn(i64),
    run_test: extern "C" fn(i64),
    count: i64,
    length: i64,
    stop: fn(i32),
) -> i32 {
    let bytes = program_handed_over(length, stop);
    let ran = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || execute_tests(entry, run_test, count, &bytes))
            .map(std::thread::ScopedJoinHandle::join)
    });
    i32::from(!matches!(ran, Ok(Ok(true))))
}

/// Runs the tests asked for on the current thread, saying whether the runtime started.
fn execute_tests(entry: extern "C" fn(i64), run_test: extern "C" fn(i64), count: i64, bytes: &[u8]) -> bool {
    let Some(Image { module, tables, literals, .. }) = Image::from_bytes(bytes) else { return false };
    if super::start(&module, tables, crate::Inputs::default()).is_err() {
        return false;
    }
    LITERALS.set(literals);
    let here = 0_u8;
    let limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
    entry(i64::try_from(limit).unwrap_or(0));
    let mut out = std::io::stdout();
    for number in std::env::args().skip(1).filter_map(|number| number.parse::<i64>().ok()) {
        if !(0..count).contains(&number) {
            continue;
        }
        run_test(number);
        let line = test_line(number, super::next_test().as_ref());
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            return false;
        }
    }
    super::finish();
    true
}

/// What a test did, as a line: its number, and for a failure where it is, its message and its
/// help, in hexadecimal so that anything can be in them.
fn test_line(number: i64, failure: Option<&fennec_syntax::Diagnostic>) -> String {
    let hex = |text: &str| {
        let mut hex = String::with_capacity(text.len() * 2);
        for byte in text.bytes() {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    };
    match failure {
        None => format!("{number} ok"),
        Some(failure) => format!(
            "{number} failed {} {} {} {}",
            failure.span.start,
            failure.span.end,
            hex(&failure.message),
            hex(failure.help.as_deref().unwrap_or(""))
        ),
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
    let (output, failure, leftovers) = super::finish();
    report_counts(&leftovers.counts);
    (output, failure.map(|failure| sources.render(&failure)).unwrap_or_default())
}

/// With `FENNEC_COUNTS` set, says on stderr what the run allocated and copied, so a test can
/// check that a program doesn't copy.
fn report_counts(counts: &heap::Counts) {
    if std::env::var_os("FENNEC_COUNTS").is_none() {
        return;
    }
    let _ = writeln!(std::io::stderr(), "{}", counts_line(counts));
}

/// The line `report_counts` writes.
pub(super) fn counts_line(counts: &heap::Counts) -> String {
    let line = format!(
        "copies {} copied_bytes {} largest_copy {} allocations {} allocated_bytes {}",
        counts.copies, counts.copied_bytes, counts.largest_copy, counts.allocations, counts.allocated_bytes
    );
    // `parallel` calls native code made, when there were any.
    let line = match super::parallel::calls() {
        0 => line,
        calls => format!("{line} parallel_calls {calls}"),
    };
    // Network waits that blocked a thread rather than parking a task, when there were any.
    #[cfg(any(test, feature = "threads"))]
    let line = match super::serve::blocked_waits() {
        0 => line,
        blocked => format!("{line} blocked_waits {blocked}"),
    };
    line
}

/// Ends the program for a failure where nothing can pass it on, printing what it printed and
/// the failure, as the end of a run would. In anything but an executable, the failure just
/// stays for whoever finishes the run.
pub(super) fn stop_for_failure() {
    let Some(sources) = SOURCES.take() else { return };
    let (output, failure, leftovers) = super::finish();
    report_counts(&leftovers.counts);
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

/// The lowest address native code on this thread may grow its stack to, for a worker of
/// `parallel`'s pool: 0 on any other thread.
pub extern "C" fn stack_limit() -> i64 {
    i64::try_from(super::parallel::stack_limit()).unwrap_or(0)
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
