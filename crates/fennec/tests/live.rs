//! A long-running native server holds no more after many requests than after a few: the runtime's
//! count of what's live, `FENNEC_LIVE_REPORT`, is the same once it's idle again. The count says
//! what's live, not what's cached for reuse, so it tells a leak from a cache filling, as the
//! process's memory can't on a busy machine. Each kind of request the soak test sends is driven on
//! its own, from one client, so nothing's in flight when the count is read: crashes among them.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::executables::{Within, fennec_command, runtime_built, threads_runtime_built};

/// How many requests warm the server up, and how many more must leave nothing behind.
const WARM: usize = 300;
const MORE: usize = 1500;

/// How many hangups go before the client waits for the server to have taken them, well under the
/// 128 connections the Mac keeps waiting for a server.
const TAKEN_EVERY: usize = 50;

struct Server {
    child: Child,
    port: u16,
    report: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn folder(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-live-{name}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The soak test's server, built natively into `dir`.
fn build(dir: &Path) -> std::io::Result<PathBuf> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/soak/server.fen");
    let executable = dir.join("server");
    let built = fennec_command()
        .args(["build"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    Ok(executable)
}

/// The server, running on two workers in `dir` with its live count reported.
fn start(executable: &Path, dir: &Path) -> std::io::Result<Server> {
    let report = dir.join("live.txt");
    let data = dir.join("data");
    std::fs::create_dir_all(&data)?;
    let mut child = Command::new(executable)
        .arg(&data)
        .env("FENNEC_LIVE_REPORT", &report)
        .env("FENNEC_THREADS", "2")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?)
        .read_line(&mut line)?;
    let port = line.trim().parse().map_err(std::io::Error::other)?;
    Ok(Server { child, port, report })
}

/// What turns an error into one that says what was being done when it came, so a failure here says
/// which kind of request, which of them, and which call: a bare "Connection reset by peer" says
/// none of it.
fn during(what: impl std::fmt::Display) -> impl FnOnce(std::io::Error) -> std::io::Error {
    move |error| std::io::Error::new(error.kind(), format!("{what}: {error}"))
}

/// Sends one request on `stream` and reads its whole response, giving back its status.
fn ask(stream: &mut TcpStream, method: &str, path: &str, body: &str) -> std::io::Result<u16> {
    // In one write: `write!` on a socket sends each piece as a write of its own.
    let request =
        format!("{method} {path} HTTP/1.1\r\nhost: here\r\ncontent-length: {}\r\n\r\n{body}", body.len());
    stream.write_all(request.as_bytes()).map_err(during("writing the request"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(during("copying the connection"))?);
    let mut status = String::new();
    reader.read_line(&mut status).map_err(during("reading the status line"))?;
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(during("reading a header"))?;
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(during("reading the body"))?;
    status
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::Error::other(status))
}

/// One request of a kind, the `n`th: on a kept-alive connection, or on one of its own for the kinds
/// that end their connection.
fn send(server: &Server, kept: &mut Option<TcpStream>, kind: &str, n: usize) -> std::io::Result<()> {
    let connect = || {
        let stream = TcpStream::connect(("127.0.0.1", server.port)).map_err(during("connecting"))?;
        stream
            .set_read_timeout(Some(crate::executables::WALL_DEADLINE))
            .map_err(during("setting a timeout"))?;
        // As a real client would, so no write waits for the last to be acknowledged.
        stream.set_nodelay(true).map_err(during("setting no delay"))?;
        Ok::<_, std::io::Error>(stream)
    };
    match kind {
        "crash" => {
            // It closes its connection.
            let mut stream = connect()?;
            stream
                .write_all(format!("GET /crash?n={} HTTP/1.1\r\nhost: here\r\n\r\n", n % 100 + 1).as_bytes())
                .map_err(during("writing the request"))?;
            let mut back = [0; 16];
            let _ = stream.read(&mut back);
            return Ok(());
        }
        "garbage" => {
            let mut stream = connect()?;
            stream.write_all(b"NOT HTTP AT ALL\r\n\r\n").map_err(during("writing the request"))?;
            let mut back = [0; 16];
            let _ = stream.read(&mut back).map_err(during("reading the answer"))?;
            return Ok(());
        }
        "hangup" => {
            let mut stream = connect()?;
            stream
                .write_all(b"POST /note?n=1 HTTP/1.1\r\ncontent-length: 100\r\n\r\nhalf")
                .map_err(during("writing half a request"))?;
            drop(stream);
            // A hangup waits for nothing, so on a loaded machine the client could open connections
            // faster than the server took them, and the Mac resets those past the 128 it keeps
            // waiting (#1903). The server takes them in the order they came, so one answered
            // request after every `TAKEN_EVERY` says it has taken them all.
            if (n + 1).is_multiple_of(TAKEN_EVERY) {
                let mut stream = connect().map_err(during("waiting for the hangups to be taken"))?;
                ask(&mut stream, "GET", "/count", "")
                    .map_err(during("waiting for the hangups to be taken"))?;
            }
            return Ok(());
        }
        _ => {}
    }
    // Whether this request opened the kept-alive connection, or came after others on it.
    let on =
        if kept.is_none() { "on a kept-alive connection it opened" } else { "on the kept-alive connection" };
    if kept.is_none() {
        *kept = Some(connect()?);
    }
    let stream = kept.as_mut().ok_or_else(|| std::io::Error::other("no connection"))?;
    let status = match kind {
        "count" => ask(stream, "GET", "/count", ""),
        "stats" => ask(stream, "GET", "/stats", ""),
        "note" => ask(stream, "POST", &format!("/note?n={}", n + 1_000_000), "a note to keep"),
        "duplicate" => ask(stream, "POST", "/note?n=7", "the same note again"),
        "file" => ask(stream, "POST", &format!("/file?n={n}"), &"y".repeat(500)),
        _ => return Err(std::io::Error::other(format!("no kind {kind}"))),
    }
    .map_err(during(on))?;
    assert!(status == 200 || (kind == "duplicate" && status == 500), "{kind} answered {status}");
    Ok(())
}

/// The live count once the server's idle: read until no task or connection is left and it holds
/// for a while. A request still in flight under load, like a hangup's round waiting out its
/// connection, would otherwise be counted as though it had leaked.
fn settled(server: &Server) -> String {
    let started = Instant::now();
    let mut last = String::new();
    let mut held_since = Instant::now();
    while started.elapsed() < crate::executables::WALL_DEADLINE {
        std::thread::sleep(Duration::from_millis(150));
        // What's cached for reuse grows as the server warms up, within its bounds: only what's
        // live has to hold.
        let report = std::fs::read_to_string(&server.report).unwrap_or_default();
        let now = report
            .lines()
            .filter(|line| !line.starts_with("cached "))
            .fold(String::new(), |now, line| now + line + "\n");
        let idle =
            now.lines().any(|line| line == "tasks 0") && now.lines().any(|line| line == "connections 0");
        if now != last || !idle {
            last = now;
            held_since = Instant::now();
        } else if held_since.elapsed() > Duration::from_millis(900) {
            return last;
        }
    }
    // A count that never settles is a server that never went idle: a leak, or a request stuck.
    assert!(
        started.elapsed() < crate::executables::WALL_DEADLINE,
        "the server never went idle and stayed so:\n{last}"
    );
    last
}

/// Drives `kind` `WARM` times and reads the count, then `MORE` times and reads it again, on a
/// server of its own.
fn counts_before_and_after(executable: &Path, kind: &str) -> std::io::Result<(String, String)> {
    let dir = folder(kind)?;
    let server = start(executable, &dir)?;
    let mut kept = None;
    for n in 0..WARM {
        send(&server, &mut kept, kind, n).map_err(during(format!("{kind} request {n}")))?;
    }
    // Its kept-alive connection closes before each count, so each counts with nothing open.
    kept = None;
    let before = settled(&server);
    for n in WARM..WARM + MORE {
        send(&server, &mut kept, kind, n).map_err(during(format!("{kind} request {n}")))?;
    }
    kept = None;
    let after = settled(&server);
    drop(kept);
    drop(server);
    std::fs::remove_dir_all(&dir)?;
    Ok((before, after))
}

/// Each kind's count before and after, and those that changed, as the failure says them.
fn changed(kinds: &[&str]) -> std::io::Result<Vec<String>> {
    let built = folder(&format!("build-{}", kinds.join("-")))?;
    let executable = build(&built)?;
    let mut grew = Vec::new();
    for kind in kinds {
        let (before, after) = counts_before_and_after(&executable, kind)?;
        assert!(before.starts_with("blocks"), "{kind}: no live count was reported: {before:?}");
        if before != after {
            grew.push(format!("{kind}, after {WARM} requests:\n{before}after {MORE} more:\n{after}"));
        }
    }
    std::fs::remove_dir_all(&built)?;
    Ok(grew)
}

#[test]
fn a_server_holds_no_more_after_thousands_of_requests_of_each_kind() -> std::io::Result<()> {
    if !runtime_built() || !threads_runtime_built() {
        return Ok(());
    }
    let grew = changed(&["count", "stats", "note", "duplicate", "file", "garbage", "hangup", "crash"])?;
    assert!(grew.is_empty(), "the live count changed:\n{}", grew.join("\n"));
    Ok(())
}
