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
}

/// The program an input asks for.
struct Plan {
    tasks: Vec<Vec<Step>>,
    rounds: Vec<Vec<Step>>,
    main: Vec<MainStep>,
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
    Plan { tasks, rounds, main }
}

impl Plan {
    fn has_inbox(&self, task: usize) -> bool {
        self.tasks.get(task).is_some_and(|steps| steps.iter().any(|step| matches!(step, Step::Receive)))
    }

    fn in_pieces(&self) -> bool {
        self.tasks.iter().chain(&self.rounds).flatten().any(|step| matches!(step, Step::Pieces(_)))
    }

    /// What each `print` prints, its new line too, as often as it's printed, sorted. Step numbers
    /// are one digit, and no text ends another, so none can be found inside another.
    fn printed(&self) -> Vec<String> {
        let mut printed: Vec<String> = Vec::new();
        for (who, steps) in self.bodies() {
            let mut taken = 0;
            for (number, step) in steps.iter().enumerate() {
                match step {
                    Step::Print => printed.push(format!("{who} s{number}\n")),
                    Step::Pieces(_) => printed.push(" b\n".to_owned()),
                    Step::Wait(_) => {}
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
        printed.push("over\n".to_owned());
        printed.sort();
        printed
    }

    /// Every line the program prints, in no particular order.
    fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (who, steps) in self.bodies() {
            let mut taken = 0;
            for (number, step) in steps.iter().enumerate() {
                match step {
                    Step::Print => lines.push(format!("{who} s{number}")),
                    Step::Pieces(_) => lines.push(format!("{who} s{number} a b")),
                    Step::Wait(_) => {}
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
        lines.push("over".to_owned());
        lines
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

/// The body of a task or a round, indented once.
fn body(who: &str, steps: &[Step]) -> String {
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

/// The program `plan` asks for, as Fennec.
fn source(plan: &Plan) -> String {
    let sends = plan.main.iter().any(|step| matches!(step, MainStep::Send(_)));
    let mut out = String::from(if sends {
        "from std.concurrent import Closed, Crashed, Inbox\n"
    } else {
        "from std.concurrent import Crashed\n"
    });
    if !plan.rounds.is_empty() {
        out.push_str("from std.net import Connection, NetError, Server\n");
    }
    out.push_str(
        "\n# Counts `turns` turns, making way for others at the runtime's ticks.\n\
         def counted(turns: int) -> int:\n    mut total = 0\n    for turn in range(turns):\n        \
         total = (total + turn * turn) % 1_000_003\n    return total\n",
    );
    let mut main_clock = plan.main.iter().any(|step| matches!(step, MainStep::Wait(Wait::Sleep(_))));
    for (task, steps) in plan.tasks.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        main_clock |= clock;
        let parameters = given((plan.has_inbox(task), clock, stdout), true);
        let _ = write!(out, "\ndef task_{task}({parameters}):\n");
        out.push_str(&body(&format!("t{task}"), steps));
    }
    let mut server_clock = false;
    for (round, steps) in plan.rounds.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        server_clock |= clock;
        let parameters = given((false, clock, stdout), true);
        let _ = write!(out, "\ndef round_{round}({parameters}):\n");
        out.push_str(&body(&format!("r{round}"), steps));
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

/// `main`: it runs the server, if there are rounds, and launches each task, one block inside
/// another, then asks for every round at once and takes its own steps, sending each task the
/// numbers it takes, before it reads what each round said. `clock` says whether it needs one, and
/// `handed` is what the server's handed.
fn main(plan: &Plan, clock: bool, handed: &str) -> String {
    let serves = !plan.rounds.is_empty();
    let sends = plan.main.iter().any(|step| matches!(step, MainStep::Send(_)));
    let fails = match (serves, sends) {
        (true, true) => "NetError | Crashed | Closed",
        (true, false) => "NetError | Crashed",
        (false, true) => "Crashed | Closed",
        (false, false) => "Crashed",
    };
    let net = if serves { "net: Net, " } else { "" };
    let clock = if clock { "clock: Clock, " } else { "" };
    let mut out = format!("\ndef main({net}{clock}stdout: Stdout) fails {fails}:\n");
    let mut indent = String::from("    ");
    if serves {
        let _ = writeln!(out, "{indent}with net.listen(0)? as server:");
        indent.push_str("    ");
        let _ = writeln!(out, "{indent}port = server.port");
        let _ = writeln!(out, "{indent}with launch(() => serving(server, {handed}), stop_after=100ms)?:");
        indent.push_str("    ");
    }
    for (task, steps) in plan.tasks.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        let arguments = given((plan.has_inbox(task), clock, stdout), false);
        if plan.has_inbox(task) {
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
    let mut sent_to = vec![0; plan.tasks.len()];
    for (number, step) in plan.main.iter().enumerate() {
        match step {
            MainStep::Print => {
                let _ = writeln!(out, "{indent}stdout.print(\"main s{number}\")");
            }
            MainStep::Wait(wait) => out.push_str(&waited(*wait, &indent)),
            MainStep::Send(task) => {
                let Some(count) = sent_to.get_mut(*task) else { continue };
                let _ = writeln!(out, "{indent}to_{task}.send({})?", sent(&format!("t{task}"), *count));
                *count += 1;
            }
        }
    }
    for round in 0..plan.rounds.len() {
        let _ = writeln!(out, "{indent}stdout.print(asking_{round}.read_line()? else \"nothing\")");
    }
    out.push_str("    stdout.print(\"over\")\n");
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

/// Every line `printed` has, sorted.
fn sorted_lines(printed: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(printed).lines().map(str::to_owned).collect();
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
        if !plan.in_pieces() && sorted_lines(&recorded.0) != expected {
            return Err(format!(
                "recorded on {recorded_on} threads, it printed lines it doesn't print, or not as often:\n{}",
                String::from_utf8_lossy(&recorded.0)
            ));
        }
        for replayed_on in [1, 4, 16] {
            let replayed = ran(&executable, &work, replayed_on, "FENNEC_REPLAY", &log)?;
            let quiet = replayed.1.is_empty() || replayed.1 == SERVER_STOPPED;
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
