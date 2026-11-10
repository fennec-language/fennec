//! Watching a worker process: it runs until something goes wrong or time runs out, and says
//! which input it's on, so a failure can be traced to that input and the next worker can start
//! after it.

use std::fmt;
use std::fs::File;
use std::io::{Seek, Write};
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
/// How many times its CPU time a process may go on the clock before it counts as stuck anyway:
/// one waiting on something forever uses no CPU time at all.
const WAITING_TIMES: u32 = 30;
/// The most memory a worker may hold.
pub(crate) const MOST_KILOBYTES: u64 = 4 * 1024 * 1024;
/// How much CPU time building one program may take, the tools `fennec build` starts included.
/// A generated program builds in a few seconds; one `opt` once ran 18 hours on one.
pub(crate) const BUILD_STUCK: Duration = Duration::from_secs(120);

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
/// its group, so it goes too.
fn stop(child: &mut Child) {
    let id = child.id();
    let below = descendants(id);
    if cfg!(unix) {
        kill(&format!("-{id}"));
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
    let failure = watch_as(&mut child, Watched::Group, stuck, None, most_kilobytes, || false);
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
    /// The process group it leads, every process in it summed, where groups can be asked about.
    Group,
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
    // When it last made progress, and the CPU time it had used by then.
    let mut since = (Instant::now(), Duration::ZERO);
    // The CPU time it's used, as last asked, if it can be.
    let mut used = None;
    let mut checks = 0_u32;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return failure_of(status);
        }
        if moved() {
            since = (Instant::now(), used.unwrap_or_default());
        }
        checks += 1;
        let mut too_big = false;
        if checks.is_multiple_of(10) {
            let (held, cpu) = measured(child.id(), watched);
            too_big = held.is_some_and(|held| held > most_kilobytes);
            used = cpu.or(used);
        }
        let on_the_clock = since.0.elapsed();
        let busy = |used: Option<Duration>| used.map_or(on_the_clock, |used| used.saturating_sub(since.1));
        let mut is_stuck = busy(used) > stuck || on_the_clock > stuck * WAITING_TIMES;
        // CPU time is asked for only now and then, and until it's first known the time on the
        // clock stands in for it, which a busy machine runs up without the process doing
        // anything. So it's asked again before anything is called stuck.
        if is_stuck && on_the_clock <= stuck * WAITING_TIMES {
            used = measured(child.id(), watched).1.or(used);
            is_stuck = busy(used) > stuck;
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
    match watched {
        Watched::Group if cfg!(unix) => {
            group_use(id).map_or((None, None), |(held, used)| (Some(held), Some(used)))
        }
        _ => (kilobytes(id), cpu_time(id)),
    }
}

/// What fuzzing one target found.
pub(crate) struct Outcome {
    pub(crate) inputs: u64,
    /// Each kind of failure once, with the input that caused it.
    pub(crate) failures: Vec<(Failure, u64)>,
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
        let folder = std::env::temp_dir().join(format!("fennec-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
        let progress = folder.join(self.target.name);
        let mut outcome = Outcome { inputs: 0, failures: Vec::new() };
        let mut next = 0_u64;
        while next < inputs && deadline.is_none_or(|deadline| Instant::now() < deadline) {
            let _ = std::fs::remove_file(&progress);
            let mut child = self.worker(next, inputs, &progress)?;
            let mut last = None;
            let failure = watch(&mut child, self.stuck, deadline, MOST_KILOBYTES, || {
                let now = noted(&progress);
                let moved = now != last;
                last = now;
                moved
            });
            let reached = noted(&progress).unwrap_or(next);
            outcome.inputs = reached + 1;
            let Some(failure) = failure else { break };
            if !outcome.failures.iter().any(|(kind, _)| *kind == failure) {
                outcome.failures.push((failure, reached));
            }
            next = reached + 1;
        }
        let _ = std::fs::remove_dir_all(&folder);
        Ok(outcome)
    }

    fn worker(&self, first: u64, end: u64, progress: &Path) -> Result<Child, String> {
        Command::new(self.fennec_fuzz)
            .args(["worker", self.target.name, &self.seed.to_string(), &first.to_string(), &end.to_string()])
            .arg(progress)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("couldn't start a worker: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, cpu_time_in};

    #[test]
    fn cpu_time_reads_as_ps_writes_it() {
        assert_eq!(cpu_time_in("0:01.25"), Some(Duration::from_millis(1250)));
        assert_eq!(cpu_time_in("00:00:07"), Some(Duration::from_secs(7)));
        assert_eq!(cpu_time_in("01:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(cpu_time_in("2-00:00:01"), Some(Duration::from_secs(2 * 86_400 + 1)));
        assert_eq!(cpu_time_in(""), None);
        assert_eq!(cpu_time_in("NaN"), None);
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
