//! The `http-audited` target: `std.net`'s HTTP server built for the heap audit, as
//! `FENNEC_AUDIT=1` builds a program, sent an input whole and then cut short at each of a fixed
//! set of places, each on a connection of its own. The server runs on one worker and then on
//! four, and each time it's stopped with `SIGTERM`, so it ends on its own and audits its heap as
//! it does.
//!
//! Where `http-server` holds what the server says to a reader of HTTP/1.1, this holds the server
//! to itself: what each connection hears has to be the same on one worker as on four, the server
//! has to end with nothing wrong, and its audit has to find no block left with no holder, or held
//! otherwise than counted. A request refused partway, or cut off, is where a block would be left.
//!
//! Every read waits as long as it takes: a server that never answers or never ends is the
//! supervisor's to catch, as an input that's stuck, never a reading of the clock here. Each
//! client resets its connection once it's heard everything, so none is left waiting out its close
//! holding a port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use crate::http_sessions::split_points;

/// The server: `http-server`'s, answering each request with what it was asked, which serves
/// until it's sent a signal, so it ends on its own and its heap is audited as it does.
const SERVER: &str = "\
from std import json
from std.net import Request, Response

struct Echo:
    method: str
    path: str
    query: dict[str, str]
    headers: dict[str, str]
    body: list[int]

def respond(request: Request) -> Response:
    echo = Echo(
        method=request.method,
        path=request.path,
        query=request.query,
        headers=request.headers,
        body=[b for b in request.body],
    )
    return Response(status=200, body=json.dumps(echo).encode())

def main(net: Net, signals: Signals, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=5s, until=signals)
";

/// The most bytes an input is cut short after every one of: past that, it's cut at a few places
/// from its start, its end and between, as `split_points` places them. An input is met about 50
/// times on each server at most, so a run never holds many connections at once or in all.
const EVERY_BYTE_UP_TO: usize = 16;

/// The worker counts each input is met on, which have to say the same.
const WORKERS: [usize; 2] = [1, 4];

/// The server, built once in this process for the heap audit. Nothing that can be built here, or
/// a server that doesn't build, is a failure that says why, never a reason to skip: this target is
/// only run where it's asked for.
fn built() -> Result<PathBuf, String> {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            crate::llvm::ready().map_err(|why| {
                format!("`http-audited` needs its server built for the heap audit, and nothing can be built here: {why}")
            })?;
            let work = crate::llvm::work().join("http-audited");
            std::fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
            let (program, executable) = (work.join("program.fen"), work.join("program"));
            std::fs::write(&program, SERVER).map_err(|err| format!("{}: {err}", program.display()))?;
            let mut command = Command::new(crate::llvm::fennec());
            command
                .args(["build", "--llvm"])
                .arg(&program)
                .arg("-o")
                .arg(&executable)
                // Only this build is for the audit, whatever the fuzzer itself was started with.
                .env("FENNEC_AUDIT", "1")
                .env("FENNEC_HOME", &work)
                .env("FENNEC_NO_CACHE", "1")
                .env("FENNEC_NO_DAEMON", "1");
            let built = crate::llvm::built_within(&mut command, SERVER.lines().count())?;
            if !built.status.success() {
                return Err(format!(
                    "`http-audited`'s server didn't build for the heap audit, which needs the runtimes built with `audit` beside `fennec` ({}):\n{}",
                    built.status,
                    String::from_utf8_lossy(&built.stderr)
                ));
            }
            Ok(executable)
        })
        .clone()
}

/// A server that's running, stopped if it's dropped before it ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The server started on `workers` workers for the heap audit, and the port it says it listens on.
fn started(executable: &std::path::Path, workers: usize) -> Result<(Running, u16), String> {
    let mut server = Running(
        Command::new(executable)
            .env("FENNEC_THREADS", workers.to_string())
            .env("FENNEC_AUDIT", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("couldn't start the server: {err}"))?,
    );
    let mut first = String::new();
    if let Some(out) = server.0.stdout.take() {
        BufReader::new(out).read_line(&mut first).map_err(|err| err.to_string())?;
    }
    let port: u16 = first.trim().parse().map_err(|_| format!("the server said {first:?}, not its port"))?;
    Ok((server, port))
}

/// Everything the server says to `bytes`, sent with the sending side then closed, read until it
/// closes. A reset reads as what came before it, marked. The connection is reset when it's dropped,
/// so it leaves nothing waiting out its close: set as it opens, since macOS refuses it once both
/// sides have closed.
fn asked(port: u16, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|err| format!("couldn't connect: {err}"))?;
    socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO)).map_err(|err| err.to_string())?;
    // The server may close before it's read everything, which isn't wrong.
    let _ = stream.write_all(bytes);
    let _ = stream.shutdown(Shutdown::Write);
    let mut heard = Vec::new();
    if stream.read_to_end(&mut heard).is_err() {
        heard.extend_from_slice(b"[reset]");
    }
    Ok(heard)
}

/// Asks the server to stop with `SIGTERM`, then waits for it to end, as long as it takes: it has to
/// end well, with nothing from its heap audit.
#[cfg(unix)]
fn ended(mut server: Running, workers: usize) -> Result<(), String> {
    let id = server.0.id().to_string();
    let sent = Command::new("kill").args(["-TERM", &id]).status().map_err(|err| err.to_string())?;
    if !sent.success() {
        return Err(format!("couldn't send the server SIGTERM ({sent})"));
    }
    let mut said = String::new();
    if let Some(mut err) = server.0.stderr.take() {
        let _ = err.read_to_string(&mut said);
    }
    let status = server.0.wait().map_err(|err| err.to_string())?;
    let on = if workers == 1 { "1 worker".to_owned() } else { format!("{workers} workers") };
    let audit: Vec<&str> = said.lines().filter(|line| line.starts_with(crate::llvm::AUDIT)).collect();
    if !audit.is_empty() {
        return Err(format!("on {on}, the heap audit found:\n{}", audit.join("\n")));
    }
    if !status.success() {
        return Err(format!("on {on}, the server ended with {status} once asked to stop:\n{said}"));
    }
    Ok(())
}

/// Windows has no `SIGTERM` to ask the server to stop with, so the target can't run there.
#[cfg(not(unix))]
fn ended(_: Running, _: usize) -> Result<(), String> {
    Err("`http-audited` stops its server with SIGTERM, which this system hasn't".to_owned())
}

/// The target: the input whole and cut short, on one worker and on four, each server then stopped
/// and audited, and what each connection heard the same on both.
pub(crate) fn http_audited(input: &[u8]) -> Result<(), String> {
    let executable = built()?;
    let mut sent = vec![input];
    sent.extend(split_points(input.len(), EVERY_BYTE_UP_TO).into_iter().filter_map(|at| input.get(..at)));
    let mut heard = Vec::new();
    for workers in WORKERS {
        let (server, port) = started(&executable, workers)?;
        let answers = sent.iter().map(|bytes| asked(port, bytes)).collect::<Result<Vec<_>, _>>()?;
        ended(server, workers)?;
        heard.push(answers);
    }
    let [one, four] = heard.as_slice() else { return Ok(()) };
    for ((bytes, alone), together) in sent.iter().zip(one).zip(four) {
        if alone != together {
            return Err(format!(
                "{} of the input's {} bytes heard one thing on 1 worker and another on 4:\n{}\n{}",
                bytes.len(),
                input.len(),
                String::from_utf8_lossy(alone),
                String::from_utf8_lossy(together)
            ));
        }
    }
    Ok(())
}
