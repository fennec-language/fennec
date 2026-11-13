//! The build daemon: one per user and Fennec version, started by the first command that needs
//! it, keeping the build engine's state in memory between commands. It's never needed for
//! correctness: when it can't be reached or started, a command does the work itself.
//!
//! A command connects to `~/.fennec/daemon/<version>-<build>.sock` (see `unix::socket_path`), sends
//! `fennec <version> <build>` and its request one line each, and reads back its exit code and what to
//! print. The build is the hash of all of Fennec's source (`FENNEC_BUILD`), so a command never talks
//! to a daemon built from other source, even of the same version, which would answer with what an
//! older compiler does.
//!
//! The daemon owns the cache in `~/.fennec/cache`: it keeps it under its cap (1 GiB, or
//! `FENNEC_CACHE_CAP` bytes) when it starts and every five minutes after, while it's idle.
//!
//! `fennec lsp` sends `lsp` and then keeps the connection open: the daemon serves the
//! language server's session over it until the editor is done.
//!
//! `fennec daemon status` and `fennec daemon stop` manage it. It exits by itself after ten
//! minutes with nothing to do; `FENNEC_DAEMON_IDLE` sets that in seconds. `FENNEC_NO_DAEMON`
//! makes commands do their work in-process.

use std::path::Path;
use std::process::ExitCode;

#[cfg(unix)]
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The hash of all of this build's source, which the daemon must share with a command it answers.
#[cfg(unix)]
const BUILD: &str = env!("FENNEC_BUILD");

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
    #[cfg(unix)]
    fn encode(&self) -> String {
        format!("{}\n{} {}\n{}{}", self.code, self.stdout.len(), self.stderr.len(), self.stdout, self.stderr)
    }

    #[cfg(unix)]
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

/// `fennec daemon status`, `stop` and `serve`.
pub(crate) fn command(action: &str) -> ExitCode {
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: couldn't find a home folder; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match action {
        "status" => {
            if let unix::Reply::Answered(outcome) = unix::send(&home, &["status"]) {
                outcome.print()
            } else {
                println!("not running");
                ExitCode::SUCCESS
            }
        }
        "stop" => {
            let stopped = matches!(unix::send(&home, &["stop"]), unix::Reply::Answered(_));
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
/// reached, or takes the request and gives no answer in time, so the caller does the work itself.
pub(crate) fn ask(home: &Path, request: &[&str]) -> Option<Outcome> {
    if std::env::var_os("FENNEC_NO_DAEMON").is_some() {
        return None;
    }
    unix::ask_within(home, request, unix::REPLY_WAIT)
}

/// A connection to the daemon serving a language server session, starting the daemon first
/// if it isn't running. None if it can't be reached, so the session runs in-process.
#[cfg(unix)]
pub(crate) fn language_server(home: &Path) -> Option<std::os::unix::net::UnixStream> {
    if std::env::var_os("FENNEC_NO_DAEMON").is_some() {
        return None;
    }
    unix::open_session(home).or_else(|| {
        unix::start(home)?;
        unix::open_session(home)
    })
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{ExitCode, Stdio};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use fennec_build::Cache;

    use super::{BUILD, Outcome, VERSION};
    use crate::changes::Snapshots;

    /// How long a socket's path may be: a little under the 104 bytes macOS takes, and the 108
    /// Linux does.
    const LONGEST_SOCKET: usize = 100;

    /// How long a request has to arrive whole, once its connection is made, however it's sent:
    /// a byte at a time, each in time, doesn't make it longer. The commands that send them write
    /// the whole of it at once.
    const REQUEST_WAIT: Duration = Duration::from_secs(10);

    /// The most bytes a request's first two lines may take, and the most the rest may.
    const LONGEST_LINE: u64 = 4_096;
    const LARGEST_REQUEST: u64 = 1 << 20;

    /// Where the daemon listens: in Fennec's folder, unless that path is too long for a socket, in
    /// which case the user's runtime folder, or a short folder of its own under `/tmp` named by a
    /// hash of Fennec's folder. None when none of those will do, and then commands do their work
    /// themselves. Only the daemon, which listens there, `makes` that folder.
    fn socket_path(home: &Path, makes: bool) -> Option<PathBuf> {
        socket_in(home, std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from), makes)
    }

    /// `socket_path`, with the user's runtime folder as given.
    fn socket_in(home: &Path, runtime: Option<PathBuf>, makes: bool) -> Option<PathBuf> {
        let name = format!("{VERSION}-{}.sock", BUILD.get(..16).unwrap_or(BUILD));
        // Wherever it is, its folder is its user's alone: connecting needs a way into the folder,
        // so nobody else can reach the daemon, which reads any file it's pointed at.
        let preferred = home.join("daemon").join(&name);
        if preferred.as_os_str().len() < LONGEST_SOCKET {
            owned_privately(&home.join("daemon"), home, makes)?;
            return Some(preferred);
        }
        let hash = fennec_blake3::hash(home.to_string_lossy().as_bytes()).to_hex();
        let hash = hash.get(..16).unwrap_or(&hash);
        // The user's own runtime folder, which only they can reach, as long as it is theirs alone.
        if let Some(runtime) = runtime.filter(|runtime| owned_privately(runtime, home, false).is_some()) {
            let socket = runtime.join(format!("fennec-{hash}-{name}"));
            if socket.as_os_str().len() < LONGEST_SOCKET {
                return Some(socket);
            }
        }
        let folder = Path::new("/tmp").join(format!("fennec-{hash}"));
        owned_privately(&folder, home, makes)?;
        Some(folder.join(name))
    }

    /// Whether `folder` will do for a socket: open to nobody but its owner, who owns Fennec's
    /// folder at `home` too. `/tmp` is shared, so a folder someone else made there first, or one
    /// others can reach, is never used. The daemon `makes` it if it isn't there; for a command
    /// that only connects, one that isn't there will do, as there's nothing in it to connect to.
    fn owned_privately(folder: &Path, home: &Path, makes: bool) -> Option<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        // What its group and everyone else may do with it, which has to be nothing.
        const OTHERS: u32 = 0o077;
        if makes {
            match std::fs::DirBuilder::new().mode(0o700).create(folder) {
                Err(err) if err.kind() != ErrorKind::AlreadyExists => return None,
                _ => {}
            }
        }
        // Not followed if it's a link, which could point anywhere.
        let found = match std::fs::symlink_metadata(folder) {
            Ok(found) => found,
            Err(err) if err.kind() == ErrorKind::NotFound && !makes => return Some(()),
            Err(_) => return None,
        };
        let owner = std::fs::metadata(home).ok()?.uid();
        if !found.is_dir() || found.uid() != owner {
            return None;
        }
        // One of its owner's own made before folders were private, the daemon makes private.
        if found.mode() & OTHERS != 0 && makes {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).ok()?;
            return Some(());
        }
        (found.mode() & OTHERS == 0).then_some(())
    }

    /// Connects to the daemon at `socket`, as long as the socket is the user's own: whoever owns
    /// Fennec's folder at `home`. A socket another user bound in the folder before it was made
    /// private is theirs, and they can't give it away, so its answers, and an editor's files sent
    /// to it, never reach them.
    fn connect(socket: &Path, home: &Path) -> Option<UnixStream> {
        use std::os::unix::fs::MetadataExt as _;
        let owner = std::fs::metadata(home).ok()?.uid();
        socket_owned_by(socket, owner).then(|| UnixStream::connect(socket).ok()).flatten()
    }

    /// Whether `socket` is a socket, not followed if it's a link, that `owner` bound.
    fn socket_owned_by(socket: &Path, owner: u32) -> bool {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
        std::fs::symlink_metadata(socket)
            .is_ok_and(|found| found.file_type().is_socket() && found.uid() == owner)
    }

    /// A connection read until a moment: each read waits only what's left, so a request sent a
    /// byte at a time still has `REQUEST_WAIT` in all. With no moment, a read waits as long as it
    /// takes, as an editor's session does.
    struct Within {
        stream: UnixStream,
        until: Option<Instant>,
    }

    impl Read for Within {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if let Some(until) = self.until {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(ErrorKind::TimedOut.into());
                }
                // macOS refuses a timeout, with "invalid argument", once the other end has closed the
                // connection, as the daemon does after its whole answer. A read then gives the end
                // at once, so it can't wait past the deadline either.
                match self.stream.set_read_timeout(Some(left)) {
                    Err(err) if err.kind() != ErrorKind::InvalidInput => return Err(err),
                    _ => {}
                }
            }
            self.stream.read(buf)
        }
    }

    /// The first line of every request: the version and build of the `fennec` asking.
    fn header() -> String {
        format!("fennec {VERSION} {BUILD}")
    }

    /// How often the daemon checks the cache against its cap.
    const CLEANUP_EVERY: Duration = Duration::from_secs(300);

    /// Whether it's time to check the cache: at first, then every `CLEANUP_EVERY`.
    pub(super) fn cleanup_due(since_last: Option<Duration>) -> bool {
        since_last.is_none_or(|elapsed| elapsed > CLEANUP_EVERY)
    }

    #[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal place")]
    fn megabytes(bytes: u64) -> String {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }

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
        /// How many language server sessions are open. The daemon isn't idle while one is.
        sessions: usize,
        stopping: bool,
    }

    /// How long a command waits for the daemon's answer, all told, once it has sent a request: far
    /// longer than any answer takes, and short enough that a daemon that's stuck never holds a
    /// command up for good. Past it, the command does the work itself.
    pub(super) const REPLY_WAIT: Duration = Duration::from_secs(30);

    /// What came of sending the daemon a request.
    pub(super) enum Reply {
        Answered(Outcome),
        /// No daemon could be reached, so one may be started.
        Unreached,
        /// One took the request and gave no answer in time, or a broken one. Another isn't
        /// started, as this one may yet answer, and the request isn't sent again.
        Unanswered,
    }

    pub(super) fn send(home: &Path, request: &[&str]) -> Reply {
        send_within(home, request, REPLY_WAIT)
    }

    /// `send`, waiting at most `wait` for the answer.
    fn send_within(home: &Path, request: &[&str], wait: Duration) -> Reply {
        let Some(mut stream) = socket_path(home, false).and_then(|socket| connect(&socket, home)) else {
            return Reply::Unreached;
        };
        let message = format!("{}\n{}\n", header(), request.join("\n"));
        if stream.write_all(message.as_bytes()).is_err()
            || stream.shutdown(std::net::Shutdown::Write).is_err()
        {
            return Reply::Unanswered;
        }
        let mut reply = String::new();
        let mut within = Within { stream, until: Some(Instant::now() + wait) };
        match within.read_to_string(&mut reply).ok().and_then(|_| Outcome::decode(&reply)) {
            Some(outcome) => Reply::Answered(outcome),
            None => Reply::Unanswered,
        }
    }

    /// `ask`, waiting at most `wait` for each answer: a daemon is started only when none could be
    /// reached, and one that took the request and didn't answer leaves the work to the command.
    pub(super) fn ask_within(home: &Path, request: &[&str], wait: Duration) -> Option<Outcome> {
        match send_within(home, request, wait) {
            Reply::Answered(outcome) => Some(outcome),
            Reply::Unanswered => None,
            Reply::Unreached => {
                start(home)?;
                match send_within(home, request, wait) {
                    Reply::Answered(outcome) => Some(outcome),
                    Reply::Unreached | Reply::Unanswered => None,
                }
            }
        }
    }

    /// Opens a language server session: after `lsp`, the connection carries its messages.
    pub(super) fn open_session(home: &Path) -> Option<UnixStream> {
        let mut stream = connect(&socket_path(home, false)?, home)?;
        stream.write_all(format!("{}\nlsp\n", header()).as_bytes()).ok()?;
        Some(stream)
    }

    /// Starts the daemon in the background and waits until it answers, for up to 2 seconds.
    pub(super) fn start(home: &Path) -> Option<()> {
        let socket = socket_path(home, false)?;
        let exe = std::env::current_exe().ok()?;
        // The daemon outlives the command that starts it, so it's never waited for.
        let _daemon = std::process::Command::new(exe)
            .args(["daemon", "serve"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        for _ in 0..200 {
            if connect(&socket, home).is_some() {
                return Some(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Serves requests until stopped or idle for too long.
    pub(super) fn serve(home: &Path) -> ExitCode {
        // Its owner is whom the socket's folder has to belong to.
        let _ = std::fs::create_dir_all(home);
        let Some(socket) = socket_path(home, true) else { return ExitCode::FAILURE };
        // A daemon of its own already listening. A socket that isn't the user's is removed below,
        // from the folder that is, and this daemon listens in its place.
        if connect(&socket, home).is_some() {
            return ExitCode::SUCCESS;
        }
        let listener = socket
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| match std::fs::remove_file(&socket) {
                Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
                _ => UnixListener::bind(&socket),
            })
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
            // Its owner's alone too, where the system looks at a socket's own permissions.
            .and_then(|listener| {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).map(|()| listener)
            });
        let Ok(listener) = listener else { return ExitCode::FAILURE };
        // Which socket is this daemon's: another daemon started at the same moment binds the
        // same path after it, and then this one can't be reached, so it goes.
        let bound =
            std::fs::symlink_metadata(&socket).map(|found| std::os::unix::fs::MetadataExt::ino(&found)).ok();
        let replaced = || {
            std::fs::symlink_metadata(&socket).map(|found| std::os::unix::fs::MetadataExt::ino(&found)).ok()
                != bound
        };
        let mut ticks: u32 = 0;
        let state = Arc::new(Mutex::new(State {
            home: home.to_path_buf(),
            snapshots: HashMap::new(),
            served: 0,
            started: Instant::now(),
            last_work: Instant::now(),
            sessions: 0,
            stopping: false,
        }));
        let idle = idle_limit();
        let cache = Cache::new(home);
        let mut last_cleanup: Option<Instant> = None;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || answer(stream, &state, REQUEST_WAIT));
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    let done = {
                        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                        state.stopping || (state.sessions == 0 && state.last_work.elapsed() > idle)
                    };
                    if done {
                        break;
                    }
                    // Once a second: whether another daemon took the path, leaving this one
                    // unreachable. It leaves the other's socket where it is.
                    ticks = ticks.wrapping_add(1);
                    if ticks.is_multiple_of(50) && replaced() {
                        return ExitCode::SUCCESS;
                    }
                    // The daemon owns the cache, so it keeps it under its cap, in time when
                    // nothing else is going on.
                    if cleanup_due(last_cleanup.map(|last| last.elapsed())) {
                        fennec_build::kept("clean up", cache.cleanup(Cache::cap()));
                        fennec_build::sweep_work(home, fennec_build::WORK_KEPT);
                        last_cleanup = Some(Instant::now());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        let _ = std::fs::remove_file(&socket);
        // The short folder under `/tmp`, once no daemon of another build is listening in it.
        if let Some(folder) = socket.parent().filter(|folder| folder.parent() == Some(Path::new("/tmp"))) {
            let _ = std::fs::remove_dir(folder);
        }
        ExitCode::SUCCESS
    }

    /// Answers one connection, whose request has `wait` to arrive whole.
    fn answer(stream: UnixStream, state: &Mutex<State>, wait: Duration) {
        let Ok(mut writer) = stream.set_nonblocking(false).and_then(|()| stream.try_clone()) else { return };
        // A client that connects and says nothing, says without end, or says a byte at a time
        // can't hold a thread: a request has a while to arrive, and a size it can't pass.
        let mut reader = BufReader::new(Within { stream, until: Some(Instant::now() + wait) });
        let (mut header, mut kind) = (String::new(), String::new());
        let mut lines = (&mut reader).take(LONGEST_LINE);
        if lines.read_line(&mut header).is_err() || lines.read_line(&mut kind).is_err() {
            return;
        }
        if header.trim_end() == self::header() && kind.trim_end() == "lsp" {
            // An editor's session is quiet for as long as nobody types.
            reader.get_mut().until = None;
            if reader.get_ref().stream.set_read_timeout(None).is_err() {
                return;
            }
            language_server(&mut reader, &mut writer, state);
            return;
        }
        let mut rest = String::new();
        let read = (&mut reader).take(LARGEST_REQUEST + 1).read_to_string(&mut rest);
        if read.is_err() || rest.len() as u64 > LARGEST_REQUEST {
            return;
        }
        let request = format!("{header}{kind}{rest}");
        let outcome = handle(&request, &mut state.lock().unwrap_or_else(PoisonError::into_inner));
        let _ = writer.write_all(outcome.encode().as_bytes());
    }

    /// Serves a language server session over a connection, on a thread with room for the
    /// checker to recurse, until the editor is done.
    fn language_server(reader: &mut BufReader<Within>, writer: &mut UnixStream, state: &Mutex<State>) {
        let lock = || state.lock().unwrap_or_else(PoisonError::into_inner);
        {
            let mut state = lock();
            state.sessions += 1;
            state.served += 1;
        }
        std::thread::scope(|scope| {
            let session = std::thread::Builder::new()
                .stack_size(crate::STACK_BYTES)
                .spawn_scoped(scope, || fennec_lsp::serve(reader, writer));
            let _ = session.map(std::thread::ScopedJoinHandle::join);
        });
        let mut state = lock();
        state.sessions = state.sessions.saturating_sub(1);
        state.last_work = Instant::now();
    }

    fn handle(request: &str, state: &mut State) -> Outcome {
        let mut lines = request.lines();
        if lines.next() != Some(&header()) {
            return Outcome::failure(format!(
                "error: this daemon is Fennec {VERSION} built from other source, {}\n",
                BUILD.get(..16).unwrap_or(BUILD)
            ));
        }
        match (lines.next(), lines.next()) {
            (Some("status"), _) => Outcome {
                stdout: format!(
                    "running: Fennec {VERSION}, process {}, up {} s, {} served\ncache: {} of {}\n",
                    std::process::id(),
                    state.started.elapsed().as_secs(),
                    if state.served == 1 {
                        "1 request".to_owned()
                    } else {
                        format!("{} requests", state.served)
                    },
                    megabytes(Cache::new(&state.home).size()),
                    megabytes(Cache::cap()),
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

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;
        use std::io::{Read as _, Write as _};
        use std::os::unix::net::UnixStream;
        use std::path::Path;
        use std::time::{Duration, Instant};

        use super::{
            BUILD, Reply, State, VERSION, answer, ask_within, handle, send_within, socket_in,
            socket_owned_by, socket_path,
        };

        fn state() -> State {
            State {
                home: Path::new("/home/fennec/.fennec").to_path_buf(),
                snapshots: HashMap::new(),
                served: 0,
                started: Instant::now(),
                last_work: Instant::now(),
                sessions: 0,
                stopping: false,
            }
        }

        /// What a daemon answers a request sent as `send` does, and how long it took: one that
        /// doesn't arrive whole in `wait`, or passes 1 MiB, gets no answer.
        fn answered(
            send: impl FnOnce(&mut UnixStream) + Send + 'static,
            wait: Duration,
        ) -> (String, Duration) {
            let (mut client, server) = UnixStream::pair().unwrap();
            let state = std::sync::Mutex::new(state());
            let started = Instant::now();
            let sender = std::thread::spawn(move || {
                send(&mut client);
                let mut reply = String::new();
                let _ = client.read_to_string(&mut reply);
                reply
            });
            answer(server, &state, wait);
            let took = started.elapsed();
            (sender.join().unwrap(), took)
        }

        /// A request has its wait in all: one sent a byte at a time, each in time, is dropped once
        /// it's spent, as is one that stops halfway. One past 1 MiB is dropped too. One that
        /// arrives whole, in time, is answered.
        #[test]
        fn a_request_slow_or_too_big_holds_no_thread() {
            let whole = format!("fennec {VERSION} {BUILD}\nstatus\n");
            let asked = whole.clone();
            let (reply, _) = answered(
                move |client| {
                    client.write_all(asked.as_bytes()).unwrap();
                    client.shutdown(std::net::Shutdown::Write).unwrap();
                },
                Duration::from_secs(10),
            );
            assert!(reply.contains("running: Fennec"), "{reply}");
            let asked = whole.clone();
            let (reply, took) = answered(
                move |client| {
                    for byte in asked.bytes().chain(std::iter::repeat(b' ')) {
                        if client.write_all(&[byte]).is_err() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                },
                Duration::from_millis(300),
            );
            assert_eq!(reply, "", "a request dripped past its wait was answered");
            assert!(took < Duration::from_secs(5), "a dripped request held its thread for {took:?}");
            let (reply, took) = answered(
                move |client| {
                    let _ = client.write_all(b"fennec ");
                    // Never more, and never closed, until the daemon gives up.
                    let mut rest = Vec::new();
                    let _ = client.read_to_end(&mut rest);
                },
                Duration::from_millis(300),
            );
            assert_eq!(reply, "");
            assert!(took < Duration::from_secs(5), "a request left halfway held its thread for {took:?}");
            let (reply, _) = answered(
                move |client| {
                    let mut asked = whole.into_bytes();
                    asked.resize(asked.len() + (1 << 20) + 1, b'x');
                    let _ = client.write_all(&asked);
                    let _ = client.shutdown(std::net::Shutdown::Write);
                },
                Duration::from_secs(10),
            );
            assert_eq!(reply, "", "a request past 1 MiB was answered");
        }

        /// A daemon that takes a request and never answers holds a command up only so long: past
        /// its wait, the command does the work itself, without starting another daemon or asking
        /// again. One that answers is heard.
        #[test]
        fn a_daemon_that_never_answers_holds_a_command_up_only_so_long() {
            // Short, so the socket fits in Fennec's own folder rather than the one under `/tmp`.
            let home = fennec_temp::Temp::at(Path::new("/tmp").join(format!("fpq-{}", std::process::id())));
            std::fs::create_dir_all(&home).unwrap();
            let socket = socket_in(&home, None, true).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            // Takes each request, reads it, and keeps the connection open without a word.
            let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let holding = std::sync::Arc::clone(&held);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut stream = stream;
                    let mut asked = Vec::new();
                    let _ = stream.read_to_end(&mut asked);
                    if let Ok(mut held) = holding.lock() {
                        held.push(stream);
                    }
                }
            });
            let started = Instant::now();
            assert!(matches!(send_within(&home, &["status"], Duration::from_millis(300)), Reply::Unanswered));
            assert!(ask_within(&home, &["status"], Duration::from_millis(300)).is_none());
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "a daemon that never answers held a command for {:?}",
                started.elapsed()
            );
            // Asked twice, once each: not sent again, and no other daemon started over it.
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(held.lock().map(|held| held.len()).unwrap_or_default(), 2);
            assert!(socket_owned_by(
                &socket,
                std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&home).unwrap())
            ));
        }

        /// A daemon that answers and closes the connection, as the daemon does, is heard: on macOS
        /// a socket whose other end has closed refuses a read timeout, which once lost every
        /// answer, and the command did the work again itself.
        #[test]
        fn an_answer_and_then_a_closed_connection_is_heard() {
            let home = fennec_temp::Temp::at(Path::new("/tmp").join(format!("fpa-{}", std::process::id())));
            std::fs::create_dir_all(&home).unwrap();
            let socket = socket_in(&home, None, true).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut stream = stream;
                    let mut asked = Vec::new();
                    let _ = stream.read_to_end(&mut asked);
                    let answer = super::Outcome { stdout: "heard\n".to_owned(), ..super::Outcome::default() };
                    let _ = stream.write_all(answer.encode().as_bytes());
                    // Closed, as the daemon closes it once its whole answer is written.
                }
            });
            match send_within(&home, &["status"], Duration::from_secs(10)) {
                Reply::Answered(outcome) => assert_eq!(outcome.stdout, "heard\n"),
                Reply::Unreached | Reply::Unanswered => {
                    panic!("an answer the daemon wrote whole wasn't heard")
                }
            }
        }

        /// A command connects only to a socket the user's own: one another user bound, where the
        /// folder once let them, isn't connected to. Nor is a file that isn't a socket.
        #[test]
        fn only_a_socket_of_the_users_own_is_connected_to() {
            use std::os::unix::fs::MetadataExt as _;
            let folder = fennec_temp::Temp::at(Path::new("/tmp").join(format!("fpo-{}", std::process::id())));
            std::fs::create_dir_all(&folder).unwrap();
            let socket = folder.join("d.sock");
            let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let me = std::fs::metadata(&folder).unwrap().uid();
            assert!(socket_owned_by(&socket, me));
            assert!(
                !socket_owned_by(&socket, me + 1),
                "a socket bound by someone else was taken as the user's"
            );
            assert!(super::connect(&socket, &folder).is_some());
            let plain = folder.join("plain.sock");
            std::fs::write(&plain, "").unwrap();
            assert!(!socket_owned_by(&plain, me));
            assert!(super::connect(&plain, &folder).is_none());
        }

        /// A daemon answers only a command of its own build: one of the same version built from
        /// other source would get what an older compiler does.
        #[test]
        fn a_daemon_answers_only_its_own_build() {
            let home = Path::new("/home/fennec/.fennec");
            let socket = socket_path(home, false);
            let named = format!("{VERSION}-{}.sock", BUILD.get(..16).unwrap_or(BUILD));
            assert_eq!(socket, Some(home.join("daemon").join(named)));
            let mut state = State {
                home: home.to_path_buf(),
                snapshots: HashMap::new(),
                served: 0,
                started: Instant::now(),
                last_work: Instant::now(),
                sessions: 0,
                stopping: false,
            };
            let own = handle(&format!("fennec {VERSION} {BUILD}\nstatus\n"), &mut state);
            assert!(own.stdout.starts_with("running: Fennec"), "{own:?}");
            for other in [format!("fennec {VERSION}"), format!("fennec {VERSION} {}", "0".repeat(64))] {
                let refused = handle(&format!("{other}\nstatus\n"), &mut state);
                assert_eq!(refused.code, 1, "{other}");
                assert!(refused.stderr.contains("built from other source"), "{refused:?}");
            }
        }

        /// Where Fennec's folder is too deep for a socket, the daemon listens in a short folder of
        /// its own under `/tmp`, open to nobody else, and a folder there that others can reach is
        /// never used.
        #[test]
        fn a_deep_fennec_folder_listens_in_a_short_private_folder() {
            use std::os::unix::fs::PermissionsExt;
            let folder = fennec_temp::Temp::folder("deep-daemon").unwrap();
            let home = folder.join("a folder with a long name, ".repeat(4));
            std::fs::create_dir_all(&home).unwrap();
            let hash = fennec_blake3::hash(home.to_string_lossy().as_bytes()).to_hex();
            let short =
                fennec_temp::Temp::at(Path::new("/tmp").join(format!("fennec-{}", hash.get(..16).unwrap())));
            assert_eq!(socket_in(&home, None, false).as_deref().map(Path::parent), Some(Some(short.path())));
            assert!(!short.exists(), "only the daemon makes it");
            let socket = socket_in(&home, None, true).unwrap();
            assert!(socket.as_os_str().len() < 100, "{}", socket.display());
            let made = std::fs::symlink_metadata(&short).unwrap();
            assert_eq!(made.permissions().mode() & 0o777, 0o700);
            std::fs::set_permissions(&short, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                socket_in(&home, None, false),
                None,
                "a command connected through one others can reach"
            );
            // The daemon makes its own folder private again.
            assert_eq!(socket_in(&home, None, true), Some(socket.clone()));
            assert_eq!(std::fs::symlink_metadata(&short).unwrap().permissions().mode() & 0o777, 0o700);
            // The runtime folder, when there is one, comes before `/tmp`, and nothing's made in it.
            let runtime = Path::new("/run/user/1000");
            let in_runtime = socket_in(&home, Some(runtime.to_path_buf()), true).unwrap();
            assert_eq!(in_runtime.parent(), Some(runtime));
        }

        /// The socket's usual folder, in Fennec's own, is its user's alone too, whatever the umask
        /// made it: connecting needs a way into it, so nobody else reaches the daemon, which reads
        /// any file it's pointed at. A command doesn't connect through one others can reach, and
        /// the daemon makes it private again.
        #[test]
        fn the_sockets_folder_is_its_users_alone() {
            use std::os::unix::fs::PermissionsExt;
            // Short, so the socket fits in Fennec's own folder rather than the one under `/tmp`.
            let home = fennec_temp::Temp::at(Path::new("/tmp").join(format!("fpd-{}", std::process::id())));
            std::fs::create_dir_all(&home).unwrap();
            let socket = socket_in(&home, None, true).unwrap();
            assert_eq!(socket.parent(), Some(home.join("daemon").as_path()));
            let mode =
                || std::fs::symlink_metadata(home.join("daemon")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(), 0o700);
            std::fs::set_permissions(home.join("daemon"), std::fs::Permissions::from_mode(0o777)).unwrap();
            assert_eq!(socket_in(&home, None, false), None);
            assert_eq!(socket_in(&home, None, true), Some(socket));
            assert_eq!(mode(), 0o700);
        }
    }
}

/// Where there are no Unix sockets, there's no daemon yet: commands do their work in-process.
#[cfg(not(unix))]
mod unix {
    use std::path::Path;
    use std::process::ExitCode;

    use super::Outcome;

    pub(super) const REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

    /// What came of sending a request, which here is always that there's no daemon.
    pub(super) enum Reply {
        #[expect(dead_code, reason = "no daemon runs here to answer, but `fennec daemon` names it")]
        Answered(Outcome),
        Unreached,
    }

    pub(super) fn send(_: &Path, _: &[&str]) -> Reply {
        Reply::Unreached
    }

    pub(super) fn ask_within(_: &Path, _: &[&str], _: std::time::Duration) -> Option<Outcome> {
        None
    }

    pub(super) fn serve(_: &Path) -> ExitCode {
        eprintln!("error: the build daemon isn't available on this platform yet");
        ExitCode::FAILURE
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    #[test]
    fn the_cache_is_checked_at_first_and_then_every_five_minutes() {
        assert!(super::unix::cleanup_due(None));
        assert!(!super::unix::cleanup_due(Some(Duration::from_secs(300))));
        assert!(super::unix::cleanup_due(Some(Duration::from_secs(301))));
    }
}
