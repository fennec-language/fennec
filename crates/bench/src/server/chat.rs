//! The chat load of `fennec-bench server`: clients in rooms, each saying lines that the server
//! sends to every member of its room, the sender too, as `chat.fen` and its twins do. Each client
//! is a thread of this program. It joins, waits until every client has, then says its lines, each
//! once its own last one has come back, reading everything its room says as it goes. Every line
//! heard is checked: from a member of the room, in the order its sender said them, and none lost.
//!
//! Two runs make a server's numbers. One is paced, as people chat, with pauses and some clients
//! slow to send: how long a line takes to come back, and the memory each connection costs. The
//! other floods: each client says its next line as soon as its last comes back, so the server is
//! never idle, and what's measured is how many lines it delivers a second.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use super::{LONGEST_PAUSE_MS, NO_ANSWER, SLOW_EVERY, SLOW_SEND, at_percent, failed, resident, server_state};

/// How many clients chat at once.
pub(super) const CHATTERS: usize = 500;
/// How many rooms they're in, the same number in each.
pub(super) const ROOMS: usize = 50;
/// Lines each client says.
pub(super) const LINES: usize = 40;
/// What each line says after its number.
const SAID: &str = "anyone up for the raid at nine";

/// What one chat server came to: lines delivered a second when flooded, and from the paced run,
/// how long a line took to come back in milliseconds and what a connection cost in kilobytes.
#[derive(Clone, Copy, Default)]
pub(super) struct Chatted {
    pub(super) per_second: f64,
    pub(super) median: f64,
    pub(super) p99: f64,
    pub(super) slowest: f64,
    pub(super) per_connection: f64,
}

/// How the clients chat.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pace {
    /// With pauses, and one client in `SLOW_EVERY` sending each line in two parts.
    Paced,
    /// Each line as soon as the last came back.
    Flood,
}

/// A client's room.
fn room_of(at: usize) -> usize {
    at % ROOMS
}

/// Connects as client `at`, joins its room and waits for the server to say it has.
fn joined(port: u16, at: usize) -> Result<TcpStream, String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(failed(at, "connecting"))?;
    stream.set_read_timeout(Some(NO_ANSWER)).map_err(failed(at, "setting its timeout"))?;
    stream.set_nodelay(true).map_err(failed(at, "setting no delay"))?;
    let room = room_of(at);
    (&stream).write_all(format!("JOIN r{room} u{at}\n").as_bytes()).map_err(failed(at, "joining"))?;
    // Nothing else comes until every client has joined, so this reader holds nothing more.
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).map_err(failed(at, "hearing it had joined"))?;
    if answer != format!("JOINED r{room}\n") {
        return Err(format!("client {at} joined r{room} and heard {answer:?}"));
    }
    Ok(stream)
}

/// What a client keeps of what its room says, to check each line it hears.
struct Heard {
    at: usize,
    /// The number each member says next.
    next: HashMap<String, usize>,
    count: usize,
}

impl Heard {
    /// Checks a line: from a member of the room, the next its sender said, as it was said. Gives
    /// who said it and its number.
    fn check(&mut self, line: &str) -> Result<(String, usize), String> {
        let at = self.at;
        let wrong = || format!("client {at} heard {line:?}");
        let mut parts = line.strip_suffix('\n').ok_or_else(wrong)?.splitn(3, ' ');
        let (Some(from), Some(number), Some(said)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(wrong());
        };
        let number: usize = number.parse().map_err(|_| wrong())?;
        let next = self.next.get_mut(from).ok_or_else(wrong)?;
        if number != *next || said != SAID {
            return Err(format!("{}, where u{from}'s next line was {next}", wrong()));
        }
        *next += 1;
        self.count += 1;
        Ok((from.to_owned(), number))
    }
}

/// What a client's thread does: each of its lines' times to come back, for one that isn't slow.
fn chatter(port: u16, at: usize, all_joined: &Barrier, pace: Pace) -> Result<Vec<f64>, String> {
    let made = joined(port, at);
    // Every client reaches the barrier, so one that couldn't join doesn't hold the others.
    all_joined.wait();
    let stream = made?;
    let (mut writing, mut lines) = (&stream, BufReader::new(&stream));
    let members: Vec<usize> = (0..CHATTERS).filter(|other| room_of(*other) == room_of(at)).collect();
    let mut heard =
        Heard { at, next: members.iter().map(|member| (format!("u{member}"), 0)).collect(), count: 0 };
    let me = format!("u{at}");
    let slow = pace == Pace::Paced && at.is_multiple_of(SLOW_EVERY);
    let mut times = Vec::with_capacity(LINES);
    let mut line = String::new();
    let mut hear =
        |lines: &mut BufReader<&TcpStream>, heard: &mut Heard| -> Result<(String, usize), String> {
            line.clear();
            let read = lines.read_line(&mut line).map_err(failed(at, "reading its room"))?;
            if read == 0 {
                return Err(format!("client {at}: the server hung up after {} lines", heard.count));
            }
            heard.check(&line)
        };
    for round in 0..LINES {
        let saying = format!("{round} {SAID}\n");
        let sending = format!("saying line {round}");
        let started = Instant::now();
        if slow {
            let (first, rest) = saying.split_at(saying.len() / 2);
            writing.write_all(first.as_bytes()).map_err(failed(at, &sending))?;
            std::thread::sleep(SLOW_SEND);
            writing.write_all(rest.as_bytes()).map_err(failed(at, &sending))?;
        } else {
            writing.write_all(saying.as_bytes()).map_err(failed(at, &sending))?;
        }
        // Everything heard until this client's own line comes back.
        while hear(&mut lines, &mut heard)?.0 != me {}
        if !slow {
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        if pace == Pace::Paced {
            let pause = u64::try_from((at + round) % 31).unwrap_or(0).min(LONGEST_PAUSE_MS);
            std::thread::sleep(Duration::from_millis(pause));
        }
    }
    // The rest of what the room says.
    while heard.count < members.len() * LINES {
        hear(&mut lines, &mut heard)?;
    }
    Ok(times)
}

/// One run: every client's times, how long the chatting took, and the memory each connection
/// cost.
fn drive(server: &mut Child, port: u16, pace: Pace) -> Result<(Vec<f64>, Duration, f64), String> {
    let before = resident(server)?;
    let all_joined = Arc::new(Barrier::new(CHATTERS + 1));
    let clients: Vec<_> = (0..CHATTERS)
        .map(|at| {
            let all_joined = Arc::clone(&all_joined);
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn(move || chatter(port, at, &all_joined, pace))
                .map_err(|err| format!("couldn't start a client: {err}"))
        })
        .collect::<Result<_, _>>()?;
    all_joined.wait();
    let started = Instant::now();
    // Every connection has its tasks or threads by now, part way through its first lines.
    std::thread::sleep(Duration::from_millis(200));
    let during = resident(server)?;
    let mut times = Vec::with_capacity(CHATTERS * LINES);
    let (mut first, mut failures) = (None, 0);
    for client in clients {
        match client.join() {
            Ok(Ok(taken)) => times.extend(taken),
            Ok(Err(err)) => {
                failures += 1;
                first.get_or_insert(err);
            }
            Err(_) => {
                failures += 1;
                first.get_or_insert_with(|| "a client's thread panicked".to_owned());
            }
        }
    }
    let took = started.elapsed();
    if let Some(first) = first {
        let server = server_state(server);
        return Err(format!("{failures} of {CHATTERS} clients failed, {server}; the first: {first}"));
    }
    let count = f64::from(u32::try_from(CHATTERS).unwrap_or(1));
    Ok((times, took, (during - before).max(0.0) / count))
}

/// One chat server measured: a paced run, then a flood, each on a server of its own.
pub(super) fn measured(executable: &Path) -> Result<Chatted, String> {
    let (mut times, _, per_connection) =
        super::with_server(executable, |server, port| drive(server, port, Pace::Paced))
            .map_err(|err| format!("paced: {err}"))?;
    let (_, took, _) = super::with_server(executable, |server, port| drive(server, port, Pace::Flood))
        .map_err(|err| format!("flooded: {err}"))?;
    times.sort_by(f64::total_cmp);
    let delivered = CHATTERS * (CHATTERS / ROOMS) * LINES;
    Ok(Chatted {
        per_second: f64::from(u32::try_from(delivered).unwrap_or(0)) / took.as_secs_f64().max(1e-9),
        median: at_percent(&times, 50),
        p99: at_percent(&times, 99),
        slowest: times.last().copied().unwrap_or(0.0),
        per_connection,
    })
}

/// The best of each measure over a side's runs, as what's noise only makes things worse.
pub(super) fn best_of(name: &str, runs: usize, executable: &Path) -> Result<Chatted, String> {
    let mut best: Option<Chatted> = None;
    for at in 1..=runs {
        let run =
            measured(executable).map_err(|err| format!("{name}'s chat server, run {at} of {runs}: {err}"))?;
        best = Some(best.map_or(run, |best| Chatted {
            per_second: best.per_second.max(run.per_second),
            median: best.median.min(run.median),
            p99: best.p99.min(run.p99),
            slowest: best.slowest.min(run.slowest),
            per_connection: best.per_connection.min(run.per_connection),
        }));
    }
    best.ok_or_else(|| "nothing ran".to_owned())
}

/// The chat section's table.
pub(super) fn section(sides: &[(&str, Chatted)]) -> String {
    let members = CHATTERS / ROOMS;
    let mut text = format!(
        "### A chat server\n\n{CHATTERS} clients in {ROOMS} rooms of {members}, each saying {LINES} lines, \
         which the server sends to every member of the room.\n\n"
    );
    text += "| server | lines delivered a second | median | 99th percentile | slowest | memory per connection |\n";
    text += "|---|---:|---:|---:|---:|---:|\n";
    for (name, chatted) in sides {
        let _ = writeln!(
            text,
            "| {name} | {:.0} | {:.2} ms | {:.2} ms | {:.2} ms | {:.1} KB |",
            chatted.per_second, chatted.median, chatted.p99, chatted.slowest, chatted.per_connection
        );
    }
    text += "\nLines a second are from a run where each client says its next line as soon as its last \
             comes back. The times are from a run paced as people chat, one client in ten slow to send: \
             from saying a line to hearing it back from the server, for the clients that aren't slow. \
             Each is the best of its side's runs. Every side has a hub that owns the rooms and a task, \
             thread or goroutine to read and one to write each connection. Rust's are on tokio, as a \
             Rust server would be, and on a thread for each.\n";
    text
}
