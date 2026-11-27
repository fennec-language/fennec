//! `fennec build`: every example that fails while running fails the same way as a native
//! program, with the message its `.err` keeps. That every example builds and
//! prints exactly its `.out` file is `llvm.rs`'s `the_examples_it_takes_print_what_they_should`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How much CPU time a test program gets. Time is CPU time, not time by the clock, so a busy
/// machine, which only makes a program wait, never fails one: a program that spends a minute of
/// CPU is in a loop it never leaves.
pub(crate) const PROGRAM_DEADLINE: Duration = Duration::from_secs(60);

/// How much CPU time `fennec` itself gets, for a build and maybe a run. The programs it starts,
/// like LLVM's tools and the program it built, each get as much.
pub(crate) const FENNEC_DEADLINE: Duration = Duration::from_secs(180);

/// How long by the clock any test program may run, whatever its CPU time: for one waiting for
/// something that never comes, which spends none. A busy machine never takes this long, and a
/// stuck program fails the test in ten minutes rather than holding the run for hours.
pub(crate) const WALL_DEADLINE: Duration = Duration::from_secs(600);

/// The signal a program gets for spending more CPU time than it may: SIGXCPU, on macOS and Linux.
#[cfg(unix)]
const CPU_SPENT: i32 = 24;

/// Running a command with limits, as every program a test starts runs: so much CPU time, and
/// `WALL_DEADLINE` by the clock.
pub(crate) trait Within {
    /// What `output()` gives, once the program ends within `PROGRAM_DEADLINE` of CPU time. One that
    /// spends more, or runs past `WALL_DEADLINE`, is killed, and that's a `TimedOut` error saying
    /// which limit it reached.
    fn output_within(&mut self) -> std::io::Result<Output>;
    /// The same with a CPU time of its own.
    fn output_before(&mut self, cpu: Duration) -> std::io::Result<Output>;
    /// The same with `input` as its standard input.
    fn output_given(&mut self, input: &[u8], cpu: Duration) -> std::io::Result<Output>;
    /// The same with a limit by the clock of its own too.
    fn output_limited(&mut self, cpu: Duration, wall: Duration) -> std::io::Result<Output>;
}

impl Within for Command {
    fn output_within(&mut self) -> std::io::Result<Output> {
        self.output_before(PROGRAM_DEADLINE)
    }

    fn output_before(&mut self, cpu: Duration) -> std::io::Result<Output> {
        self.output_limited(cpu, WALL_DEADLINE.max(cpu))
    }

    fn output_limited(&mut self, cpu: Duration, wall: Duration) -> std::io::Result<Output> {
        let counted = counted_for_what_it_leaves(self);
        let child = cpu_limited(self, cpu)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut output = finished_before(child, Limits { cpu, wall })?;
        left_nothing(counted, &mut output);
        Ok(output)
    }

    fn output_given(&mut self, input: &[u8], cpu: Duration) -> std::io::Result<Output> {
        let counted = counted_for_what_it_leaves(self);
        let mut limited = cpu_limited(self, cpu);
        let mut child =
            limited.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut stdin = child.stdin.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
        let input = input.to_vec();
        // Written from a thread of its own, so a program that never reads it can't hold this up.
        std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input));
        let mut output = finished_before(child, Limits { cpu, wall: WALL_DEADLINE.max(cpu) })?;
        left_nothing(counted, &mut output);
        Ok(output)
    }
}

/// Programs that end normally holding blocks, by the name of what's run, each with who is finding
/// out why. There are none. A new one isn't added here: it's a leak to fix.
const MAY_LEAVE_BLOCKS: &[&str] = &[];

/// Has a built program count what it leaves, as every one a test runs does: its name, and whether
/// the test asked for the counts itself. `None` for the compiler and for anything that isn't a
/// program a test built.
fn counted_for_what_it_leaves(command: &mut Command) -> Option<(String, bool)> {
    let name = Path::new(command.get_program()).file_name()?.to_string_lossy().into_owned();
    if name.starts_with("fennec") || name == "sh" || name == "cargo" {
        return None;
    }
    let asked = command.get_envs().any(|(key, _)| key == "FENNEC_COUNTS");
    command.env("FENNEC_COUNTS", "1");
    Some((name, asked))
}

/// The rule every program a test runs is held to: **one that ends normally, with exit code 0, has
/// let go of everything it made.** Its last line of counts says how many blocks it still held,
/// and anything but 0 fails the test, whatever the test was looking at.
///
/// A program the runtime stops for a failure nothing handled in `main` isn't held to it: a failed
/// `assert`, an overflow that stops it, an error out of `main`. The process ends there, holding
/// the failure it's saying and what `main` still held. A task or a generator that fails while the
/// program carries on and ends normally is held to it like any other.
///
/// The counts' line is taken back out of what the program said unless the test asked for it.
fn left_nothing(counted: Option<(String, bool)>, output: &mut Output) {
    let Some((name, asked)) = counted else { return };
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    let Some(counts) = said.lines().rev().find(|line| line.starts_with("copies ")) else { return };
    let left = counts.rsplit_once(" live ").map_or("0", |(_, left)| left.trim());
    let excused = MAY_LEAVE_BLOCKS.iter().any(|known| name.starts_with(known));
    assert!(
        output.status.code() != Some(0) || left == "0" || excused,
        "`{name}` ended normally holding {left} blocks it never let go of: {counts}"
    );
    if !asked {
        let mut kept = String::with_capacity(said.len());
        for line in said.lines().filter(|line| !line.starts_with("copies ")) {
            kept.push_str(line);
            kept.push('\n');
        }
        output.stderr = kept.into_bytes();
    }
}

/// A program's two limits: CPU time, and time by the clock.
#[derive(Clone, Copy)]
struct Limits {
    cpu: Duration,
    wall: Duration,
}

/// `command`, run with at most `cpu` of CPU time, as each program it starts is too: through `sh`,
/// which sets the limit and becomes the program, so it's the same process with the same
/// arguments, environment and folder. Where there's no `sh` limit to set, as on Windows, it's
/// `command` as it is, held to the clock alone.
fn cpu_limited(command: &Command, cpu: Duration) -> Command {
    if cfg!(windows) {
        let mut same = Command::new(command.get_program());
        same.args(command.get_args());
        copy_setting(command, &mut same);
        return same;
    }
    let mut limited = Command::new("sh");
    limited
        .arg("-c")
        // The soft limit sends SIGXCPU, which the test hears as CPU time spent. The hard limit, a
        // second later, is only for a program that catches it. Set as one, as `ulimit -t` sets
        // them, Linux can find both passed at once and send SIGKILL, which says nothing. Soft
        // first, as the hard limit can't go below it; a hard limit already lower stays.
        .arg("ulimit -S -t \"$0\" && { ulimit -H -t \"$(($0 + 1))\" 2>/dev/null || :; } && exec \"$@\"")
        .arg(cpu.as_secs().max(1).to_string())
        .arg(command.get_program())
        .args(command.get_args());
    copy_setting(command, &mut limited);
    limited
}

/// Gives `to` the environment and folder `from` was given.
fn copy_setting(from: &Command, to: &mut Command) {
    for (name, value) in from.get_envs() {
        match value {
            Some(value) => to.env(name, value),
            None => to.env_remove(name),
        };
    }
    if let Some(folder) = from.get_current_dir() {
        to.current_dir(folder);
    }
}

/// What `child` printed and how it ended, once it ends within its limits. Both pipes are read
/// while it runs, so a program printing a lot isn't stopped by a full one. One past a limit is
/// killed, and the error says which limit and what it printed by then, which shows where it
/// stopped.
fn finished_before(mut child: Child, limits: Limits) -> std::io::Result<Output> {
    /// What a pipe has given so far, kept as it comes, and whether it's ended.
    type Kept = std::sync::Arc<std::sync::Mutex<(Vec<u8>, bool)>>;
    fn read_all(pipe: Option<impl Read + Send + 'static>) -> Kept {
        let kept: Kept = Kept::default();
        let filling = std::sync::Arc::clone(&kept);
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            if let Some(mut pipe) = pipe {
                while let Ok(read @ 1..) = pipe.read(&mut chunk) {
                    if let Ok(mut kept) = filling.lock() {
                        kept.0.extend_from_slice(chunk.get(..read).unwrap_or_default());
                    }
                }
            }
            if let Ok(mut kept) = filling.lock() {
                kept.1 = true;
            }
        });
        kept
    }
    // What a pipe gave, once it ends or `wait` passes: a program it started may still hold it
    // open after it's killed.
    fn taken(kept: &Kept, wait: Duration) -> Vec<u8> {
        let until = Instant::now() + wait;
        loop {
            let ended = kept.lock().map_or(true, |kept| kept.1);
            if ended || Instant::now() > until {
                return kept.lock().map(|mut kept| std::mem::take(&mut kept.0)).unwrap_or_default();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let stdout = read_all(child.stdout.take());
    let stderr = read_all(child.stderr.take());
    let ended = wait_before(&mut child, limits.wall).and_then(|status| {
        if spent_its_cpu(status) {
            let why =
                format!("a test's program used all {}s of CPU time it may", limits.cpu.as_secs().max(1));
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, why));
        }
        Ok(status)
    });
    match ended {
        Ok(status) => {
            let wait = Duration::from_secs(10);
            Ok(Output { status, stdout: taken(&stdout, wait), stderr: taken(&stderr, wait) })
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            let so_far = |kept: &Kept| {
                let printed = String::from_utf8_lossy(&taken(kept, Duration::from_secs(2))).into_owned();
                let start = printed.char_indices().rev().nth(2_000).map_or(0, |(at, _)| at);
                printed.get(start..).unwrap_or(&printed).to_owned()
            };
            let why =
                format!("{error}, having printed:\n{}\nand to stderr:\n{}", so_far(&stdout), so_far(&stderr));
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, why))
        }
        Err(error) => Err(error),
    }
}

/// Whether a program was stopped for spending all the CPU time it may.
fn spent_its_cpu(status: std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal() == Some(CPU_SPENT)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

/// How `child` ended, once it ends within `deadline` by the clock. One still running then is
/// killed.
pub(crate) fn wait_before(
    child: &mut Child,
    deadline: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let until = Instant::now() + deadline;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() > until {
            // Where it waited, before it's killed: a wait that never ends leaves nothing else
            // behind to say why, as `within.sh` says for the gate's own commands.
            let waiting = where_it_waits(child.id());
            child.kill()?;
            child.wait()?;
            let why = format!(
                "a test's program was still running after {}s by the clock{waiting}",
                deadline.as_secs()
            );
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, why));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How long the look at where a program waits may take by the clock: `sample` samples for a second,
/// and then writes what it saw.
const LOOKING: Duration = Duration::from_secs(20);

/// The most lines of a sample of where a program waits that a failure says.
const LOOKED_LINES: usize = 150;

/// Where the process `pid` waits, each of its threads, for a test that stops it for its time by
/// the clock to say: by a second's `sample` on macOS, and by what the kernel says each thread waits
/// on elsewhere. Empty where neither can be asked. It's asked only of a program that's failing its
/// test already, and the look is held to `LOOKING` and to its own `LOOKED_LINES`, so it can't make
/// a test fail that wouldn't have, or hold one up for long.
fn where_it_waits(pid: u32) -> String {
    if cfg!(target_os = "macos") {
        let Some(file) = std::env::temp_dir().to_str().map(|dir| format!("{dir}/fennec-wait-{pid}.txt"))
        else {
            return String::new();
        };
        let sampled = Command::new("sample")
            .args([pid.to_string().as_str(), "1", "-mayDie", "-file", file.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut sampling| {
                let until = Instant::now() + LOOKING;
                loop {
                    if sampling.try_wait()?.is_some() {
                        return Ok(());
                    }
                    if Instant::now() > until {
                        let _ = sampling.kill();
                        let _ = sampling.wait();
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        let text = sampled.ok().and_then(|()| fs::read_to_string(&file).ok()).unwrap_or_default();
        let _ = fs::remove_file(&file);
        let graph: Vec<&str> = text
            .lines()
            .skip_while(|line| !line.starts_with("Call graph:"))
            .take_while(|line| !line.starts_with("Total number in stack"))
            .take(LOOKED_LINES)
            .collect();
        if graph.is_empty() {
            return String::new();
        }
        return format!("; where it was, sampled for a second, each thread:\n{}", graph.join("\n"));
    }
    let Ok(threads) = fs::read_dir(format!("/proc/{pid}/task")) else { return String::new() };
    let mut waits: Vec<String> = threads
        .flatten()
        .map(|thread| {
            let path = thread.path();
            let wchan = fs::read_to_string(path.join("wchan")).unwrap_or_default();
            let name = fs::read_to_string(path.join("comm")).unwrap_or_default();
            format!(
                "  thread {} ({}) waits on {}",
                thread.file_name().to_string_lossy(),
                name.trim(),
                wchan.trim()
            )
        })
        .collect();
    waits.sort();
    waits.truncate(LOOKED_LINES);
    if waits.is_empty() {
        return String::new();
    }
    format!("; what each thread waited on:\n{}", waits.join("\n"))
}

/// A program that waits forever, spending no CPU time, is killed at its limit by the clock, and
/// the test hears which limit it was, and where it waited: each thread's stack, sampled, on macOS,
/// and what each thread waited on, on Linux.
#[test]
fn a_program_past_its_deadline_is_killed() {
    let started = Instant::now();
    let ran =
        Command::new("sleep").arg("600").output_limited(Duration::from_secs(60), Duration::from_secs(1));
    let why = ran.err().map(|error| (error.kind(), error.to_string()));
    assert!(
        why.as_ref()
            .is_some_and(|(kind, why)| *kind == std::io::ErrorKind::TimedOut && why.contains("by the clock")),
        "{why:?}"
    );
    let said = why.map(|(_, why)| why).unwrap_or_default();
    if cfg!(target_os = "macos") {
        assert!(said.contains("sampled for a second") && said.contains("Thread_"), "{said}");
    } else if cfg!(target_os = "linux") {
        assert!(said.contains("waits on"), "{said}");
    }
    assert!(started.elapsed() < Duration::from_secs(30));
}

/// A program in a loop it never leaves is stopped once it has spent its CPU time, however long
/// that takes by the clock on a busy machine, and the test hears that it was the CPU time.
#[test]
#[cfg(unix)]
fn a_program_that_spends_its_cpu_time_is_stopped() {
    let ran = Command::new("sh").args(["-c", "while :; do :; done"]).output_before(Duration::from_secs(1));
    let why = ran.err().map(|error| (error.kind(), error.to_string()));
    assert!(
        why.as_ref()
            .is_some_and(|(kind, why)| *kind == std::io::ErrorKind::TimedOut && why.contains("of CPU time")),
        "{why:?}"
    );
    // Its environment and folder are its own still.
    let dir = std::env::temp_dir();
    let ran = Command::new("sh")
        .args(["-c", "echo \"$KEPT\" && pwd"])
        .env("KEPT", "kept")
        .current_dir(&dir)
        .output_within()
        .unwrap();
    let said = String::from_utf8_lossy(&ran.stdout).into_owned();
    assert!(said.starts_with("kept\n"), "{said}");
}

/// A program killed at its deadline says what it printed by then, so a test's failure shows where
/// the program stopped.
#[test]
fn a_program_past_its_deadline_says_what_it_printed() {
    let ran = Command::new("sh")
        .args(["-c", "echo got this far; exec sleep 600"])
        .output_limited(Duration::from_secs(60), Duration::from_secs(1));
    let why = ran.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(why.contains("having printed:\ngot this far\n"), "{why}");
}

/// A program a test started and talks to, like a server, killed once the test ends however it
/// ends, so a failed test never leaves one running.
pub(crate) struct Running(pub(crate) Child);

impl Running {
    /// The first line it prints, like the port a server listens on, within `WALL_DEADLINE`: a server
    /// on a busy machine can take a while to start.
    /// The rest of what it prints is read and dropped, so it never waits on a full pipe.
    pub(crate) fn first_line(&mut self) -> std::io::Result<String> {
        use std::io::BufRead;
        let stdout = self.0.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
        let (says, said) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut lines = std::io::BufReader::new(stdout).lines();
            let _ = says.send(lines.next());
            lines.for_each(drop);
        });
        match said.recv_timeout(WALL_DEADLINE) {
            Ok(Some(line)) => line,
            Ok(None) => Err(std::io::ErrorKind::UnexpectedEof.into()),
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "it printed nothing")),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A `fennec` to start, with the runtime library built first. `fennec run` links against that
/// library, and one left from before a rebase links without a word and gets things wrong: a
/// dict's layout that no longer matches, say, printing zeroes. So nothing starts `fennec` until
/// the library is current. Under mutation that's each mutant's own library, or a mutant in the
/// runtime would meet the old one. A test that needs the library says so when it couldn't be built.
pub(crate) fn fennec_command() -> Command {
    runtime_built();
    let mut command = Command::new(env!("CARGO_BIN_EXE_fennec"));
    // A test builds in its own process, as a first build does: one that wants the build daemon
    // takes this away (`with_daemon` in cli.rs), so no test leaves daemons behind.
    command.env("FENNEC_NO_DAEMON", "1");
    command
}

/// Where a test's build daemons for Fennec's folder `home` note themselves (`FENNEC_DAEMONS`):
/// apart from the user's, so they count against the cap only among the test's own, and may start
/// though `home` is in the temp folder. A test that starts one stops it, and finds the folder
/// empty after (`stops` in cli.rs).
pub(crate) fn daemons_of(home: &Path) -> PathBuf {
    home.join("noted-daemons")
}

/// How long a test's build daemon waits with nothing to do before it goes (`FENNEC_DAEMON_IDLE`):
/// far past any gap between one test's commands, however loaded the machine. At 30 seconds, a
/// daemon a test used for several commands went between two of them while two gates ran, and the
/// next command started another: its count of requests served came out 2, not 8. A test stops its
/// daemons itself when it ends, however it ends (`DaemonsStopped`).
#[cfg(unix)]
pub(crate) const DAEMON_IDLE: &str = "600";

/// Starts the build daemon `serve` starts, and waits until `status` finds it running. A command
/// that starts one itself waits only 2 seconds for it, and on a loaded machine does its work in its
/// own process instead, keeping nothing: a test whose first build has to be the daemon's starts the
/// daemon first. `serve` and `status` are made as the test's commands are, with the same
/// environment, or they'd look for another daemon's socket.
#[cfg(unix)]
pub(crate) fn daemon_started(
    mut serve: Command,
    status: &dyn Fn() -> std::io::Result<Output>,
) -> std::io::Result<()> {
    let mut daemon = serve.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    // It's waited for once it ends, which `DaemonsStopped` sees to when the test does.
    std::thread::spawn(move || daemon.wait());
    let waiting = Instant::now();
    while !String::from_utf8_lossy(&status()?.stdout).starts_with("running") {
        if waiting.elapsed() > Duration::from_secs(120) {
            return Err(std::io::Error::other("the build daemon didn't start in 2 minutes"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Stops every build daemon of the Fennec folder `home` when it's dropped: at the end of the test
/// that holds it, however the test ends, so one that fails leaves none to wait out `DAEMON_IDLE`.
#[cfg(unix)]
pub(crate) struct DaemonsStopped<'a> {
    pub(crate) home: &'a Path,
}

#[cfg(unix)]
impl Drop for DaemonsStopped<'_> {
    fn drop(&mut self) {
        // One stopped already, or never started, leaves nothing to stop.
        let _ = fennec_command()
            .args(["daemon", "stop"])
            .env("FENNEC_HOME", self.home)
            .env_remove("FENNEC_NO_DAEMON")
            .env("FENNEC_DAEMONS", daemons_of(self.home))
            .output();
    }
}

/// Cargo, to build a runtime library from inside a test as it's built outside one. `cargo test`
/// gives a test the variables it gives the package it builds, `CARGO_MANIFEST_DIR` among them, and
/// a crate whose code reads one is built again whenever its value changes. Inherited, it had
/// `ring` and all that's built on it, `rustls` and the runtime among them, built again by every
/// test run, for the plain, threads and audited runtimes in turn, and the test binary built again
/// by the `cargo test` after it: on CI, two to three minutes that every test starting first waited.
fn cargo() -> Command {
    const SET_FOR_A_PACKAGE: [&str; 7] = [
        "CARGO_MANIFEST_DIR",
        "CARGO_MANIFEST_PATH",
        "CARGO_CRATE_NAME",
        "CARGO_BIN_NAME",
        "CARGO_TARGET_TMPDIR",
        "CARGO_PRIMARY_PACKAGE",
        "OUT_DIR",
    ];
    let mut cargo = Command::new(env!("CARGO"));
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        if SET_FOR_A_PACKAGE.contains(&key)
            || key.starts_with("CARGO_PKG_")
            || key.starts_with("CARGO_BIN_EXE_")
        {
            cargo.env_remove(key);
        }
    }
    cargo
}

/// `fennec_command()` with the toolchain's store turned off, for a test of what a home's own cache
/// does, empty or not: the store, which every home reads std's parse and the runtime's stamp from,
/// would otherwise do that work for it. The tests of the cache, its cleanup, a cache that can't be
/// written and objects lost before a link use it.
pub(crate) fn fennec_command_with_empty_store() -> Command {
    let mut command = fennec_command();
    command.env("FENNEC_TOOLCHAIN_STORE", "");
    command
}

/// The runtime library executables link against, which `fennec build` expects next to
/// itself. `cargo test` doesn't build it, so this does, once a run: `fennec` refuses one from
/// other source. Only once, and not at all when it's this build's already (`current`), because
/// even when it's current Cargo puts the library in place again, removing it first, so a test
/// linking while another builds it could find it missing. `llvm.rs` uses this too.
pub(crate) fn runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    *BUILT.get_or_init(|| {
        let library = Path::new(env!("CARGO_BIN_EXE_fennec")).with_file_name(runtime_file());
        if current(&library) {
            threads_up_to_date();
            return true;
        }
        // One process at a time builds it, as nextest runs a process for each test: the first
        // builds it, and each one after finds it current and leaves it, rather than putting it in
        // place again under another test linking it. Held until this one's built it.
        let building =
            fs::OpenOptions::new().create(true).append(true).open(library.with_extension("a.building"));
        let _held = building.and_then(|file| file.lock().map(|()| file));
        if current(&library) {
            threads_up_to_date();
            return true;
        }
        let profile = if cfg!(debug_assertions) { None } else { Some("--release") };
        let mut build = cargo();
        build.args(["build", "--quiet", "-p", "fennec-runtime"]).args(profile);
        // Coverage builds `fennec` in a target folder of its own, so the library goes there too.
        if under_coverage()
            && let Some(target) = Path::new(env!("CARGO_BIN_EXE_fennec")).parent().and_then(Path::parent)
        {
            build.arg("--target-dir").arg(target).env_remove("RUSTC_WRAPPER");
        }
        let plain = build.status().is_ok_and(|status| status.success());
        threads_up_to_date();
        plain
    })
}

/// Brings the threads library beside `fennec` up to date, or takes it away when it can't be. A
/// program that uses `parallel` links it whenever there's one, so one left there by a build from
/// other source would be taken by any test, not only the ones that ask for it, and these programs
/// link the plain library instead.
fn threads_up_to_date() {
    if !threads_runtime_built()
        && let Some(beside) = Path::new(env!("CARGO_BIN_EXE_fennec")).parent()
    {
        let _ = fs::remove_file(beside.join("libfennec_runtime_threads.a"));
    }
}

/// Whether the runtime library at `path` is from this build of Fennec, by the build its bytes are
/// stamped with, as `fennec` reads it before it links one. Then it needn't be built again. Under
/// nextest, which mutation testing runs the tests with, each test is a process of its own, so the
/// `OnceLock` above built the library once for each test, and some of 639 LLVM tests failed,
/// finding it missing as another put it in place again. What a library's stamp says is kept beside
/// it, by its size and when it changed, so each process needn't read all of it again.
fn current(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else { return false };
    let changed = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    let seen = format!("{} {changed}", metadata.len());
    let kept = path.with_extension("a.stamp");
    if let Ok(text) = fs::read_to_string(&kept)
        && let Some((was, build)) = text.split_once('\n')
        && was == seen
    {
        return build == env!("FENNEC_BUILD");
    }
    let Some(build) = fs::read(path).ok().as_deref().and_then(stamp_in) else { return false };
    // Written under another name and renamed, so another process reads the old note or the new one.
    let writing = path.with_extension(format!("a.stamp.{}", std::process::id()));
    if fs::write(&writing, format!("{seen}\n{build}")).is_ok() {
        let _ = fs::rename(&writing, &kept);
    }
    build == env!("FENNEC_BUILD")
}

/// The build a runtime library's bytes are stamped with: the 64 hex digits after its marker, as
/// `aot.rs`'s `stamp_in` reads it.
fn stamp_in(bytes: &[u8]) -> Option<String> {
    let marker = ["fennec-runtime-", "build:"].concat().into_bytes();
    let at = bytes.windows(marker.len()).position(|window| window == marker.as_slice())? + marker.len();
    let hex = std::str::from_utf8(bytes.get(at..at + 64)?).ok()?;
    hex.bytes().all(|digit| digit.is_ascii_hexdigit()).then(|| hex.to_owned())
}

/// Whether coverage is being measured. What it adds to the code it builds isn't for a runtime
/// library, which programs link with nothing to write the counts.
fn under_coverage() -> bool {
    std::env::var_os("CARGO_LLVM_COV").is_some()
}

/// The runtime library's file name, as Cargo names a static library on every platform Fennec
/// builds for, Windows's GNU target among them.
pub(crate) const fn runtime_file() -> &'static str {
    "libfennec_runtime.a"
}

/// Whether the runtime library is beside the `fennec` these tests run, which is where it's looked
/// for. Under coverage, `fennec` is built somewhere else, without one.
pub(crate) fn runtime_beside_fennec() -> bool {
    Path::new(env!("CARGO_BIN_EXE_fennec")).with_file_name(runtime_file()).is_file()
}

/// The runtime library with `threads`, which `fennec build` takes for a program that uses
/// `parallel` when `FENNEC_THREADS_RUNTIME=1` asks, next to `fennec` as
/// `libfennec_runtime_threads.a`. It's built in a folder of its own, since Cargo puts the same
/// file name in place for both, once a run as `runtime_built` does.
///
/// Where `FENNEC_EXPECT_LLVM` is set, one that doesn't build fails the test, rather than letting
/// it pass by returning early (fennec#2557).
pub(crate) fn threads_runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    let built = *BUILT.get_or_init(|| runtime_variant_built("threads", "threads", "fennec_runtime_threads"));
    assert!(
        built || !crate::llvm::llvm_expected(),
        "FENNEC_EXPECT_LLVM is set, but the runtime library with threads didn't build"
    );
    built
}

/// Builds both runtime libraries with the heap audit in them, next to `fennec`, as
/// `libfennec_runtime_audit.a` and `libfennec_runtime_threads_audit.a`: what a program built under
/// `FENNEC_AUDIT=1` links. Each is built in the folder the gate and CI build it in before the
/// tests, so here it's found built.
///
/// Where `FENNEC_EXPECT_LLVM` is set, one that doesn't build fails the test, rather than letting
/// it pass by returning early (fennec#2557).
pub(crate) fn audited_runtimes_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    let built = *BUILT.get_or_init(|| {
        runtime_variant_built("audit", "audit", "fennec_runtime_audit")
            && runtime_variant_built("threads,audit", "threads-audit", "fennec_runtime_threads_audit")
    });
    assert!(
        built || !crate::llvm::llvm_expected(),
        "FENNEC_EXPECT_LLVM is set, but the runtime libraries with the heap audit didn't build"
    );
    built
}

/// Builds the runtime library with `features`, in a folder of its own named `folder`, beside the
/// plain build's `target/debug`, and puts it next to `fennec` as `name`.
fn runtime_variant_built(features: &str, folder: &str, name: &str) -> bool {
    let Some(beside) = Path::new(env!("CARGO_BIN_EXE_fennec")).parent() else { return false };
    // As `runtime_built` takes the plain library: built again only when it isn't this build's.
    if current(&beside.join(format!("lib{name}.a"))) {
        return true;
    }
    let Some(own) = beside.parent().map(|target| target.join(folder)) else { return false };
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let mut cargo = cargo();
    cargo
        .args(["build", "--quiet", "-p", "fennec-runtime", "--features", features, "--target-dir"])
        .arg(&own)
        .args((profile == "release").then_some("--release"));
    if under_coverage() {
        cargo.env_remove("RUSTC_WRAPPER");
    }
    let built = cargo.status().is_ok_and(|status| status.success());
    let library = own.join(profile).join(runtime_file());
    let file = format!("lib{name}.a");
    // Copied under another name and then renamed, so a build linking it at that moment, in this
    // test run or another, finds the old library or the new one and never half of one.
    let copying = beside.join(format!("{file}.{}.copying", std::process::id()));
    built && fs::copy(library, &copying).is_ok() && fs::rename(&copying, beside.join(file)).is_ok()
}

/// Whether mutation testing is running the tests (CI's mutation jobs set
/// `FENNEC_UNDER_MUTANTS`). Building and linking every example takes longer than it's worth for
/// each mutant, so these two tests stand aside there: every other run of the tests runs
/// them, and the LLVM tests run under mutants too.
fn under_mutants() -> bool {
    std::env::var_os("FENNEC_UNDER_MUTANTS").is_some()
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn programs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Builds a program into `work`, giving back the executable, or what `fennec build` said
/// when it doesn't build.
fn build(path: &Path, work: &Path) -> std::io::Result<Result<PathBuf, Output>> {
    let executable = work.join(path.file_stem().unwrap_or_default());
    let built =
        fennec_command().arg("build").arg(path).arg("-o").arg(&executable).output_before(FENNEC_DEADLINE)?;
    Ok(if built.status.success() { Ok(executable) } else { Err(built) })
}

fn run(executable: &Path) -> std::io::Result<Output> {
    Command::new(executable).output_within()
}

/// How many builds the examples' tests run at once: one per core, up to 8. One at a time, LLVM's
/// took 71 seconds and 8 take 29; holding under 100 megabytes each, they stay well inside 4
/// gigabytes. `llvm.rs` uses this too.
fn builds_at_once() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(8)
}

/// Runs `each` on every item, `builds_at_once()` at a time, each worker with its own number for a
/// folder of its own. The results come back in the items' order.
pub(crate) fn at_once<T: Sync, R: Send>(
    items: &[T],
    each: impl Fn(usize, &T) -> std::io::Result<R> + Sync,
) -> std::io::Result<Vec<R>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..builds_at_once().min(items.len()))
            .map(|worker| {
                let (next, done, each) = (&next, &done, &each);
                scope.spawn(move || -> std::io::Result<()> {
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(at) else { return Ok(()) };
                        let result = each(worker, item)?;
                        done.lock()
                            .map_err(|_| std::io::Error::other("a worker panicked"))?
                            .push((at, result));
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().map_err(|_| std::io::Error::other("a worker panicked"))??;
        }
        Ok::<_, std::io::Error>(())
    })?;
    let mut done = done.into_inner().map_err(|_| std::io::Error::other("a worker panicked"))?;
    done.sort_by_key(|(at, _)| *at);
    Ok(done.into_iter().map(|(_, result)| result).collect())
}

/// A fresh folder for this test's executables.
fn work_dir(test: &str) -> std::io::Result<fennec_temp::Temp> {
    let work = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-executables-{test}-{}", std::process::id())),
    );
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    Ok(work)
}

/// Error examples whose native program fails otherwise than its `.err` says, with what differs.
/// Each must still differ, so the list only shrinks. The first is a check native code doesn't
/// make yet: it runs on, where the `.err` stops.
const FAILS_DIFFERENTLY: &[(&str, &str)] = &[
    ("expect_without_reason.fen", "native code doesn't check `expect`'s reason isn't blank, and runs on"),
    ("change_missing_key.fen", "native code adds a help line"),
    ("where_on_copy.fen", "native code leaves out the help line"),
    ("where_on_write.fen", "native code leaves out the help line"),
    ("where_rule.fen", "native code leaves out the help line"),
    ("where_through_an_item.fen", "native code leaves out the help line"),
];

#[test]
#[cfg_attr(windows, ignore = "on Windows, `llvm.rs` builds and runs the examples")]
fn failing_examples_fail_the_same_way_as_executables() {
    // `fennec build` is LLVM's, so this runs where LLVM's tests do.
    if under_mutants() || !crate::llvm::ready() {
        return;
    }
    assert!(runtime_built(), "couldn't build the runtime library");
    let work = work_dir("errors").unwrap();
    let paths = programs(&examples_dir().join("errors")).unwrap();
    // For each that built, what was wrong with it, if anything.
    let found = at_once(&paths, |_, path| {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        // Most error examples are mistakes the checker catches, which don't build.
        let Ok(executable) = build(path, &work)? else { return Ok(None) };
        let ran = run(&executable)?;
        // An executable's failure is held to its `.err`, with the path it was built from.
        let theirs = fs::read_to_string(path.with_extension("err"))?
            .replace(&format!("examples/errors/{name}"), &path.display().to_string());
        let ours = String::from_utf8_lossy(&ran.stderr);
        let differs = if ran.status.success() {
            Some(format!("{name} should fail"))
        } else if ours != theirs {
            Some(format!("{name} failed differently:\n{ours}\nwhere its `.err` says\n{theirs}"))
        } else {
            None
        };
        let known = FAILS_DIFFERENTLY.iter().any(|(differing, _)| *differing == name);
        Ok(Some(match differs {
            Some(_) if known => None,
            None if known => {
                Some(format!("{name} fails as its `.err` says now: take it off `FAILS_DIFFERENTLY`"))
            }
            differs => differs,
        }))
    })
    .unwrap();
    let built: Vec<Option<String>> = found.into_iter().flatten().collect();
    assert!(!built.is_empty(), "no error example built");
    let wrong: Vec<String> = built.into_iter().flatten().collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
    let _ = fs::remove_dir_all(&work);
}
