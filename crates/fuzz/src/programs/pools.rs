//! `pooled{id}(stdout)`: a pool from `std.concurrent`, `pool[T](open=..., size=...)`, of up to
//! three made-up links, each kept by a keeper task and lent to one `with` block at a time. `main`
//! takes them, nested up to the pool's size. Each block prints the number of the link it got, and
//! some fail once their inner blocks are done. A launched task takes one and crashes with it. Once
//! every link is held, a take with a short wait times out. Opening a link is refused on one
//! attempt, which that take fails with, and a pool whose first link is refused fails at its
//! `with`. Links whose reset fails are closed after a failed block, and opened again when next
//! needed.
//!
//! Only `main` and a task it waits for take links, so which link each take gets is what
//! `std.concurrent` promises: values given back are lent again in the order they were given back,
//! and one whose block failed comes after every free one that's open. Which empty keeper a take
//! reaches isn't promised, as keepers start at once, so a pool of more than one link has every
//! empty keeper alike: either no block fails, or no link survives a reset. A short wait comes only
//! where every link is held, so nothing can come free in it, and a take with the pool's own wait
//! only where one can.
//!
//! What each step prints is worked out here, from a model of the pool's keepers.

use std::collections::VecDeque;
use std::fmt::Write as _;

/// A step `main` takes with the pool, at some depth of nested takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `with links.take()? as held:`, printing the link's number, then the inner steps, then
    /// failing with `Broke` where `breaks` says.
    Take { inner: Vec<Step>, breaks: bool },
    /// A take with a 5 ms wait, where every link is held: it times out.
    Waits,
    /// A launched task takes a link and crashes with it.
    Crashes,
}

/// One `Pooled` statement.
#[derive(Clone, Debug)]
pub(crate) struct Pooling {
    pub(crate) id: usize,
    /// How many links the pool keeps, 1 to 3.
    pub(crate) size: usize,
    /// Whether a link's `reset()` puts it back as it was after a failed block.
    pub(crate) resets: bool,
    /// The attempt to open a link that's refused, counting from 1, or 0 for none.
    pub(crate) refuse: i64,
    pub(crate) steps: Vec<Step>,
}

/// The steps at `depth` takes deep: a take with a short wait where every link is held, and
/// otherwise takes, nested no deeper than the pool's size, and crashes where `failing` allows.
fn steps_picked(
    depth: usize,
    size: usize,
    failing: bool,
    choose: &mut impl FnMut(usize) -> usize,
) -> Vec<Step> {
    let count = if depth == 0 { 2 + choose(4) } else { choose(3) };
    let mut steps = Vec::with_capacity(count);
    for _ in 0..count {
        if depth == size {
            steps.push(Step::Waits);
            continue;
        }
        if failing && choose(4) == 0 {
            steps.push(Step::Crashes);
            continue;
        }
        let breaks = failing && choose(3) == 0;
        let inner = steps_picked(depth + 1, size, failing, choose);
        steps.push(Step::Take { inner, breaks });
    }
    steps
}

/// A `Pooled` statement: 1 to 3 links, resetting or not, one attempt to open in three refused,
/// and 2 to 5 steps, each taking up to the pool's size deep.
pub(crate) fn picked(id: usize, choose: &mut impl FnMut(usize) -> usize) -> Pooling {
    let size = 1 + choose(3);
    let resets = choose(3) != 0;
    let refuse = if choose(3) == 0 { 1 + i64::try_from(choose(5)).unwrap_or(0) } else { 0 };
    // A block fails, or a task crashes, only where every empty keeper is alike.
    let failing = size == 1 || !resets;
    let steps = steps_picked(0, size, failing, choose);
    Pooling { id, size, resets, refuse, steps }
}

/// Why a take failed, as `std.concurrent`'s `PoolError` says.
enum Refusal {
    TimedOut,
    Open(String),
}

/// The pool, as its dispatcher and keepers have it.
struct Model {
    resets: bool,
    refuse: i64,
    /// How many times a link has been opened, refused ones among them.
    tried: i64,
    /// The link each keeper holds open, by its number.
    held: Vec<Option<i64>>,
    /// Keepers free with a link open, and free with none or after a failed block, in the order
    /// the dispatcher heard of them.
    open_free: VecDeque<usize>,
    empty_free: VecDeque<usize>,
}

impl Model {
    /// The next link's number, or why it couldn't be opened.
    fn opened(&mut self) -> Result<i64, String> {
        self.tried += 1;
        if self.tried == self.refuse {
            return Err(format!("refused {}", self.tried));
        }
        Ok(self.tried)
    }

    /// The keeper a take is lent from, and the link's number.
    fn take(&mut self) -> Result<(usize, i64), Refusal> {
        if let Some(keeper) = self.open_free.pop_front() {
            let number = self.held.get(keeper).copied().flatten().unwrap_or(0);
            return Ok((keeper, number));
        }
        let Some(keeper) = self.empty_free.pop_front() else { return Err(Refusal::TimedOut) };
        if let Some(number) = self.held.get(keeper).copied().flatten() {
            return Ok((keeper, number));
        }
        match self.opened() {
            Ok(number) => {
                if let Some(held) = self.held.get_mut(keeper) {
                    *held = Some(number);
                }
                Ok((keeper, number))
            }
            Err(reason) => {
                // The keeper says it's free, with none open, once it's told the borrower why.
                self.empty_free.push_back(keeper);
                Err(Refusal::Open(reason))
            }
        }
    }

    /// A link given back to its keeper: after a failed block, reset or closed.
    fn given_back(&mut self, keeper: usize, succeeded: bool) {
        if succeeded {
            self.open_free.push_back(keeper);
            return;
        }
        if !self.resets
            && let Some(held) = self.held.get_mut(keeper)
        {
            *held = None;
        }
        self.empty_free.push_back(keeper);
    }

    fn refused(refusal: &Refusal, lines: &mut Vec<String>) {
        lines.push(match refusal {
            Refusal::TimedOut => "timed out".to_owned(),
            Refusal::Open(reason) => format!("open failed {reason}"),
        });
    }

    fn step(&mut self, step: &Step, lines: &mut Vec<String>) {
        match step {
            Step::Take { inner, breaks } => match self.take() {
                Ok((keeper, number)) => {
                    lines.push(format!("took {number}"));
                    for step in inner {
                        self.step(step, lines);
                    }
                    self.given_back(keeper, !breaks);
                    if *breaks {
                        lines.push("broke".to_owned());
                    }
                }
                Err(refusal) => Self::refused(&refusal, lines),
            },
            Step::Waits => match self.take() {
                // Every link is held, so this is never lent one.
                Ok((keeper, number)) => {
                    lines.push(format!("took {number}"));
                    self.given_back(keeper, true);
                }
                Err(refusal) => Self::refused(&refusal, lines),
            },
            Step::Crashes => match self.take() {
                Ok((keeper, number)) => {
                    lines.push(format!("task took {number}"));
                    self.given_back(keeper, false);
                    lines.push("crashed".to_owned());
                }
                Err(refusal) => Self::refused(&refusal, lines),
            },
        }
    }
}

/// What `pooled{id}` prints.
pub(crate) fn printed(pooling: &Pooling) -> Vec<String> {
    let mut model = Model {
        resets: pooling.resets,
        refuse: pooling.refuse,
        tried: 0,
        held: vec![None; pooling.size],
        open_free: VecDeque::new(),
        empty_free: (1..pooling.size).collect(),
    };
    let mut lines = Vec::new();
    // The pool opens its first link at its `with`, for keeper 0, the first one lent.
    match model.opened() {
        Ok(number) => {
            if let Some(held) = model.held.first_mut() {
                *held = Some(number);
            }
            model.open_free.push_back(0);
            for step in &pooling.steps {
                model.step(step, &mut lines);
            }
        }
        // A pool whose first link is refused fails at its `with`, with the same `PoolError`.
        Err(reason) => lines.push(format!("open failed {reason}")),
    }
    lines.push(format!("tried {}", model.tried));
    lines
}

/// The cases a call handling a `PoolError` writes, at `indent`, each printing what the model says.
/// Where `PoolError` is all the call fails with, its type is known, so its cases start with a dot.
fn pool_cases(out: &mut String, indent: &str, alone: bool) {
    let named = if alone { "" } else { "PoolError" };
    let _ = write!(
        out,
        "{indent}case {named}.Timeout():\n{indent}    stdout.print(\"timed out\")\n{indent}case \
         {named}.Open(reason=reason):\n{indent}    stdout.print(\"open failed\", reason)\n{indent}case \
         {named}.Closed:\n{indent}    stdout.print(\"closed\")\n"
    );
}

/// The functions each step calls, numbering them from `made`, and the calls `steps` make, at
/// `indent`, into `body`.
fn steps_written(
    id: usize,
    steps: &[Step],
    indent: &str,
    made: &mut usize,
    helpers: &mut String,
    body: &mut String,
) {
    for step in steps {
        *made += 1;
        let at = *made;
        match step {
            Step::Take { inner, breaks } => {
                let mut block = String::new();
                steps_written(id, inner, "        ", made, helpers, &mut block);
                if *breaks {
                    let _ = writeln!(block, "        fail Broke{id}(code={at})");
                }
                let _ = write!(
                    helpers,
                    "def took{id}_{at}(links: Pool[Link{id}], stdout: Stdout) fails PoolError | Broke{id}:\n    \
                     with links.take()? as held:\n        stdout.print(\"took\", held.number)\n{block}\n"
                );
                let _ = writeln!(body, "{indent}took{id}_{at}(links, stdout) except why:");
                pool_cases(body, &format!("{indent}    "), false);
                let _ =
                    writeln!(body, "{indent}    case Broke{id}():\n{indent}        stdout.print(\"broke\")");
            }
            Step::Waits => {
                let _ = write!(
                    helpers,
                    "def waited{id}_{at}(links: Pool[Link{id}], stdout: Stdout) fails PoolError:\n    with \
                     links.take(wait=5ms)? as held:\n        stdout.print(\"took\", held.number)\n\n"
                );
                let _ = writeln!(body, "{indent}waited{id}_{at}(links, stdout) except why:");
                pool_cases(body, &format!("{indent}    "), true);
            }
            Step::Crashes => {
                let _ = write!(
                    helpers,
                    "def crasher{id}_{at}(links: Pool[Link{id}], stdout: Stdout) fails PoolError:\n    with \
                     links.take()? as held:\n        stdout.print(\"task took\", held.number)\n        numbers = \
                     [held.number]\n        stdout.print(numbers[held.number])\n\ndef crashing{id}_{at}(links: \
                     Pool[Link{id}], stdout: Stdout) fails PoolError | Crashed:\n    with launch(() => \
                     crasher{id}_{at}(links, stdout)?)?:\n        pass\n\n"
                );
                let _ = writeln!(
                    body,
                    "{indent}crashing{id}_{at}(links, stdout) except why:\n{indent}    case Crashed():\n{indent}        \
                     stdout.print(\"crashed\")"
                );
                pool_cases(body, &format!("{indent}    "), false);
            }
        }
    }
}

/// The link, its failures, a function for each step, and `pooled{id}` itself.
pub(crate) fn written(pooling: &Pooling) -> String {
    let Pooling { id, size, resets, refuse, steps } = pooling;
    let id = *id;
    let mut out = format!(
        "struct Link{id}:\n    number: int\n    resets: bool\n\n    def reset(self) -> bool:\n        return \
         self.resets\n\n    def close(self):\n        pass\n\nerror Refused{id}:\n    why: str\n\n    def \
         message(self) -> str:\n        return self.why\n\nerror Broke{id}:\n    code: int\n\nstruct Opens{id}:\n    \
         tried = 0\n\ndef link{id}(tally: shared Opens{id}) -> Link{id} fails Refused{id}:\n    mut number = 0\n    \
         lock tally as ref counts:\n        counts.tried += 1\n        number = counts.tried\n    if number == \
         {refuse}:\n        fail Refused{id}(why=f\"refused {{number}}\")\n    return Link{id}(number=number, \
         resets={})\n\n",
        if *resets { "True" } else { "False" }
    );
    let (mut made, mut helpers, mut body) = (0, String::new(), String::new());
    steps_written(id, steps, "        ", &mut made, &mut helpers, &mut body);
    out.push_str(&helpers);
    let _ = write!(
        out,
        "def pool_run{id}(tally: shared Opens{id}, stdout: Stdout) fails PoolError | Crashed:\n    with \
         pool[Link{id}, Refused{id}](open=() => link{id}(tally)?, size={size})? as links:\n{body}\ndef \
         pooled{id}(stdout: Stdout):\n    shared tally = Opens{id}()\n    pool_run{id}(tally, stdout) except why:\n"
    );
    pool_cases(&mut out, "        ", false);
    let _ = write!(
        out,
        "        case Crashed():\n            stdout.print(\"crashed\")\n    lock tally as counts:\n        \
         stdout.print(\"tried\", counts.tried)\n\n"
    );
    out
}
