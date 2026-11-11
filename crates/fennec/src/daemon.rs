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

    /// How long a request has to arrive whole, once its connection is made. The commands that
    /// send them write the whole of it at once.
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

    pub(super) fn send(home: &Path, request: &[&str]) -> Option<Outcome> {
        let mut stream = UnixStream::connect(socket_path(home, false)?).ok()?;
        let message = format!("{}\n{}\n", header(), request.join("\n"));
        stream.write_all(message.as_bytes()).ok()?;
        stream.shutdown(std::net::Shutdown::Write).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        Outcome::decode(&reply)
    }

    /// Opens a language server session: after `lsp`, the connection carries its messages.
    pub(super) fn open_session(home: &Path) -> Option<UnixStream> {
        let mut stream = UnixStream::connect(socket_path(home, false)?).ok()?;
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
            if UnixStream::connect(&socket).is_ok() {
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
                    std::thread::spawn(move || answer(stream, &state));
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
                    if ticks % 50 == 0 && replaced() {
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

    fn answer(stream: UnixStream, state: &Mutex<State>) {
        let Ok(mut writer) = stream.set_nonblocking(false).and_then(|()| stream.try_clone()) else { return };
        // A client that connects and says nothing, or says without end, can't hold a thread: a
        // request has a while to arrive, and a size it can't pass.
        if stream.set_read_timeout(Some(REQUEST_WAIT)).is_err() {
            return;
        }
        let mut reader = BufReader::new(stream);
        let (mut header, mut kind) = (String::new(), String::new());
        let mut lines = (&mut reader).take(LONGEST_LINE);
        if lines.read_line(&mut header).is_err() || lines.read_line(&mut kind).is_err() {
            return;
        }
        if header.trim_end() == self::header() && kind.trim_end() == "lsp" {
            // An editor's session is quiet for as long as nobody types.
            if reader.get_ref().set_read_timeout(None).is_err() {
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
    fn language_server(reader: &mut BufReader<UnixStream>, writer: &mut UnixStream, state: &Mutex<State>) {
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
        use std::path::Path;
        use std::time::Instant;

        use super::{BUILD, State, VERSION, handle, socket_in, socket_path};

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
            let home = fennec_temp::Temp::folder("private-daemon").unwrap();
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
