//! Watching a worker process: it runs until something goes wrong or time runs out, and says
//! which input it's on, so a failure can be traced to that input and the next worker can start
//! after it.

use std::fmt;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::targets::Target;

/// How a target says an input broke a rule it checks.
pub(crate) const WRONG: u8 = 3;
/// How a run says a panic ended it.
pub(crate) const PANICKED: u8 = 4;

/// How much CPU time one input may take before it counts as stuck, unless its target says
/// otherwise. It's CPU time, not time on the clock, so a busy machine doesn't make an input stuck.
pub(crate) const STUCK: Duration = Duration::from_secs(10);
/// How many times its own limit an input may take when the fuzzer is built with `AddressSanitizer`:
/// an allowance for the sanitizer's slowdown, not a raised limit. Built without one, it's 1.
///
/// Measured in Fennec's Linux VM, 400 ordinary inputs of each target took 4 to 27 times the CPU
/// time under `AddressSanitizer` that they took in a build with the checked job's flags. The three
/// inputs an hour of it called stuck, a formatter input nesting 20,000 deep among them, finished
/// alone in 4.1 to 12.3 s, where a plain build takes 0.1 to 0.6 s. Ten times leaves those 8 times
/// room, and still stops a real hang at 100 s, inside a target's 240-second run.
#[cfg(address_sanitizer)]
pub(crate) const SANITIZER_ALLOWANCE: u32 = 10;
/// See the `AddressSanitizer` build's.
#[cfg(not(address_sanitizer))]
pub(crate) const SANITIZER_ALLOWANCE: u32 = 1;
/// How many times its CPU time a process may go on the clock before it counts as stuck anyway:
/// one waiting on something forever uses no CPU time at all.
const WAITING_TIMES: u32 = 30;
/// The most memory a worker may hold.
pub(crate) const MOST_KILOBYTES: u64 = 4 * 1024 * 1024;
/// How much CPU time building a program may take, the tools `fennec build` starts included:
/// `BUILD_STUCK`, and `BUILD_STUCK_PER_LINE` for each of its lines. A short generated program
/// builds in a few seconds; one `opt` once ran 18 hours on one.
///
/// A long one takes far more. One of 1,014 lines (fennec#2554) took 64 to 83 s of CPU time to build
/// in Fennec's Linux VM, plain or under a thread sanitizer, and up to 181 s with the Mac around it
/// busy: a VM's processor waiting for the Mac's counts as CPU time there. That's 0.18 s a line at
/// worst, and the budget gives it more than twice that, so only a runaway build passes it, in
/// minutes rather than hours.
pub(crate) fn build_stuck(lines: usize) -> Duration {
    BUILD_STUCK.saturating_add(BUILD_STUCK_PER_LINE.saturating_mul(u32::try_from(lines).unwrap_or(u32::MAX)))
}
const BUILD_STUCK: Duration = Duration::from_secs(120);
const BUILD_STUCK_PER_LINE: Duration = Duration::from_millis(400);

/// How a worker went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A rule the target checks was broken.
    Wrong,
    Panicked,
    /// The process died another way, like a stack overflow.
    Crashed,
    Stuck,
    TooMuchMemory,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Failure::Wrong => "broke a rule",
            Failure::Panicked => "panicked",
            Failure::Crashed => "crashed",
            Failure::Stuck => "got stuck",
            Failure::TooMuchMemory => "used too much memory",
        })
    }
}

/// Where a worker notes which input it's on: a number written over itself, so it costs almost
/// nothing and is still there if the process dies.
pub(crate) struct Progress(File);

impl Progress {
    pub(crate) fn create(path: &Path) -> Result<Self, String> {
        File::create(path).map(Progress).map_err(|err| format!("{}: {err}", path.display()))
    }

    pub(crate) fn note(&mut self, number: u64) {
        let _ = self.0.rewind();
        let _ = self.0.write_all(&number.to_le_bytes());
    }
}

/// The input a worker last noted, if it got that far.
fn noted(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    Some(u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?))
}

/// What a process's exit says about how it went, or None if it was asked to stop or succeeded.
pub(crate) fn failure_of(status: std::process::ExitStatus) -> Option<Failure> {
    match status.code() {
        Some(0) => None,
        Some(code) if code == i32::from(WRONG) => Some(Failure::Wrong),
        Some(code) if code == i32::from(PANICKED) => Some(Failure::Panicked),
        _ => Some(Failure::Crashed),
    }
}

/// How much memory a process holds, where that can be asked.
pub(crate) fn kilobytes(id: u32) -> Option<u64> {
    let output = Command::new("ps").args(["-o", "rss=", "-p", &id.to_string()]).output().ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// The descriptors a process holds open, each as its number and what it is, where that can be
/// asked: from `/proc` on Linux and from `lsof` on macOS.
pub(crate) fn descriptors(id: u32) -> Option<Vec<String>> {
    if cfg!(target_os = "linux") {
        let listed = std::fs::read_dir(format!("/proc/{id}/fd")).ok()?;
        return Some(
            listed
                .flatten()
                .map(|entry| {
                    let what = std::fs::read_link(entry.path()).map(|to| to.display().to_string());
                    format!("{} {}", entry.file_name().to_string_lossy(), what.unwrap_or_default())
                })
                .collect(),
        );
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output =
        Command::new("lsof").args(["-n", "-P", "-a", "-p", &id.to_string(), "-F", "ftn"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    // `lsof` says each descriptor as lines of its own: `f` and its number, `t` and its type, `n`
    // and its name. It lists the working folder and the executable as `fcwd` and `ftxt`, which
    // aren't descriptors.
    let mut open: Vec<String> = Vec::new();
    let mut counted = false;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(fd) = line.strip_prefix('f') {
            counted = fd.parse::<u32>().is_ok();
            if counted {
                open.push(fd.to_owned());
            }
        } else if counted && let Some(last) = open.last_mut() {
            last.push(' ');
            last.push_str(line.get(1..).unwrap_or_default());
        }
    }
    Some(open)
}

/// What a process group holds and has used, summed over every process in it, where that can be
/// asked.
fn group_use(group: u32) -> Option<(u64, Duration)> {
    let output = Command::new("ps").args(["-A", "-o", "pgid=,rss=,time="]).output().ok()?;
    let mut sum = (0, Duration::ZERO);
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        if fields.next().and_then(|pgid| pgid.parse::<u32>().ok()) != Some(group) {
            continue;
        }
        sum.0 += fields.next().and_then(|rss| rss.parse::<u64>().ok()).unwrap_or(0);
        sum.1 += fields.next().and_then(cpu_time_in).unwrap_or_default();
    }
    Some(sum)
}

/// Every process started from `id`, however deep, with its process group: what's left running
/// when `id` is killed unless they're killed too.
fn descendants(id: u32) -> Vec<(u32, u32)> {
    let Ok(output) = Command::new("ps").args(["-A", "-o", "pid=,ppid=,pgid="]).output() else {
        return Vec::new();
    };
    let listed: Vec<(u32, u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(|field| field.parse::<u32>().ok());
            Some((fields.next()??, fields.next()??, fields.next()??))
        })
        .collect();
    let mut found: Vec<(u32, u32)> = Vec::new();
    let mut parents = vec![id];
    while let Some(parent) = parents.pop() {
        for &(pid, ppid, pgid) in &listed {
            if ppid == parent && !found.iter().any(|&(seen, _)| seen == pid) {
                found.push((pid, pgid));
                parents.push(pid);
            }
        }
    }
    found
}

/// Sends `SIGKILL` to `target`: a process, or a process group written as its negative.
fn kill(target: &str) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Kills `child` and everything it started: the groups any of them lead, as a build's tools are,
/// and on Unix `child`'s own group when it leads one. A tool whose parent died first is still in
/// its group, so it goes too. On Windows, which has no `ps` to find them by, `taskkill` stops every
/// process it started, however deep.
fn stop(child: &mut Child) {
    let id = child.id();
    let below = descendants(id);
    if cfg!(unix) {
        kill(&format!("-{id}"));
    }
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &id.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    for (pid, pgid) in below {
        // Only a group one of them leads, never the group this process is in.
        if pid == pgid {
            kill(&format!("-{pgid}"));
        }
        kill(&pid.to_string());
    }
    let _ = child.wait();
}

/// Runs `command` to the end and gives back what it wrote, as `Command::output` does, but in a
/// process group of its own, so `fennec build` and the `opt`, `llc` and linker it starts are
/// watched together: all of them are stopped once together they hold more than `most_kilobytes`
/// or use `stuck` CPU time, and how it went wrong comes back with what was written by then.
pub(crate) fn output_watched(
    command: &mut Command,
    stuck: Duration,
    most_kilobytes: u64,
) -> Result<(Output, Option<Failure>), String> {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("couldn't run `{}`: {err}", command.get_program().display()))?;
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>));
    let stderr = read(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>));
    // How it ended is the status's to say: only being stopped is a failure here.
    let failure = watch_as(&mut child, Watched::Group, stuck, None, most_kilobytes, || false)
        .filter(|failure| matches!(failure, Failure::Stuck | Failure::TooMuchMemory));
    let status = child
        .wait()
        .map_err(|err| format!("couldn't wait for `{}`: {err}", command.get_program().display()))?;
    let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    Ok((Output { status, stdout, stderr }, failure))
}

/// How much CPU time a process has used, where that can be asked.
pub(crate) fn cpu_time(id: u32) -> Option<Duration> {
    let output = Command::new("ps").args(["-o", "time=", "-p", &id.to_string()]).output().ok()?;
    cpu_time_in(String::from_utf8_lossy(&output.stdout).trim())
}

/// The CPU time `ps` writes as `[days-][hours:]minutes:seconds`, the seconds with or without a
/// fraction: `0:01.25` on macOS, `00:00:01` on Linux.
fn cpu_time_in(written: &str) -> Option<Duration> {
    let (days, clock) = match written.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, written),
    };
    let mut parts = clock.rsplit(':');
    let seconds = Duration::try_from_secs_f64(parts.next()?.parse().ok()?).ok()?;
    let mut whole = 0_u64;
    for (part, scale) in parts.zip([60, 3600]) {
        whole += part.parse::<u64>().ok()? * scale;
    }
    Some(seconds + Duration::from_secs(whole + days * 86_400))
}

/// Waits for a process, stopping it if it uses `stuck` CPU time without `moved` saying it's
/// making progress, or if it holds too much memory. Where CPU time can't be asked, `stuck` is
/// time on the clock. Reaching `deadline` stops it too, which isn't a failure. Gives back how it
/// went wrong, if it did.
pub(crate) fn watch(
    child: &mut Child,
    stuck: Duration,
    deadline: Option<Instant>,
    most_kilobytes: u64,
    moved: impl FnMut() -> bool,
) -> Option<Failure> {
    watch_as(child, Watched::Process, stuck, deadline, most_kilobytes, moved)
}

/// What `watch_as` counts memory and CPU time over.
#[derive(Clone, Copy)]
enum Watched {
    /// The process alone.
    Process,
    /// The process group it leads, every process in it summed, where groups can be asked about:
    /// on Windows, every process it started, however deep.
    Group,
}

/// Whether asking what a process has used costs little, so it's asked every tenth check: `ps`
/// on Unix. Windows' list of processes takes PowerShell about half a second to give, so there
/// it's asked only when the process could have used `stuck` CPU time since it was last asked.
const ASKED_OFTEN: bool = cfg!(unix);

/// How recently CPU time must have been asked to stand for what a process had used at a moment.
const JUST_NOW: Duration = Duration::from_millis(200);

/// What `watch_as` knows of the CPU time a process has used.
struct Cpu {
    /// What it had used when last asked, and when that was.
    known: Option<(Instant, Duration)>,
    /// What it had used when it last made progress, where that's known. Where CPU time isn't
    /// asked often, it's what was first asked after the progress, so what it used between the
    /// two goes uncounted, and a process counts as stuck later rather than sooner.
    base: Option<Duration>,
    /// How many cores could have run it at once.
    cores: u32,
}

impl Cpu {
    /// Asks what the process holds and has used, keeping the CPU time, and says whether it holds
    /// more than `most_kilobytes`.
    fn ask(&mut self, id: u32, watched: Watched, most_kilobytes: u64) -> bool {
        let (held, used) = measured(id, watched);
        if let Some(used) = used {
            self.known = Some((Instant::now(), used));
            self.base.get_or_insert(used);
        }
        held.is_some_and(|held| held > most_kilobytes)
    }

    /// The CPU time it's used since it last made progress, as last asked.
    fn busy(&self) -> Option<Duration> {
        let ((_, used), base) = self.known.zip(self.base)?;
        Some(used.saturating_sub(base))
    }

    /// The most CPU time it could have used since it last made progress, `on_the_clock` ago: what
    /// was last asked, and every core busy with it since.
    fn could_have_used(&self, on_the_clock: Duration) -> Duration {
        match (self.known, self.busy()) {
            (Some((at, _)), Some(busy)) => busy.saturating_add(at.elapsed().saturating_mul(self.cores)),
            _ => on_the_clock.saturating_mul(self.cores),
        }
    }
}

/// `watch`, counting over what `watched` says. Stopping it stops what it started too.
fn watch_as(
    child: &mut Child,
    watched: Watched,
    stuck: Duration,
    deadline: Option<Instant>,
    most_kilobytes: u64,
    mut moved: impl FnMut() -> bool,
) -> Option<Failure> {
    // When it last made progress.
    let mut since = Instant::now();
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let mut cpu =
        Cpu { known: None, base: Some(Duration::ZERO), cores: u32::try_from(cores).unwrap_or(u32::MAX) };
    let mut checks = 0_u32;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return failure_of(status);
        }
        if moved() {
            since = Instant::now();
            cpu.base = cpu.known.filter(|(at, _)| at.elapsed() <= JUST_NOW).map(|(_, used)| used);
        }
        checks += 1;
        let mut too_big =
            ASKED_OFTEN && checks.is_multiple_of(10) && cpu.ask(child.id(), watched, most_kilobytes);
        let on_the_clock = since.elapsed();
        let mut is_stuck = on_the_clock > stuck * WAITING_TIMES;
        // Only what it's used, asked just now, makes it stuck: a busy machine runs up the time on
        // the clock without the process doing anything. Where CPU time can't be asked at all, the
        // clock stands in for it.
        if !is_stuck && !too_big && cpu.could_have_used(on_the_clock) > stuck {
            too_big = cpu.ask(child.id(), watched, most_kilobytes);
            is_stuck = cpu.busy().map_or(on_the_clock > stuck, |busy| busy > stuck);
        }
        let out_of_time = deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if too_big || out_of_time || is_stuck {
            stop(child);
            return match (too_big, out_of_time) {
                (true, _) => Some(Failure::TooMuchMemory),
                (false, true) => None,
                (false, false) => Some(Failure::Stuck),
            };
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// What `id` holds and has used, alone or with its group as `watched` says.
fn measured(id: u32, watched: Watched) -> (Option<u64>, Option<Duration>) {
    let both =
        |found: Option<(u64, Duration)>| found.map_or((None, None), |(held, used)| (Some(held), Some(used)));
    if cfg!(windows) {
        return both(windows_use(id, matches!(watched, Watched::Group)));
    }
    match watched {
        Watched::Group if cfg!(unix) => both(group_use(id)),
        _ => (kilobytes(id), cpu_time(id)),
    }
}

/// What PowerShell lists for each process on Windows, a line each: its id, its parent's, when it
/// started, the CPU time it's used in user and kernel mode, in 100-nanosecond units, and the
/// memory it holds, in bytes. A process with no start time, as Windows' own have, says 0.
const WINDOWS_PROCESSES: &str = "Get-CimInstance Win32_Process | ForEach-Object { '{0} {1} {2} {3} {4} {5}' -f \
     $_.ProcessId, $_.ParentProcessId, $(if ($_.CreationDate) { $_.CreationDate.ToFileTimeUtc() } else { 0 }), \
     $_.UserModeTime, $_.KernelModeTime, $_.WorkingSetSize }";

/// What `id` holds and has used on Windows, with every process it started, however deep, where
/// `tree` says. The standard library can't ask, and asking Windows itself would be new `unsafe`,
/// so it's asked of Windows' list of processes through PowerShell.
fn windows_use(id: u32, tree: bool) -> Option<(u64, Duration)> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", WINDOWS_PROCESSES])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    tree_use(&String::from_utf8_lossy(&output.stdout), id, tree)
}

/// What `listed`, in `WINDOWS_PROCESSES`'s lines, says `id` holds and has used, in kilobytes and
/// CPU time, with every process it started, however deep, where `tree` says. A process counts as
/// one `id` started only if it started after it: Windows reuses a process's id once it's gone, so
/// a parent's id alone may name another process.
fn tree_use(listed: &str, id: u32, tree: bool) -> Option<(u64, Duration)> {
    let processes: Vec<[u64; 6]> = listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(|field| field.parse::<u64>().ok());
            let mut row = [0; 6];
            for slot in &mut row {
                *slot = fields.next()??;
            }
            Some(row)
        })
        .collect();
    let first = processes.iter().find(|[pid, ..]| *pid == u64::from(id))?;
    let mut counted = vec![*first];
    let mut at = 0;
    while tree && let Some(&[parent, _, started, ..]) = counted.get(at) {
        at += 1;
        for row in &processes {
            let [child, its_parent, born, ..] = *row;
            if its_parent == parent
                && born >= started
                && child != parent
                && !counted.iter().any(|seen| seen[0] == child)
            {
                counted.push(*row);
            }
        }
    }
    let (ticks, bytes) = counted.iter().fold((0_u64, 0_u64), |(ticks, bytes), [.., user, kernel, held]| {
        (ticks.saturating_add(*user).saturating_add(*kernel), bytes.saturating_add(*held))
    });
    Some((bytes / 1024, Duration::from_nanos(ticks.saturating_mul(100))))
}

/// What fuzzing one target found.
pub(crate) struct Outcome {
    pub(crate) inputs: u64,
    /// Each kind of failure once, with every input that failed that way, in order: the first is
    /// the one cut down and kept, and the rest say how widespread it is, where a list of firsts
    /// alone left a second failure of a kind unseen.
    pub(crate) failures: Vec<Failed>,
}

/// One kind of failure, and the inputs that failed that way.
pub(crate) struct Failed {
    pub(crate) failure: Failure,
    pub(crate) inputs: Vec<u64>,
    /// What the first `SAID_KEPT` of them wrote to standard error, as `said_in` keeps it, in the
    /// same order: why each failed, where a run once found 48 and said nothing of why.
    pub(crate) said: Vec<Vec<u8>>,
}

/// How many inputs of each kind of failure have what they wrote to standard error kept.
pub(crate) const SAID_KEPT: usize = 8;

impl Outcome {
    /// Counts `input` as failing as `failure`, having written `said` to standard error.
    pub(crate) fn note(&mut self, failure: Failure, input: u64, said: Vec<u8>) {
        match self.failures.iter_mut().find(|failed| failed.failure == failure) {
            Some(failed) => {
                failed.inputs.push(input);
                if failed.said.len() < SAID_KEPT {
                    failed.said.push(said);
                }
            }
            None => self.failures.push(Failed { failure, inputs: vec![input], said: vec![said] }),
        }
    }
}

/// The most of what a failing input wrote to standard error that's kept.
pub(crate) const MOST_SAID: u64 = 64 * 1024;

/// What `path` holds from `from` on, at most `MOST_SAID` bytes of it: all of it if it fits, or its
/// first and last halves, with a line between saying how much was left out, since why an input
/// failed may come first, as a sanitizer's report does, or last, as a panic's message does. A
/// file that can't be read gives nothing.
pub(crate) fn said_in(path: &Path, from: u64) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let end = file.metadata().map_or(0, |metadata| metadata.len());
    let from = from.min(end);
    let mut read = |at: u64, length: u64| {
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(at)).is_ok() {
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(&mut file, length), &mut bytes);
        }
        bytes
    };
    if end - from <= MOST_SAID {
        return read(from, end - from);
    }
    let half = MOST_SAID / 2;
    let mut said = read(from, half);
    said.extend_from_slice(format!("\n[... {} bytes left out ...]\n", end - from - 2 * half).as_bytes());
    said.extend(read(end - half, half));
    said
}

/// A target being fuzzed.
pub(crate) struct Run<'r> {
    pub(crate) fennec_fuzz: &'r Path,
    pub(crate) target: &'r Target,
    pub(crate) seed: u64,
    /// How long one input may take before it counts as stuck.
    pub(crate) stuck: Duration,
}

impl Run<'_> {
    /// Runs workers until the time is up or `inputs` have run, each starting after the input
    /// that ended the last.
    pub(crate) fn until(&self, seconds: Option<u64>, inputs: u64) -> Result<Outcome, String> {
        let deadline = seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
        // A folder of each run's own: two runs in one process, as tests are, would otherwise share
        // one, and the first to end would take the other's progress with it.
        static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("fennec-fuzz-{}-{run}", std::process::id()));
        crate::owned::own(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        let progress = folder.join(self.target.name);
        let said = folder.join(format!("{}.stderr", self.target.name));
        let mut outcome = Outcome { inputs: 0, failures: Vec::new() };
        let mut next = 0_u64;
        while next < inputs && deadline.is_none_or(|deadline| Instant::now() < deadline) {
            let _ = std::fs::remove_file(&progress);
            let mut child = self.worker(next, inputs, &progress, &said)?;
            let mut heard = Heard::default();
            let failure =
                watch(&mut child, self.stuck, deadline, MOST_KILOBYTES, || heard.check(&progress, &said));
            // A stopped worker can't remove its own.
            crate::llvm::forget(child.id());
            // The worker notes an input as it starts it, and `inputs` once it's run them all. So
            // only inputs before the one it last noted are done, and the one it failed on: one it
            // was stopped in at the deadline isn't counted, nor one it never got to.
            let reached = noted(&progress).unwrap_or(next);
            let Some(failure) = failure else {
                outcome.inputs = reached;
                break;
            };
            outcome.inputs = reached + 1;
            outcome.note(failure, reached, said_in(&said, heard.from(Some(reached))));
            next = reached + 1;
        }
        crate::owned::let_go(&folder);
        Ok(outcome)
    }

    /// Starts a worker on the inputs from `first` up to `end`, noting each in `progress` and
    /// writing to standard error in `said`, which it starts empty.
    fn worker(&self, first: u64, end: u64, progress: &Path, said: &Path) -> Result<Child, String> {
        let said = File::create(said).map_err(|err| format!("{}: {err}", said.display()))?;
        Command::new(self.fennec_fuzz)
            .args(["worker", self.target.name, &self.seed.to_string(), &first.to_string(), &end.to_string()])
            .arg(progress)
            .stdout(Stdio::null())
            .stderr(said)
            .spawn()
            .map_err(|err| format!("couldn't start a worker: {err}"))
    }
}

/// Where in what a worker writes to standard error each input's part starts, as far as it's seen
/// while watching it: how long the file was before the input was noted, so the part starts no
/// later than that. It may start with what inputs before it wrote, but never misses any of its own.
#[derive(Default)]
struct Heard {
    /// The input last noted, and how long the file was just before that was read.
    last: Option<(Option<u64>, u64)>,
    /// How long the file was before the input last noted was, as far as was seen.
    from: u64,
}

impl Heard {
    /// Reads how long `said` is and then which input `progress` notes, and says whether that's
    /// moved since the last check. Read in that order, the length is from before any input after
    /// the one noted had started.
    fn check(&mut self, progress: &Path, said: &Path) -> bool {
        let length = std::fs::metadata(said).map_or(0, |metadata| metadata.len());
        let now = noted(progress);
        let moved = self.last.is_none_or(|(last, _)| last != now);
        if moved {
            self.from = self.last.map_or(0, |(_, before)| before);
        }
        self.last = Some((now, length));
        moved
    }

    /// Where input `reached`'s part starts: what was seen before it was noted, or if it was never
    /// seen noted, what was seen before the last check.
    fn from(&self, reached: Option<u64>) -> u64 {
        match self.last {
            Some((last, _)) if last == reached => self.from,
            Some((_, before)) => before,
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Duration, Failure, Heard, MOST_SAID, Outcome, Progress, SAID_KEPT, cpu_time_in, said_in, tree_use,
    };

    /// Every input that fails is counted under its kind, in order, the first kept first: a second
    /// failure of a kind once went unseen, as only the first of each was kept. What each wrote to
    /// standard error is kept with it, for the first few of a kind.
    #[test]
    fn every_failing_input_is_counted_under_its_kind() {
        let mut outcome = Outcome { inputs: 0, failures: Vec::new() };
        let mut failing =
            vec![(Failure::Wrong, 2), (Failure::Crashed, 4), (Failure::Wrong, 5), (Failure::Wrong, 9)];
        failing.extend((20..30).map(|input| (Failure::Wrong, input)));
        for (failure, input) in failing {
            outcome.note(failure, input, format!("why {input} failed").into_bytes());
        }
        let found: Vec<(Failure, Vec<u64>)> =
            outcome.failures.iter().map(|failed| (failed.failure, failed.inputs.clone())).collect();
        let wrong: Vec<u64> = [2, 5, 9].into_iter().chain(20..30).collect();
        assert_eq!(found, [(Failure::Wrong, wrong.clone()), (Failure::Crashed, vec![4])]);
        let said: Vec<Vec<u8>> =
            wrong.iter().take(SAID_KEPT).map(|input| format!("why {input} failed").into_bytes()).collect();
        assert_eq!(outcome.failures[0].said, said);
        assert_eq!(outcome.failures[1].said, [b"why 4 failed".to_vec()]);
    }

    /// What an input wrote to standard error is kept from where its part starts, all of it if it's
    /// short, and its first and last halves if it's long, as a report can be first or last.
    #[test]
    fn what_was_said_is_kept_from_its_start_first_and_last() {
        let dir = std::env::temp_dir().join(format!("fennec-fuzz-said-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("said");
        std::fs::write(&file, b"before\nwhy it failed\n").unwrap();
        assert_eq!(said_in(&file, 7), b"why it failed\n");
        assert_eq!(said_in(&file, 0), b"before\nwhy it failed\n");
        assert_eq!(said_in(&file, 1000), b"");
        assert_eq!(said_in(&dir.join("none"), 0), b"");
        let long = usize::try_from(MOST_SAID).unwrap() * 3;
        let mut written = b"the first line\n".to_vec();
        written.resize(long, b'.');
        written.extend_from_slice(b"\nthe last line\n");
        std::fs::write(&file, &written).unwrap();
        let said = said_in(&file, 0);
        let left_out = written.len() - usize::try_from(MOST_SAID).unwrap();
        let shown = String::from_utf8_lossy(&said);
        assert!(shown.starts_with("the first line\n"), "{:?}", shown.lines().next());
        assert!(shown.ends_with("\nthe last line\n"));
        assert!(shown.contains(&format!("[... {left_out} bytes left out ...]")));
        assert!(said.len() < usize::try_from(MOST_SAID).unwrap() + 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An input's part of what a worker writes starts no later than how long it was before the
    /// input was noted, as seen while watching: the part may hold what came before, never less.
    #[test]
    fn an_inputs_part_of_what_was_said_starts_before_it_was_noted() {
        let dir = std::env::temp_dir().join(format!("fennec-fuzz-heard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (progress, said) = (dir.join("progress"), dir.join("said"));
        let say = |what: &str| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&said).unwrap();
            file.write_all(what.as_bytes()).unwrap();
        };
        let mut heard = Heard::default();
        assert!(heard.check(&progress, &said));
        assert_eq!(heard.from(None), 0);
        say("starting\n");
        let mut noting = Progress::create(&progress).unwrap();
        noting.note(0);
        assert!(heard.check(&progress, &said));
        say("0 runs\n");
        assert!(!heard.check(&progress, &said));
        say("0 is done\n");
        noting.note(1);
        say("1 fails\n");
        assert!(heard.check(&progress, &said));
        // Seen noted: its part starts where the file was before, which holds 0's last line.
        assert_eq!(heard.from(Some(1)), u64::try_from("starting\n0 runs\n".len()).unwrap());
        // Never seen noted, as when it fails at once: where the file was at the last check, which
        // was before 2 was noted.
        noting.note(2);
        say("2 fails at once\n");
        assert_eq!(
            heard.from(Some(2)),
            u64::try_from("starting\n0 runs\n0 is done\n1 fails\n".len()).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A worker's failing input comes back with what it wrote to standard error, which was once
    /// thrown away, so a run found 48 failures and said nothing of why. The worker here is a script
    /// standing in for `fennec-fuzz worker`, noting inputs as it does.
    #[cfg(unix)]
    #[test]
    fn a_failing_input_says_why() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fennec-fuzz-why-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let worker = dir.join("worker");
        let script = "#!/bin/sh\n\
            echo 'input 0 is fine' >&2\n\
            printf '\\000\\000\\000\\000\\000\\000\\000\\000' > \"$6\"\n\
            sleep 0.3\n\
            printf '\\001\\000\\000\\000\\000\\000\\000\\000' > \"$6\"\n\
            echo 'why input 1 failed' >&2\n\
            exit 3\n";
        std::fs::write(&worker, script).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run = super::Run {
            fennec_fuzz: &worker,
            target: &crate::targets::UTF8,
            seed: 1,
            stuck: Duration::from_secs(10),
        };
        let outcome = run.until(None, 2).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(outcome.failures.len(), 1);
        let failed = &outcome.failures[0];
        assert_eq!((failed.failure, failed.inputs.clone()), (Failure::Wrong, vec![1]));
        let said = String::from_utf8_lossy(&failed.said[0]).into_owned();
        assert!(said.ends_with("why input 1 failed\n"), "{said:?}");
    }

    /// Only inputs a worker finished are counted: every one when it notes the end, those before the
    /// one it was stopped in at the deadline, and none when it ends having noted nothing, as a
    /// worker that dies at its start does. Counting the input it last noted, done or not, read a
    /// worker that ran nothing as one input run.
    #[cfg(unix)]
    #[test]
    fn only_inputs_a_worker_finished_are_counted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fennec-fuzz-counted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let note =
            |number: u8| format!("printf '\\{number:03o}\\000\\000\\000\\000\\000\\000\\000' > \"$6\"\n");
        let counted = |name: &str, body: &str, seconds: Option<u64>| {
            let worker = dir.join(name);
            std::fs::write(&worker, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
            let run = super::Run {
                fennec_fuzz: &worker,
                target: &crate::targets::UTF8,
                seed: 1,
                stuck: Duration::from_secs(30),
            };
            let outcome = run.until(seconds, 3).unwrap();
            assert!(outcome.failures.is_empty(), "{name}");
            outcome.inputs
        };
        let all = format!("{}{}{}{}exit 0\n", note(0), note(1), note(2), note(3));
        let stopped = format!("{}{}{}sleep 30\n", note(0), note(1), note(2));
        let counts = (
            counted("all", &all, None),
            counted("stopped", &stopped, Some(1)),
            counted("nothing", "exit 0\n", None),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(counts, (3, 2, 0));
    }

    #[test]
    fn cpu_time_reads_as_ps_writes_it() {
        assert_eq!(cpu_time_in("0:01.25"), Some(Duration::from_millis(1250)));
        assert_eq!(cpu_time_in("00:00:07"), Some(Duration::from_secs(7)));
        assert_eq!(cpu_time_in("01:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(cpu_time_in("2-00:00:01"), Some(Duration::from_secs(2 * 86_400 + 1)));
        assert_eq!(cpu_time_in(""), None);
        assert_eq!(cpu_time_in("NaN"), None);
    }

    /// Windows' list of processes is read as PowerShell writes it, a process counted with every one
    /// it started, however deep, and only those that started after it: one whose parent's id is
    /// reused, or a line that isn't a process, is left out.
    #[test]
    fn a_process_tree_is_read_from_windows_list() {
        let listed = "\
            0 0 0 0 0 8192\n\
            40 4 100 20000000 10000000 2048000\n\
            41 40 110 5000000 0 1024000\n\
            42 41 120 0 2500000 512000\n\
            43 40 90 99000000 0 4096000\n\
            44 7 130 70000000 0 4096000\n\
            not a process\n";
        // 40's own: 3 s and 2,000 kilobytes.
        assert_eq!(tree_use(listed, 40, false), Some((2000, Duration::from_secs(3))));
        // With 41 and 42 below it, but not 43, which started before it, nor 44.
        assert_eq!(tree_use(listed, 40, true), Some((3500, Duration::from_millis(3750))));
        assert_eq!(tree_use(listed, 0, true), Some((8, Duration::ZERO)));
        assert_eq!(tree_use(listed, 99, true), None);
        assert_eq!(tree_use("", 40, true), None);
    }

    /// On Windows, CPU time counts too: a process that only waits isn't stuck however long it
    /// takes on the clock, one that keeps the CPU busy is, and so is a build whose tool does.
    #[cfg(windows)]
    #[test]
    fn stuck_counts_cpu_time_on_windows() {
        use super::{MOST_KILOBYTES, output_watched, watch};
        use std::process::{Command, Stdio};
        let mut waits =
            Command::new("ping").args(["-n", "3", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
        assert_eq!(watch(&mut waits, Duration::from_millis(500), None, MOST_KILOBYTES, || false), None);
        let mut spins =
            Command::new("powershell").args(["-NoProfile", "-Command", "while ($true) {}"]).spawn().unwrap();
        assert_eq!(
            watch(&mut spins, Duration::from_millis(500), None, MOST_KILOBYTES, || false),
            Some(Failure::Stuck)
        );
        // A tool spinning under a build that only waits for it: the build's own CPU time doesn't
        // show it. It spins for 30 seconds at most, should the build be stopped without it.
        let mut builds = Command::new("powershell");
        builds.args([
            "-NoProfile",
            "-Command",
            "Start-Process powershell -NoNewWindow -Wait -ArgumentList '-NoProfile', '-Command', \
             '$end = (Get-Date).AddSeconds(30); while ((Get-Date) -lt $end) {}'",
        ]);
        let (_, failure) = output_watched(&mut builds, Duration::from_secs(2), MOST_KILOBYTES).unwrap();
        assert_eq!(failure, Some(Failure::Stuck));
    }

    /// A process that only waits isn't stuck however long it takes on the clock, and one that
    /// keeps the CPU busy is.
    #[cfg(unix)]
    #[test]
    fn stuck_counts_cpu_time() {
        use super::{Failure, watch};
        let start = |script: &str| std::process::Command::new("sh").args(["-c", script]).spawn().unwrap();
        let mut waits = start("sleep 1");
        assert_eq!(
            watch(&mut waits, Duration::from_millis(500), None, super::MOST_KILOBYTES, || false),
            None
        );
        let mut spins = start("while :; do :; done");
        assert_eq!(
            watch(&mut spins, Duration::from_millis(500), None, super::MOST_KILOBYTES, || false),
            Some(Failure::Stuck)
        );
    }

    /// Whether a process is gone, given two seconds to be: a killed one is reaped soon after.
    #[cfg(unix)]
    fn gone(pid: &str) -> bool {
        for _ in 0..200 {
            let alive = std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// A build's tools are counted with it, and stopped with it: here a spinning grandchild,
    /// as `opt` under `fennec build`, whose CPU time its parent's own doesn't show.
    #[cfg(unix)]
    #[test]
    fn a_build_is_stopped_with_every_tool_it_started() {
        use super::{Failure, MOST_KILOBYTES, output_watched};
        let dir = std::env::temp_dir().join(format!("fennec-fuzz-group-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let noted = dir.join("pid");
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(format!("sh -c 'while :; do :; done' & echo $! > {}; wait", noted.display()));
        let (_, failure) = output_watched(&mut command, Duration::from_millis(500), MOST_KILOBYTES).unwrap();
        assert_eq!(failure, Some(Failure::Stuck));
        let spinner = std::fs::read_to_string(&noted).unwrap();
        assert!(gone(spinner.trim()), "the spinning tool {spinner} outlived its build");
        // Memory is counted over the group too: a limit of one kilobyte, which anything holds.
        let mut sleeps = std::process::Command::new("sh");
        sleeps.args(["-c", "sleep 5"]);
        let (_, failure) = output_watched(&mut sleeps, Duration::from_secs(10), 1).unwrap();
        assert_eq!(failure, Some(Failure::TooMuchMemory));
        // One that fails on its own isn't stopped, and says how it ended.
        let mut fails = std::process::Command::new("sh");
        fails.args(["-c", "exit 1"]);
        let (said, failure) = output_watched(&mut fails, Duration::from_secs(10), MOST_KILOBYTES).unwrap();
        assert_eq!((failure, said.status.code()), (None, Some(1)));
        // And one that finishes gives back what it wrote.
        let mut says = std::process::Command::new("sh");
        says.args(["-c", "echo out; echo err >&2"]);
        let (said, failure) = output_watched(&mut says, Duration::from_secs(10), MOST_KILOBYTES).unwrap();
        assert_eq!((failure, said.stdout, said.stderr), (None, b"out\n".to_vec(), b"err\n".to_vec()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A worker that's stopped takes what it started with it, even in a group of its own, which
    /// is how a stuck worker left an `opt` running for 18 hours.
    #[cfg(unix)]
    #[test]
    fn a_stopped_worker_leaves_nothing_running() {
        use super::{Failure, watch};
        let dir = std::env::temp_dir().join(format!("fennec-fuzz-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let noted = dir.join("pid");
        let script = format!(
            "perl -e 'setpgrp(0, 0); exec q(sleep), 30' & echo $! > {}; while :; do :; done",
            noted.display()
        );
        let mut worker = std::process::Command::new("sh").args(["-c", &script]).spawn().unwrap();
        assert_eq!(
            watch(&mut worker, Duration::from_millis(500), None, super::MOST_KILOBYTES, || false),
            Some(Failure::Stuck)
        );
        let sleeper = std::fs::read_to_string(&noted).unwrap();
        assert!(gone(sleeper.trim()), "{sleeper} outlived the worker that started it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
