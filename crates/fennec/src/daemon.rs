//! The build daemon: one per user and Fennec version, started by the first command that needs
//! it, keeping the build engine's state in memory between commands. It's never needed for
//! correctness: when it can't be reached or started, a command does the work itself.
//!
//! A command connects to `~/.fennec/daemon/<version>.sock` (see `socket_path`), sends `fennec <version>` and its
//! request one line each, and reads back its exit code and what to print. The socket is named
//! by version, so a command never talks to a daemon from another version.
//!
//! `fennec daemon status` and `fennec daemon stop` manage it. It exits by itself after ten
//! minutes with nothing to do; `FENNEC_DAEMON_IDLE` sets that in seconds. `FENNEC_NO_DAEMON`
//! makes commands do their work in-process.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a command prints, and how it exits.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) code: u8,
}

impl Outcome {
    pub(crate) fn failure(stderr: String) -> Self {
        Outcome { stdout: String::new(), stderr, code: 1 }
    }

    pub(crate) fn print(self) -> ExitCode {
        print!("{}", self.stdout);
        eprint!("{}", self.stderr);
        ExitCode::from(self.code)
    }

    /// The outcome as the daemon sends it: the code, the two lengths, then the two texts.
    fn encode(&self) -> String {
        format!("{}\n{} {}\n{}{}", self.code, self.stdout.len(), self.stderr.len(), self.stdout, self.stderr)
    }

    fn decode(text: &str) -> Option<Outcome> {
        let (code, rest) = text.split_once('\n')?;
        let (lengths, rest) = rest.split_once('\n')?;
        let (stdout_len, stderr_len) = lengths.split_once(' ')?;
        let (stdout_len, stderr_len): (usize, usize) = (stdout_len.parse().ok()?, stderr_len.parse().ok()?);
        let stdout = rest.get(..stdout_len)?;
        let stderr = rest.get(stdout_len..stdout_len + stderr_len)?;
        Some(Outcome { stdout: stdout.to_owned(), stderr: stderr.to_owned(), code: code.parse().ok()? })
    }
}

/// Where the daemon listens: in Fennec's folder, unless that path is too long for a socket
/// (about 100 bytes on macOS), in which case the user's runtime folder or the temp folder,
/// named by a hash of Fennec's folder.
fn socket_path(home: &Path) -> PathBuf {
    let preferred = home.join("daemon").join(format!("{VERSION}.sock"));
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    let hash = fennec_blake3::hash(home.to_string_lossy().as_bytes()).to_hex();
    let folder = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    folder.join(format!("fennec-{}-{VERSION}.sock", hash.get(..16).unwrap_or(&hash)))
}

/// `fennec daemon status`, `stop` and `serve`.
pub(crate) fn command(action: &str) -> ExitCode {
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: couldn't find a home folder; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match action {
        "status" => {
            if let Some(outcome) = unix::send(&home, &["status"]) {
                outcome.print()
            } else {
                println!("not running");
                ExitCode::SUCCESS
            }
        }
        "stop" => {
            let stopped = unix::send(&home, &["stop"]).is_some();
            println!("{}", if stopped { "stopped" } else { "not running" });
            ExitCode::SUCCESS
        }
        "serve" => unix::serve(&home),
        _ => {
            eprintln!("error: `fennec daemon` takes `status` or `stop`");
            ExitCode::from(2)
        }
    }
}

/// Sends a request to the daemon, starting it first if it isn't running. None if it can't be
/// reached, so the caller does the work itself.
pub(crate) fn ask(home: &Path, request: &[&str]) -> Option<Outcome> {
    if std::env::var_os("FENNEC_NO_DAEMON").is_some() {
        return None;
    }
    unix::send(home, request).or_else(|| {
        unix::start(home)?;
        unix::send(home, request)
    })
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{ExitCode, Stdio};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use super::{Outcome, VERSION, socket_path};
    use crate::changes::Snapshots;

    /// How long the daemon waits with nothing to do before it exits.
    fn idle_limit() -> Duration {
        let seconds =
            std::env::var("FENNEC_DAEMON_IDLE").ok().and_then(|text| text.parse().ok()).unwrap_or(600);
        Duration::from_secs(seconds)
    }

    /// What the daemon keeps between requests.
    struct State {
        home: PathBuf,
        snapshots: Snapshots,
        served: usize,
        started: Instant,
        /// When it last did work. Asking for its status doesn't count, so it doesn't keep an
        /// idle daemon alive.
        last_work: Instant,
        stopping: bool,
    }

    pub(super) fn send(home: &Path, request: &[&str]) -> Option<Outcome> {
        let mut stream = UnixStream::connect(socket_path(home)).ok()?;
        let message = format!("fennec {VERSION}\n{}\n", request.join("\n"));
        stream.write_all(message.as_bytes()).ok()?;
        stream.shutdown(std::net::Shutdown::Write).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        Outcome::decode(&reply)
    }

    /// Starts the daemon in the background and waits until it answers, for up to 2 seconds.
    pub(super) fn start(home: &Path) -> Option<()> {
        let exe = std::env::current_exe().ok()?;
        // The daemon outlives the command that starts it, so it's never waited for.
        let _daemon = std::process::Command::new(exe)
            .args(["daemon", "serve"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let socket = socket_path(home);
        for _ in 0..200 {
            if UnixStream::connect(&socket).is_ok() {
                return Some(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Serves requests until stopped or idle for too long.
    pub(super) fn serve(home: &Path) -> ExitCode {
        let socket = socket_path(home);
        if UnixStream::connect(&socket).is_ok() {
            return ExitCode::SUCCESS;
        }
        let listener = socket
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| match std::fs::remove_file(&socket) {
                Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
                _ => UnixListener::bind(&socket),
            })
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener));
        let Ok(listener) = listener else { return ExitCode::FAILURE };
        let state = Arc::new(Mutex::new(State {
            home: home.to_path_buf(),
            snapshots: HashMap::new(),
            served: 0,
            started: Instant::now(),
            last_work: Instant::now(),
            stopping: false,
        }));
        let idle = idle_limit();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || answer(stream, &state));
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    let done = {
                        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                        state.stopping || state.last_work.elapsed() > idle
                    };
                    if done {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        let _ = std::fs::remove_file(&socket);
        ExitCode::SUCCESS
    }

    fn answer(mut stream: UnixStream, state: &Mutex<State>) {
        let mut request = String::new();
        if stream.set_nonblocking(false).is_err() || stream.read_to_string(&mut request).is_err() {
            return;
        }
        let outcome = handle(&request, &mut state.lock().unwrap_or_else(PoisonError::into_inner));
        let _ = stream.write_all(outcome.encode().as_bytes());
    }

    fn handle(request: &str, state: &mut State) -> Outcome {
        let mut lines = request.lines();
        if lines.next() != Some(&format!("fennec {VERSION}")) {
            return Outcome::failure(format!("error: this daemon is Fennec {VERSION}\n"));
        }
        match (lines.next(), lines.next()) {
            (Some("status"), _) => Outcome {
                stdout: format!(
                    "running: Fennec {VERSION}, process {}, up {} s, {} served\n",
                    std::process::id(),
                    state.started.elapsed().as_secs(),
                    if state.served == 1 {
                        "1 request".to_owned()
                    } else {
                        format!("{} requests", state.served)
                    }
                ),
                ..Outcome::default()
            },
            (Some("stop"), _) => {
                state.stopping = true;
                Outcome::default()
            }
            (Some("changes"), Some(path)) => {
                state.served += 1;
                state.last_work = Instant::now();
                crate::changes::report(Path::new(path), &state.home.clone(), &mut state.snapshots)
            }
            _ => Outcome::failure(format!("error: the daemon doesn't know the request `{request}`\n")),
        }
    }
}

/// Where there are no Unix sockets, there's no daemon yet: commands do their work in-process.
#[cfg(not(unix))]
mod unix {
    use std::path::Path;
    use std::process::ExitCode;

    use super::Outcome;

    pub(super) fn send(_: &Path, _: &[&str]) -> Option<Outcome> {
        None
    }

    pub(super) fn start(_: &Path) -> Option<()> {
        None
    }

    pub(super) fn serve(_: &Path) -> ExitCode {
        eprintln!("error: the build daemon isn't available on this platform yet");
        ExitCode::FAILURE
    }
}
