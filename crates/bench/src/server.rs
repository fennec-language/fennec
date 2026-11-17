//! `fennec-bench server`: a server under load, against twins in Rust and Go. Many connections stay
//! open at once, some clients are slow to send, and what counts is how long the slowest requests
//! take and how much memory each connection costs. Each client is a thread of this program, which
//! sends a line, waits for it back, and waits a little before the next. Then a chat server, whose
//! lines go to every member of a room (see `chat`), by lines delivered a second as well.
//!
//! Nothing here fails for being slow: only a server that doesn't answer with what it was sent. It
//! prints one Markdown section, and with `--markdown PATH` writes it there too for the report.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use crate::parallel::{built, twins};

mod chat;

const OUT_DIR: &str = "target/bench/server";
/// How many connections are open at once. Staying under 1024 keeps it within a system's usual limit
/// on open files.
const CONNECTIONS: usize = 900;
/// Requests each client sends.
const ROUNDS: usize = 20;
/// One client in this many sends each line in two parts, with a wait between.
const SLOW_EVERY: usize = 10;
const SLOW_SEND: Duration = Duration::from_millis(20);
/// The longest a client waits between requests.
const LONGEST_PAUSE_MS: u64 = 30;
/// How long a client waits for an answer before calling the run a failure.
const NO_ANSWER: Duration = Duration::from_secs(30);

/// What one server came to, in milliseconds and kilobytes.
#[derive(Clone, Copy, Default)]
struct Measured {
    median: f64,
    p99: f64,
    slowest: f64,
    per_connection: f64,
}

/// What a client's error says: which client, what it was doing, and what went wrong.
fn failed(at: usize, step: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |err| format!("client {at}, {step}: {err}")
}

/// What a client's thread does: each request's time, for a client that isn't slow to send.
fn client(port: u16, at: usize, all_open: &Barrier) -> Result<Vec<f64>, String> {
    let made = TcpStream::connect(("127.0.0.1", port)).map_err(failed(at, "connecting"));
    // Every client reaches the barrier, so one that couldn't connect doesn't hold the others.
    let set_up = made.and_then(|stream| {
        stream.set_read_timeout(Some(NO_ANSWER)).map_err(failed(at, "setting its timeout"))?;
        stream.set_nodelay(true).map_err(failed(at, "setting no delay"))?;
        Ok(stream)
    });
    all_open.wait();
    // Read and written through the one socket, so a client holds one file, not two: 900 clients
    // stay under a limit of 1,024 open files, as `CONNECTIONS` says.
    let stream = set_up?;
    let (mut writing, mut answers) = (&stream, BufReader::new(&stream));
    let slow = at.is_multiple_of(SLOW_EVERY);
    let mut times = Vec::with_capacity(ROUNDS);
    let mut answer = String::new();
    for round in 0..ROUNDS {
        let line = format!("client {at} sends line {round}\n");
        let started = Instant::now();
        let sending = format!("sending line {round}");
        if slow {
            let (first, rest) = line.split_at(line.len() / 2);
            writing.write_all(first.as_bytes()).map_err(failed(at, &sending))?;
            std::thread::sleep(SLOW_SEND);
            writing.write_all(rest.as_bytes()).map_err(failed(at, &sending))?;
        } else {
            writing.write_all(line.as_bytes()).map_err(failed(at, &sending))?;
        }
        answer.clear();
        answers.read_line(&mut answer).map_err(failed(at, &format!("reading the answer to line {round}")))?;
        if answer != line {
            return Err(format!("client {at} sent {line:?} and heard {answer:?}"));
        }
        if !slow {
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let pause = u64::try_from((at + round) % 31).unwrap_or(0).min(LONGEST_PAUSE_MS);
        std::thread::sleep(Duration::from_millis(pause));
    }
    Ok(times)
}

/// How much memory a process holds, in kilobytes.
fn resident(server: &Child) -> Result<f64, String> {
    let shown = Command::new("ps")
        .args(["-o", "rss=", "-p", &server.id().to_string()])
        .output()
        .map_err(|err| format!("couldn't run ps: {err}"))?;
    let text = String::from_utf8_lossy(&shown.stdout);
    text.trim().parse().map_err(|_| format!("ps said {text:?}"))
}

/// The value this percent of the way through sorted times.
fn at_percent(sorted: &[f64], percent: usize) -> f64 {
    let at = sorted.len().saturating_sub(1) * percent / 100;
    sorted.get(at).copied().unwrap_or(0.0)
}

fn drive(server: &mut Child, port: u16) -> Result<Measured, String> {
    let before = resident(server)?;
    let all_open = Arc::new(Barrier::new(CONNECTIONS + 1));
    let clients: Vec<_> = (0..CONNECTIONS)
        .map(|at| {
            let all_open = Arc::clone(&all_open);
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn(move || client(port, at, &all_open))
                .map_err(|err| format!("couldn't start a client: {err}"))
        })
        .collect::<Result<_, _>>()?;
    all_open.wait();
    // Every connection has its task or thread by now, part way through its first requests.
    std::thread::sleep(Duration::from_millis(200));
    let during = resident(server)?;
    let mut times = Vec::with_capacity(CONNECTIONS * ROUNDS);
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
    if let Some(first) = first {
        let server = server_state(server);
        return Err(format!("{failures} of {CONNECTIONS} clients failed, {server}; the first: {first}"));
    }
    times.sort_by(f64::total_cmp);
    let count = f64::from(u32::try_from(CONNECTIONS).unwrap_or(1));
    Ok(Measured {
        median: at_percent(&times, 50),
        p99: at_percent(&times, 99),
        slowest: times.last().copied().unwrap_or(0.0),
        per_connection: (during - before).max(0.0) / count,
    })
}

/// Whether the server's still up, once clients have failed: that tells a server that stopped from
/// one that dropped a connection.
fn server_state(server: &mut Child) -> String {
    match server.try_wait() {
        Ok(Some(status)) => format!("the server had stopped, {status}"),
        Ok(None) => "the server was still running".to_owned(),
        Err(err) => format!("couldn't tell whether the server was running: {err}"),
    }
}

/// Starts a server, measures it with `measure` given the port it printed, and stops it.
fn with_server<T>(
    executable: &Path,
    measure: impl FnOnce(&mut Child, u16) -> Result<T, String>,
) -> Result<T, String> {
    let mut server = Command::new(executable)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| format!("couldn't start {}: {err}", executable.display()))?;
    let printed = server.stdout.take().ok_or("the server has no standard output")?;
    let mut first = String::new();
    BufReader::new(printed).read_line(&mut first).map_err(|err| err.to_string())?;
    let port = first.trim().parse().map_err(|_| format!("the server said {first:?}, not a port"));
    let result = port.and_then(|port| measure(&mut server, port));
    let _ = server.kill();
    let _ = server.wait();
    result
}

/// Starts a server, measures it, and stops it.
fn measured(executable: &Path) -> Result<Measured, String> {
    with_server(executable, drive)
}

/// The best of each measure over a side's runs, as what's noise only makes things worse.
fn best_of(name: &str, runs: usize, executable: &Path) -> Result<Measured, String> {
    let mut best: Option<Measured> = None;
    for at in 1..=runs {
        let run =
            measured(executable).map_err(|err| format!("{name}'s server, run {at} of {runs}: {err}"))?;
        best = Some(best.map_or(run, |best| Measured {
            median: best.median.min(run.median),
            p99: best.p99.min(run.p99),
            slowest: best.slowest.min(run.slowest),
            per_connection: best.per_connection.min(run.per_connection),
        }));
    }
    best.ok_or_else(|| "nothing ran".to_owned())
}

/// The Go twin of the server `name`, `name.go`, when Go is installed.
fn go_twin(dir: &Path, out: &Path, name: &str) -> Result<Option<std::path::PathBuf>, String> {
    if Command::new("go").arg("version").output().is_err() {
        return Ok(None);
    }
    let executable =
        std::env::current_dir().map_err(|err| err.to_string())?.join(out).join(format!("{name}_go"));
    let source = dir.join(format!("{name}.go"));
    let built = Command::new("go").arg("build").arg("-o").arg(&executable).arg(&source).output();
    let built = built.map_err(|err| format!("couldn't run go: {err}"))?;
    if built.status.success() {
        return Ok(Some(executable));
    }
    Err(format!("the Go twin didn't build: {}", String::from_utf8_lossy(&built.stderr)))
}

fn section(sides: &[(&str, Measured)]) -> String {
    let (cpu, cores) = (crate::machine::cpu(), crate::machine::cores());
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let mut text = format!("## A server under load, on {os} {arch}, {cpu} with {cores} cores\n\n");
    let _ = writeln!(
        text,
        "{CONNECTIONS} connections open at once, each sending {ROUNDS} lines, one in {SLOW_EVERY} slow to send.\n"
    );
    text += "| server | median | 99th percentile | slowest | memory per connection |\n";
    text += "|---|---:|---:|---:|---:|\n";
    for (name, measured) in sides {
        let _ = writeln!(
            text,
            "| {name} | {:.2} ms | {:.2} ms | {:.2} ms | {:.1} KB |",
            measured.median, measured.p99, measured.slowest, measured.per_connection
        );
    }
    text += "\nBest of each side's runs, measure by measure. Time is from sending a line to hearing it \
             back, for the clients that aren't slow. Memory is what the server holds with all of them \
             open, over what it held before. Rust is a thread for each connection, and Go a goroutine.\n";
    text
}

/// Runs the section. It fails only for a server that doesn't build or doesn't answer.
pub(crate) fn run(fennec: &Path, dir: &Path, runs: usize, markdown: Option<&Path>) -> Result<(), String> {
    let out = Path::new(OUT_DIR);
    std::fs::create_dir_all(out).map_err(|err| format!("couldn't make {}: {err}", out.display()))?;
    let ours = built(fennec, &dir.join("echo.fen"), out.join("echo"), true)?;
    let rust_twins = twins(dir, out)?;
    let rust = rust_twins.join("echo");
    let go = go_twin(dir, out, "echo")?;
    let mut sides =
        vec![("Fennec", best_of("Fennec", runs, &ours)?), ("Rust", best_of("Rust", runs, &rust)?)];
    if let Some(go) = go {
        sides.push(("Go", best_of("Go", runs, &go)?));
    }
    let our_chat = built(fennec, &dir.join("chat.fen"), out.join("chat"), true)?;
    let mut chats = vec![
        ("Fennec", chat::best_of("Fennec", runs, &our_chat)?),
        ("Rust, tokio", chat::best_of("Rust's tokio", runs, &rust_twins.join("chat"))?),
        ("Rust, threads", chat::best_of("Rust's threaded", runs, &rust_twins.join("chat_threads"))?),
    ];
    if let Some(go) = go_twin(dir, out, "chat")? {
        chats.push(("Go", chat::best_of("Go", runs, &go)?));
    }
    let text = format!("{}\n{}", section(&sides), chat::section(&chats));
    println!("{text}");
    if let Some(path) = markdown {
        std::fs::write(path, &text).map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
    }
    Ok(())
}
