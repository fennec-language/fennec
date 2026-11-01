//! Watching a worker process: it runs until something goes wrong or time runs out, and says
//! which input it's on, so a failure can be traced to that input and the next worker can start
//! after it.

use std::fmt;
use std::fs::File;
use std::io::{Seek, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::targets::Target;

/// How a target says an input broke a rule it checks.
pub(crate) const WRONG: u8 = 3;
/// How a run says a panic ended it.
pub(crate) const PANICKED: u8 = 4;

/// How long one input may take before it counts as stuck, unless its target says otherwise.
pub(crate) const STUCK: Duration = Duration::from_secs(10);
/// The most memory a worker may hold.
const MOST_KILOBYTES: u64 = 4 * 1024 * 1024;

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

/// Waits for a process, stopping it if it goes `stuck` without `moved` saying it's making
/// progress, or if it holds too much memory. Reaching `deadline` stops it too, which isn't a
/// failure. Gives back how it went wrong, if it did.
pub(crate) fn watch(
    child: &mut Child,
    stuck: Duration,
    deadline: Option<Instant>,
    mut moved: impl FnMut() -> bool,
) -> Option<Failure> {
    let mut since = Instant::now();
    let mut checks = 0_u32;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return failure_of(status);
        }
        if moved() {
            since = Instant::now();
        }
        checks += 1;
        let too_big =
            checks.is_multiple_of(10) && kilobytes(child.id()).is_some_and(|held| held > MOST_KILOBYTES);
        let out_of_time = deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if too_big || out_of_time || since.elapsed() > stuck {
            let _ = child.kill();
            let _ = child.wait();
            return match (too_big, out_of_time) {
                (true, _) => Some(Failure::TooMuchMemory),
                (false, true) => None,
                (false, false) => Some(Failure::Stuck),
            };
        }
        std::thread::sleep(Duration::from_millis(10));
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
}

impl Run<'_> {
    /// Runs workers until the time is up or `inputs` have run, each starting after the input
    /// that ended the last.
    pub(crate) fn until(&self, seconds: u64, inputs: u64) -> Result<Outcome, String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let folder = std::env::temp_dir().join(format!("fennec-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
        let progress = folder.join(self.target.name);
        let mut outcome = Outcome { inputs: 0, failures: Vec::new() };
        let mut next = 0_u64;
        while next < inputs && Instant::now() < deadline {
            let _ = std::fs::remove_file(&progress);
            let mut child = self.worker(next, inputs, &progress)?;
            let mut last = None;
            let failure = watch(&mut child, self.target.stuck, Some(deadline), || {
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
