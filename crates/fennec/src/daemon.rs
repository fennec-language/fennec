//! The build daemon: one per user, Fennec build and build environment, started by the first command
//! that needs it, keeping the build engine's state in memory between commands. It's never needed
//! for correctness: when it can't be reached or started, a command does the work itself.
//!
//! A command connects to `~/.fennec/daemon/<version>-<build>-<environment>.sock` (see
//! `unix::socket_path`), sends
//! `fennec <version> <build>` and its request one line each, and reads back its exit code and what to
//! print. The build is the hash of all of Fennec's source (`FENNEC_BUILD`), so a command never talks
//! to a daemon built from other source, even of the same version, which would answer with what an
//! older compiler does.
//!
//! The daemon owns the cache in `~/.fennec/cache`: it keeps it under its cap (20 GiB, or a tenth of
//! the space free on its disk if that's less, or `FENNEC_CACHE_CAP` bytes) when it starts and
//! every five minutes after, while it's idle.
//!
//! `fennec lsp` sends `lsp` and then keeps the connection open: the daemon serves the
//! language server's session over it until the editor is done.
//!
//! `fennec build`, `fennec run` and `fennec test` send `build`, `run` or `test` with their folder
//! and arguments, and keep the connection open while the daemon works: one command's work at a
//! time, in the command's folder, with what it prints sent back. The environment is part of the
//! socket's name, so the work runs in the command's own, as `cc` and the linker see it. A command
//! that goes before the answer, as with Ctrl-C, stops the work. Under CI it's done in the
//! command's own process, as a first build is. Each `fennec build` keeps what it loaded for the
//! next (`aot::KEPT_LOAD`): each file's parse, and its rewriting of names across files, so an edit
//! build parses and rewrites only the files the edit reaches.
//!
//! For `fennec run`, the daemon does what comes before the program starts: the tests a change
//! affects, the check and the build, to a file the command names. The command starts the
//! program itself, so it has the command's input, output, signals and exit, as before.
//! `fennec test --real` runs in the command's own process, since its tests reach the terminal.
//!
//! `fennec daemon status` and `fennec daemon stop` manage every daemon of this build the user has
//! running, one for each environment commands ran in. Each exits by itself after thirty
//! minutes with nothing to do, or one minute if it has served one request; `FENNEC_DAEMON_IDLE`
//! sets the thirty in seconds. `FENNEC_NO_DAEMON` makes commands do their work in-process.
//!
//! So daemons don't pile up:
//! - A command never starts one for a Fennec folder in the temp folder. A fuzzer, a script or a
//!   test makes one there for a build or two, and its daemon would wait half an hour.
//! - A user runs at most four, of any build. Each notes its socket in a folder of the user's own
//!   in the temp folder, and one starting past four stops the one used longest ago.
//! - A test's daemons are kept apart in a folder of their own, `FENNEC_DAEMONS`, which also lets
//!   one start for a Fennec folder in the temp folder.

use std::path::Path;
use std::process::ExitCode;

#[cfg(unix)]
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The hash of all of this build's source, which the daemon must share with a command it answers.
#[cfg(unix)]
const BUILD: &str = env!("FENNEC_BUILD");

pub(crate) use fennec_daemon::Outcome;

/// This build, as a daemon and the commands it answers name it.
#[cfg(unix)]
const THIS: fennec_daemon::Build<'static> = fennec_daemon::Build { version: VERSION, build: BUILD };

/// `fennec daemon status`, `stop`, `serve` and `protocol`.
pub(crate) fn command(action: &str) -> ExitCode {
    // What a release's folder says of its `fennec`, so the launcher can ask its daemon itself
    // (`fennec_daemon::PROTOCOL_FILE`). It's written when a release is packed, with no Fennec
    // folder needed.
    #[cfg(unix)]
    if action == "protocol" {
        print!("{}", THIS.protocol_line());
        return ExitCode::SUCCESS;
    }
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: couldn't find a home folder; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match action {
        // Every daemon of this build the user has running is reached, one for each environment
        // commands ran in, so a terminal set up another way still sees and stops them all.
        "status" => {
            let answers = unix::ask_every(&home, &["status"]);
            if answers.is_empty() {
                println!("not running");
            }
            for answer in answers {
                print!("{}", answer.stdout);
            }
            ExitCode::SUCCESS
        }
        "stop" => {
            match unix::ask_every(&home, &["stop"]).len() {
                0 => println!("not running"),
                1 => println!("stopped"),
                stopped => println!("stopped {stopped} daemons"),
            }
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
    if crate::no_daemon() {
        return None;
    }
    unix::ask_within(home, request, unix::REPLY_WAIT)
}

/// A command whose work the daemon does for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Build,
    Run,
    Test,
}

impl Kind {
    /// How a request names it.
    #[cfg(unix)]
    fn word(self) -> &'static str {
        match self {
            Kind::Build => "build",
            Kind::Run => "run",
            Kind::Test => "test",
        }
    }

    #[cfg(unix)]
    fn of(word: &str) -> Option<Kind> {
        [Kind::Build, Kind::Run, Kind::Test].into_iter().find(|kind| kind.word() == word)
    }

    /// What a command that gets no answer does, as it says.
    #[cfg(unix)]
    fn done_here(self) -> &'static str {
        match self {
            Kind::Build | Kind::Run => "this was built here",
            Kind::Test => "the tests ran here",
        }
    }
}

/// What a `build` request says for a build with no output named.
const NO_OUTPUT: &str = "-";

/// A command's work, with its three arguments, done by the daemon, which starts first if it isn't
/// running: what it printed and how it ended. None means it's to be done here: under CI, where
/// what's measured is a command on its own, with `FENNEC_NO_DAEMON`, or when no daemon answers.
fn served(kind: Kind, arguments: [&str; 3]) -> Option<Outcome> {
    if crate::no_daemon() || std::env::var_os("CI").is_some() {
        return None;
    }
    #[cfg(unix)]
    {
        let home = fennec_build::fennec_home()?;
        unix::served(&home, kind, arguments)
    }
    #[cfg(not(unix))]
    {
        let _ = (kind, arguments);
        None
    }
}

/// `fennec build` made by the daemon: how it ended, once what it printed is printed here. None
/// means it's to be built here (`served`).
pub(crate) fn build(path: &str, output: Option<&str>, release: bool) -> Option<ExitCode> {
    let mode = if release { "release" } else { "dev" };
    served(Kind::Build, [path, output.unwrap_or(NO_OUTPUT), mode]).map(Outcome::print)
}

/// The program `fennec run` starts, made by the daemon as `run::prepared` makes it, at
/// `executable`, after the tests a change affects if `tested`. Ok once it's there to start; or
/// else how the command ends, once what was printed is printed here. None means it's to be made
/// here (`served`).
pub(crate) fn run(path: &str, executable: &Path, tested: bool) -> Option<Result<(), ExitCode>> {
    let tests = if tested { "tested" } else { "untested" };
    let outcome = served(Kind::Run, [path, executable.to_str()?, tests])?;
    // A run that stopped short made nothing to start.
    let made = outcome.code == 0 && executable.is_file();
    let code = outcome.print();
    Some(if made { Ok(()) } else { Err(code) })
}

/// `fennec test` run by the daemon: how it ended, once what it printed is printed here. None means
/// they're to run here: with `--real`, whose tests reach this terminal and its input, or as
/// `served` says.
pub(crate) fn test(path: &str, options: crate::test::Options) -> Option<ExitCode> {
    let [run, shown] = options.words()?;
    served(Kind::Test, [path, run, shown]).map(Outcome::print)
}

/// A connection to the daemon serving a language server session, starting the daemon first
/// if it isn't running. None if it can't be reached, so the session runs in-process.
#[cfg(unix)]
pub(crate) fn language_server(home: &Path) -> Option<std::os::unix::net::UnixStream> {
    if crate::no_daemon() {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use fennec_build::Cache;

    use fennec_daemon::{ONE_STREAM, TWO_STREAMS, Within, connect, one_stream, owned_privately};
    #[cfg(test)]
    use fennec_daemon::{environment, socket_owned_by};

    use super::{BUILD, Kind, NO_OUTPUT, Outcome, VERSION};
    use crate::changes::Snapshots;

    /// How long a request has to arrive whole, once its connection is made, however it's sent:
    /// a byte at a time, each in time, doesn't make it longer. The commands that send them write
    /// the whole of it at once.
    const REQUEST_WAIT: Duration = Duration::from_secs(10);

    /// The most bytes a request's first two lines may take, and the most the rest may.
    const LONGEST_LINE: u64 = 4_096;
    const LARGEST_REQUEST: u64 = 1 << 20;

    /// Where this build's daemon for this environment listens (`fennec_daemon::socket_path`).
    fn socket_path(home: &Path, makes: bool) -> Option<PathBuf> {
        fennec_daemon::socket_path(super::THIS, home, makes)
    }

    /// `socket_path`, with the user's runtime folder as given.
    #[cfg(test)]
    fn socket_in(home: &Path, runtime: Option<PathBuf>, makes: bool) -> Option<PathBuf> {
        fennec_daemon::socket_in(super::THIS, home, runtime, makes)
    }

    /// The sockets of this build's daemons, one for each environment commands ran in.
    fn sockets(home: &Path) -> Vec<PathBuf> {
        fennec_daemon::sockets(super::THIS, home)
    }

    /// The first line of every request: the version and build of the `fennec` asking.
    fn header() -> String {
        super::THIS.header()
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
            std::env::var("FENNEC_DAEMON_IDLE").ok().and_then(|text| text.parse().ok()).unwrap_or(1_800);
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
        /// Where it's noted among the user's daemons (`note_daemon`).
        noted: Option<PathBuf>,
    }

    impl State {
        /// Notes that it worked just now: for its idle exit, and for which of the user's daemons
        /// was used longest ago, which goes first past the cap (`keep_under_cap`).
        fn worked(&mut self) {
            self.last_work = Instant::now();
            if let Some(noted) = &self.noted {
                let file = std::fs::File::options().write(true).open(noted);
                let _ = file.and_then(|file| file.set_modified(std::time::SystemTime::now()));
            }
        }
    }

    /// How long a daemon that has served one request, or none, waits with nothing to do before it
    /// exits: one started for a single build, as a script's is, goes soon.
    const ONE_REQUEST_IDLE: Duration = Duration::from_mins(1);

    /// How long a daemon that has served `served` requests waits with nothing to do before it exits,
    /// when `idle` is how long it waits once it's been used again.
    fn idle_after(served: usize, idle: Duration) -> Duration {
        if served <= 1 { idle.min(ONE_REQUEST_IDLE) } else { idle }
    }

    /// How many build daemons a user keeps running at once. Past it, starting one stops the one
    /// used longest ago, which only means that one's next command starts it again, or builds on
    /// its own.
    const MOST_DAEMONS: usize = 4;

    /// Where each of the user's daemons notes the socket it listens at: a folder of the user's own
    /// in the temp folder, or the one `FENNEC_DAEMONS` names, as tests keep their daemons apart in.
    fn noted_daemons() -> PathBuf {
        std::env::var_os("FENNEC_DAEMONS").map_or_else(
            || std::env::temp_dir().join(format!("fennec-daemons-{}", rustix::process::getuid().as_raw())),
            PathBuf::from,
        )
    }

    /// Notes this daemon, listening at `socket` for Fennec's folder `home`, among the user's, and
    /// stops those used longest ago past `MOST_DAEMONS`. Gives back the note, which goes when the
    /// daemon does; none when the folder can't be the user's alone, and then nothing is counted.
    fn note_daemon(home: &Path, socket: &Path) -> Option<PathBuf> {
        let folder = noted_daemons();
        owned_privately(&folder, home, true)?;
        let noted = folder.join(format!("{}.daemon", std::process::id()));
        std::fs::write(&noted, format!("{}\n", socket.display())).ok()?;
        keep_under_cap(&folder, &noted, home);
        Some(noted)
    }

    /// Stops the daemons noted in `folder` but `own`, those used longest ago first, until no more
    /// than `MOST_DAEMONS` are running, `own`'s included. A note whose daemon can't be reached, as
    /// one killed leaves, goes.
    fn keep_under_cap(folder: &Path, own: &Path, home: &Path) {
        let Ok(listing) = std::fs::read_dir(folder) else { return };
        let mut others = Vec::new();
        for entry in listing.flatten() {
            let path = entry.path();
            if path == own || path.extension().is_none_or(|extension| extension != "daemon") {
                continue;
            }
            let socket =
                std::fs::read_to_string(&path).ok().and_then(|text| text.lines().next().map(PathBuf::from));
            match socket.filter(|socket| connect(socket, home).is_some()) {
                Some(socket) => {
                    let used = entry.metadata().and_then(|metadata| metadata.modified()).ok();
                    others.push((used, socket));
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        others.sort_by_key(|(used, _)| *used);
        let over = (others.len() + 1).saturating_sub(MOST_DAEMONS);
        for (_, socket) in others.into_iter().take(over) {
            // Any build of Fennec's daemon takes `stop` (`handle`).
            let _ = send_at(&socket, home, &["stop"], Duration::from_secs(5));
        }
    }

    /// Whether a command may start a daemon for Fennec's folder `home`. Not for one in the temp
    /// folder, as a fuzzer's, a script's or a test's is: each is made for a build or two, and
    /// would leave a daemon of its own waiting half an hour. `FENNEC_DAEMONS`, which keeps a test's
    /// daemons apart, lets one start anywhere.
    pub(super) fn may_start(home: &Path) -> bool {
        if std::env::var_os("FENNEC_DAEMONS").is_some() {
            return true;
        }
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let temps = [std::env::temp_dir(), PathBuf::from("/tmp")];
        !temps.iter().any(|temp| home.starts_with(temp) || canonical(home).starts_with(canonical(temp)))
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

    /// Sends a request to every daemon of this build the user has running, and gives back the
    /// answers of those that answered.
    pub(super) fn ask_every(home: &Path, request: &[&str]) -> Vec<Outcome> {
        sockets(home)
            .iter()
            .filter_map(|socket| match send_at(socket, home, request, REPLY_WAIT) {
                Reply::Answered(outcome) => Some(outcome),
                Reply::Unreached | Reply::Unanswered => None,
            })
            .collect()
    }

    /// Sends a request to the daemon for this command's environment, waiting at most `wait` for
    /// the answer.
    fn send_within(home: &Path, request: &[&str], wait: Duration) -> Reply {
        match socket_path(home, false) {
            Some(socket) => send_at(&socket, home, request, wait),
            None => Reply::Unreached,
        }
    }

    /// `send_within`, to the daemon listening at `socket`.
    fn send_at(socket: &Path, home: &Path, request: &[&str], wait: Duration) -> Reply {
        let Some(mut stream) = connect(socket, home) else {
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

    /// A command's work, asked of the daemon, started first if none is running: what it printed
    /// and how it ended. None when it's to be done here: a request that can't be sent as lines, no
    /// daemon, or one that went without a whole answer, which says so in a line.
    pub(super) fn served(home: &Path, kind: Kind, arguments: [&str; 3]) -> Option<Outcome> {
        let folder = std::env::current_dir().ok()?;
        let folder = folder.to_str()?;
        let request = fennec_daemon::work_request(super::THIS, kind.word(), folder, arguments, one_stream())?;
        let socket = socket_path(home, false)?;
        let stream = if let Some(stream) = connect(&socket, home) {
            stream
        } else {
            start(home)?;
            connect(&socket, home)?
        };
        let outcome = fennec_daemon::answer(stream, &request);
        if outcome.is_none() {
            eprintln!("note: the build daemon gave no answer, so {}", kind.done_here());
        }
        outcome
    }

    /// Starts the daemon in the background and waits until it answers, for up to 2 seconds.
    pub(super) fn start(home: &Path) -> Option<()> {
        if !may_start(home) {
            return None;
        }
        let socket = socket_path(home, false)?;
        let exe = std::env::current_exe().ok()?;
        // The daemon outlives the command that starts it, so it's never waited for. It gives what a
        // build let go of back to the system at once: mimalloc otherwise does so only as later
        // allocations come, which an idle daemon never makes, and a 200k-line build left 2.2 GB held
        // through the idle half hour. That costs a build about 0.8% more instructions.
        let _daemon = std::process::Command::new(exe)
            .args(["daemon", "serve"])
            .env("MIMALLOC_PURGE_DELAY", "0")
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
        // Its builds go to processes of their own, whose memory goes when they end.
        crate::child::serving(home);
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
        // The daemon keeps each kit's check, for its build after an edit to carry over from.
        crate::kept::keep_checks();
        // Noted among the user's daemons, past whose cap the one used longest ago stops.
        let noted = note_daemon(home, &socket);
        let gone = |noted: &Option<PathBuf>| {
            if let Some(noted) = noted {
                let _ = std::fs::remove_file(noted);
            }
        };
        // Which socket is this daemon's: another daemon started at the same moment binds the
        // same path after it, and then this one can't be reached, so it goes.
        let bound =
            std::fs::symlink_metadata(&socket).map(|found| std::os::unix::fs::MetadataExt::ino(&found)).ok();
        let replaced = || {
            std::fs::symlink_metadata(&socket).map(|found| std::os::unix::fs::MetadataExt::ino(&found)).ok()
                != bound
        };
        // Each build keeps what it loaded for the next.
        *crate::aot::KEPT_LOAD.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(fennec_kit::KeptLoad::default());
        // And each function's analysis: here, or in each build's own process, which hands what it
        // kept back for the next (see `crate::child`). A build made here when a child gives no
        // answer then works out its own.
        if !crate::child::wanted() {
            fennec_llvm::keep_analyses_between_builds();
        }
        let state = Arc::new(Mutex::new(State {
            home: home.to_path_buf(),
            snapshots: HashMap::new(),
            served: 0,
            started: Instant::now(),
            last_work: Instant::now(),
            sessions: 0,
            stopping: false,
            noted: noted.clone(),
        }));
        let ended = listen(&listener, home, &state, &replaced);
        // What builds left to write is written before it goes, and what they used again of the
        // analyses kept is checked.
        crate::aot::later::finish(Duration::from_secs(60));
        crate::kept::finish_analyses_check(home);
        if ended == Ended::Replaced {
            gone(&noted);
            return ExitCode::SUCCESS;
        }
        let _ = std::fs::remove_file(&socket);
        gone(&noted);
        // The short folder under `/tmp`, once no daemon of another build is listening in it.
        if let Some(folder) = socket.parent().filter(|folder| folder.parent() == Some(Path::new("/tmp"))) {
            let _ = std::fs::remove_dir(folder);
        }
        ExitCode::SUCCESS
    }

    /// How a daemon's serving ended: another daemon took its socket's path, and the socket is
    /// that one's now, or it stopped or had nothing to do for long enough, and its socket goes.
    #[derive(PartialEq, Eq)]
    enum Ended {
        Replaced,
        Done,
    }

    /// Answers each connection on a thread of its own, until the daemon is told to stop, has had
    /// nothing to do for long enough, or another daemon has taken its socket's path (`replaced`).
    /// While nothing else is going on, it keeps the cache under its cap.
    fn listen(
        listener: &UnixListener,
        home: &Path,
        state: &Arc<Mutex<State>>,
        replaced: &dyn Fn() -> bool,
    ) -> Ended {
        let mut ticks: u32 = 0;
        let idle = idle_limit();
        let cache = Cache::new(home);
        let mut last_cleanup: Option<Instant> = None;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(state);
                    std::thread::spawn(move || answer(stream, &state, REQUEST_WAIT));
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    let done = {
                        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                        let idle = idle_after(state.served, idle);
                        state.stopping || (state.sessions == 0 && state.last_work.elapsed() > idle)
                    };
                    if done {
                        return Ended::Done;
                    }
                    // Once a second: whether another daemon took the path, leaving this one
                    // unreachable. It leaves the other's socket where it is.
                    ticks = ticks.wrapping_add(1);
                    if ticks.is_multiple_of(50) && replaced() {
                        return Ended::Replaced;
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
                Err(_) => return Ended::Done,
            }
        }
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
        if let Some(kind) = Kind::of(kind.trim_end()) {
            // A daemon of other source says nothing, so the command does its work itself.
            if header.trim_end() != self::header() {
                return;
            }
            let mut fields = Vec::new();
            for _ in 0..6 {
                let mut line = String::new();
                if (&mut reader).take(LONGEST_LINE).read_line(&mut line).is_err() || !line.ends_with('\n') {
                    return;
                }
                fields.push(line.trim_end_matches('\n').to_owned());
            }
            let [folder, first, second, third, streams, end] = fields.as_slice() else { return };
            let one_stream = match streams.as_str() {
                ONE_STREAM => true,
                TWO_STREAMS => false,
                _ => return,
            };
            if end != "end" {
                return;
            }
            let Some(job) = Job::of(kind, [first, second, third]) else { return };
            // Work takes as long as it takes, and what watches for the command going reads the
            // same connection, so it has no deadline from here.
            reader.get_mut().until = None;
            if reader.get_ref().stream.set_read_timeout(None).is_err() {
                return;
            }
            let Some(outcome) = done(state, &writer, (folder, &job), one_stream) else {
                return;
            };
            let _ = writer.write_all(outcome.encode().as_bytes());
            // The answer ends here, though what watches for the command going holds the
            // connection still: the command reads to the end, then closes it.
            let _ = writer.shutdown(std::net::Shutdown::Write);
            return;
        }
        if header.trim_end() == self::header() && kind.trim_end() == "lsp" {
            // An editor's session is quiet for as long as nobody types.
            reader.get_mut().until = None;
            if reader.get_ref().stream.set_read_timeout(None).is_err() {
                return;
            }
            language_server(reader, &mut writer, state);
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

    /// A command's work, as its request says it.
    enum Job<'a> {
        Build { path: &'a str, output: Option<&'a str>, release: bool },
        Run { path: &'a str, executable: &'a Path, tested: bool },
        Test { path: &'a str, options: crate::test::Options },
    }

    impl<'a> Job<'a> {
        /// The work a `kind` of request asks for with these three lines. None for lines it doesn't
        /// take.
        fn of(kind: Kind, [first, second, third]: [&'a String; 3]) -> Option<Self> {
            let either = |yes: &str, no: &str| match third.as_str() {
                said if said == yes => Some(true),
                said if said == no => Some(false),
                _ => None,
            };
            Some(match kind {
                Kind::Build => Job::Build {
                    path: first,
                    output: (second != NO_OUTPUT).then_some(second.as_str()),
                    release: either("release", "dev")?,
                },
                Kind::Run => Job::Run {
                    path: first,
                    executable: Path::new(second),
                    tested: either("tested", "untested")?,
                },
                Kind::Test => Job::Test {
                    path: first,
                    options: crate::test::Options::of_words([second.as_str(), third.as_str()])?,
                },
            })
        }

        /// Does the work here, as the command would in its own process, giving back how it ends.
        fn done(&self) -> ExitCode {
            match *self {
                Job::Build { path, output, release } => crate::built::build_here(path, output, release),
                Job::Run { path, executable, tested } => {
                    crate::run::prepared(path, tested, executable).err().unwrap_or(ExitCode::SUCCESS)
                }
                Job::Test { path, options } => crate::test::test_here(path, options),
            }
        }
    }

    /// Asks the work in progress to stop, when the command that wanted it has gone: the build, and
    /// any tests running, which may never end by themselves.
    fn stop_the_work() {
        fennec_build::cancel();
        crate::llvm_tests::stop_workers();
    }

    /// Does a command's work, as the command would in its own process: in its folder, with what it
    /// prints taken into files to send back, or into one when the command's two are one. One
    /// command's work at a time, which is also what a process's one working folder and one
    /// standard output and error allow. The command going before the answer, as on Ctrl-C, stops
    /// the work. None when the printing can't be taken, so nothing is sent and the command does
    /// the work itself.
    fn done(
        state: &Mutex<State>,
        client: &UnixStream,
        (folder, job): (&str, &Job<'_>),
        one_stream: bool,
    ) -> Option<Outcome> {
        static BUILDING: Mutex<u64> = Mutex::new(0);
        let mut building = BUILDING.lock().unwrap_or_else(PoisonError::into_inner);
        *building += 1;
        let home = state.lock().unwrap_or_else(PoisonError::into_inner).home.clone();
        // The daemon's own folder in the Fennec folder, made here when its socket is elsewhere.
        owned_privately(&home.join("daemon"), &home, true)?;
        {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            state.served += 1;
            state.sessions += 1;
        }
        let printed = home.join("daemon").join(format!("work-{}-{}", std::process::id(), *building));
        let outcome = taking_printing(&printed, one_stream, || {
            fennec_build::clear_cancel();
            let running = Arc::new(AtomicBool::new(true));
            let _watching = stop_when_gone(client, &running, stop_the_work);
            let before = std::env::current_dir().ok();
            let code = if std::env::set_current_dir(folder).is_ok() {
                let made = std::thread::scope(|scope| {
                    let work = || {
                        let code = job.done();
                        // Lint errors a run was finding and didn't show, as when it stopped
                        // before loading, are let go of as the command's own process would, but
                        // only once they're found: they're found in this folder.
                        crate::lint::forget_errors_found();
                        code_of(code)
                    };
                    std::thread::Builder::new()
                        .stack_size(crate::STACK_BYTES)
                        .spawn_scoped(scope, work)
                        .ok()
                        .and_then(|working| working.join().ok())
                });
                made.unwrap_or(1)
            } else {
                eprintln!("error: couldn't go to {folder}");
                1
            };
            if let Some(before) = before {
                let _ = std::env::set_current_dir(before);
            }
            running.store(false, Ordering::Relaxed);
            code
        });
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.sessions = state.sessions.saturating_sub(1);
        state.worked();
        drop(state);
        // A build that carried over what the kit's last check found is checked all again, off the
        // build's way, once it's answered.
        if crate::kept::pending() {
            let home = home.clone();
            let _ = std::thread::Builder::new()
                .stack_size(crate::STACK_BYTES)
                .spawn(move || crate::kept::verify_pending(&home));
        }
        // A build made in a process of its own checks there what it used again of the analyses
        // the builds before kept: once it has, what it found wrong is taken back, now that the
        // command's tests are counted.
        if crate::child::wanted() {
            let _ = std::thread::Builder::new().spawn(crate::child::after_builds);
        }
        // So are the analyses its builds used again of the ones the builds before kept: taken now,
        // so the next build waits for the check rather than using what it may let go of.
        if let Some(check) = fennec_llvm::AnalysesCheck::take() {
            let home = home.clone();
            let _ = std::thread::Builder::new()
                .stack_size(crate::STACK_BYTES)
                .spawn(move || crate::kept::verify_analyses(&home, check));
        }
        outcome
    }

    /// Watches a command's connection while its build runs: the command going, by Ctrl-C or
    /// otherwise, closes it, and `stop` is called if the build is still `running`. The connection
    /// closes after the answer too, by when nothing is. Anything more the command sends is read
    /// and passed over, so only its going stops the build.
    fn stop_when_gone(
        client: &UnixStream,
        running: &Arc<AtomicBool>,
        stop: fn(),
    ) -> Option<std::thread::JoinHandle<()>> {
        let mut watched = client.try_clone().ok()?;
        let running = Arc::clone(running);
        let watching = std::thread::spawn(move || {
            let mut sent = [0; 64];
            while matches!(watched.read(&mut sent), Ok(1..)) {}
            if running.load(Ordering::Relaxed) {
                stop();
            }
        });
        Some(watching)
    }

    /// Runs `run` with standard output and error going to files beside `printed`, or both to one
    /// if `one_stream`, and gives back what it printed with the code it gave: all of it as standard
    /// output when it went to one. None when they can't be put in place.
    fn taking_printing(printed: &Path, one_stream: bool, run: impl FnOnce() -> u8) -> Option<Outcome> {
        let (out_path, err_path) = (printed.with_extension("out"), printed.with_extension("err"));
        let out = std::fs::File::create(&out_path).ok()?;
        let err = if one_stream { None } else { Some(std::fs::File::create(&err_path).ok()?) };
        let _ = (std::io::stdout().flush(), std::io::stderr().flush());
        let saved = (rustix::io::dup(std::io::stdout()).ok()?, rustix::io::dup(std::io::stderr()).ok()?);
        if rustix::stdio::dup2_stdout(&out).is_err()
            || rustix::stdio::dup2_stderr(err.as_ref().unwrap_or(&out)).is_err()
        {
            let _ = (rustix::stdio::dup2_stdout(&saved.0), rustix::stdio::dup2_stderr(&saved.1));
            return None;
        }
        let code = run();
        let _ = (std::io::stdout().flush(), std::io::stderr().flush());
        let _ = (rustix::stdio::dup2_stdout(&saved.0), rustix::stdio::dup2_stderr(&saved.1));
        let read =
            |path: &Path| String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned();
        let stderr = if one_stream { String::new() } else { read(&err_path) };
        let outcome = Outcome { stdout: read(&out_path), stderr, code };
        let _ = std::fs::remove_file(&out_path);
        if !one_stream {
            let _ = std::fs::remove_file(&err_path);
        }
        Some(outcome)
    }

    /// The number a process ending with `code` exits with.
    fn code_of(code: ExitCode) -> u8 {
        (0..=u8::MAX).find(|number| ExitCode::from(*number) == code).unwrap_or(1)
    }

    /// Serves a language server session over a connection, on a thread with room for the
    /// checker to recurse, until the editor is done. The session reads the connection on a thread
    /// of its own, so it's shut once the session ends, which ends that thread and tells the
    /// editor's side the session is over.
    fn language_server(reader: BufReader<Within>, writer: &mut UnixStream, state: &Mutex<State>) {
        let lock = || state.lock().unwrap_or_else(PoisonError::into_inner);
        {
            let mut state = lock();
            state.sessions += 1;
            state.served += 1;
        }
        let output = &mut *writer;
        std::thread::scope(|scope| {
            let session = std::thread::Builder::new()
                .stack_size(crate::STACK_BYTES)
                .spawn_scoped(scope, move || fennec_lsp::serve(reader, output));
            let _ = session.map(std::thread::ScopedJoinHandle::join);
        });
        let _ = writer.shutdown(std::net::Shutdown::Both);
        let mut state = lock();
        state.sessions = state.sessions.saturating_sub(1);
        state.worked();
    }

    fn handle(request: &str, state: &mut State) -> Outcome {
        let mut lines = request.lines();
        let (asking, kind) = (lines.next(), lines.next());
        // Any build of Fennec may ask a daemon to stop: past the user's cap, a daemon starting stops
        // the one used longest ago, whatever its build (`keep_under_cap`).
        if kind == Some("stop") && asking.is_some_and(|asking| asking.starts_with("fennec ")) {
            state.stopping = true;
            return Outcome::default();
        }
        if asking != Some(&header()) {
            return Outcome::failure(format!(
                "error: this daemon is Fennec {VERSION} built from other source, {}\n",
                BUILD.get(..16).unwrap_or(BUILD)
            ));
        }
        match (kind, lines.next()) {
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
                ) + &crate::kept::status().unwrap_or_default(),
                ..Outcome::default()
            },
            (Some("changes"), Some(path)) => {
                state.served += 1;
                state.worked();
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
            BUILD, Reply, State, VERSION, answer, ask_within, environment, handle, send_within, socket_in,
            socket_owned_by, socket_path, stop_when_gone,
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
                noted: None,
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

        /// A daemon that has served one request, or none, goes after a minute with nothing to do, or
        /// sooner if it's told to; one used again waits as long as it's told.
        #[test]
        fn a_daemon_used_once_goes_soon() {
            let half_hour = Duration::from_mins(30);
            assert_eq!(super::idle_after(0, half_hour), Duration::from_mins(1));
            assert_eq!(super::idle_after(1, half_hour), Duration::from_mins(1));
            assert_eq!(super::idle_after(2, half_hour), half_hour);
            assert_eq!(super::idle_after(1, Duration::from_secs(30)), Duration::from_secs(30));
        }

        /// A command for Fennec's folder in the temp folder, as a fuzzer's is, starts no daemon; one
        /// for a folder elsewhere does.
        #[test]
        fn no_daemon_starts_for_a_fennec_folder_in_the_temp_folder() {
            if std::env::var_os("FENNEC_DAEMONS").is_some() {
                return;
            }
            assert!(!super::may_start(&std::env::temp_dir().join("fennec-fuzz-llvm-1")));
            assert!(!super::may_start(Path::new("/tmp/fennec-run")));
            assert!(super::may_start(Path::new("/home/fennec/.fennec")));
        }

        /// A command that goes while its build runs, by Ctrl-C or otherwise, closes its connection,
        /// and that stops the build. Bytes it sent before going don't, and once the build is done
        /// its going stops nothing.
        #[test]
        fn a_command_gone_while_its_build_runs_stops_it() {
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, Ordering};
            static STOPPED: AtomicBool = AtomicBool::new(false);
            fn stop() {
                STOPPED.store(true, Ordering::Relaxed);
            }
            // Gone mid-build, after sending a stray byte or two, and gone once the build is done.
            for building in [true, false] {
                STOPPED.store(false, Ordering::Relaxed);
                let (client, command) = UnixStream::pair().unwrap();
                let running = Arc::new(AtomicBool::new(building));
                let watching = stop_when_gone(&client, &running, stop).unwrap();
                (&command).write_all(b"\n\n").unwrap();
                drop(command);
                watching.join().unwrap();
                assert_eq!(STOPPED.load(Ordering::Relaxed), building, "building: {building}");
            }
        }

        /// A daemon answers only a command of its own build: one of the same version built from
        /// other source would get what an older compiler does.
        #[test]
        fn a_daemon_answers_only_its_own_build() {
            let home = Path::new("/home/fennec/.fennec");
            let socket = socket_path(home, false);
            // Named by its version, its build and its environment, so another build's is another.
            let named = format!("{VERSION}-{}-{}.sock", BUILD.get(..16).unwrap_or(BUILD), environment());
            assert_eq!(socket, Some(home.join("daemon").join(named)));
            let mut state = State {
                home: home.to_path_buf(),
                snapshots: HashMap::new(),
                served: 0,
                started: Instant::now(),
                last_work: Instant::now(),
                sessions: 0,
                stopping: false,
                noted: None,
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

    /// There's no daemon here to answer.
    pub(super) fn ask_every(_: &Path, _: &[&str]) -> Vec<Outcome> {
        Vec::new()
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
