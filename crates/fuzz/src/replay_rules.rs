//! The `replay` target: a program of tasks and a server's rounds that print and make way for each
//! other, recorded on 1 and on 4 threads, and each recording replayed on 1, 4 and 16. A replay must
//! print what its recording printed, byte for byte, and end the same way. Standard error is held to
//! what's expected: nothing from a recording, and from a replay nothing or the one note that the
//! recording's end stopped the server, so a warning, like a recording that stopped early, fails.
//!
//! What each `print` prints has to be in the recording as often as it's printed, so a print torn
//! in two or put in twice is caught in the recording as well. Where nothing printed a line in
//! pieces, every line has to be one the program prints. A `write` isn't promised whole, so a line
//! printed in pieces may have another task's line between them.
//!
//! The input chooses how many tasks `main` launches and how many rounds it asks a server it runs
//! for, and what each does, in steps: print a line, print one in two pieces with a wait between,
//! count for a while, which makes way for others at its ticks, sleep, or take a number from its
//! inbox, which `main` sends it among its own steps. A replay once put a round's lines among
//! `main`'s, or tore one in two, when the round made way after printing (#2033): that's the shape
//! this hunts, and every other way tasks print and wait around each other.
//!
//! Past those bytes, which older inputs end before, so they mean what they did, the input can ask
//! for three more things, each of which the recording's order decides and a replay has to keep:
//! - **Waits that time out.** A task may take its numbers with `receive_within`, and `main` send
//!   them with `send_within` to an inbox of 1, each giving up after a millisecond or two. What such
//!   a task got is any number, as one that timed out takes the next one later.
//! - **A work queue.** Workers loop over one channel `main` sends jobs to, each taking one stop
//!   number at the end. Which worker took a job is the recording's, but each job is taken once.
//! - **`parallel` cut short,** by a `break` in a loop's range or a failure in a list's, which `main`
//!   handles. Which ranges started is the recording's, but the one that ended it always did.
//!
//! The program is built once, with `fennec build --llvm`, and run with `FENNEC_RECORD` and then
//! `FENNEC_REPLAY`, as `fennec run --record` and `fennec replay` run it. It needs what `llvm`
//! needs, and the runtime with threads beside `fennec`.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::supervise::{MOST_KILOBYTES, STUCK, watch};

/// The most tasks `main` launches, and the most rounds it asks for.
const MOST_TASKS: usize = 3;
const MOST_ROUNDS: usize = 3;
/// The most steps a task, a round or `main` takes.
const MOST_STEPS: usize = 5;
/// How long a run may take on the clock before it counts as stuck: a replay waiting for good
/// uses no CPU time, which is all `STUCK` counts.
const LONGEST: Duration = Duration::from_secs(30);

/// One step of a task's or a round's.
#[derive(Clone, Copy)]
enum Step {
    /// Prints a line.
    Print,
    /// Writes the start of a line, makes way as the wait says, then prints its end.
    Pieces(Wait),
    /// Makes way.
    Wait(Wait),
    /// Takes the next number from its inbox and prints it: a task's only.
    Receive,
}

/// How a step makes way for others.
#[derive(Clone, Copy)]
enum Wait {
    /// Counts this many thousand turns, making way at the runtime's ticks.
    Count(u32),
    /// Sleeps this many milliseconds.
    Sleep(u32),
}

/// One of `main`'s steps.
#[derive(Clone, Copy)]
enum MainStep {
    Print,
    Wait(Wait),
    /// Sends the task the next number it takes.
    Send(usize),
    /// Sends the work queue its next job.
    Job,
    /// Calls the `parallel` call.
    Pieces,
}

/// The most workers on the work queue, and the most jobs `main` sends them.
const MOST_WORKERS: usize = 3;
const MOST_JOBS: usize = 6;
/// The most ranges a `parallel` call has, so each prints one digit.
const MOST_RANGES: usize = 8;

/// The program an input asks for.
struct Plan {
    tasks: Vec<Vec<Step>>,
    rounds: Vec<Vec<Step>>,
    main: Vec<MainStep>,
    /// Which tasks take their numbers within a time, from an inbox of 1, and are sent them so.
    timed: Vec<bool>,
    queue: Option<Queue>,
    pieces: Option<Pieces>,
}

/// Workers taking jobs from one channel, each looping until it takes a stop number.
struct Queue {
    /// Each worker's wait after a job, if it makes way.
    workers: Vec<Option<Wait>>,
    /// The channel's size.
    size: usize,
}

/// A `parallel` call over `count` ranges, which range `at` cuts short, each printing, then making
/// way as `wait` says.
struct Pieces {
    count: usize,
    at: usize,
    /// Whether range `at` fails, in a list's ranges, or breaks out of a loop's.
    fails: bool,
    wait: Wait,
}

/// The input's bytes, one at a time, then zeros.
struct Bytes<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }

    /// A number below `count`.
    fn below(&mut self, count: usize) -> usize {
        usize::from(self.next()) % count
    }

    fn wait(&mut self) -> Wait {
        let byte = self.next();
        if byte.is_multiple_of(2) {
            Wait::Count(25 * (1 + u32::from(byte / 2 % 8)))
        } else {
            Wait::Sleep(1 + u32::from(byte / 2 % 3))
        }
    }

    fn steps(&mut self, receives: bool) -> Vec<Step> {
        let count = 1 + self.below(MOST_STEPS);
        (0..count)
            .map(|_| match self.below(if receives { 5 } else { 4 }) {
                0 | 1 => Step::Print,
                2 => Step::Pieces(self.wait()),
                3 => Step::Wait(self.wait()),
                _ => Step::Receive,
            })
            .collect()
    }
}

fn plan(input: &[u8]) -> Plan {
    let mut bytes = Bytes { bytes: input, at: 0 };
    let tasks: Vec<Vec<Step>> = (0..bytes.below(MOST_TASKS + 1)).map(|_| bytes.steps(true)).collect();
    // A program with no task has a round, so there's always someone besides `main`.
    let rounds = (bytes.below(MOST_ROUNDS + 1)).max(usize::from(tasks.is_empty()));
    let rounds = (0..rounds).map(|_| bytes.steps(false)).collect();
    let count = 1 + bytes.below(MOST_STEPS);
    let mut main: Vec<MainStep> = (0..count)
        .map(|_| match bytes.below(2) {
            0 => MainStep::Print,
            _ => MainStep::Wait(bytes.wait()),
        })
        .collect();
    // Each number a task takes, `main` sends it, somewhere among its own steps.
    for (task, steps) in tasks.iter().enumerate() {
        for _ in steps.iter().filter(|step| matches!(step, Step::Receive)) {
            let at = bytes.below(main.len() + 1);
            main.insert(at, MainStep::Send(task));
        }
    }
    // What's past here, an older input ends before, and the zeros after it ask for none of it.
    let timed_tasks = bytes.next();
    let timed = (0..tasks.len()).map(|task| timed_tasks >> task & 1 == 1).collect();
    let workers = bytes.below(MOST_WORKERS + 1);
    let queue = (workers > 0).then(|| {
        let workers = (0..workers).map(|_| bytes.below(2).eq(&1).then(|| bytes.wait())).collect();
        let size = 1 + bytes.below(2);
        for _ in 0..=bytes.below(MOST_JOBS) {
            let at = bytes.below(main.len() + 1);
            main.insert(at, MainStep::Job);
        }
        Queue { workers, size }
    });
    let ranges = bytes.below(MOST_RANGES + 1);
    let pieces = (ranges > 0).then(|| {
        let pieces =
            Pieces { count: ranges, at: bytes.below(ranges), fails: bytes.below(2) == 1, wait: bytes.wait() };
        let at = bytes.below(main.len() + 1);
        main.insert(at, MainStep::Pieces);
        pieces
    });
    Plan { tasks, rounds, main, timed, queue, pieces }
}

impl Plan {
    fn has_inbox(&self, task: usize) -> bool {
        self.tasks.get(task).is_some_and(|steps| steps.iter().any(|step| matches!(step, Step::Receive)))
    }

    fn is_timed(&self, task: usize) -> bool {
        self.timed.get(task).copied().unwrap_or(false)
    }

    /// Whether any task takes its numbers within a time.
    fn any_timed(&self) -> bool {
        (0..self.tasks.len()).any(|task| self.is_timed(task) && self.has_inbox(task))
    }

    fn jobs(&self) -> usize {
        self.main.iter().filter(|step| matches!(step, MainStep::Job)).count()
    }

    fn in_pieces(&self) -> bool {
        self.tasks.iter().chain(&self.rounds).flatten().any(|step| matches!(step, Step::Pieces(_)))
    }

    /// What `main` prints once the `parallel` call is over.
    fn pieces_over(&self) -> Option<&'static str> {
        self.pieces.as_ref().map(|pieces| if pieces.fails { "pieces gave -1" } else { "pieces ended" })
    }

    /// What each `print` prints, its new line too, as often as it's printed, sorted, but for what
    /// the recording's order decides, which `vague` checks. Step numbers are one digit, and no text
    /// ends another, so none can be found inside another.
    fn printed(&self) -> Vec<String> {
        let mut printed: Vec<String> = Vec::new();
        for (task, (who, steps)) in self.bodies().enumerate() {
            let mut taken = 0;
            for (number, step) in steps.iter().enumerate() {
                match step {
                    Step::Print => printed.push(format!("{who} s{number}\n")),
                    Step::Pieces(_) => printed.push(" b\n".to_owned()),
                    Step::Wait(_) => {}
                    Step::Receive if self.is_timed(task) => {}
                    Step::Receive => {
                        printed.push(format!("{who} got {}\n", sent(&who, taken)));
                        taken += 1;
                    }
                }
            }
        }
        for (number, step) in self.main.iter().enumerate() {
            if matches!(step, MainStep::Print) {
                printed.push(format!("main s{number}\n"));
            }
        }
        printed.extend((0..self.rounds.len()).map(|round| format!("done r{round}\n")));
        if let Some(pieces) = &self.pieces {
            printed.push(format!("p {}\n", pieces.at));
        }
        printed.extend(self.pieces_over().map(|over| format!("{over}\n")));
        printed.push("over\n".to_owned());
        printed.sort();
        printed
    }

    /// Every line the program prints, in no particular order, as `plain` gives each.
    fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (task, (who, steps)) in self.bodies().enumerate() {
            let mut taken = 0;
            for (number, step) in steps.iter().enumerate() {
                match step {
                    Step::Print => lines.push(format!("{who} s{number}")),
                    Step::Pieces(_) => lines.push(format!("{who} s{number} a b")),
                    Step::Wait(_) => {}
                    Step::Receive if self.is_timed(task) => lines.push(format!("{who} got ?")),
                    Step::Receive => {
                        lines.push(format!("{who} got {}", sent(&who, taken)));
                        taken += 1;
                    }
                }
            }
        }
        for (number, step) in self.main.iter().enumerate() {
            if matches!(step, MainStep::Print) {
                lines.push(format!("main s{number}"));
            }
        }
        lines.extend((0..self.rounds.len()).map(|round| format!("done r{round}")));
        lines.extend((0..self.jobs()).map(|job| format!("w? took {job}")));
        lines.extend(self.pieces_over().map(str::to_owned));
        lines.push("over".to_owned());
        lines
    }

    /// A line as `lines` gives it, with what the recording's order decides taken out: the number
    /// a timed task got, the worker that took a job, and the `parallel` call's ranges, which go.
    fn plain(&self, line: &str) -> Option<String> {
        if line.starts_with("p ") {
            return None;
        }
        if let Some((who, _)) = line.split_once(" got ")
            && let Some(task) = who.strip_prefix('t').and_then(|task| task.parse::<usize>().ok())
            && self.is_timed(task)
        {
            return Some(format!("{who} got ?"));
        }
        if line.starts_with('w')
            && let Some((_, job)) = line.split_once(" took ")
        {
            return Some(format!("w? took {job}"));
        }
        Some(line.to_owned())
    }

    /// Checks what the recording's order decides: a timed task printed once for each number it
    /// takes, each job was taken once, by any worker, and each of the `parallel` call's ranges
    /// printed at most once, the one that cut it short once.
    fn vague(&self, text: &str) -> Result<(), String> {
        for task in (0..self.tasks.len()).filter(|task| self.is_timed(*task)) {
            let takes = self
                .tasks
                .get(task)
                .map_or(0, |steps| steps.iter().filter(|step| matches!(step, Step::Receive)).count());
            let got = text.matches(&format!("t{task} got ")).count();
            if got != takes {
                return Err(format!("task {task} printed what it got {got} times, not {takes}"));
            }
        }
        let jobs = self.jobs();
        if text.matches(" took ").count() != jobs {
            return Err(format!("the workers took {} jobs, not {jobs}", text.matches(" took ").count()));
        }
        if let Some(job) = (0..jobs).find(|job| text.matches(&format!(" took {job}\n")).count() != 1) {
            return Err(format!("job {job} wasn't taken once"));
        }
        let count = self.pieces.as_ref().map_or(0, |pieces| pieces.count);
        let ranges: usize = (0..count).map(|range| text.matches(&format!("p {range}\n")).count()).sum();
        if text.matches("p ").count() != ranges {
            return Err(format!("the `parallel` call's {count} ranges printed something else"));
        }
        if let Some(range) = (0..count).find(|range| text.matches(&format!("p {range}\n")).count() > 1) {
            return Err(format!("range {range} printed twice"));
        }
        Ok(())
    }

    /// Each task's and round's name and steps.
    fn bodies(&self) -> impl Iterator<Item = (String, &Vec<Step>)> {
        let tasks = self.tasks.iter().enumerate().map(|(task, steps)| (format!("t{task}"), steps));
        let rounds = self.rounds.iter().enumerate().map(|(round, steps)| (format!("r{round}"), steps));
        tasks.chain(rounds)
    }
}

/// The number `main` sends as task `who`'s `taken`th.
fn sent(who: &str, taken: usize) -> usize {
    let task: usize = who.trim_start_matches('t').parse().unwrap_or(0);
    100 * task + taken
}

fn waited(wait: Wait, indent: &str) -> String {
    match wait {
        Wait::Count(thousands) => format!("{indent}_ = counted({thousands}_000)\n"),
        Wait::Sleep(ms) => format!("{indent}clock.sleep({ms}ms)\n"),
    }
}

/// The body of a task or a round, indented once. A timed task takes each number within a
/// millisecond or two.
fn body(who: &str, steps: &[Step], timed: bool) -> String {
    let mut out = String::new();
    for (number, step) in steps.iter().enumerate() {
        match step {
            Step::Print => {
                let _ = writeln!(out, "    stdout.print(\"{who} s{number}\")");
            }
            Step::Pieces(wait) => {
                let _ = writeln!(out, "    stdout.write(\"{who} s{number} a\")");
                out.push_str(&waited(*wait, "    "));
                out.push_str("    stdout.print(\" b\")\n");
            }
            Step::Wait(wait) => out.push_str(&waited(*wait, "    ")),
            Step::Receive if timed => {
                let _ = writeln!(out, "    got_{number} = got_within(inbox, {}ms)", 1 + number % 2);
                let _ = writeln!(out, "    stdout.print(\"{who} got\", got_{number})");
            }
            Step::Receive => {
                let _ = writeln!(out, "    got_{number} = inbox.receive() else -1");
                let _ = writeln!(out, "    stdout.print(\"{who} got\", got_{number})");
            }
        }
    }
    out
}

/// Whether steps sleep, and whether they print.
fn needs(steps: &[Step]) -> (bool, bool) {
    let sleeps = |wait: &Wait| matches!(wait, Wait::Sleep(_));
    let clock = steps.iter().any(|step| match step {
        Step::Pieces(wait) | Step::Wait(wait) => sleeps(wait),
        Step::Print | Step::Receive => false,
    });
    (clock, steps.iter().any(|step| !matches!(step, Step::Wait(_))))
}

/// The parameters a function takes, or the arguments it's called with, for an inbox, a clock and
/// standard output where it needs each: only what it uses, since a parameter left unused is an
/// error.
fn given((inbox, clock, stdout): (bool, bool, bool), parameters: bool) -> String {
    let all = [(inbox, "inbox", "Inbox[int]"), (clock, "clock", "Clock"), (stdout, "stdout", "Stdout")];
    let said: Vec<String> = all
        .iter()
        .filter(|(needed, ..)| *needed)
        .map(|(_, name, kind)| if parameters { format!("{name}: {kind}") } else { format!("{name}={name}") })
        .collect();
    said.join(", ")
}

/// Whether `main` sends a task a number with `send`, which fails with `Closed`, rather than with
/// `handed`, which gives up.
fn sends_plainly(plan: &Plan) -> bool {
    plan.main.iter().any(|step| match step {
        MainStep::Send(task) => !plan.is_timed(*task),
        MainStep::Job => true,
        MainStep::Print | MainStep::Wait(_) | MainStep::Pieces => false,
    })
}

/// What the program imports, the error it fails with if any, and the helpers it calls.
fn heading(plan: &Plan) -> String {
    let inbox = (0..plan.tasks.len()).any(|task| plan.has_inbox(task));
    let mut imported = Vec::new();
    if plan.queue.is_some() {
        imported.push("Channel");
    }
    if sends_plainly(plan) || plan.any_timed() {
        imported.push("Closed");
    }
    imported.push("Crashed");
    if inbox {
        imported.push("Inbox");
    }
    if plan.any_timed() {
        imported.extend(["Sender", "Timeout"]);
    }
    if plan.queue.is_some() {
        imported.push("channel");
    }
    let mut out = format!("from std.concurrent import {}\n", imported.join(", "));
    if !plan.rounds.is_empty() {
        out.push_str("from std.net import Connection, NetError, Server\n");
    }
    if plan.any_timed() {
        out.push_str("from std.time import Duration\n");
    }
    if plan.pieces.as_ref().is_some_and(|pieces| pieces.fails) {
        out.push_str("\nerror TooFar:\n    at: int\n");
    }
    out.push_str(
        "\n# Counts `turns` turns, making way for others at the runtime's ticks.\n\
         def counted(turns: int) -> int:\n    mut total = 0\n    for turn in range(turns):\n        \
         total = (total + turn * turn) % 1_000_003\n    return total\n",
    );
    if plan.any_timed() {
        out.push_str(
            "\n# The next number, if it comes within `wait`: -1 when it doesn't, -2 when the inbox closed.\n\
             def got_within(inbox: Inbox[int], wait: Duration) -> int:\n    \
             got = inbox.receive_within(wait) except Timeout(): return -1\n    return got else -2\n\
             \n# Sends `number` within `wait`, or gives up, when the task has ended or its inbox stays full.\n\
             def handed(to: Sender[int], number: int, wait: Duration):\n    \
             to.send_within(number, wait) except Closed() | Timeout(): return\n",
        );
    }
    out
}

/// The program `plan` asks for, as Fennec.
fn source(plan: &Plan) -> String {
    let mut out = heading(plan);
    let sleeps = |wait: &Wait| matches!(wait, Wait::Sleep(_));
    let mut main_clock = plan.main.iter().any(|step| matches!(step, MainStep::Wait(wait) if sleeps(wait)));
    for (task, steps) in plan.tasks.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        main_clock |= clock;
        let parameters = given((plan.has_inbox(task), clock, stdout), true);
        let _ = write!(out, "\ndef task_{task}({parameters}):\n");
        out.push_str(&body(&format!("t{task}"), steps, plan.is_timed(task)));
    }
    if let Some(queue) = &plan.queue {
        for (worker, wait) in queue.workers.iter().enumerate() {
            let clock = wait.as_ref().is_some_and(sleeps);
            main_clock |= clock;
            let parameters = given((false, clock, true), true);
            let _ = write!(
                out,
                "\ndef worker_{worker}(jobs: Channel[int], {parameters}):\n    for job in jobs:\n        \
                 if job < 0:\n            return\n        stdout.print(\"w{worker} took\", job)\n"
            );
            out.extend(wait.map(|wait| waited(wait, "        ")));
        }
    }
    if let Some(pieces) = &plan.pieces {
        let clock = sleeps(&pieces.wait);
        main_clock |= clock;
        out.push_str(&parallel(
            pieces,
            &given((false, clock, true), true),
            &given((false, clock, true), false),
        ));
    }
    let mut server_clock = false;
    for (round, steps) in plan.rounds.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        server_clock |= clock;
        let parameters = given((false, clock, stdout), true);
        let _ = write!(out, "\ndef round_{round}({parameters}):\n");
        out.push_str(&body(&format!("r{round}"), steps, false));
    }
    // What the server's rounds need between them, which the server is handed. It always prints,
    // as a round that can't read or write says so.
    let handed = given((false, server_clock, true), false);
    if !plan.rounds.is_empty() {
        out.push_str(&server(plan, &given((false, server_clock, true), true), &handed));
    }
    out.push_str(&main(plan, main_clock || server_clock, &handed));
    out
}

/// The server: `answered`, which runs the round a connection asks for and says it's done, and
/// `serving`, which answers each connection in a piece of a `parallel` call. Each takes
/// `parameters`, and passes on `arguments`.
fn server(plan: &Plan, parameters: &str, arguments: &str) -> String {
    let mut out = format!(
        "\ndef answered(conn: Connection, {parameters}):\n    \
         line = conn.read_line() except why:\n        stdout.print(\"a read failed\", f\"{{why}}\")\n        \
         return\n    asked = line else \"\"\n"
    );
    out.push_str("    match asked:\n");
    for (round, steps) in plan.rounds.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        let passed = given((false, clock, stdout), false);
        let _ = write!(out, "        case \"r{round}\":\n            round_{round}({passed})\n");
    }
    let _ = write!(
        out,
        "        case _:\n            stdout.print(\"asked for no round\", asked)\n    \
         conn.write(f\"done {{asked}}\\n\") except why:\n        \
         stdout.print(\"a write failed\", f\"{{why}}\")\n\n\
         def serving(server: Server, {parameters}):\n    \
         for conn in parallel(server):\n        answered(conn, {arguments})\n"
    );
    out
}

/// `pieces`, the `parallel` call, which takes `parameters`: a loop over its ranges that one breaks
/// out of, or a list of what each gives, which one fails, which `pieces` passes up.
fn parallel(pieces: &Pieces, parameters: &str, arguments: &str) -> String {
    let Pieces { count, at, fails, wait } = *pieces;
    if fails {
        format!(
            "\ndef piece(number: int, {parameters}) -> int fails TooFar:\n    stdout.print(\"p\", number)\n{}    \
             if number == {at}:\n        fail TooFar(at=number)\n    return number\n\
             \ndef pieces({parameters}) -> int fails TooFar:\n    \
             passed = [piece(number, {arguments})? for number in parallel(range({count}))]\n    return len(passed)\n",
            waited(wait, "    ")
        )
    } else {
        format!(
            "\ndef pieces({parameters}):\n    for number in parallel(range({count})):\n        \
             stdout.print(\"p\", number)\n{}        if number == {at}:\n            break\n",
            waited(wait, "        ")
        )
    }
}

/// `main`: it runs the server, if there are rounds, opens the work queue and launches its workers,
/// if there are any, and launches each task, one block inside another, then asks for every round at
/// once and takes its own steps, sending each task the numbers it takes and the queue its jobs,
/// then each worker its stop, before it reads what each round said. `clock` says whether it needs
/// one, and `handed` is what the server's handed.
fn main(plan: &Plan, clock: bool, handed: &str) -> String {
    let serves = !plan.rounds.is_empty();
    let mut fails = Vec::new();
    if serves {
        fails.push("NetError");
    }
    fails.push("Crashed");
    if sends_plainly(plan) {
        fails.push("Closed");
    }
    let fails = fails.join(" | ");
    let net = if serves { "net: Net, " } else { "" };
    let main_clock = if clock { "clock: Clock, " } else { "" };
    let mut out = format!("\ndef main({net}{main_clock}stdout: Stdout) fails {fails}:\n");
    let mut indent = String::from("    ");
    if serves {
        let _ = writeln!(out, "{indent}with net.listen(0)? as server:");
        indent.push_str("    ");
        let _ = writeln!(out, "{indent}port = server.port");
        let _ = writeln!(out, "{indent}with launch(() => serving(server, {handed}), stop_after=100ms)?:");
        indent.push_str("    ");
    }
    if let Some(queue) = &plan.queue {
        let _ = writeln!(out, "{indent}with channel[int](size={}) as jobs:", queue.size);
        indent.push_str("    ");
        for (worker, wait) in queue.workers.iter().enumerate() {
            let arguments = given((false, matches!(wait, Some(Wait::Sleep(_))), true), false);
            let _ = writeln!(out, "{indent}with launch(() => worker_{worker}(jobs, {arguments}))?:");
            indent.push_str("    ");
        }
    }
    for (task, steps) in plan.tasks.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        let arguments = given((plan.has_inbox(task), clock, stdout), false);
        if plan.has_inbox(task) && plan.is_timed(task) {
            let _ = writeln!(
                out,
                "{indent}with launch((inbox) => task_{task}({arguments}), inbox_size=1)? as to_{task}:"
            );
        } else if plan.has_inbox(task) {
            let _ = writeln!(out, "{indent}with launch((inbox) => task_{task}({arguments}))? as to_{task}:");
        } else {
            let _ = writeln!(out, "{indent}with launch(() => task_{task}({arguments}))?:");
        }
        indent.push_str("    ");
    }
    for round in 0..plan.rounds.len() {
        let _ = writeln!(out, "{indent}with net.connect(\"127.0.0.1\", port)? as asking_{round}:");
        indent.push_str("    ");
    }
    for round in 0..plan.rounds.len() {
        let _ = writeln!(out, "{indent}asking_{round}.write(\"r{round}\\n\")?");
    }
    out.push_str(&main_steps(plan, &indent));
    for round in 0..plan.rounds.len() {
        let _ = writeln!(out, "{indent}stdout.print(asking_{round}.read_line()? else \"nothing\")");
    }
    out.push_str("    stdout.print(\"over\")\n");
    out
}

/// `main`'s own steps, at `indent`, then a stop for each worker.
fn main_steps(plan: &Plan, indent: &str) -> String {
    let mut out = String::new();
    let mut sent_to = vec![0; plan.tasks.len()];
    let mut jobs = 0;
    for (number, step) in plan.main.iter().enumerate() {
        match step {
            MainStep::Print => {
                let _ = writeln!(out, "{indent}stdout.print(\"main s{number}\")");
            }
            MainStep::Wait(wait) => out.push_str(&waited(*wait, indent)),
            MainStep::Send(task) => {
                let Some(count) = sent_to.get_mut(*task) else { continue };
                let number = sent(&format!("t{task}"), *count);
                if plan.is_timed(*task) {
                    let _ = writeln!(out, "{indent}handed(to_{task}, {number}, {}ms)", 1 + *count % 2);
                } else {
                    let _ = writeln!(out, "{indent}to_{task}.send({number})?");
                }
                *count += 1;
            }
            MainStep::Job => {
                let _ = writeln!(out, "{indent}jobs.send({jobs})?");
                jobs += 1;
            }
            MainStep::Pieces => {
                let fails = plan.pieces.as_ref().is_some_and(|pieces| pieces.fails);
                let clock = plan.pieces.as_ref().is_some_and(|pieces| matches!(pieces.wait, Wait::Sleep(_)));
                let arguments = given((false, clock, true), false);
                if fails {
                    let _ = writeln!(out, "{indent}gave = pieces({arguments}) except TooFar(): -1");
                    let _ = writeln!(out, "{indent}stdout.print(\"pieces gave\", gave)");
                } else {
                    let _ = writeln!(out, "{indent}pieces({arguments})");
                    let _ = writeln!(out, "{indent}stdout.print(\"pieces ended\")");
                }
            }
        }
    }
    // One stop for each worker, which each takes one of, once every job's been taken.
    for _ in plan.queue.iter().flat_map(|queue| &queue.workers) {
        let _ = writeln!(out, "{indent}jobs.send(-1)?");
    }
    out
}

/// Prints the program an input writes, for `fennec-fuzz show replay <file>`.
pub(crate) fn written(input: &[u8]) -> String {
    source(&plan(input))
}

/// What a replay of a server says when the recording's end stops it, and all it may say.
const SERVER_STOPPED: &str = "replay: the recording ends here, so the server is stopped\n";

/// What a run printed, what it said on standard error, and its exit code.
type Ran = (Vec<u8>, String, Option<i32>);

/// Runs `executable` on `threads` threads with `variable` set to `log`, stopping it if it gets
/// stuck, on the clock or by CPU time, or holds too much memory.
fn ran(executable: &Path, work: &Path, threads: usize, variable: &str, log: &Path) -> Result<Ran, String> {
    let (out, err) = (work.join("stdout"), work.join("stderr"));
    let file = |path: &Path| std::fs::File::create(path).map_err(|err| format!("{}: {err}", path.display()));
    let mut command = Command::new(executable);
    // In a group of its own, which stopping it stops whole.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .env("FENNEC_THREADS", threads.to_string())
        .env(variable, log)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(file(&out)?)
        .stderr(file(&err)?)
        .spawn()
        .map_err(|err| format!("couldn't run what LLVM built: {err}"))?;
    let deadline = Instant::now() + LONGEST;
    let failure = watch(&mut child, STUCK, Some(deadline), MOST_KILOBYTES, || false);
    let status = child.wait().map_err(|err| format!("couldn't wait for the program: {err}"))?;
    let printed = std::fs::read(&out).unwrap_or_default();
    let said = String::from_utf8_lossy(&std::fs::read(&err).unwrap_or_default()).into_owned();
    let doing = if variable == "FENNEC_RECORD" { "recorded" } else { "replayed" };
    if Instant::now() >= deadline {
        return Err(format!("{doing} on {threads} threads, it was still running after {LONGEST:?}:\n{said}"));
    }
    match failure {
        Some(crate::supervise::Failure::Stuck | crate::supervise::Failure::TooMuchMemory) => {
            Err(format!("{doing} on {threads} threads, it {failure:?}:\n{said}"))
        }
        _ => Ok((printed, said, status.code())),
    }
}

/// Every line `printed` has, as `plan.plain` gives it, sorted.
fn plain_lines(plan: &Plan, printed: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> =
        String::from_utf8_lossy(printed).lines().filter_map(|line| plan.plain(line)).collect();
    lines.sort();
    lines
}

pub(crate) fn replay(input: &[u8]) -> Result<(), String> {
    let plan = plan(input);
    let source = source(&plan);
    let work = crate::llvm::work().join("replay");
    let Some(executable) = crate::llvm::build(&source, &work, false)? else {
        return Err(format!("LLVM doesn't take a program this target writes:\n{source}"));
    };
    let mut expected = plan.lines();
    expected.sort();
    let printed = plan.printed();
    for recorded_on in [1, 4] {
        let log = work.join(format!("recorded-{recorded_on}.log"));
        // What `fennec run --record` writes first, which a replay reads to know it's a recording.
        fennec_rt::replay::start_log(&log, b"native\0program.fen")
            .map_err(|err| format!("{}: {err}", log.display()))?;
        let recorded = ran(&executable, &work, recorded_on, "FENNEC_RECORD", &log)?;
        if recorded.2 != Some(0) || !recorded.1.is_empty() {
            return Err(format!(
                "recorded on {recorded_on} threads, it ended with {:?}, saying:\n{}",
                recorded.2, recorded.1
            ));
        }
        let text = String::from_utf8_lossy(&recorded.0);
        let mut counted = printed.clone();
        counted.dedup();
        if let Some(print) = counted.iter().find(|print| {
            text.matches(print.as_str()).count() != printed.iter().filter(|each| each == print).count()
        }) {
            return Err(format!(
                "recorded on {recorded_on} threads, it didn't print {print:?} as often as the program does:\n{text}"
            ));
        }
        plan.vague(&text).map_err(|why| format!("recorded on {recorded_on} threads, {why}:\n{text}"))?;
        if !plan.in_pieces() && plain_lines(&plan, &recorded.0) != expected {
            return Err(format!(
                "recorded on {recorded_on} threads, it printed lines it doesn't print, or not as often:\n{}",
                String::from_utf8_lossy(&recorded.0)
            ));
        }
        for replayed_on in [1, 4, 16] {
            let replayed = ran(&executable, &work, replayed_on, "FENNEC_REPLAY", &log)?;
            // Only a program with a server has one for the recording's end to stop.
            let quiet = replayed.1.is_empty() || (!plan.rounds.is_empty() && replayed.1 == SERVER_STOPPED);
            if (&replayed.0, replayed.2) != (&recorded.0, recorded.2) || !quiet {
                return Err(format!(
                    "recorded on {recorded_on} threads and replayed on {replayed_on}, the replay printed \
                     {:?} and ended with {:?}, saying:\n{}\nwhere the recording printed {:?}",
                    String::from_utf8_lossy(&replayed.0),
                    replayed.2,
                    replayed.1,
                    String::from_utf8_lossy(&recorded.0),
                ));
            }
        }
    }
    Ok(())
}
