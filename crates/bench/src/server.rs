//! `fennec-bench server`: a server under load, against twins in Rust and Go. Many connections stay
//! open at once, some clients are slow to send, and what counts is how long the slowest requests
//! take and how much memory each connection costs. Each client is a thread of this program, which
//! sends a line, waits for it back, and waits a little before the next.
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

/// What a client's thread does: each request's time, for a client that isn't slow to send.
fn client(port: u16, at: usize, all_open: &Barrier) -> Result<Vec<f64>, String> {
    let fail = |err: std::io::Error| format!("client {at}: {err}");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(fail)?;
    stream.set_read_timeout(Some(NO_ANSWER)).map_err(fail)?;
    stream.set_nodelay(true).map_err(fail)?;
    let mut answers = BufReader::new(stream.try_clone().map_err(fail)?);
    all_open.wait();
    let slow = at.is_multiple_of(SLOW_EVERY);
    let mut times = Vec::with_capacity(ROUNDS);
    let mut answer = String::new();
    for round in 0..ROUNDS {
        let line = format!("client {at} sends line {round}\n");
        let started = Instant::now();
        if slow {
            let (first, rest) = line.split_at(line.len() / 2);
            stream.write_all(first.as_bytes()).map_err(fail)?;
            std::thread::sleep(SLOW_SEND);
            stream.write_all(rest.as_bytes()).map_err(fail)?;
        } else {
            stream.write_all(line.as_bytes()).map_err(fail)?;
        }
        answer.clear();
        answers.read_line(&mut answer).map_err(fail)?;
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

fn drive(server: &Child, port: u16) -> Result<Measured, String> {
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
    for client in clients {
        times.extend(client.join().map_err(|_| "a client stopped".to_owned())??);
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

/// Starts a server, measures it, and stops it.
fn measured(executable: &Path) -> Result<Measured, String> {
    let mut server = Command::new(executable)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| format!("couldn't start {}: {err}", executable.display()))?;
    let printed = server.stdout.take().ok_or("the server has no standard output")?;
    let mut first = String::new();
    BufReader::new(printed).read_line(&mut first).map_err(|err| err.to_string())?;
    let port = first.trim().parse().map_err(|_| format!("the server said {first:?}, not a port"));
    let result = port.and_then(|port| drive(&server, port));
    let _ = server.kill();
    let _ = server.wait();
    result
}

/// The best of each measure over a side's runs, as what's noise only makes things worse.
fn best_of(runs: usize, executable: &Path) -> Result<Measured, String> {
    let mut best: Option<Measured> = None;
    for _ in 0..runs {
        let run = measured(executable)?;
        best = Some(best.map_or(run, |best| Measured {
            median: best.median.min(run.median),
            p99: best.p99.min(run.p99),
            slowest: best.slowest.min(run.slowest),
            per_connection: best.per_connection.min(run.per_connection),
        }));
    }
    best.ok_or_else(|| "nothing ran".to_owned())
}

/// The Go twin, when Go is installed.
fn go_twin(dir: &Path, out: &Path) -> Result<Option<std::path::PathBuf>, String> {
    if Command::new("go").arg("version").output().is_err() {
        return Ok(None);
    }
    let executable = std::env::current_dir().map_err(|err| err.to_string())?.join(out).join("echo_go");
    let source = dir.join("echo.go");
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
    let rust = twins(dir, out)?.join("echo");
    let go = go_twin(dir, out)?;
    let mut sides = vec![("Fennec", best_of(runs, &ours)?), ("Rust", best_of(runs, &rust)?)];
    if let Some(go) = go {
        sides.push(("Go", best_of(runs, &go)?));
    }
    let text = section(&sides);
    println!("{text}");
    if let Some(path) = markdown {
        std::fs::write(path, &text).map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
    }
    Ok(())
}
