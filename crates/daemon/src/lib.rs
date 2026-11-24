//! What a command and the build daemon say to each other, and where the daemon listens. The
//! `fennec` command runs the daemon and asks it for work (see `crates/fennec/src/daemon.rs`), and
//! the launcher asks it too, for a run, so it can start a program without starting the command.
//! Both use this, so they agree byte for byte.
//!
//! A daemon answers only a command of its own version and build, which every request starts with
//! (`Build::header`). The launcher speaks to the daemons of toolchains of other versions than its
//! own, so what's said here has a number, `PROTOCOL`, which a toolchain tells the launcher. Any
//! change to what a request or an answer says, or where the daemon listens, changes that number:
//! a launcher that doesn't know a toolchain's number hands its commands to the toolchain.

#[cfg(unix)]
use std::io::{ErrorKind, Read};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(unix)]
use std::time::Instant;

/// The number of what's said here. The tests below hold every request's bytes and where a socket
/// is, so one changed without it fails them.
pub const PROTOCOL: u32 = 1;

/// The `fennec` a daemon is from, or a command asks as: its version, and its build, the hash of
/// all of Fennec's source (`FENNEC_BUILD`).
#[derive(Clone, Copy, Debug)]
pub struct Build<'a> {
    pub version: &'a str,
    pub build: &'a str,
}

impl Build<'_> {
    /// The first line of every request: a daemon of another version or build says nothing back,
    /// so the command does its work itself.
    #[must_use]
    pub fn header(&self) -> String {
        format!("fennec {} {}", self.version, self.build)
    }

    /// How every socket of this build's daemons is named: its version, then its build.
    #[must_use]
    pub fn socket_start(&self) -> String {
        format!("{}-{}-", self.version, self.build.get(..16).unwrap_or(self.build))
    }
}

/// What a command prints, and how it exits.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: u8,
}

impl Outcome {
    #[must_use]
    pub fn failure(stderr: String) -> Self {
        Outcome { stdout: String::new(), stderr, code: 1 }
    }

    /// Prints what the work printed, and gives how it ended.
    #[expect(clippy::print_stdout, clippy::print_stderr, reason = "it's what the command prints")]
    #[must_use]
    pub fn print(self) -> ExitCode {
        print!("{}", self.stdout);
        eprint!("{}", self.stderr);
        ExitCode::from(self.code)
    }

    /// The outcome as the daemon sends it: the code, the two lengths, then the two texts.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}\n{} {}\n{}{}", self.code, self.stdout.len(), self.stderr.len(), self.stdout, self.stderr)
    }

    #[must_use]
    pub fn decode(text: &str) -> Option<Outcome> {
        let (code, rest) = text.split_once('\n')?;
        let (lengths, rest) = rest.split_once('\n')?;
        let (stdout_len, stderr_len) = lengths.split_once(' ')?;
        let (stdout_len, stderr_len): (usize, usize) = (stdout_len.parse().ok()?, stderr_len.parse().ok()?);
        let stdout = rest.get(..stdout_len)?;
        let stderr = rest.get(stdout_len..stdout_len + stderr_len)?;
        Some(Outcome { stdout: stdout.to_owned(), stderr: stderr.to_owned(), code: code.parse().ok()? })
    }
}

/// What a request says when the command's standard output and error are one file, and when
/// they're two.
pub const ONE_STREAM: &str = "one stream";
pub const TWO_STREAMS: &str = "two streams";

/// A request for a command's work: `build`, `run` or `test`, then the command's folder, the work's
/// three lines, and whether its output is one stream, each a line, then `end`. None when a line
/// can't be sent as one.
#[must_use]
pub fn work_request(
    from: Build<'_>,
    kind: &str,
    folder: &str,
    arguments: [&str; 3],
    one_stream: bool,
) -> Option<String> {
    let [first, second, third] = arguments;
    let streams = if one_stream { ONE_STREAM } else { TWO_STREAMS };
    let lines = [folder, first, second, third, streams];
    if lines.iter().any(|line| line.contains('\n') || line.contains('\r')) {
        return None;
    }
    Some(format!("{}\n{kind}\n{}\nend\n", from.header(), lines.join("\n")))
}

/// Whether this process's standard output and error are one file, as a terminal or `2>&1` makes
/// them. The daemon then prints both into one, so what's printed reads in the order it was, as it
/// does from the command's own process.
#[cfg(unix)]
#[must_use]
pub fn one_stream() -> bool {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    let file = |fd: std::os::fd::BorrowedFd<'_>| {
        fd.try_clone_to_owned().and_then(|owned| std::fs::File::from(owned).metadata())
    };
    let (Ok(out), Ok(err)) = (file(std::io::stdout().as_fd()), file(std::io::stderr().as_fd())) else {
        return false;
    };
    (out.dev(), out.ino()) == (err.dev(), err.ino())
}

/// How long a socket's path may be: a little under the 104 bytes macOS takes, and the 108 Linux
/// does.
#[cfg(unix)]
pub const LONGEST_SOCKET: usize = 100;

/// The variables that differ from one terminal to the next and change nothing a build does, left
/// out of the environment's hash so one terminal's daemon serves the others'.
#[cfg(unix)]
const PER_TERMINAL: [&str; 15] = [
    "_",
    "COLORFGBG",
    "ITERM_PROFILE",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "OLDPWD",
    "PWD",
    "SHLVL",
    "SSH_TTY",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "TMUX_PANE",
    "WINDOWID",
];

/// What the allocator's own settings start with. They change how a process holds memory, not what
/// a build makes, and the daemon is started with one of its own.
#[cfg(unix)]
pub const ALLOCATOR: &str = "MIMALLOC_";

/// A short hash of this process's environment but for what's only a terminal's: a daemon builds in
/// the environment it was started in, which `cc`, the linker and every setting read, so a command
/// whose environment differs has a daemon of its own.
#[cfg(unix)]
#[must_use]
pub fn environment() -> String {
    environment_of(std::env::vars_os())
}

/// `environment`, of these variables.
#[cfg(unix)]
pub fn environment_of(variables: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut variables: Vec<(std::ffi::OsString, std::ffi::OsString)> = variables
        .filter(|(name, _)| !PER_TERMINAL.iter().any(|terminal| name == terminal))
        .filter(|(name, _)| !name.as_bytes().starts_with(ALLOCATOR.as_bytes()))
        .collect();
    variables.sort();
    let mut text = Vec::new();
    for (name, value) in &variables {
        text.extend_from_slice(name.as_bytes());
        text.push(0);
        text.extend_from_slice(value.as_bytes());
        text.push(0);
    }
    let hash = fennec_blake3::hash(&text).to_hex();
    hash.get(..16).unwrap_or(&hash).to_owned()
}

/// Where the daemon of `build` for this environment listens: in Fennec's folder, unless that path
/// is too long for a socket, in which case the user's runtime folder, or a short folder of its own
/// under `/tmp` named by a hash of Fennec's folder. None when none of those will do, and then
/// commands do their work themselves. Only the daemon, which listens there, `makes` that folder.
#[cfg(unix)]
#[must_use]
pub fn socket_path(build: Build<'_>, home: &Path, makes: bool) -> Option<PathBuf> {
    socket_in(build, home, std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from), makes)
}

/// `socket_path`, with the user's runtime folder as given.
#[cfg(unix)]
#[must_use]
pub fn socket_in(build: Build<'_>, home: &Path, runtime: Option<PathBuf>, makes: bool) -> Option<PathBuf> {
    let name = format!("{}{}.sock", build.socket_start(), environment());
    // Wherever it is, its folder is its user's alone: connecting needs a way into the folder, so
    // nobody else can reach the daemon, which reads any file it's pointed at.
    let preferred = home.join("daemon").join(&name);
    if preferred.as_os_str().len() < LONGEST_SOCKET {
        owned_privately(&home.join("daemon"), home, makes)?;
        return Some(preferred);
    }
    let hash = home_hash(home);
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

/// A short hash of Fennec's folder, naming its sockets outside it.
#[cfg(unix)]
#[must_use]
pub fn home_hash(home: &Path) -> String {
    let hash = fennec_blake3::hash(home.to_string_lossy().as_bytes()).to_hex();
    hash.get(..16).unwrap_or(&hash).to_owned()
}

/// The sockets of `build`'s daemons, one for each environment commands ran in, in every folder
/// `socket_in` may choose. A folder that isn't the user's alone is passed over.
#[cfg(unix)]
#[must_use]
pub fn sockets(build: Build<'_>, home: &Path) -> Vec<PathBuf> {
    let hash = home_hash(home);
    let mut folders = vec![(home.join("daemon"), String::new())];
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        folders.push((PathBuf::from(runtime), format!("fennec-{hash}-")));
    }
    folders.push((Path::new("/tmp").join(format!("fennec-{hash}")), String::new()));
    let mut found = Vec::new();
    for (folder, prefix) in folders {
        let start = format!("{prefix}{}", build.socket_start());
        let Some(entries) =
            owned_privately(&folder, home, false).and_then(|()| std::fs::read_dir(&folder).ok())
        else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&start) && name.ends_with(".sock") {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

/// Whether `folder` will do for a socket: open to nobody but its owner, who owns Fennec's folder at
/// `home` too. `/tmp` is shared, so a folder someone else made there first, or one others can
/// reach, is never used. The daemon `makes` it if it isn't there; for a command that only
/// connects, one that isn't there will do, as there's nothing in it to connect to.
#[cfg(unix)]
pub fn owned_privately(folder: &Path, home: &Path, makes: bool) -> Option<()> {
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
/// Fennec's folder at `home`. A socket another user bound in the folder before it was made private
/// is theirs, and they can't give it away, so its answers, and an editor's files sent to it, never
/// reach them.
#[cfg(unix)]
#[must_use]
pub fn connect(socket: &Path, home: &Path) -> Option<UnixStream> {
    use std::os::unix::fs::MetadataExt as _;
    let owner = std::fs::metadata(home).ok()?.uid();
    socket_owned_by(socket, owner).then(|| UnixStream::connect(socket).ok()).flatten()
}

/// Whether `socket` is a socket, not followed if it's a link, that `owner` bound.
#[cfg(unix)]
#[must_use]
pub fn socket_owned_by(socket: &Path, owner: u32) -> bool {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    std::fs::symlink_metadata(socket).is_ok_and(|found| found.file_type().is_socket() && found.uid() == owner)
}

/// A connection read until a moment: each read waits only what's left, so a request sent a byte at
/// a time still has its time in all. With no moment, a read waits as long as it takes, as an
/// editor's session does.
#[cfg(unix)]
pub struct Within {
    pub stream: UnixStream,
    pub until: Option<Instant>,
}

#[cfg(unix)]
impl Read for Within {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(until) = self.until {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ErrorKind::TimedOut.into());
            }
            // macOS refuses a timeout, with "invalid argument", once the other end has closed the
            // connection, as the daemon does after its whole answer. A read then gives the end at
            // once, so it can't wait past the deadline either.
            match self.stream.set_read_timeout(Some(left)) {
                Err(err) if err.kind() != ErrorKind::InvalidInput => return Err(err),
                _ => {}
            }
        }
        self.stream.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::{Build, Outcome, PROTOCOL, work_request};

    const BUILD: Build<'static> = Build { version: "0.4.2", build: "0123456789abcdef0123456789abcdef" };

    /// What's said is held byte for byte at this `PROTOCOL`: a change here is a change to it, and
    /// the launcher reads the number to know whether it can ask a toolchain's daemon itself.
    #[test]
    fn a_request_is_these_bytes_at_protocol_one() {
        assert_eq!(PROTOCOL, 1, "a new protocol holds its own bytes here");
        assert_eq!(BUILD.header(), "fennec 0.4.2 0123456789abcdef0123456789abcdef");
        assert_eq!(BUILD.socket_start(), "0.4.2-0123456789abcdef-");
        assert_eq!(
            work_request(
                BUILD,
                "run",
                "/home/k",
                ["main.fen", "/home/.fennec/work/fennec-run-1-a-0", "tested"],
                true
            )
            .as_deref(),
            Some(
                "fennec 0.4.2 0123456789abcdef0123456789abcdef\nrun\n/home/k\nmain.fen\n\
                 /home/.fennec/work/fennec-run-1-a-0\ntested\none stream\nend\n"
            )
        );
        assert_eq!(
            work_request(BUILD, "build", "/k", [".", "-", "dev"], false).as_deref(),
            Some("fennec 0.4.2 0123456789abcdef0123456789abcdef\nbuild\n/k\n.\n-\ndev\ntwo streams\nend\n")
        );
        assert_eq!(work_request(BUILD, "run", "/k", ["a\nb", "/x", "tested"], false), None);
        assert_eq!(work_request(BUILD, "run", "/k\r", ["a", "/x", "tested"], false), None);
        let outcome = Outcome { stdout: "out\n".into(), stderr: "é".into(), code: 3 };
        assert_eq!(outcome.encode(), "3\n4 2\nout\né");
        assert_eq!(Outcome::decode(&outcome.encode()), Some(outcome));
        assert_eq!(Outcome::decode("0\n9 0\nshort"), None);
    }

    /// The environment's hash leaves out what differs from terminal to terminal and the
    /// allocator's settings, which the daemon is started with one of, and nothing else.
    #[cfg(unix)]
    #[test]
    fn the_environment_hash_leaves_out_terminals_and_the_allocator() {
        let of = |pairs: &[(&str, &str)]| {
            super::environment_of(pairs.iter().map(|(name, value)| ((*name).into(), (*value).into())))
        };
        let base = of(&[("PATH", "/usr/bin"), ("CC", "cc")]);
        let terminal =
            of(&[("PATH", "/usr/bin"), ("PWD", "/x"), ("CC", "cc"), ("MIMALLOC_PURGE_DELAY", "0")]);
        assert_eq!(base, terminal);
        assert_ne!(base, of(&[("PATH", "/opt/bin"), ("CC", "cc")]));
        assert_ne!(base, of(&[("PATH", "/usr/bin"), ("CC", "cc"), ("LD", "ld")]));
    }

    /// The environment's hash, which names a daemon's socket, is held too: the launcher works it
    /// out as the toolchain would.
    #[cfg(unix)]
    #[test]
    fn the_environment_hash_is_held() {
        let of = |pairs: &[(&str, &str)]| {
            super::environment_of(pairs.iter().map(|(name, value)| ((*name).into(), (*value).into())))
        };
        assert_eq!(
            of(&[("PATH", "/usr/bin"), ("CC", "cc")]),
            of(&[("CC", "cc"), ("PATH", "/usr/bin"), ("PWD", "/x")])
        );
        let empty = fennec_blake3::hash(b"").to_hex();
        assert_eq!(of(&[]), empty.get(..16).unwrap());
        let one = fennec_blake3::hash(b"CC\0cc\0PATH\0/usr/bin\0").to_hex();
        assert_eq!(of(&[("PATH", "/usr/bin"), ("CC", "cc"), ("MIMALLOC_X", "1")]), one.get(..16).unwrap());
    }

    /// A socket in Fennec's folder is named by the build, then the environment.
    #[cfg(unix)]
    #[test]
    fn a_socket_is_in_fennecs_folder_named_by_build_and_environment() {
        // Short, so the socket fits in Fennec's folder: the system's temporary folder can be long.
        let short = std::path::PathBuf::from(format!("/tmp/fennec-daemon-{}", std::process::id()));
        std::fs::create_dir_all(&short).unwrap();
        let home = fennec_temp::Temp::at(short);
        let socket = super::socket_in(BUILD, home.path(), None, false).unwrap();
        let name = format!("0.4.2-0123456789abcdef-{}.sock", super::environment());
        assert_eq!(socket, home.path().join("daemon").join(name));
    }
}
