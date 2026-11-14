//! A test oracle for Fennec: it runs a program a second, independent way, by walking its syntax
//! tree with plain Rust values, so a difference from the compiled program exposes a bug in the
//! compiler or the runtime. It's never part of `fennec` itself and never runs a user's program:
//! see the README.
//!
//! It shares the parser and the checker with the compiler, and nothing after them. Anything it
//! doesn't model, it refuses as unsupported rather than guess.

mod interp;
mod load;
pub mod report;
mod text;
mod value;

use std::sync::Arc;

pub use load::{Program, Refused, load, load_source};

/// The stack the oracle runs on: the checker and the evaluator both recurse for each nested
/// expression and call.
const STACK_BYTES: usize = 1 << 30;

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

/// Loads and runs the program at `path`, shown as `shown`, with empty arguments and input. A
/// program that can't be loaded, or the checker refuses, is unsupported.
pub fn run_path(path: &std::path::Path, shown: &str) -> Outcome {
    let path = path.to_path_buf();
    let shown = shown.to_owned();
    on_a_big_stack(move || match load(&path, &shown) {
        Ok(program) => run_here(program, interp::MOST_STEPS),
        Err(Refused::Unsupported(why)) => Outcome::unsupported(why),
        Err(Refused::Invalid(why)) => Outcome::unsupported(format!("it doesn't load: {why}")),
    })
}

/// Runs a program's source, for tests.
pub fn run_source(source: &str, shown: &str) -> Outcome {
    run_source_within(source, shown, interp::MOST_STEPS)
}

/// Runs a program's source, stopping it as unsupported past `most_steps` expressions and
/// statements, which every run has a limit of.
pub fn run_source_within(source: &str, shown: &str, most_steps: u64) -> Outcome {
    let source = source.to_owned();
    let shown = shown.to_owned();
    on_a_big_stack(move || match load_source(&source, &shown) {
        Ok(program) => run_here(program, most_steps),
        Err(Refused::Unsupported(why)) => Outcome::unsupported(why),
        Err(Refused::Invalid(why)) => Outcome::unsupported(format!("it doesn't load: {why}")),
    })
}

fn on_a_big_stack(work: impl FnOnce() -> Outcome + Send + 'static) -> Outcome {
    let thread = std::thread::Builder::new().stack_size(STACK_BYTES).spawn(work);
    match thread.map(std::thread::JoinHandle::join) {
        Ok(Ok(outcome)) => outcome,
        _ => Outcome::unsupported("the oracle itself stopped".to_owned()),
    }
}

/// Runs a loaded program on this thread.
fn run_here(program: &'static Program, most_steps: u64) -> Outcome {
    let shared = Arc::new(interp::Shared::new(program, most_steps));
    let mut runner = interp::Interp::new(Arc::clone(&shared), None);
    let ended = match runner.run_main() {
        interp::MainEnded::Returned(value) => match value {
            value::Value::Int(code) => match u8::try_from(code) {
                Ok(code) => Ended::Exited(code),
                Err(_) => Ended::Unsupported("an exit code outside 0 to 255".to_owned()),
            },
            _ => Ended::Exited(0),
        },
        interp::MainEnded::Failed(error, span) => match runner.repr(&error) {
            Ok(shown) => Ended::Stopped(said(program, &format!("`main` failed: {shown}"), span)),
            Err(_) => Ended::Unsupported("printing the failure `main` ended with".to_owned()),
        },
        interp::MainEnded::Crashed(message, span) => Ended::Stopped(said(program, &message, span)),
        interp::MainEnded::Unsupported(why) => Ended::Unsupported(why),
    };
    drop(runner);
    let refused = shared.refused.lock().ok().and_then(|refused| refused.clone());
    let stdout = shared.out.lock().map(|out| out.clone()).unwrap_or_default();
    // A run the oracle couldn't finish is skipped, and what it printed on the way isn't shown.
    match (refused, ended) {
        (_, Ended::Unsupported(why)) | (Some(why), _) => Outcome::unsupported(why),
        (None, ended) => Outcome { stdout, ended },
    }
}

/// What the program says on standard error as it stops: the message, pointing at where.
fn said(program: &Program, message: &str, span: fennec_syntax::Span) -> String {
    if span.start >= fennec_syntax::Span::PRELUDE || span.end as usize > program.source.len() {
        return format!("error: {message}\n");
    }
    fennec_syntax::Diagnostic::new(message, span).render(&program.path, &program.source)
}
