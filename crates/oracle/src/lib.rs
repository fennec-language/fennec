//! A test oracle for Fennec: it runs a program a second, independent way, by walking its syntax
//! tree with plain Rust values, so a difference from the compiled program exposes a bug in the
//! compiler or the runtime. It's never part of `fennec` itself and never runs a user's program:
//! see the README.
//!
//! It shares the parser and the checker with the compiler, and nothing after them. Anything it
//! doesn't model, it refuses as unsupported rather than guess.

mod interp;
mod kit;
mod load;
pub mod report;
mod text;
mod value;

use std::sync::Arc;

pub use interp::disk::Workdir;
pub use load::{Program, Refused, load, load_source};

/// The stack the oracle runs on: the checker and the evaluator both recurse for each nested
/// expression and call, and printing or comparing a value for each level it nests, 500,000 at
/// most (`interp::MOST_NESTING`). Only what's used of it is ever made.
const STACK_BYTES: usize = 2 << 30;

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// `main` returned, with this exit code.
    Exited(u8),
    /// `main` failed or the program stopped on a bug, with what it says on standard error. Its
    /// exit code is 1.
    Stopped(String),
    /// It reached something the oracle doesn't model, named.
    Unsupported(String),
}

/// What a run printed, and how it ended.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub stdout: Vec<u8>,
    pub ended: Ended,
}

impl Outcome {
    /// The exit code the compiled program would end with, unless the run was unsupported.
    pub fn exit_code(&self) -> Option<u8> {
        match &self.ended {
            Ended::Exited(code) => Some(*code),
            Ended::Stopped(_) => Some(1),
            Ended::Unsupported(_) => None,
        }
    }

    fn unsupported(why: String) -> Outcome {
        Outcome { stdout: Vec::new(), ended: Ended::Unsupported(why) }
    }
}

/// Loads and runs the program at `path`, shown as `shown`, with empty arguments and input, in the
/// folder the oracle runs in, as the gate runs an example. A program that can't be loaded, or the
/// checker refuses, is unsupported.
pub fn run_path(path: &std::path::Path, shown: &str) -> Outcome {
    run_path_in(path, shown, Workdir::current())
}

/// Loads and runs the program at `path`, in `workdir`.
pub fn run_path_in(path: &std::path::Path, shown: &str, workdir: Workdir) -> Outcome {
    let path = path.to_path_buf();
    let shown = shown.to_owned();
    on_a_big_stack(move || match load(&path, &shown) {
        Ok(program) => run_here(program, interp::MOST_STEPS, &workdir),
        Err(Refused::Unsupported(why)) => Outcome::unsupported(why),
        Err(Refused::Invalid(why)) => Outcome::unsupported(format!("it doesn't load: {why}")),
    })
}

/// Runs a program's source, for tests, in the folder the oracle runs in.
pub fn run_source(source: &str, shown: &str) -> Outcome {
    run_source_in(source, shown, Workdir::current())
}

/// Runs a program's source in `workdir`.
pub fn run_source_in(source: &str, shown: &str, workdir: Workdir) -> Outcome {
    run_source_within(source, shown, interp::MOST_STEPS, workdir)
}

/// Runs a program's source in `workdir`, stopping it as unsupported past `most_steps`
/// expressions and statements, which its runs share: one for each order of its tasks' messages.
pub fn run_source_within(source: &str, shown: &str, most_steps: u64, workdir: Workdir) -> Outcome {
    let source = source.to_owned();
    let shown = shown.to_owned();
    on_a_big_stack(move || match load_source(&source, &shown) {
        Ok(program) => run_here(program, most_steps, &workdir),
        Err(Refused::Unsupported(why)) => Outcome::unsupported(why),
        Err(Refused::Invalid(why)) => Outcome::unsupported(format!("it doesn't load: {why}")),
    })
}

fn on_a_big_stack(work: impl FnOnce() -> Outcome + Send + 'static) -> Outcome {
    let thread = std::thread::Builder::new().stack_size(STACK_BYTES).spawn(move || {
        interp::walks_within(STACK_BYTES);
        work()
    });
    match thread.map(std::thread::JoinHandle::join) {
        Ok(Ok(outcome)) => outcome,
        _ => Outcome::unsupported("the oracle itself stopped".to_owned()),
    }
}

/// The most orders of its tasks' messages a run tries, each a run of its own.
const MOST_ORDERS: usize = 256;

/// Runs a loaded program on this thread: once, or, where which of several tasks' messages comes
/// next hangs on time, once for each order, depth first, each of which has to print the same and
/// end the same. The orders share `most_steps`, each run taking what the runs before it left, so
/// a program whose every order is a long run is refused within the one budget, not 256 of them.
fn run_here(program: &'static Program, most_steps: u64, workdir: &Workdir) -> Outcome {
    let mut script = Vec::new();
    let mut first: Option<Outcome> = None;
    let mut left = most_steps;
    for tried in 0..MOST_ORDERS {
        let (outcome, mut made, used) = run_once(program, left, workdir, script, tried > 0);
        if matches!(outcome.ended, Ended::Unsupported(_)) {
            // Out of steps on a later order is the orders' cost together, not a run that's stuck.
            if tried > 0 && used > left {
                return Outcome::unsupported(format!(
                    "ran out of steps across {} orders of its tasks' messages",
                    tried + 1
                ));
            }
            return outcome;
        }
        left = left.saturating_sub(used);
        match &first {
            None => first = Some(outcome),
            Some(seen) if seen.stdout != outcome.stdout || seen.ended != outcome.ended => {
                return Outcome::unsupported(format!(
                    "what it prints, or how it ends, hangs on the order its tasks' messages come in: {}",
                    first_difference(seen, &outcome)
                ));
            }
            Some(_) => {}
        }
        // The next order, depth first: the last choice that has another way, taken that way.
        while let Some((chosen, count)) = made.pop() {
            if chosen + 1 < count {
                made.push((chosen + 1, count));
                break;
            }
        }
        if made.is_empty() {
            return first.unwrap_or_else(|| Outcome::unsupported("no run".to_owned()));
        }
        script = made.into_iter().map(|(chosen, _)| chosen).collect();
    }
    Outcome::unsupported(format!("more than {MOST_ORDERS} orders of its tasks' messages to try"))
}

/// Where two orders' runs part: the first line one prints differently from the other, or how
/// each ended.
fn first_difference(one: &Outcome, other: &Outcome) -> String {
    let lines = |out: &[u8]| String::from_utf8_lossy(out).lines().map(str::to_owned).collect::<Vec<_>>();
    let (mine, theirs) = (lines(&one.stdout), lines(&other.stdout));
    let at = mine.iter().zip(&theirs).take_while(|(a, b)| a == b).count();
    match (mine.get(at), theirs.get(at)) {
        (None, None) => format!("one ends {:?}, another {:?}", one.ended, other.ended),
        (a, b) => {
            let shown =
                |line: Option<&String>| line.map_or_else(|| "nothing".to_owned(), |line| format!("{line:?}"));
            format!("line {} is {} in one order and {} in another", at + 1, shown(a), shown(b))
        }
    }
}

/// One run, making the choices of `script` where messages could come in more than one order,
/// and giving back the choices it made and the steps it took. `quiet` keeps standard error to the
/// first run.
fn run_once(
    program: &'static Program,
    most_steps: u64,
    workdir: &Workdir,
    script: Vec<usize>,
    quiet: bool,
) -> (Outcome, Vec<(usize, usize)>, u64) {
    let shared = Arc::new(interp::Shared::new(program, most_steps, workdir));
    shared.quiet.store(quiet, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut orders) = shared.sched.orders.lock() {
        orders.script = script;
    }
    let mut runner = interp::Interp::new(Arc::clone(&shared), None);
    let ended = match runner.run_main() {
        interp::MainEnded::Returned(value) => match value {
            value::Value::Int(code) => match u8::try_from(code) {
                Ok(code) => Ended::Exited(code),
                Err(_) => Ended::Unsupported("an exit code outside 0 to 255".to_owned()),
            },
            _ => Ended::Exited(0),
        },
        interp::MainEnded::Failed(error, span) => match runner.shown_ending_main(&error) {
            Ok(shown) => Ended::Stopped(said(program, &format!("`main` failed: {shown}"), span)),
            Err(_) => Ended::Unsupported("printing the failure `main` ended with".to_owned()),
        },
        interp::MainEnded::Crashed(message, span) => Ended::Stopped(said(program, &message, span)),
        interp::MainEnded::Unsupported(why) => Ended::Unsupported(why),
    };
    drop(runner);
    let refused = shared.refused.lock().ok().and_then(|refused| refused.clone());
    let stdout = shared.out.lock().map(|out| out.clone()).unwrap_or_default();
    let made =
        shared.sched.orders.lock().map(|mut orders| std::mem::take(&mut orders.made)).unwrap_or_default();
    // A run the oracle couldn't finish is skipped, and what it printed on the way isn't shown.
    let outcome = match (refused, ended) {
        // Every task waits on another where the runtime finds it, which stops the program after
        // what it printed.
        (Some(why), _) | (None, Ended::Unsupported(why)) if why == interp::DEADLOCK => {
            Outcome { stdout, ended: Ended::Stopped(format!("error: {why}\n")) }
        }
        (_, Ended::Unsupported(why)) | (Some(why), _) => Outcome::unsupported(why),
        (None, ended) => Outcome { stdout, ended },
    };
    (outcome, made, shared.steps_taken())
}

/// What the program says on standard error as it stops: the message, pointing at where.
fn said(program: &Program, message: &str, span: fennec_syntax::Span) -> String {
    if span.start >= fennec_syntax::Span::PRELUDE || program.sources.locate(span).is_none() {
        return format!("error: {message}\n");
    }
    program.sources.render(&fennec_syntax::Diagnostic::new(message, span))
}
