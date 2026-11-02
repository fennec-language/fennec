//! Parked tasks driven by any sequence of spawning, resuming, closing, failing, cancelling and
//! dropping, each step checked against a model of what every task should do. It counts steps,
//! never time, so an input does the same thing every run.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use fennec_parked::{Request, Scheduler, Step, TaskId, Wake};

/// Small stacks, as servers use.
const STACK: usize = 64 * 1024;

/// A wake meaning the task's connection closed.
const CLOSED: u64 = u64::MAX;

/// What a task gives back: done, failed, saw its connection close, or was cancelled.
const DONE: i64 = 0;
const FAILED: i64 = 1;
const SAW_CLOSE: i64 = 2;
const SAW_CANCEL: i64 = 3;

fn request(task: TaskId, step: u64) -> Request {
    task.wrapping_mul(1_000).wrapping_add(step)
}

fn answer(request: Request) -> u64 {
    request ^ 0x5eed
}

/// What the model expects of a task.
struct Expected {
    parks: u64,
    fails: bool,
    /// How many times it has parked so far.
    parked: u64,
}

/// A task that parks `parks` times, checking each wake is its own answer, and then fails or
/// not. What it was woken with that it didn't expect is counted in `wrong`.
fn task(task: TaskId, parks: u64, fails: bool, wrong: Rc<Cell<u32>>) -> impl FnOnce() -> i64 {
    move || {
        for step in 0..parks {
            let asked = request(task, step);
            match fennec_parked::park(asked) {
                Some(Wake::Ready(CLOSED)) => return SAW_CLOSE,
                Some(Wake::Cancelled) => return SAW_CANCEL,
                Some(Wake::Ready(got)) if got == answer(asked) => {}
                _ => wrong.set(wrong.get() + 1),
            }
        }
        if fails { FAILED } else { DONE }
    }
}

struct Run {
    scheduler: Scheduler,
    expected: HashMap<TaskId, Expected>,
    /// Every task ever made, ended or not, so resuming an ended one is tried too.
    made: Vec<TaskId>,
    wrong: Rc<Cell<u32>>,
}

impl Run {
    fn new() -> Run {
        Run {
            scheduler: Scheduler::new(STACK),
            expected: HashMap::new(),
            made: Vec::new(),
            wrong: Rc::default(),
        }
    }

    fn spawn(&mut self, how: u8) -> Result<(), String> {
        if self.expected.len() >= 64 {
            return Ok(());
        }
        let (parks, fails) = (u64::from(how % 8), how & 0x80 != 0);
        // Each scheduler numbers its own tasks, so the number is known only once it's made.
        let next = self.made.iter().max().map_or(1, |last| last + 1);
        let made = self.scheduler.spawn(task(next, parks, fails, Rc::clone(&self.wrong)));
        let made = made.map_err(|error| format!("couldn't make a stack: {error}"))?;
        if made != next {
            return Err(format!("expected task {next}, made {made}"));
        }
        self.made.push(made);
        self.expected.insert(made, Expected { parks, fails, parked: 0 });
        Ok(())
    }

    /// Resumes a task with `wake`, or with its answer when `wake` is `None`, and checks what it
    /// does.
    fn resume(&mut self, pick: u8, wake: Option<Wake>) -> Result<(), String> {
        let Some(task) = self.made.get(usize::from(pick) % self.made.len().max(1)).copied() else {
            return Ok(());
        };
        let Some(expected) = self.expected.get_mut(&task) else {
            let step = self.scheduler.resume(task, Wake::Ready(0));
            return if step == Step::Unknown {
                Ok(())
            } else {
                Err(format!("ended task {task} gave {step:?}"))
            };
        };
        // A task that hasn't parked yet is starting, and its first wake isn't read.
        let wake =
            wake.unwrap_or_else(|| Wake::Ready(answer(request(task, expected.parked.saturating_sub(1)))));
        let started = expected.parked > 0;
        let step = self.scheduler.resume(task, wake);
        let wanted = match wake {
            Wake::Cancelled => Step::Done(SAW_CANCEL),
            Wake::Ready(CLOSED) if started => Step::Done(SAW_CLOSE),
            Wake::Ready(_) if expected.parked < expected.parks => {
                Step::Parked(request(task, expected.parked))
            }
            Wake::Ready(_) if expected.fails => Step::Done(FAILED),
            Wake::Ready(_) => Step::Done(DONE),
        };
        // A cancelled task that never started runs from the start, told it's cancelled when it
        // first parks, unless it never parks.
        let wanted = match (wanted, started, expected.parks) {
            (Step::Done(SAW_CANCEL), false, 0) => Step::Done(if expected.fails { FAILED } else { DONE }),
            (wanted, ..) => wanted,
        };
        if step != wanted {
            return Err(format!("task {task} woken with {wake:?} gave {step:?}, not {wanted:?}"));
        }
        match step {
            Step::Parked(_) => expected.parked += 1,
            _ => {
                self.expected.remove(&task);
            }
        }
        Ok(())
    }

    /// Drops the scheduler, which cancels what's left, and starts another.
    fn restart(&mut self) {
        *self = Run::new();
    }
}

pub(crate) fn parked(input: &[u8]) -> Result<(), String> {
    let mut run = Run::new();
    let mut bytes = input.iter().copied().take(4_096);
    while let (Some(op), Some(arg)) = (bytes.next(), bytes.next()) {
        match op % 5 {
            0 => run.spawn(arg)?,
            1 => run.resume(arg, None)?,
            2 => run.resume(arg, Some(Wake::Ready(CLOSED)))?,
            3 => run.resume(arg, Some(Wake::Cancelled))?,
            _ => {
                if arg % 8 == 0 {
                    run.restart();
                }
            }
        }
        if run.wrong.get() != 0 {
            return Err("a task was woken with another's answer".to_owned());
        }
    }
    if fennec_parked::park(0).is_some() {
        return Err("the fuzzing thread looks like a task after its scheduler's tasks ran".to_owned());
    }
    Ok(())
}
