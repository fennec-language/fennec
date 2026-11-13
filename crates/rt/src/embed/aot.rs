//! Starting a compiled executable. Its C `main` hands over the program it carries, a word
//! at a time, then calls `run`, which runs the program's native entry point on a thread
//! with a big stack and reports what happened the way `fennec run` does.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::{BufRead, Read, Write};

use fennec_syntax::Sources;

use super::codec::Image;
use super::{State, heap, with};
use crate::fakes::{TestCapabilities, TestWorld};

/// The stack an executable's program runs on, off macOS. It's only reserved, not used, until
/// calls nest deeply.
const STACK_BYTES: usize = 1 << 30;

/// The main thread's stack on macOS, as `fennec build` links it: the most the linker gives on
/// arm64. Native calls may use `NATIVE_STACK_BYTES` of it, which leaves as much for the runtime.
pub const MAIN_STACK_BYTES: usize = 1 << 29;

/// How much of the stack native calls may use, leaving the rest for the runtime.
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
    /// The numbers of the tests a test executable has left to run, the next one last.
    static TESTS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
    /// The numbers of the flags set by the test just run.
    static RAN: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
}

/// The first room the program's bytes get: most programs fit, so they come over with no copying
/// as they grow, which early in a process costs a fresh allocator page each time.
const IMAGE_ROOM: usize = 64 << 10;

/// The next eight bytes of the program the executable carries.
pub extern "C" fn image(word: i64) {
    IMAGE.with_borrow_mut(|bytes| {
        if bytes.capacity() == 0 {
            bytes.reserve(IMAGE_ROOM);
        }
        bytes.extend_from_slice(&word.to_le_bytes());
    });
}

/// Runs the program handed over, `length` bytes of it, by calling `entry` with the lowest
/// address its stack may grow to. Prints what it printed, and the failure if it failed.
/// Gives back the process's exit code.
pub fn run(entry: extern "C" fn(i64), length: i64, stop: fn(i32)) -> i32 {
    let bytes = program_handed_over(length, stop);
    // On macOS `fennec build` links every executable with a main thread of `MAIN_STACK_BYTES`, so
    // the program runs there, with no thread to start. Elsewhere the main thread's stack is the
    // system's, so it gets a thread of its own.
    if cfg!(target_os = "macos") {
        return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(entry, &bytes))) {
            Ok((output, failure)) => report(&output, &failure),
            Err(_) => report(b"", "error: the program stopped unexpectedly\n"),
        };
    }
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

/// The program handed over, `length` bytes of it, ready for workers to start from. They share it
/// with this thread, so it's never copied.
fn program_handed_over(length: i64, stop: fn(i32)) -> std::sync::Arc<Vec<u8>> {
    let _ = STOP.set(stop);
    super::write_kept_output_on_panic();
    let mut bytes = IMAGE.take();
    bytes.truncate(usize::try_from(length).unwrap_or(0));
    let bytes = std::sync::Arc::new(bytes);
    // Each worker `parallel` runs on starts a runtime of its own, holding the program.
    let program = std::sync::Arc::clone(&bytes);
    super::parallel::set_worker_setup(Box::new(move || {
        if let Some(Image { sources, tables, .. }) = Image::from_bytes(&program) {
            // What it prints goes straight out: `parallel`'s output follows real call order.
            super::start(tables, live_inputs());
            heap::share_abandons();
            // A worker can end the program itself, and has to say where.
            SOURCES.set(Some(sources));
        }
    }));
    bytes
}

/// The process's arguments, its own name first. Rust's library reads them where the C library
/// hands them over, which musl does only for Rust's own `main`, and a Fennec program's `main` is
/// its own: so a program linked on its own with musl, as every Linux build from a release is,
/// reads them where Linux keeps them instead.
fn arguments() -> Vec<String> {
    let given: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    if given.is_empty()
        && let Ok(line) = std::fs::read("/proc/self/cmdline")
    {
        let line = line.strip_suffix(&[0]).unwrap_or(&line);
        return line
            .split(|byte| *byte == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect();
    }
    given
}

/// What the process was handed from outside, for a thread of it to run with: every thread sees
/// the same arguments and reads the same standard input.
fn live_inputs() -> crate::Inputs {
    let mut arguments = arguments().into_iter();
    crate::Inputs {
        live: true,
        program: program_name(arguments.next().unwrap_or_default()),
        arguments: arguments.collect(),
        stdin: Box::new(StdinLines::default()),
    }
}

/// What `args.program` says. `fennec run` starts a program it built for the run, and where it can't
/// set the name a process sees, it says it this way, for that one executable: what the program
/// starts in turn gets the variable too, and goes by its own name.
fn program_name(own: String) -> String {
    let told = std::env::var("FENNEC_RUN_AS").ok();
    match told.as_deref().and_then(|told| told.split_once('|')) {
        Some((executable, program)) if executable == own => program.to_owned(),
        _ => own,
    }
}

/// The process's standard input for one thread. It takes a line at a time, so a thread never holds
/// more than the line it's reading, and no line is lost to the threads reading after it.
#[derive(Default)]
pub(super) struct StdinLines {
    line: Vec<u8>,
    read: usize,
}

impl Read for StdinLines {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.fill_buf()?.read(bytes)?;
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for StdinLines {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.read >= self.line.len() {
            self.line.clear();
            self.read = 0;
            std::io::stdin().lock().read_until(b'\n', &mut self.line)?;
        }
        Ok(self.line.get(self.read..).unwrap_or_default())
    }

    fn consume(&mut self, count: usize) {
        self.read += count;
    }
}

/// Starts running a test executable's tests: those whose numbers are its arguments, in turn. A
/// failure stays for `test_done` to say, rather than ending the program.
pub fn tests_start() {
    SOURCES.take();
    let mut numbers: Vec<i64> =
        arguments().into_iter().skip(1).filter_map(|number| number.parse().ok()).collect();
    numbers.reverse();
    TESTS.set(numbers);
}

/// The number of the next test to run, or -1 when they've all run.
pub fn next_test_number() -> i64 {
    TESTS.with_borrow_mut(Vec::pop).unwrap_or(-1)
}

/// Notes that the test just run set the flag with this number: its function ran.
pub fn test_ran(flag: i64) {
    RAN.with_borrow_mut(|ran| ran.push(flag));
}

thread_local! {
    /// Whether an attempt of a property test's shrinking failed since it was last asked.
    static ATTEMPT_FAILED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// What a failed property test's simplest failing input was, as text.
    static FAILED_FOR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// `fennec_aot_attempt_failed()`: an attempt of a property test's shrinking failed, so its failure
/// is cleared, as one test's is before the next, and noted for `attempt_passed`.
pub fn attempt_failed() {
    let _ = super::next_test();
    ATTEMPT_FAILED.set(true);
}

/// `fennec_aot_attempt_passed()`: whether the attempt just made passed, starting the next afresh.
pub fn attempt_passed() -> i8 {
    i8::from(!ATTEMPT_FAILED.replace(false))
}

/// `fennec_aot_failed_for(text)`: what the property test failing next was given, for its report.
pub fn failed_for(text: i64) {
    let given = with(|state: &mut State| {
        u64::try_from(text)
            .ok()
            .and_then(|address| state.heap.block(address))
            .filter(|block| heap::is_text(state, block))
            .map(heap::block_text)
    });
    FAILED_FOR.set(given);
}

/// Starts a test: one that takes capabilities gets fakes in a world of its own, with nothing left
/// by the one before.
pub fn test_world(fakes: i64) {
    crate::stdlib::net::labels_start_over();
    with(|state: &mut State| {
        state.world = (fakes != 0 && !real_tests())
            .then(|| Box::new(TestWorld::new(TestCapabilities::Fakes)) as Box<dyn std::any::Any>);
    });
}

/// Whether `fennec test --real` runs these tests, with the real capabilities rather than fakes:
/// what they print then goes out as it is, and each test's line goes to standard error.
fn real_tests() -> bool {
    static REAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REAL.get_or_init(|| std::env::var_os("FENNEC_TESTS_REAL").is_some())
}

/// Says what the test just run did, on standard output as soon as it's done, in a line `fennec
/// test` reads, so a crash loses only the tests not yet run. A test's `Stdout` is a fake, so
/// nothing else writes there.
pub fn test_done(number: i64) {
    let ran: Vec<String> = RAN.take().iter().map(ToString::to_string).collect();
    let failure = super::next_test().map(|failure| match FAILED_FOR.take() {
        // What a property test that failed was given, once shrinking found the simplest.
        Some(given) => {
            let help = match &failure.help {
                Some(help) => format!("it failed for {given}; {help}"),
                None => format!("it failed for {given}"),
            };
            fennec_syntax::Diagnostic { help: Some(help), ..failure }
        }
        None => failure,
    });
    FAILED_FOR.take();
    // The test's fakes go now rather than with the next test's, so the last test's go too, and
    // what they hold outside the program, like a test's databases, is cleaned up.
    let world = with(|state: &mut State| state.world.take());
    drop(world);
    let trace = super::take_trace();
    let line = test_line(number, failure.as_ref().map(|failure| (failure, &trace)), &ran);
    if real_tests() {
        // What the test printed comes first.
        super::write_kept_output();
        let mut said = std::io::stderr();
        let _ = writeln!(said, "{line}").and_then(|()| said.flush());
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

/// What a test did, as a line: its number, and what it ran if it passed, as the numbers of the
/// flags that were set, or for a failure where it is, its message and its help, in hexadecimal so
/// that anything can be in them, and its call chain.
fn test_line(
    number: i64,
    failure: Option<(&fennec_syntax::Diagnostic, &fennec_syntax::Trace)>,
    ran: &[String],
) -> String {
    let hex = |text: &str| {
        let mut hex = String::with_capacity(text.len() * 2);
        for byte in text.bytes() {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    };
    match failure {
        None => format!("{number} ok {}", ran.join(",")),
        Some((failure, trace)) => format!(
            "{number} failed {} {} {} {} {}",
            failure.span.start,
            failure.span.end,
            hex(&failure.message),
            hex(failure.help.as_deref().unwrap_or("")),
            trace.encode()
        ),
    }
}

/// Runs the program, on the current thread: what it printed, and its failure, rendered,
/// or nothing.
fn execute(entry: extern "C" fn(i64), bytes: &[u8]) -> (Vec<u8>, String) {
    let Some(Image { sources, tables, literals }) = Image::from_bytes(bytes) else {
        return (Vec::new(), "error: this executable's program is damaged\n".to_owned());
    };
    super::start(tables, live_inputs());
    heap::share_abandons();
    LITERALS.set(literals);
    SOURCES.set(Some(sources.clone()));
    let here = 0_u8;
    let limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
    // The runtime has as much again below native code's part: a read into a tree may use most.
    heap::reads_may_reach(limit.saturating_sub(NATIVE_STACK_BYTES / 4 * 3));
    entry(i64::try_from(limit).unwrap_or(0));
    let (output, failure, leftovers) = super::finish_for_exit();
    crate::replay::ended();
    // A replay that ended with its recording not all read isn't the run that was recorded.
    let failure = failure.or_else(|| {
        crate::replay::unfinished()
            .map(|why| fennec_syntax::Diagnostic::new(why, fennec_syntax::Span::default()))
    });
    report_counts(&leftovers.counts, leftovers.boxes);
    (output, failure.map(|failure| rendered(&sources, &failure)).unwrap_or_default())
}

/// A failure as it's shown when it ends the program: with its call chain under it.
fn rendered(sources: &Sources, failure: &fennec_syntax::Diagnostic) -> String {
    let trace = super::take_trace();
    sources.render(failure) + &sources.render_trace(failure.span, &trace)
}

/// Native code built to trace what it costs is about to do what's at this place in the source.
pub fn trace_at(site: i64) {
    with(|state: &mut State| state.heap.trace_at(u32::try_from(site).unwrap_or(0)));
}

/// Whether this run counts what it allocates: under `FENNEC_COUNTS`, after `count_allocations`,
/// and in tests. Native code that makes blocks of its own reads it once, and says what it made
/// through `trace_allocated` only when it's 1, so a plain run pays nothing to count.
pub fn counting() -> i64 {
    with(|state: &mut State| i64::from(state.heap.counting()))
}

/// Native code made a block of its own, of this many bytes: one built to trace what it costs, or
/// one making blocks outside the runtime's table while `counting` says 1.
pub fn trace_allocated(bytes: i64) {
    with(|state: &mut State| state.heap.allocated(u64::try_from(bytes).unwrap_or(0)));
}

/// The native blocks made and not yet freed, in a run that counts: on any thread, as a block made
/// in a task is freed there. Native blocks aren't in the runtime's table, so without this one left
/// held, or a whole tree of them, would still end a run with `live 0`.
static NATIVE_LIVE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// How many native blocks a run that counts has made and not freed.
fn native_left() -> usize {
    usize::try_from(NATIVE_LIVE.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0)
}

/// Native code made (`1`) or freed (`-1`) a block of its own while `counting` says 1.
pub fn native_live(change: i64) {
    NATIVE_LIVE.fetch_add(change, std::sync::atomic::Ordering::Relaxed);
}

/// With `FENNEC_TRACE_FILE` set, writes what each place in the source cost there, for
/// `fennec dev trace` to read: a line of numbers for each place.
pub(super) fn report_sites(sites: &std::collections::HashMap<u32, heap::Counts>) {
    let Some(file) = std::env::var_os("FENNEC_TRACE_FILE") else { return };
    let mut lines = String::new();
    for (site, counts) in sites {
        let _ = writeln!(
            lines,
            "{site} {} {} {} {} {}",
            counts.allocations,
            counts.allocated_bytes,
            counts.copies,
            counts.copied_bytes,
            counts.largest_copy
        );
    }
    let _ = std::fs::write(file, lines);
}

/// With `FENNEC_COUNTS` set, says on stderr what the run allocated and copied, so a test can
/// check that a program doesn't copy, and how many blocks were still held when it ended, so a
/// test can check that it lets go of what it made.
fn report_counts(counts: &heap::Counts, live: usize) {
    if std::env::var_os("FENNEC_COUNTS").is_none() {
        return;
    }
    #[cfg(any(test, feature = "threads"))]
    let blocked = super::serve::blocked_waits();
    #[cfg(not(any(test, feature = "threads")))]
    let blocked = 0;
    // The main thread's, with every worker's, whose tasks the main thread's counts never see.
    let mut all = *counts;
    all.add(&heap::worker_counts());
    let said = counts_line(&all, super::parallel::calls(), blocked);
    let said = match super::dicts::found_puts() {
        0 => said,
        puts => format!("{said} found_puts {puts}"),
    };
    let said = match crate::stdlib::net::connection_writes() {
        0 => said,
        writes => format!("{said} connection_writes {writes}"),
    };
    let said = match heap::strays() {
        0 => said,
        strays => format!("{said} stray_frees {strays}"),
    };
    // The runtime's own blocks, and native code's, which its table doesn't hold.
    let _ = writeln!(std::io::stderr(), "{said} live {}", live + native_left());
}

/// The line `report_counts` writes, given how many `parallel` calls native code made and how many
/// network waits blocked a thread rather than parking a task. Those are kept for the whole process,
/// so they're passed in, for a test to say what they are.
pub(super) fn counts_line(counts: &heap::Counts, parallel_calls: u64, blocked_waits: u64) -> String {
    let line = format!(
        "copies {} copied_bytes {} largest_copy {} allocations {} allocated_bytes {}",
        counts.copies, counts.copied_bytes, counts.largest_copy, counts.allocations, counts.allocated_bytes
    );
    let line = match parallel_calls {
        0 => line,
        calls => format!("{line} parallel_calls {calls}"),
    };
    match blocked_waits {
        0 => line,
        blocked => format!("{line} blocked_waits {blocked}"),
    }
}

/// Ends the program at once with `code`.
pub(crate) fn stop_now(code: i32) -> ! {
    crate::replay::ended();
    if let Some(stop) = STOP.get() {
        stop(code);
    }
    std::process::abort()
}

/// Ends the program for a failure where nothing can pass it on, printing what it printed and
/// the failure, as the end of a run would. In anything but an executable, the failure just
/// stays for whoever finishes the run.
pub(super) fn stop_for_failure() {
    let Some(sources) = SOURCES.take() else { return };
    let (output, failure, leftovers) = super::finish_for_exit();
    report_counts(&leftovers.counts, leftovers.boxes);
    let failure = failure.map(|failure| rendered(&sources, &failure)).unwrap_or_default();
    let code = report(&output, &failure);
    if let Some(stop) = STOP.get() {
        stop(code);
    }
}

/// Prints a run's output and, if there is one, its failure; gives back the exit code.
fn report(output: &[u8], failure: &str) -> i32 {
    let printed = std::io::stdout().write_all(output).and_then(|()| std::io::stdout().flush());
    if failure.is_empty() && printed.is_ok() {
        // What `main` gave back, when it gave an int.
        return i32::from(super::take_exit_code());
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

#[cfg(test)]
mod tests {
    /// A native block made and not freed is one left held, wherever it was made, and none is once
    /// it's freed: what `live` adds to the runtime's own blocks.
    #[test]
    fn native_blocks_made_and_not_freed_are_left() {
        let before = super::native_left();
        super::native_live(1);
        std::thread::spawn(|| super::native_live(1)).join().unwrap_or(());
        assert_eq!(super::native_left(), before + 2);
        super::native_live(-1);
        super::native_live(-1);
        assert_eq!(super::native_left(), before);
    }
}
