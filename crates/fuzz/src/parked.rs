//! Parked tasks driven by any sequence of spawning, resuming, closing, failing, cancelling and
//! dropping, each step checked against a model of what every task should do. Some tasks run
//! tasks of their own between their parks. It counts steps, never time, so an input does the
//! same thing every run. A panic in a task stops the process by design, which the crate's own
//! tests check, so tasks here fail by giving back a failure instead.

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

/// What the task the model calls `key` asks for on its `step`th park.
fn request(key: u64, step: u64) -> Request {
    key.wrapping_mul(1_000).wrapping_add(step)
}

fn answer(request: Request) -> u64 {
    request ^ 0x5eed
}

/// What the model expects of a task.
struct Expected {
    key: u64,
    parks: u64,
    fails: bool,
    /// How many times it has parked so far.
    parked: u64,
}

/// How a task the model makes behaves.
#[derive(Clone, Copy)]
struct Plan {
    key: u64,
    parks: u64,
    fails: bool,
    /// How many tasks of its own it runs, each a step further between its own parks.
    inner: u64,
}

/// A task that parks `plan.parks` times, checking each wake is its own answer, and then fails
/// or not. One with inner tasks moves each on a step before each park, and runs them to their
/// end before it ends. What any task was woken with that it didn't expect, or gave back that
/// it shouldn't have, is counted in `wrong`.
fn task(plan: Plan, wrong: Rc<Cell<u32>>) -> impl FnOnce() -> i64 {
    move || {
        let mut inner = Inner::new(plan, &wrong);
        for step in 0..plan.parks {
            inner.step(&wrong);
            let asked = request(plan.key, step);
            match fennec_parked::park(asked) {
                Some(Wake::Ready(CLOSED)) => return SAW_CLOSE,
                Some(Wake::Cancelled) => return SAW_CANCEL,
                Some(Wake::Ready(got)) if got == answer(asked) => {}
                _ => wrong.set(wrong.get() + 1),
            }
        }
        while !inner.parked.is_empty() {
            inner.step(&wrong);
        }
        if plan.fails { FAILED } else { DONE }
    }
}

/// A task's own tasks, each parked on a request.
struct Inner {
    scheduler: Scheduler,
    parked: Vec<(TaskId, Request)>,
}

impl Inner {
    fn new(plan: Plan, wrong: &Rc<Cell<u32>>) -> Inner {
        let mut inner = Inner { scheduler: Scheduler::new(STACK), parked: Vec::new() };
        for at in 0..plan.inner {
            let key = plan.key.wrapping_mul(10).wrapping_add(at + 1);
            let theirs = Plan { key, parks: (plan.key + at) % 4, fails: false, inner: 0 };
            let Ok(made) = inner.scheduler.spawn(task(theirs, Rc::clone(wrong))) else { continue };
            let step = inner.scheduler.resume(made, Wake::Ready(0));
            inner.resumed(made, step, wrong);
        }
        inner
    }

    /// Moves each parked inner task on a step, answering it.
    fn step(&mut self, wrong: &Rc<Cell<u32>>) {
        for (task, asked) in std::mem::take(&mut self.parked) {
            let step = self.scheduler.resume(task, Wake::Ready(answer(asked)));
            self.resumed(task, step, wrong);
        }
    }

    fn resumed(&mut self, task: TaskId, step: Step, wrong: &Rc<Cell<u32>>) {
        match step {
            Step::Parked(asked) => self.parked.push((task, asked)),
            Step::Done(DONE | SAW_CANCEL) => {}
            Step::Done(_) | Step::Unknown => wrong.set(wrong.get() + 1),
        }
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
        let key = u64::try_from(self.made.len()).unwrap_or(0) + 1;
        let plan =
            Plan { key, parks: u64::from(how % 8), fails: how & 0x80 != 0, inner: u64::from((how >> 3) % 4) };
        let made = self.scheduler.spawn(task(plan, Rc::clone(&self.wrong)));
        let made = made.map_err(|error| format!("couldn't make a stack: {error}"))?;
        if self.made.contains(&made) {
            return Err(format!("task number {made} given twice"));
        }
        self.made.push(made);
        self.expected.insert(made, Expected { key, parks: plan.parks, fails: plan.fails, parked: 0 });
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
        let wake = wake
            .unwrap_or_else(|| Wake::Ready(answer(request(expected.key, expected.parked.saturating_sub(1)))));
        let started = expected.parked > 0;
        let step = self.scheduler.resume(task, wake);
        let wanted = match wake {
            Wake::Cancelled => Step::Done(SAW_CANCEL),
            Wake::Ready(CLOSED) if started => Step::Done(SAW_CLOSE),
            Wake::Ready(_) if expected.parked < expected.parks => {
                Step::Parked(request(expected.key, expected.parked))
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
            return Err("a task was woken with another's answer, or an inner task ended wrongly".to_owned());
        }
    }
    if fennec_parked::park(0).is_some() {
        return Err("the fuzzing thread looks like a task after its scheduler's tasks ran".to_owned());
    }
    Ok(())
}
