//! The `replay` target: a program of tasks and a server's rounds that print and make way for each
//! other, recorded on 1 and on 4 threads, and each recording replayed on 1, 4 and 16. A replay must
//! print what its recording printed, byte for byte, and end the same way. Standard error is held to
//! what's expected: from a recording nothing but what a lock's crash says, and from a replay what
//! its recording said, in its order, and the note that the recording's end stopped a server, once
//! for each of its rounds' and HTTP's, wherever the stop came among the crashes, so a warning, like
//! a recording that stopped early, fails.
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
//! - **Several senders on one channel,** which `main` takes every number from once its steps are
//!   done. Which arrived first is the recording's, but each number arrives once.
//! - **A deadlock:** `main` stuck on a channel only it holds, receiving from it empty or sending
//!   to it full, while tasks wait on it too. The recording ends with 1 and the report of who waits
//!   for what, and a replay has to come to the same deadlock and say it the same way. A program
//!   with a server's rounds gets none: they wait on the network, which isn't a deadlock.
//! - **Locks,** on two `shared` accounts `main` makes, taken by tasks, by `main` and by each of the
//!   `parallel` call's ranges: one account, or both in one `lock`, named in either order. A lock
//!   adds to an account's total and appends to its moves, and prints the total it left. Some are
//!   undone: they add 100 and append 1000, then `fail`, or crash in a task of their own, which the
//!   one taking them handles. What each printed is the recording's, but once every task's ended,
//!   `main` prints each account, and its total, its count of moves and whether they agree are
//!   known, but for the ranges a `parallel` call cut short didn't run. A crash says so on standard
//!   error, which a replay has to say the same.
//! - **Messages as structs:** each number sent to a task's inbox or into the gathered channel goes
//!   as a `Note` of the number and a text, a short one for an even number and, for an odd one, one
//!   long enough to need a block of its own. Whoever takes one prints its number and whether its
//!   text is the one sent with it, so a message whose text came apart from its number is caught.
//!   A byte past the HTTP clients' makes the message an enum instead: `Sent`, flat, its case
//!   chosen by the number; a struct holding a `Sent`; an enum holding a `Sent` in one case and a
//!   number in another; or an enum with a case holding a list, which isn't flat and goes as a
//!   block. Each carries its number and texts the receiver checks the same way.
//! - **An HTTP server,** std's `serve`, which clients `main` connects and launches each ask on a
//!   connection they keep: a head written whole, a line at a time, cut in two anywhere in a line
//!   or its ending, or two requests in one write; a request line too long for a 414, more headers
//!   than a head may have for a 431, or a head the client closes the connection partway through.
//!   The server prints each path it serves, and each client the response it read. Which order those
//!   came in is the recording's. A program with it gets no deadlock, as with a server's rounds.
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
    /// Takes the lock the plan's lock with this number says, which also numbers what it adds.
    Lock(usize),
}

/// The `shared` accounts a lock takes: `a`, `b`, or both, named in this order.
#[derive(Clone, Copy)]
enum Taken {
    A,
    B,
    AB,
    BA,
}

impl Taken {
    fn a(self) -> bool {
        !matches!(self, Self::B)
    }

    fn b(self) -> bool {
        !matches!(self, Self::A)
    }

    /// The `lock` line, which names `a`'s account `x` and `b`'s `y`.
    fn statement(self) -> &'static str {
        match self {
            Self::A => "lock a as ref x:",
            Self::B => "lock b as ref y:",
            Self::AB => "lock a, b as ref x, ref y:",
            Self::BA => "lock b, a as ref y, ref x:",
        }
    }

    /// The `lock` line that takes them to read.
    fn reading(self) -> &'static str {
        match self {
            Self::A => "lock a as x:",
            Self::B => "lock b as y:",
            Self::AB => "lock a, b as x, y:",
            Self::BA => "lock b, a as y, x:",
        }
    }
}

/// How a lock's body ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Its changes are kept, and it prints the total it left.
    Kept,
    /// It fails, which undoes its changes.
    Failed,
    /// It crashes, in a task of its own, which undoes its changes.
    Crashed,
    /// It reads, changing nothing, and prints the total it saw. Readers hold a value together, so
    /// two readers of both accounts can log their takes crossed (#2313).
    Read,
}

/// A lock a task or `main` takes.
#[derive(Clone, Copy)]
struct Lock {
    taken: Taken,
    ending: Ending,
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
    /// Takes the plan's lock with this number.
    Lock(usize),
    /// Waits on a channel of its own for good, so the program deadlocks there.
    Stuck(Stuck),
}

/// How `main` gets stuck: on a channel only it holds, which no task sends to or takes from.
#[derive(Clone, Copy)]
enum Stuck {
    /// Receives from it, empty.
    Receiving,
    /// Sends to it a second time, full.
    Sending,
}

/// The most workers on the work queue, and the most jobs `main` sends them.
const MOST_WORKERS: usize = 3;
const MOST_JOBS: usize = 6;
/// The most ranges a `parallel` call has, so each prints one digit.
const MOST_RANGES: usize = 8;
/// The most locks tasks and `main` take between them, so a step's number stays one digit.
const MOST_LOCKS: usize = 4;
/// The most clients of the HTTP server, and the most requests each sends on its connection.
const MOST_CLIENTS: usize = 3;
const MOST_ASKS: usize = 3;
/// More header lines than a head may have, so it's answered with a 431.
const TOO_MANY_HEADERS: usize = 101;
/// A request line longer than a whole head may be, so it's answered with a 414.
const TOO_LONG_TARGET: usize = 17_000;

/// How a client sends a request, on the connection it keeps for all of them.
#[derive(Clone, Copy)]
enum Ask {
    /// Its head in one write.
    Whole,
    /// Each line of its head in a write of its own, making way after each.
    Lines(Wait),
    /// Its head in two writes, cut where `Cut` says, making way between them.
    Split(Cut, Wait),
    /// Two requests in one write, the second cut off in its request line and finished after
    /// making way.
    Two(Wait),
    /// Part of its head, then the connection closed: nothing's answered.
    Closed,
    /// A request line longer than a whole head may be: a 414, and the connection closed.
    LongLine,
    /// More header lines than a head may have: a 431, and the connection closed.
    ManyHeaders,
}

impl Ask {
    /// Whether the server closes the connection after it, or the client does.
    fn ends(self) -> bool {
        matches!(self, Self::Closed | Self::LongLine | Self::ManyHeaders)
    }
}

/// Where a head written in two pieces is cut.
#[derive(Clone, Copy)]
enum Cut {
    /// Inside `HTTP/1.1` in the request line.
    InLine,
    /// Right after the request line's ending.
    AtLine,
    /// Between the request line's `\r` and its `\n`.
    InEnding,
    /// Before the blank line that ends the head.
    BeforeEnd,
}

/// A request's head for `path`, with a `host`.
fn head(path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n")
}

/// Where `cut` cuts `head`'s text: always inside it.
fn cut_at(head: &str, cut: Cut) -> usize {
    let line = head.find("\r\n").unwrap_or(0);
    match cut {
        Cut::InLine => line.saturating_sub(4),
        Cut::AtLine => line + 2,
        Cut::InEnding => line + 1,
        Cut::BeforeEnd => head.len() - 2,
    }
}

/// `text` as a Fennec string's literal.
fn literal(text: &str) -> String {
    format!("\"{}\"", text.replace('\r', "\\r").replace('\n', "\\n"))
}

/// What a message holding a number is, when the input asks for messages: each carries its number
/// and texts made from it, which whoever takes it checks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A struct of the number and a text.
    Note,
    /// `Sent`, a flat enum: `Even` or `Odd` by the number, with its text, or `Missing`.
    Flat,
    /// A struct of the number and a `Sent`.
    Carried,
    /// An enum holding a `Sent` and a text in one case, and only the number in the other.
    Wrapped,
    /// An enum whose case holds a list of texts, which isn't flat.
    Listed,
}

impl Shape {
    /// Its type's name in the program.
    fn name(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Flat => "Sent",
            Self::Carried => "Carried",
            Self::Wrapped => "Wrapped",
            Self::Listed => "Listed",
        }
    }

    /// Its types and the program's `made`, `number_of` and `held` for it.
    fn helpers(self) -> &'static str {
        match self {
            Self::Note => NOTE_HELPERS,
            Self::Flat => FLAT_HELPERS,
            Self::Carried => CARRIED_HELPERS,
            Self::Wrapped => WRAPPED_HELPERS,
            Self::Listed => LISTED_HELPERS,
        }
    }
}

/// What a message's text is, which every shape uses.
const SAID: &str = "
# What a message of `number` says: for an odd number, a text long enough to need a block of its own.
def said(number: int) -> str:
    if number % 2 == 0:
        return f\"n{number}\"
    return f\"note {number} says something long enough to need a block of its own\"
";

const NOTE_HELPERS: &str = "
struct Note:
    number: int
    said: str

# The message sent for `number`.
def made(number: int) -> Note:
    return Note(number=number, said=said(number))

def number_of(got: Note) -> int:
    return got.number

# Whether a message holds the text sent with its number.
def held(got: Note) -> bool:
    return got.said == said(got.number)
";

/// `Sent` and what reads it, which the shapes holding one use too.
const SENT: &str = "
enum Sent:
    Even(number: int, said: str)
    Odd(number: int, said: str)
    Missing(number: int)

# `number` as a `Sent`: its case by its parity, or `Missing` for one below zero.
def made_sent(number: int) -> Sent:
    if number < 0:
        return .Missing(number=number)
    if number % 2 == 0:
        return .Even(number=number, said=said(number))
    return .Odd(number=number, said=said(number))

def sent_number(got: Sent) -> int:
    match got:
        case .Even(number, _):
            return number
        case .Odd(number, _):
            return number
        case .Missing(number):
            return number

# Whether a `Sent` holds the text sent with its number.
def sent_held(got: Sent) -> bool:
    match got:
        case .Even(number, text):
            return text == said(number)
        case .Odd(number, text):
            return text == said(number)
        case .Missing(_):
            return True
";

const FLAT_HELPERS: &str = "
# The message sent for `number`.
def made(number: int) -> Sent:
    return made_sent(number)

def number_of(got: Sent) -> int:
    return sent_number(got)

# Whether a message holds the text sent with its number.
def held(got: Sent) -> bool:
    return sent_held(got)
";

const CARRIED_HELPERS: &str = "
struct Carried:
    seq: int
    sent: Sent

# The message sent for `number`.
def made(number: int) -> Carried:
    return Carried(seq=number, sent=made_sent(number))

def number_of(got: Carried) -> int:
    return got.seq

# Whether a message holds the text sent with its number, and the same number twice.
def held(got: Carried) -> bool:
    return sent_held(got.sent) and sent_number(got.sent) == got.seq
";

const WRAPPED_HELPERS: &str = "
enum Wrapped:
    Inside(sent: Sent, tag: str)
    Plain(number: int)

# The message sent for `number`: one in three only the number.
def made(number: int) -> Wrapped:
    if number % 3 == 1:
        return .Plain(number=number)
    return .Inside(sent=made_sent(number), tag=said(number + 1))

def number_of(got: Wrapped) -> int:
    match got:
        case .Inside(inner, _):
            return sent_number(inner)
        case .Plain(number):
            return number

# Whether a message holds the texts sent with its number.
def held(got: Wrapped) -> bool:
    match got:
        case .Inside(inner, tag):
            return sent_held(inner) and tag == said(sent_number(inner) + 1)
        case .Plain(_):
            return True
";

const LISTED_HELPERS: &str = "
enum Listed:
    Items(number: int, said: list[str])
    Gone(number: int)

# The message sent for `number`, or `Gone` for one below zero.
def made(number: int) -> Listed:
    if number < 0:
        return .Gone(number=number)
    return .Items(number=number, said=[said(number), said(number + 1)])

def number_of(got: Listed) -> int:
    match got:
        case .Items(number, _):
            return number
        case .Gone(number):
            return number

# Whether a message holds the texts sent with its number.
def held(got: Listed) -> bool:
    match got:
        case .Items(number, texts):
            return len(texts) == 2 and texts[0] == said(number) and texts[1] == said(number + 1)
        case .Gone(_):
            return True
";

/// The most tasks sending into one channel `main` gathers from, and the most each sends.
const MOST_SENDERS: usize = 3;
const MOST_SENT: usize = 3;

/// The program an input asks for.
struct Plan {
    tasks: Vec<Vec<Step>>,
    rounds: Vec<Vec<Step>>,
    main: Vec<MainStep>,
    /// Which tasks take their numbers within a time, from an inbox of 1, and are sent them so.
    timed: Vec<bool>,
    queue: Option<Queue>,
    pieces: Option<Pieces>,
    locks: Vec<Lock>,
    /// Whether each of the `parallel` call's ranges takes `a`'s lock and adds 1.
    pieces_lock: bool,
    gather: Option<Gather>,
    /// Whether the numbers sent to tasks and gathered are sent inside a message with a text.
    notes: bool,
    /// What that message is, when they are.
    shape: Shape,
    /// Each HTTP client's requests, in order, on its one connection.
    clients: Vec<Vec<Ask>>,
}

/// Tasks sending numbers into one channel, which `main` takes them all from once its steps are
/// done: the order they arrive in is the recording's.
struct Gather {
    /// How many numbers each sender sends, and its wait after each, if it makes way.
    senders: Vec<(usize, Option<Wait>)>,
    /// The channel's size.
    size: usize,
}

impl Gather {
    /// What sender `sender` sends as its `nth`: each number is one sender's, once.
    fn number(sender: usize, nth: usize) -> usize {
        1000 + 10 * sender + nth
    }

    /// Every number sent, by any sender.
    fn numbers(&self) -> impl Iterator<Item = usize> + '_ {
        self.senders
            .iter()
            .enumerate()
            .flat_map(|(sender, (count, _))| (0..*count).map(move |nth| Self::number(sender, nth)))
    }
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

    /// A client's requests, which end at the first that closes its connection.
    fn asks(&mut self) -> Vec<Ask> {
        let mut asks = Vec::new();
        for _ in 0..=self.below(MOST_ASKS) {
            let ask = match self.below(8) {
                0 | 1 => Ask::Whole,
                2 => Ask::Lines(self.wait()),
                3 => {
                    let cut = match self.below(4) {
                        0 => Cut::InLine,
                        1 => Cut::AtLine,
                        2 => Cut::InEnding,
                        _ => Cut::BeforeEnd,
                    };
                    Ask::Split(cut, self.wait())
                }
                4 => Ask::Two(self.wait()),
                5 => Ask::Closed,
                6 => Ask::LongLine,
                _ => Ask::ManyHeaders,
            };
            asks.push(ask);
            if ask.ends() {
                break;
            }
        }
        asks
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

/// The locks an input asks for, each put among a task's steps or `main`'s.
fn locks_among(bytes: &mut Bytes<'_>, tasks: &mut [Vec<Step>], main: &mut Vec<MainStep>) -> Vec<Lock> {
    (0..bytes.below(MOST_LOCKS + 1))
        .map(|number| {
            let taken = match bytes.below(4) {
                0 => Taken::A,
                1 => Taken::B,
                2 => Taken::AB,
                _ => Taken::BA,
            };
            let ending = match bytes.below(4) {
                0 | 1 => Ending::Kept,
                2 => Ending::Failed,
                _ => Ending::Crashed,
            };
            // A task's, or `main`'s, past the last task.
            let owner = bytes.below(tasks.len() + 1);
            match tasks.get_mut(owner) {
                Some(steps) => steps.insert(bytes.below(steps.len() + 1), Step::Lock(number)),
                None => main.insert(bytes.below(main.len() + 1), MainStep::Lock(number)),
            }
            Lock { taken, ending }
        })
        .collect()
}

fn plan(input: &[u8]) -> Plan {
    let mut bytes = Bytes { bytes: input, at: 0 };
    let tasks: Vec<Vec<Step>> = (0..bytes.below(MOST_TASKS + 1)).map(|_| bytes.steps(true)).collect();
    // A program with no task has a round, so there's always someone besides `main`.
    let rounds = (bytes.below(MOST_ROUNDS + 1)).max(usize::from(tasks.is_empty()));
    let rounds: Vec<Vec<Step>> = (0..rounds).map(|_| bytes.steps(false)).collect();
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
    let mut tasks = tasks;
    let mut locks = locks_among(&mut bytes, &mut tasks, &mut main);
    let pieces_lock = pieces.is_some() && bytes.below(2) == 1;
    let senders = bytes.below(MOST_SENDERS + 1);
    let gather = (senders > 0).then(|| {
        let senders = (0..senders)
            .map(|_| (1 + bytes.below(MOST_SENT), bytes.below(2).eq(&1).then(|| bytes.wait())))
            .collect();
        Gather { senders, size: 1 + bytes.below(2) }
    });
    // A server's rounds wait on the network, which isn't a wait on the program's own tasks, so a
    // program with them never deadlocks: it would wait for good, so it gets no stuck step.
    let stuck = match bytes.below(3) {
        1 => Some(Stuck::Receiving),
        2 => Some(Stuck::Sending),
        _ => None,
    };
    if let Some(stuck) = stuck.filter(|_| rounds.is_empty()) {
        main.insert(bytes.below(main.len() + 1), MainStep::Stuck(stuck));
    }
    // Which of the locks that would be kept read instead, by their numbers' bits.
    let reads = bytes.next();
    for (number, lock) in locks.iter_mut().enumerate() {
        if reads >> number & 1 == 1 && lock.ending == Ending::Kept {
            lock.ending = Ending::Read;
        }
    }
    let notes = bytes.below(2) == 1;
    let clients: Vec<Vec<Ask>> = (0..bytes.below(MOST_CLIENTS + 1)).map(|_| bytes.asks()).collect();
    let shape = match bytes.below(5) {
        1 => Shape::Flat,
        2 => Shape::Carried,
        3 => Shape::Wrapped,
        4 => Shape::Listed,
        _ => Shape::Note,
    };
    // The HTTP server waits on the network too, so `main` never deadlocks beside it.
    if !clients.is_empty() {
        main.retain(|step| !matches!(step, MainStep::Stuck(_)));
    }
    Plan { tasks, rounds, main, timed, queue, pieces, locks, pieces_lock, gather, notes, shape, clients }
}

impl Plan {
    /// Whether the program runs a server, its rounds' or HTTP's, which waits on the network.
    fn serves(&self) -> bool {
        self.servers() > 0
    }

    /// How many servers the program runs: its rounds' and HTTP's.
    fn servers(&self) -> usize {
        usize::from(!self.rounds.is_empty()) + usize::from(!self.clients.is_empty())
    }

    /// Every line the HTTP server and its clients print, without its new line: the path the server
    /// served, and the response a client read, or that it cut its request short.
    fn http_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (client, asks) in self.clients.iter().enumerate() {
            for (number, ask) in asks.iter().enumerate() {
                let name = format!("c{client}q{number}");
                let names = match ask {
                    Ask::Two(_) => vec![format!("{name}a"), format!("{name}b")],
                    _ => vec![name.clone()],
                };
                match ask {
                    Ask::Whole | Ask::Lines(_) | Ask::Split(..) | Ask::Two(_) => {
                        for name in names {
                            lines.push(format!("served /{name}"));
                            lines.push(format!("{name} HTTP/1.1 200 OK ok /{name}"));
                        }
                    }
                    Ask::Closed => lines.push(format!("{name} cut")),
                    Ask::LongLine => {
                        lines.push(format!("{name} HTTP/1.1 414 URI Too Long the request line is too long"));
                    }
                    Ask::ManyHeaders => lines.push(format!(
                        "{name} HTTP/1.1 431 Request Header Fields Too Large the headers are too big"
                    )),
                }
            }
        }
        lines
    }

    /// Whether any `Note` is sent: the input asked for them, and some task takes from its inbox or
    /// `main` gathers.
    fn sends_notes(&self) -> bool {
        self.notes && ((0..self.tasks.len()).any(|task| self.has_inbox(task)) || self.gather.is_some())
    }

    /// The type of what's sent to a task's inbox and gathered.
    fn message(&self) -> &'static str {
        if self.notes { self.shape.name() } else { "int" }
    }

    /// What's sent for `number`, which is Fennec that gives an `int`.
    fn sent_as(&self, number: &str) -> String {
        if self.notes { format!("made({number})") } else { number.to_owned() }
    }

    /// What's printed of `got`, a message taken: its number, and with a `Note`, whether its text is
    /// the one sent with that number.
    fn shown(&self, got: &str) -> String {
        if self.notes { format!("number_of({got}), held({got})") } else { got.to_owned() }
    }

    /// What a printed message ends with past its number.
    fn shown_end(&self) -> &'static str {
        if self.notes { " True" } else { "" }
    }

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

    /// Whether `main` gets stuck, so the program ends in a deadlock, with what it printed so far.
    fn deadlocks(&self) -> bool {
        self.main.iter().any(|step| matches!(step, MainStep::Stuck(_)))
    }

    fn jobs(&self) -> usize {
        self.main.iter().filter(|step| matches!(step, MainStep::Job)).count()
    }

    fn in_pieces(&self) -> bool {
        self.tasks.iter().chain(&self.rounds).flatten().any(|step| matches!(step, Step::Pieces(_)))
    }

    fn lock(&self, number: usize) -> Lock {
        self.locks.get(number).copied().unwrap_or(Lock { taken: Taken::A, ending: Ending::Kept })
    }

    /// Whether the program makes the two accounts.
    fn shares(&self) -> bool {
        !self.locks.is_empty() || self.pieces_lock
    }

    /// Which accounts steps lock, `a`'s and `b`'s.
    fn locked_by(&self, numbers: impl Iterator<Item = usize>) -> (bool, bool) {
        numbers
            .map(|number| self.lock(number).taken)
            .fold((false, false), |(a, b), taken| (a || taken.a(), b || taken.b()))
    }

    /// What `main` prints of each account at the end, `a`'s then `b`'s, its total and count of moves
    /// as `None` where the ranges a `parallel` call cut short decide them.
    fn accounts(&self) -> [(String, Option<(usize, usize)>); 2] {
        let kept = |held: fn(Taken) -> bool| {
            self.locks
                .iter()
                .enumerate()
                .filter(|(_, lock)| lock.ending == Ending::Kept && held(lock.taken))
                .fold((0, 0), |(total, moves), (number, _)| (total + number + 1, moves + 1))
        };
        [
            ("a".to_owned(), (!self.pieces_lock).then(|| kept(Taken::a))),
            ("b".to_owned(), Some(kept(Taken::b))),
        ]
    }

    /// The line `main` prints of an account at the end, as `plain` gives it.
    fn account_line(name: &str, counted: Option<(usize, usize)>) -> String {
        match counted {
            Some((total, moves)) => format!("total {name} {total} {moves} True"),
            None => format!("total {name} ? ? True"),
        }
    }

    /// The name a task or `main` prints its locks with, and how many of them end as `ending` does.
    fn locks_ending(&self, ending: Ending) -> Vec<(String, usize)> {
        let ends = |number: usize| self.lock(number).ending == ending;
        let mut each: Vec<(String, usize)> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(task, steps)| {
                let count =
                    steps.iter().filter(|step| matches!(step, Step::Lock(number) if ends(*number))).count();
                (format!("t{task}"), count)
            })
            .collect();
        let main =
            self.main.iter().filter(|step| matches!(step, MainStep::Lock(number) if ends(*number))).count();
        each.push(("main".to_owned(), main));
        each
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
                        printed.push(format!("{who} got {}{}\n", sent(&who, taken), self.shown_end()));
                        taken += 1;
                    }
                    Step::Lock(lock) if matches!(self.lock(*lock).ending, Ending::Kept | Ending::Read) => {}
                    Step::Lock(lock) => printed.push(format!("{who} undone {lock}\n")),
                }
            }
        }
        for (number, step) in self.main.iter().enumerate() {
            match step {
                MainStep::Print => printed.push(format!("main s{number}\n")),
                MainStep::Lock(lock)
                    if matches!(self.lock(*lock).ending, Ending::Failed | Ending::Crashed) =>
                {
                    printed.push(format!("main undone {lock}\n"));
                }
                _ => {}
            }
        }
        if self.shares() {
            for (name, counted) in self.accounts() {
                if counted.is_some() {
                    printed.push(format!("{}\n", Self::account_line(&name, counted)));
                }
            }
        }
        printed.extend((0..self.rounds.len()).map(|round| format!("done r{round}\n")));
        printed.extend(
            self.gather
                .iter()
                .flat_map(Gather::numbers)
                .map(|number| format!("gathered {number}{}\n", self.shown_end())),
        );
        if let Some(pieces) = &self.pieces {
            printed.push(format!("p {}\n", pieces.at));
        }
        printed.extend(self.pieces_over().map(|over| format!("{over}\n")));
        printed.extend(self.http_lines().into_iter().map(|line| format!("{line}\n")));
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
                    Step::Receive if self.is_timed(task) => {
                        lines.push(format!("{who} got ?{}", self.shown_end()));
                    }
                    Step::Receive => {
                        lines.push(format!("{who} got {}{}", sent(&who, taken), self.shown_end()));
                        taken += 1;
                    }
                    Step::Lock(lock) if self.lock(*lock).ending == Ending::Kept => {
                        lines.push(format!("{who} locked ?"));
                    }
                    Step::Lock(lock) if self.lock(*lock).ending == Ending::Read => {
                        lines.push(format!("{who} saw ?"));
                    }
                    Step::Lock(lock) => lines.push(format!("{who} undone {lock}")),
                }
            }
        }
        for (number, step) in self.main.iter().enumerate() {
            match step {
                MainStep::Print => lines.push(format!("main s{number}")),
                MainStep::Lock(lock) if self.lock(*lock).ending == Ending::Kept => {
                    lines.push("main locked ?".to_owned());
                }
                MainStep::Lock(lock) if self.lock(*lock).ending == Ending::Read => {
                    lines.push("main saw ?".to_owned());
                }
                MainStep::Lock(lock) => lines.push(format!("main undone {lock}")),
                _ => {}
            }
        }
        if self.shares() {
            lines.extend(self.accounts().iter().map(|(name, counted)| Self::account_line(name, *counted)));
        }
        lines.extend((0..self.rounds.len()).map(|round| format!("done r{round}")));
        lines.extend((0..self.jobs()).map(|job| format!("w? took {job}")));
        lines.extend(
            self.gather
                .iter()
                .flat_map(Gather::numbers)
                .map(|number| format!("gathered {number}{}", self.shown_end())),
        );
        lines.extend(self.pieces_over().map(str::to_owned));
        lines.extend(self.http_lines());
        lines.push("over".to_owned());
        lines
    }

    /// A line as `lines` gives it, with what the recording's order decides taken out: the number
    /// a timed task got, the worker that took a job, and the `parallel` call's ranges, which go.
    fn plain(&self, line: &str) -> Option<String> {
        if line.starts_with("p ") {
            return None;
        }
        if let Some((who, got)) = line.split_once(" got ")
            && let Some(task) = who.strip_prefix('t').and_then(|task| task.parse::<usize>().ok())
            && self.is_timed(task)
        {
            // Which number came is the recording's, but a note's text always matches it.
            let held = if self.notes { got.split_once(' ').map_or(" ?", |(_, held)| held) } else { "" };
            let held = if self.notes { format!(" {held}") } else { String::new() };
            return Some(format!("{who} got ?{held}"));
        }
        if line.starts_with('w')
            && let Some((_, job)) = line.split_once(" took ")
        {
            return Some(format!("w? took {job}"));
        }
        if let Some((who, _)) = line.split_once(" locked ") {
            return Some(format!("{who} locked ?"));
        }
        if let Some((who, _)) = line.split_once(" saw ") {
            return Some(format!("{who} saw ?"));
        }
        if self.pieces_lock
            && let Some(agree) = line.strip_prefix("total a ").and_then(|rest| rest.rsplit(' ').next())
        {
            return Some(format!("total a ? ? {agree}"));
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
        for (who, kept) in self.locks_ending(Ending::Kept) {
            let printed = text.matches(&format!("{who} locked ")).count();
            if printed != kept {
                return Err(format!("{who} printed {printed} kept locks, not {kept}"));
            }
        }
        for (who, read) in self.locks_ending(Ending::Read) {
            let printed = text.matches(&format!("{who} saw ")).count();
            if printed != read {
                return Err(format!("{who} printed {printed} reads, not {read}"));
            }
        }
        Ok(())
    }

    /// Whether `said` is what the locks that crash say on standard error, each once, in any order,
    /// with the length of moves they found, which the order decides.
    fn crashes_said(&self, said: &str) -> bool {
        let crashes = self.locks.iter().filter(|lock| lock.ending == Ending::Crashed).count();
        crash_reports(said).is_some_and(|reports| reports == crashes)
            && (said.is_empty() || said.ends_with('\n'))
    }

    /// Checks a recording of a program that deadlocks: it ended with 1, saying its deadlock once,
    /// after anything its locks' crashes said, and never printed what comes after `main` got stuck.
    fn deadlocked(&self, (printed, said, code): &Ran) -> Result<(), String> {
        let text = String::from_utf8_lossy(printed);
        let at = said.find(DEADLOCKED);
        let before = at.and_then(|at| said.get(..at)).unwrap_or(said);
        let crashed = self.locks.iter().filter(|lock| lock.ending == Ending::Crashed).count();
        let crashes_before = crash_reports(before).is_some_and(|reports| reports <= crashed);
        if *code != Some(1) || at.is_none() || said.matches(DEADLOCKED).count() != 1 || !crashes_before {
            return Err(format!("where it deadlocks, it ended with {code:?}, saying:\n{said}"));
        }
        if text.contains("never ") {
            return Err(format!("it went on past where it was stuck:\n{text}"));
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
fn body(plan: &Plan, who: &str, steps: &[Step], timed: bool) -> String {
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
                let _ = writeln!(
                    out,
                    "    stdout.print(\"{who} got\", {})",
                    plan.shown(&format!("got_{number}"))
                );
            }
            Step::Receive => {
                let _ = writeln!(out, "    got_{number} = inbox.receive() else {}", plan.sent_as("-1"));
                let _ = writeln!(
                    out,
                    "    stdout.print(\"{who} got\", {})",
                    plan.shown(&format!("got_{number}"))
                );
            }
            Step::Lock(lock) => out.push_str(&locking(plan, *lock, who, "    ")),
        }
    }
    out
}

/// What a lock's body does to each account it holds, at `indent`: adds `amount` to its total and
/// appends `appended` to its moves.
fn changing(taken: Taken, amount: usize, appended: usize, indent: &str) -> String {
    let mut out = String::new();
    for (holds, name) in [(taken.a(), "x"), (taken.b(), "y")] {
        if holds {
            let _ =
                write!(out, "{indent}{name}.total += {amount}\n{indent}{name}.moves.append({appended})\n");
        }
    }
    out
}

/// The accounts a function taking `taken` is handed, as parameters or as arguments.
fn accounts_given((a, b): (bool, bool), parameters: bool) -> Vec<String> {
    [(a, "a"), (b, "b")]
        .iter()
        .filter(|(needed, _)| *needed)
        .map(
            |(_, name)| if parameters { format!("{name}: shared Account") } else { format!("{name}={name}") },
        )
        .collect()
}

/// `accounts`, then `rest`, as one list of parameters or arguments.
fn joined(accounts: Vec<String>, rest: String) -> String {
    accounts.into_iter().chain((!rest.is_empty()).then_some(rest)).collect::<Vec<_>>().join(", ")
}

/// The statements that take lock `number` for `who`, at `indent`: the lock itself, when its
/// changes are kept, and otherwise a call to the function that's undone, which `who` handles.
fn locking(plan: &Plan, number: usize, who: &str, indent: &str) -> String {
    let Lock { taken, ending } = plan.lock(number);
    let arguments = accounts_given((taken.a(), taken.b()), false).join(", ");
    let inner = format!("{indent}    ");
    match ending {
        Ending::Kept => {
            let first = if taken.a() { "x" } else { "y" };
            format!(
                "{indent}{}\n{}{inner}stdout.print(\"{who} locked\", {first}.total)\n",
                taken.statement(),
                changing(taken, number + 1, number + 1, &inner)
            )
        }
        Ending::Read => {
            let first = if taken.a() { "x" } else { "y" };
            format!("{indent}{}\n{inner}stdout.print(\"{who} saw\", {first}.total)\n", taken.reading())
        }
        Ending::Failed => format!(
            "{indent}undone_{number}({arguments}) except Undone():\n{inner}stdout.print(\"{who} undone {number}\")\n"
        ),
        Ending::Crashed => format!(
            "{indent}crashed_{number}({arguments}) except Crashed():\n{inner}stdout.print(\"{who} undone {number}\")\n"
        ),
    }
}

/// The functions each lock that's undone calls: one that fails, or one that crashes in a task of
/// its own. Each adds 100 and appends 1000 first, so an account whose total and moves agree had
/// both undone.
fn undone(plan: &Plan) -> String {
    let mut out = String::new();
    for (number, Lock { taken, ending }) in plan.locks.iter().enumerate() {
        let parameters = accounts_given((taken.a(), taken.b()), true).join(", ");
        let arguments = accounts_given((taken.a(), taken.b()), false).join(", ");
        let changes = changing(*taken, 100, 1000, "        ");
        let first = if taken.a() { "x" } else { "y" };
        match ending {
            Ending::Kept | Ending::Read => {}
            Ending::Failed => {
                let _ = write!(
                    out,
                    "\ndef undone_{number}({parameters}) fails Undone:\n    {}\n{changes}        \
                     fail Undone(at={number})\n",
                    taken.statement()
                );
            }
            Ending::Crashed => {
                let _ = write!(
                    out,
                    "\ndef crashing_{number}({parameters}):\n    {}\n{changes}        _ = {first}.moves[99]\n\
                     \ndef crashed_{number}({parameters}) fails Crashed:\n    \
                     with launch(() => crashing_{number}({arguments}))?:\n        pass\n",
                    taken.statement()
                );
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
        Step::Print | Step::Receive | Step::Lock(_) => false,
    });
    (clock, steps.iter().any(|step| !matches!(step, Step::Wait(_))))
}

/// The parameters a function takes, or the arguments it's called with, for an inbox, a clock and
/// standard output where it needs each: only what it uses, since a parameter left unused is an
/// error.
fn given((inbox, clock, stdout): (Option<&str>, bool, bool), parameters: bool) -> String {
    let inbox_kind = format!("Inbox[{}]", inbox.unwrap_or("int"));
    let all = [
        (inbox.is_some(), "inbox", inbox_kind.as_str()),
        (clock, "clock", "Clock"),
        (stdout, "stdout", "Stdout"),
    ];
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
    plan.gather.is_some()
        || plan.main.iter().any(|step| match step {
            MainStep::Send(task) => !plan.is_timed(*task),
            MainStep::Job | MainStep::Stuck(Stuck::Sending) => true,
            MainStep::Print
            | MainStep::Wait(_)
            | MainStep::Pieces
            | MainStep::Lock(_)
            | MainStep::Stuck(Stuck::Receiving) => false,
        })
}

/// What the program imports, the error it fails with if any, and the helpers it calls.
fn heading(plan: &Plan) -> String {
    let inbox = (0..plan.tasks.len()).any(|task| plan.has_inbox(task));
    let mut imported = Vec::new();
    if plan.queue.is_some() || plan.gather.is_some() {
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
    if plan.queue.is_some() || plan.gather.is_some() || plan.deadlocks() {
        imported.push("channel");
    }
    let mut out = format!("from std.concurrent import {}\n", imported.join(", "));
    if plan.serves() {
        let mut net = vec!["Connection", "NetError"];
        if !plan.clients.is_empty() {
            net.extend(["Request", "Response"]);
        }
        if !plan.rounds.is_empty() {
            net.push("Server");
        }
        let _ = writeln!(out, "from std.net import {}", net.join(", "));
    }
    if plan.any_timed() {
        out.push_str("from std.time import Duration\n");
    }
    if plan.pieces.as_ref().is_some_and(|pieces| pieces.fails) {
        out.push_str("\nerror TooFar:\n    at: int\n");
    }
    if plan.locks.iter().any(|lock| lock.ending == Ending::Failed) {
        out.push_str("\nerror Undone:\n    at: int\n");
    }
    if plan.shares() {
        out.push_str("\nstruct Account:\n    total: int\n    moves: list[int]\n");
        out.push_str(&undone(plan));
    }
    if plan.sends_notes() {
        out.push_str(SAID);
        if matches!(plan.shape, Shape::Flat | Shape::Carried | Shape::Wrapped) {
            out.push_str(SENT);
        }
        out.push_str(plan.shape.helpers());
    }
    out.push_str(
        "\n# Counts `turns` turns, making way for others at the runtime's ticks.\n\
         def counted(turns: int) -> int:\n    mut total = 0\n    for turn in range(turns):\n        \
         total = (total + turn * turn) % 1_000_003\n    return total\n",
    );
    if plan.any_timed() {
        let message = plan.message();
        let (timed_out, closed) = (plan.sent_as("-1"), plan.sent_as("-2"));
        let _ = write!(
            out,
            "\n# The next number, if it comes within `wait`: -1 when it doesn't, -2 when the inbox closed.\n\
             def got_within(inbox: Inbox[{message}], wait: Duration) -> {message}:\n    \
             got = inbox.receive_within(wait) except Timeout(): return {timed_out}\n    return got else {closed}\n\
             \n# Sends `sent` within `wait`, or gives up, when the task has ended or its inbox stays full.\n\
             def handed(to: Sender[{message}], sent: {message}, wait: Duration):\n    \
             to.send_within(sent, wait) except Closed() | Timeout(): return\n",
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
        let locked = plan.locked_by(steps.iter().filter_map(|step| match step {
            Step::Lock(lock) => Some(*lock),
            _ => None,
        }));
        let parameters = joined(
            accounts_given(locked, true),
            given((plan.has_inbox(task).then(|| plan.message()), clock, stdout), true),
        );
        let _ = write!(out, "\ndef task_{task}({parameters}):\n");
        out.push_str(&body(plan, &format!("t{task}"), steps, plan.is_timed(task)));
    }
    for (sender, (count, wait)) in plan.gather.iter().flat_map(|gather| gather.senders.iter().enumerate()) {
        let clock = wait.as_ref().is_some_and(sleeps);
        main_clock |= clock;
        let parameters = if clock { ", clock: Clock" } else { "" };
        let _ = write!(
            out,
            "\ndef sender_{sender}(gathered: Channel[{}]{parameters}) fails Closed:\n",
            plan.message()
        );
        for nth in 0..*count {
            let _ = writeln!(
                out,
                "    gathered.send({})?",
                plan.sent_as(&Gather::number(sender, nth).to_string())
            );
            out.extend(wait.map(|wait| waited(wait, "    ")));
        }
    }
    if let Some(queue) = &plan.queue {
        for (worker, wait) in queue.workers.iter().enumerate() {
            let clock = wait.as_ref().is_some_and(sleeps);
            main_clock |= clock;
            let parameters = given((None, clock, true), true);
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
        let locked = (plan.pieces_lock, false);
        out.push_str(&parallel(
            pieces,
            plan.pieces_lock,
            &joined(accounts_given(locked, true), given((None, clock, true), true)),
            &joined(accounts_given(locked, false), given((None, clock, true), false)),
        ));
    }
    let mut server_clock = false;
    for (round, steps) in plan.rounds.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        server_clock |= clock;
        let parameters = given((None, clock, stdout), true);
        let _ = write!(out, "\ndef round_{round}({parameters}):\n");
        out.push_str(&body(plan, &format!("r{round}"), steps, false));
    }
    // What the server's rounds need between them, which the server is handed. It always prints,
    // as a round that can't read or write says so.
    let handed = given((None, server_clock, true), false);
    if !plan.rounds.is_empty() {
        out.push_str(&server(plan, &given((None, server_clock, true), true), &handed));
    }
    let (http, client_clock) = http_source(plan);
    out.push_str(&http);
    out.push_str(&main(plan, main_clock || server_clock || client_clock, &handed));
    out
}

/// The HTTP server's helpers and each client's function, if there are any, and whether a client
/// needs a clock.
fn http_source(plan: &Plan) -> (String, bool) {
    if plan.clients.is_empty() {
        return (String::new(), false);
    }
    let mut out = HTTP_HELPERS.to_owned();
    let mut clock = false;
    for (client, asks) in plan.clients.iter().enumerate() {
        let (text, sleeps) = client_source(client, asks);
        clock |= sleeps;
        out.push_str(&text);
    }
    (out, clock)
}

/// What the HTTP server answers with, and how a client reads a response.
const HTTP_HELPERS: &str = "
# Answers a request with its path, saying it served it.
def served(request: Request, stdout: Stdout) -> Response:
    stdout.print(\"served\", request.path)
    return Response(status=200, body=f\"ok {request.path}\".encode())

# Reads the response to `asked`, its status line, its headers and as much body as they say, and
# prints its status line and body.
def heard(conn: Connection, asked: str, stdout: Stdout) fails NetError:
    status = (conn.read_line()? else \"nothing\").strip()
    mut length = 0
    while True:
        line = (conn.read_line()? else \"\").strip()
        if line == \"\": break
        if line.lower().startswith(\"content-length:\"):
            length = int.parse(line[15:].strip()) except InvalidValue(): 0
    mut body = bytes([])
    while len(body) < length:
        more = conn.read(length - len(body))?
        if len(more) == 0: break
        body = body + more
    said = body.decode() except InvalidValue(): \"?\"
    stdout.print(asked, status, said)
";

/// Client `client`'s function, which sends `asks` on the connection it's given and reads each
/// response, and whether it needs a clock.
fn client_source(client: usize, asks: &[Ask]) -> (String, bool) {
    let mut body = String::new();
    let mut clock = false;
    let wrote = |body: &mut String, text: &str| {
        let _ = writeln!(body, "    conn.write({})?", literal(text));
    };
    for (number, ask) in asks.iter().enumerate() {
        let name = format!("c{client}q{number}");
        let path = format!("/{name}");
        match *ask {
            Ask::Whole => wrote(&mut body, &head(&path)),
            Ask::Lines(wait) => {
                for line in head(&path).split_inclusive("\r\n") {
                    wrote(&mut body, line);
                    clock |= matches!(wait, Wait::Sleep(_));
                    body.push_str(&waited(wait, "    "));
                }
            }
            Ask::Split(cut, wait) => {
                let whole = head(&path);
                let (first, rest) = whole.split_at(cut_at(&whole, cut));
                wrote(&mut body, first);
                clock |= matches!(wait, Wait::Sleep(_));
                body.push_str(&waited(wait, "    "));
                wrote(&mut body, rest);
            }
            Ask::Two(wait) => {
                let second = head(&format!("{path}b"));
                let (first, rest) = second.split_at(cut_at(&second, Cut::InLine));
                wrote(&mut body, &format!("{}{first}", head(&format!("{path}a"))));
                clock |= matches!(wait, Wait::Sleep(_));
                body.push_str(&waited(wait, "    "));
                wrote(&mut body, rest);
            }
            Ask::Closed => {
                wrote(&mut body, &format!("GET {path} HTTP/1.1\r\nhost:"));
                let _ = writeln!(body, "    conn.close()\n    stdout.print(\"{name} cut\")");
            }
            Ask::LongLine => {
                let _ = writeln!(
                    body,
                    "    conn.write(\"GET /\" + \"x\" * {TOO_LONG_TARGET} + \" HTTP/1.1\\r\\n\")?"
                );
            }
            Ask::ManyHeaders => {
                let _ = writeln!(
                    body,
                    "    mut many = {}\n    for number in range({TOO_MANY_HEADERS}):\n        \
                     many += f\"h{{number}}: v\\r\\n\"\n    conn.write(many + \"\\r\\n\")?",
                    literal(&format!("GET {path} HTTP/1.1\r\n"))
                );
            }
        }
        match ask {
            Ask::Two(_) => {
                let _ = writeln!(
                    body,
                    "    heard(conn, \"{name}a\", stdout)?\n    heard(conn, \"{name}b\", stdout)?"
                );
            }
            Ask::Closed => {}
            _ => {
                let _ = writeln!(body, "    heard(conn, \"{name}\", stdout)?");
            }
        }
    }
    let clock_parameter = if clock { ", clock: Clock" } else { "" };
    (
        format!(
            "\ndef client_{client}(conn: Connection{clock_parameter}, stdout: Stdout) fails NetError:\n{body}"
        ),
        clock,
    )
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
        let passed = given((None, clock, stdout), false);
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
fn parallel(pieces: &Pieces, locks: bool, parameters: &str, arguments: &str) -> String {
    let Pieces { count, at, fails, wait } = *pieces;
    // Each range that runs takes `a`'s lock, before the one that cuts the call short ends it.
    let locked = |indent: &str| {
        if locks {
            format!("{indent}lock a as ref x:\n{}", changing(Taken::A, 1, 1, &format!("{indent}    ")))
        } else {
            String::new()
        }
    };
    if fails {
        format!(
            "\ndef piece(number: int, {parameters}) -> int fails TooFar:\n    stdout.print(\"p\", number)\n{}{}    \
             if number == {at}:\n        fail TooFar(at=number)\n    return number\n\
             \ndef pieces({parameters}) -> int fails TooFar:\n    \
             passed = [piece(number, {arguments})? for number in parallel(range({count}))]\n    return len(passed)\n",
            waited(wait, "    "),
            locked("    ")
        )
    } else {
        format!(
            "\ndef pieces({parameters}):\n    for number in parallel(range({count})):\n        \
             stdout.print(\"p\", number)\n{}{}        if number == {at}:\n            break\n",
            waited(wait, "        "),
            locked("        ")
        )
    }
}

/// `main`: it runs the server, if there are rounds, opens the work queue and launches its workers,
/// if there are any, and launches each task, one block inside another, then asks for every round at
/// once and takes its own steps, sending each task the numbers it takes and the queue its jobs,
/// then each worker its stop, before it reads what each round said. `clock` says whether it needs
/// one, and `handed` is what the server's handed.
fn main(plan: &Plan, clock: bool, handed: &str) -> String {
    let serves = plan.serves();
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
    if plan.shares() {
        out.push_str(
            "    shared a = Account(total=0, moves=[])\n    shared b = Account(total=0, moves=[])\n",
        );
    }
    let mut indent = String::from("    ");
    out.push_str(&http_blocks(plan, &mut indent));
    if !plan.rounds.is_empty() {
        let _ = writeln!(out, "{indent}with net.listen(0)? as server:");
        indent.push_str("    ");
        let _ = writeln!(out, "{indent}port = server.port");
        let _ = writeln!(out, "{indent}with launch(() => serving(server, {handed}), stop_after=100ms)?:");
        indent.push_str("    ");
    }
    out.push_str(&channels(plan, &mut indent));
    for (task, steps) in plan.tasks.iter().enumerate() {
        let (clock, stdout) = needs(steps);
        let locked = plan.locked_by(steps.iter().filter_map(|step| match step {
            Step::Lock(lock) => Some(*lock),
            _ => None,
        }));
        let arguments = joined(
            accounts_given(locked, false),
            given((plan.has_inbox(task).then(|| plan.message()), clock, stdout), false),
        );
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
    if plan.shares() {
        for (name, held) in [("a", "x"), ("b", "y")] {
            let _ = write!(
                out,
                "    lock {name} as {held}:\n        stdout.print(\"total {name}\", {held}.total, len({held}.moves), \
                 {held}.total == sum({held}.moves))\n"
            );
        }
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
                let number = plan.sent_as(&number.to_string());
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
                let arguments = joined(
                    accounts_given((plan.pieces_lock, false), false),
                    given((None, clock, true), false),
                );
                if fails {
                    let _ = writeln!(out, "{indent}gave = pieces({arguments}) except TooFar(): -1");
                    let _ = writeln!(out, "{indent}stdout.print(\"pieces gave\", gave)");
                } else {
                    let _ = writeln!(out, "{indent}pieces({arguments})");
                    let _ = writeln!(out, "{indent}stdout.print(\"pieces ended\")");
                }
            }
            MainStep::Lock(lock) => out.push_str(&locking(plan, *lock, "main", indent)),
            MainStep::Stuck(stuck) => {
                let _ = writeln!(out, "{indent}with channel[int](size=1) as stuck:");
                match stuck {
                    Stuck::Receiving => {
                        let _ = writeln!(out, "{indent}    got_stuck = stuck.receive() else -1");
                        let _ = writeln!(out, "{indent}    stdout.print(\"never got\", got_stuck)");
                    }
                    Stuck::Sending => {
                        let _ = writeln!(out, "{indent}    stuck.send(1)?\n{indent}    stuck.send(2)?");
                        let _ = writeln!(out, "{indent}    stdout.print(\"never sent\")");
                    }
                }
            }
        }
    }
    // One stop for each worker, which each takes one of, once every job's been taken.
    for _ in plan.queue.iter().flat_map(|queue| &queue.workers) {
        let _ = writeln!(out, "{indent}jobs.send(-1)?");
    }
    // Every number the senders send, in the order they arrive.
    if let Some(gather) = &plan.gather {
        let _ = writeln!(
            out,
            "{indent}for nth in range({}):\n{indent}    got = gathered.receive() else {}\n{indent}    \
             stdout.print(\"gathered\", {})",
            gather.numbers().count(),
            plan.sent_as("-1 - nth"),
            plan.shown("got")
        );
    }
    out
}

/// The HTTP server's block and each client's connection and task, one block inside another from
/// `indent`, which ends inside the last.
fn http_blocks(plan: &Plan, indent: &mut String) -> String {
    let mut out = String::new();
    if plan.clients.is_empty() {
        return out;
    }
    let _ = writeln!(out, "{indent}with net.listen(0)? as web:");
    indent.push_str("    ");
    let _ = writeln!(out, "{indent}web_port = web.port");
    let _ = writeln!(
        out,
        "{indent}with launch(() => web.serve((request) => served(request, stdout)), stop_after=100ms)?:"
    );
    indent.push_str("    ");
    for client in 0..plan.clients.len() {
        let _ = writeln!(out, "{indent}with net.connect(\"127.0.0.1\", web_port)? as client_{client}_conn:");
        indent.push_str("    ");
    }
    for (client, asks) in plan.clients.iter().enumerate() {
        let sleeps = asks.iter().any(|ask| {
            matches!(
                ask,
                Ask::Lines(Wait::Sleep(_)) | Ask::Split(_, Wait::Sleep(_)) | Ask::Two(Wait::Sleep(_))
            )
        });
        let clock = if sleeps { ", clock" } else { "" };
        let _ = writeln!(
            out,
            "{indent}with launch(() => client_{client}(client_{client}_conn{clock}, stdout)?)?:"
        );
        indent.push_str("    ");
    }
    out
}

/// The work queue's and the gathered channel's blocks, each with its workers or senders launched in
/// it, one block inside another from `indent`, which ends inside the last.
fn channels(plan: &Plan, indent: &mut String) -> String {
    let mut out = String::new();
    if let Some(queue) = &plan.queue {
        let _ = writeln!(out, "{indent}with channel[int](size={}) as jobs:", queue.size);
        indent.push_str("    ");
        for (worker, wait) in queue.workers.iter().enumerate() {
            let arguments = given((None, matches!(wait, Some(Wait::Sleep(_))), true), false);
            let _ = writeln!(out, "{indent}with launch(() => worker_{worker}(jobs, {arguments}))?:");
            indent.push_str("    ");
        }
    }
    if let Some(gather) = &plan.gather {
        let _ = writeln!(out, "{indent}with channel[{}](size={}) as gathered:", plan.message(), gather.size);
        indent.push_str("    ");
        for (sender, (_, wait)) in gather.senders.iter().enumerate() {
            let clock = if matches!(wait, Some(Wait::Sleep(_))) { ", clock=clock" } else { "" };
            let _ = writeln!(out, "{indent}with launch(() => sender_{sender}(gathered{clock})?)?:");
            indent.push_str("    ");
        }
    }
    out
}

/// Prints the program an input writes, for `fennec-fuzz show replay <file>`.
pub(crate) fn written(input: &[u8]) -> String {
    source(&plan(input))
}

/// What a program says on standard error when its tasks wait only on each other.
const DEADLOCKED: &str = "error: a deadlock";

/// What a lock that crashes says on standard error, before the length of the moves it read past.
const CRASHED: &str = "error: index 99 is out of range for length ";

/// How many crash reports `said` is, if it's only crash reports: each a [`CRASHED`] line with the
/// length of the moves, then the lines saying where it happened, ` --> file:line:column` and the
/// source line under a `|` margin.
fn crash_reports(said: &str) -> Option<usize> {
    let mut reports = 0;
    for line in said.lines() {
        if let Some(length) = line.strip_prefix(CRASHED) {
            length.parse::<usize>().ok()?;
            reports += 1;
        } else if reports == 0 || !says_where(line) {
            return None;
        }
    }
    Some(reports)
}

/// Whether `line` is one of the lines under an error saying where it happened.
fn says_where(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("--> ")
        || line.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start().starts_with('|')
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
        if plan.deadlocks() {
            plan.deadlocked(&recorded).map_err(|why| format!("recorded on {recorded_on} threads, {why}"))?;
        } else if recorded.2 != Some(0) || !plan.crashes_said(&recorded.1) {
            return Err(format!(
                "recorded on {recorded_on} threads, it ended with {:?}, saying:\n{}",
                recorded.2, recorded.1
            ));
        }
        let text = String::from_utf8_lossy(&recorded.0);
        let mut counted = printed.clone();
        counted.dedup();
        // Where it deadlocks, it doesn't print what comes after: only its replays are held to it.
        if !plan.deadlocks()
            && let Some(print) = counted.iter().find(|print| {
                text.matches(print.as_str()).count() != printed.iter().filter(|each| each == print).count()
            })
        {
            return Err(format!(
                "recorded on {recorded_on} threads, it didn't print {print:?} as often as the program does:\n{text}"
            ));
        }
        if !plan.deadlocks() {
            plan.vague(&text).map_err(|why| format!("recorded on {recorded_on} threads, {why}:\n{text}"))?;
        }
        if !plan.in_pieces() && !plan.deadlocks() && plain_lines(&plan, &recorded.0) != expected {
            return Err(format!(
                "recorded on {recorded_on} threads, it printed lines it doesn't print, or not as often:\n{}",
                String::from_utf8_lossy(&recorded.0)
            ));
        }
        for replayed_on in [1, 4, 16] {
            let replayed = ran(&executable, &work, replayed_on, "FENNEC_REPLAY", &log)?;
            // What the recording's crashes said, in their order, and, only in a program with a
            // server, which the recording's end stops, that it's stopped, once for each server,
            // wherever the stop came among the crashes.
            let said = replayed.1.replacen(SERVER_STOPPED, "", plan.servers());
            let quiet = said == recorded.1;
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
