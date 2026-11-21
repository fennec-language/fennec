//! `fennec-oracle`: runs a Fennec program the oracle's way, compares it with the compiled
//! program, or reports how much of the examples it covers.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use fennec_oracle::{Ended, Outcome, Workdir, report};

const USAGE: &str = "usage:
  fennec-oracle run <program.fen>                    run it the oracle's way
  fennec-oracle compare <program.fen> [--fennec PATH]  hold the compiled program, debug and release, to the oracle
  fennec-oracle coverage [examples dir]              run the oracle over the examples, and say what it covers

exit codes: run gives the program's own; compare gives 0 when they agree and 1 when they differ;
3 means the oracle doesn't model something the program uses";

/// How much a step of a compare may take before it's stopped.
///
/// A build is held to the CPU time it uses. On a busy machine it waits its turn for a core, so its
/// wall-clock time says more about the machine than about the build. A generous wall-clock limit
/// beside it still stops a build that's stuck. A run of a built program is held to a short
/// wall-clock limit as well, since a program that's stuck waiting on itself uses no CPU at all.
#[derive(Clone, Copy)]
struct Limits {
    cpu_seconds: f64,
    wall_seconds: u64,
}

/// For a build, and for `fennec run`, which builds the program before it runs it.
const BUILD: Limits = Limits { cpu_seconds: 300.0, wall_seconds: 900 };

/// For a run of a built program.
const RUN: Limits = Limits { cpu_seconds: 60.0, wall_seconds: 120 };

/// How much memory a compiled program, with what it starts, may hold.
const CHILD_MEGABYTES: u64 = 4000;

/// The exit code for a program the oracle can't run.
const UNSUPPORTED: u8 = 3;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["run", path] => run(path),
        ["compare", path] => compare(path, None),
        ["compare", path, "--fennec", fennec] | ["compare", "--fennec", fennec, path] => {
            compare(path, Some(fennec))
        }
        ["coverage"] => coverage("examples"),
        ["coverage", dir] => coverage(dir),
        _ => {
            say_err(USAGE);
            ExitCode::from(2)
        }
    }
}

fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn say_err(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{text}");
}

fn run(path: &str) -> ExitCode {
    let outcome = fennec_oracle::run_path(Path::new(path), path);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&outcome.stdout);
    let _ = out.flush();
    match outcome.ended {
        Ended::Exited(code) => ExitCode::from(code),
        Ended::Stopped(said) => {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(said.as_bytes());
            ExitCode::from(1)
        }
        Ended::Unsupported(why) => {
            say_err(&format!("unsupported: {why}"));
            ExitCode::from(UNSUPPORTED)
        }
    }
}

fn coverage(dir: &str) -> ExitCode {
    let examples = match report::examples(Path::new(dir)) {
        Ok(examples) => examples,
        Err(err) => {
            say_err(&format!("couldn't read {dir}: {err}"));
            return ExitCode::from(2);
        }
    };
    let mut judged = Vec::with_capacity(examples.len());
    for example in &examples {
        let verdict = report::judge(example);
        say(&report::line(example, &verdict));
        judged.push(verdict);
    }
    say(&report::summary(&examples, &judged));
    if judged.iter().any(|verdict| matches!(verdict, report::Verdict::Differs(_))) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// What a compiled run gave: its exit code, or how it ended otherwise, and what it printed.
struct Compiled {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Holds the compiled program, built for debugging and for release, to the oracle. Each compiled
/// run starts in a fresh empty folder of its own, at one path, so what it writes goes nowhere
/// else and every run starts the same, and the oracle runs as if in an empty folder at that path.
fn compare(path: &str, fennec: Option<&str>) -> ExitCode {
    let work = std::env::temp_dir().join(format!("fennec-oracle-{}-work", std::process::id()));
    let Some(work) = fresh(&work) else {
        say_err(&format!("couldn't make an empty folder at {} to run the program in", work.display()));
        return ExitCode::from(2);
    };
    let outcome = fennec_oracle::run_path_in(Path::new(path), path, Workdir::empty(&work));
    let Some((code, _)) = report::oracle_side(&outcome) else {
        let _ = std::fs::remove_dir_all(&work);
        if let Ended::Unsupported(why) = &outcome.ended {
            say_err(&format!("unsupported: {why}"));
        }
        return ExitCode::from(UNSUPPORTED);
    };
    let fennec = fennec.map_or_else(default_fennec, PathBuf::from);
    // The compiled program runs in another folder, so a path given relative to this one is made
    // whole first. A bare name stays as it is, for the system to find on the path.
    let fennec =
        if fennec.components().count() > 1 { std::path::absolute(&fennec).unwrap_or(fennec) } else { fennec };
    let program = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let sorted = varies(path);
    let mut agree = true;
    // Each compare's builds are one-off, so they start no build daemon, which would stay half an
    // hour for nothing.
    let debug = in_fresh(
        &work,
        Command::new(&fennec).env("FENNEC_NO_DAEMON", "1").arg("run").arg("--no-test").arg(&program),
        BUILD,
    );
    agree &= judged("debug", &outcome, code, debug, sorted);
    let built = std::env::temp_dir().join(format!("fennec-oracle-{}", std::process::id()));
    let shown_built = built.to_string_lossy().into_owned();
    let release = limited(
        Command::new(&fennec).env("FENNEC_NO_DAEMON", "1").args([
            "build",
            "--release",
            path,
            "-o",
            &shown_built,
        ]),
        BUILD,
    )
    .map_err(|why| format!("the release build stopped: {why}"))
    .and_then(|build| {
        if build.code == Some(0) {
            in_fresh(&work, &mut Command::new(&built), RUN)
        } else {
            Err(format!("the release build failed:\n{}", String::from_utf8_lossy(&build.stderr)))
        }
    });
    let _ = std::fs::remove_file(&built);
    let _ = std::fs::remove_dir_all(&work);
    agree &= judged("release", &outcome, code, release, sorted);
    if agree { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// An empty folder at `path`, made afresh, as its path with every link in it followed, which is
/// the path a program run in it finds itself in.
fn fresh(path: &Path) -> Option<PathBuf> {
    let _ = std::fs::remove_dir_all(path);
    std::fs::create_dir_all(path).ok()?;
    std::fs::canonicalize(path).ok()
}

/// Runs a command in a fresh empty folder at `work`, within `limits`.
fn in_fresh(work: &Path, command: &mut Command, limits: Limits) -> Result<Compiled, String> {
    let made = fresh(work).filter(|made| made == work);
    if made.is_none() {
        return Err(format!("couldn't make an empty folder at {} to run it in", work.display()));
    }
    limited(command.current_dir(work), limits)
}

/// Holds one compiled run to the oracle's, saying how they differ if they do.
fn judged(
    which: &str,
    outcome: &Outcome,
    code: u8,
    compiled: Result<Compiled, String>,
    sorted: bool,
) -> bool {
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(why) => {
            say(&format!("{which}: couldn't run the compiled program: {why}\n"));
            return false;
        }
    };
    let ours = report::as_compared(&outcome.stdout, sorted);
    let theirs = report::as_compared(&compiled.stdout, sorted);
    let mut same = true;
    if ours != theirs {
        say(&format!(
            "{which}: standard output differs (- compiled, + oracle)\n{}",
            report::diff(&theirs, &ours)
        ));
        same = false;
    }
    if compiled.code != Some(i32::from(code)) {
        let theirs =
            compiled.code.map_or_else(|| "no exit code".to_owned(), |code| format!("exit code {code}"));
        say(&format!(
            "{which}: the compiled program ended with {theirs}, the oracle with exit code {code}\n{}",
            String::from_utf8_lossy(&compiled.stderr)
        ));
        same = false;
    }
    if same {
        say(&format!("{which}: agrees\n"));
    }
    same
}

/// Whether a program's lines may come in another order each run, as `determinism-varies.txt` at
/// the checkout's root lists them, one path a line.
fn varies(path: &str) -> bool {
    let mut dir = std::env::current_dir().ok();
    while let Some(at) = dir {
        let listed = at.join("determinism-varies.txt");
        if let Ok(text) = std::fs::read_to_string(&listed) {
            let wanted = Path::new(path);
            return text.lines().map(str::trim).any(|line| !line.is_empty() && wanted.ends_with(line));
        }
        if at.join("Cargo.toml").is_file() && at.join("examples").is_dir() {
            return false;
        }
        dir = at.parent().map(Path::to_path_buf);
    }
    false
}

/// The `fennec` built beside this, or the one on the path.
fn default_fennec() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("fennec")));
    beside.filter(|fennec| fennec.is_file()).unwrap_or_else(|| PathBuf::from("fennec"))
}

/// Runs a command in a process group of its own, with empty input, stopping the whole group past
/// its CPU-time, wall-clock or memory limit, and saying which.
fn limited(command: &mut Command, limits: Limits) -> Result<Compiled, String> {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("couldn't start it: {err}"))?;
    let group = child.id();
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);
    let started = Instant::now();
    let mut stopped = None;
    let mut seen = Seen::default();
    let mut polls: u32 = 0;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => return Err(err.to_string()),
        }
        if started.elapsed() > Duration::from_secs(limits.wall_seconds) {
            stopped = Some(format!(
                "it went past its wall-clock limit of {} seconds, having used {:.1} seconds of CPU",
                limits.wall_seconds,
                seen.cpu_seconds()
            ));
        } else if polls.is_multiple_of(10) {
            let megabytes = seen.read(&group_listing(), group);
            if seen.cpu_seconds() > limits.cpu_seconds {
                stopped = Some(format!(
                    "it went past its limit of {:.0} seconds of CPU, {} seconds in",
                    limits.cpu_seconds,
                    started.elapsed().as_secs()
                ));
            } else if megabytes > CHILD_MEGABYTES {
                stopped = Some(format!("it held more than {CHILD_MEGABYTES} MB"));
            }
        }
        if stopped.is_some() {
            kill_group(group);
            let _ = child.kill();
            break child.wait().map_err(|err| err.to_string())?;
        }
        polls = polls.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(20));
    };
    kill_group(group);
    if let Some(why) = stopped {
        return Err(why);
    }
    let stdout = stdout.map(|reader| reader.join().unwrap_or_default()).unwrap_or_default();
    let stderr = stderr.map(|reader| reader.join().unwrap_or_default()).unwrap_or_default();
    Ok(Compiled { code: status.code(), stdout, stderr })
}

fn read_all(mut from: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = from.read_to_end(&mut all);
        all
    })
}

/// Every process, as `ps` lists it: its group, its id, the kilobytes it holds and the CPU time it
/// has used, a line each. It lists them all and the group is picked out after, since on Linux
/// `ps -g` picks a session, not a process group. Where there's no `ps` the listing is empty.
fn group_listing() -> String {
    let listed =
        Command::new("ps").args(["-A", "-o", "pgid=", "-o", "pid=", "-o", "rss=", "-o", "time="]).output();
    listed.map(|listed| String::from_utf8_lossy(&listed.stdout).into_owned()).unwrap_or_default()
}

/// What a process group's processes have used, as listings of them come in.
#[derive(Default)]
struct Seen {
    /// The CPU seconds each process had used when last listed. A process that has ended keeps
    /// what it used here, so a build's tools count after they're done.
    cpu_by_process: HashMap<u32, f64>,
}

impl Seen {
    /// Reads a listing from [`group_listing`], keeping the CPU time of each process in `group`,
    /// and gives the megabytes the group's processes hold now.
    fn read(&mut self, listing: &str, group: u32) -> u64 {
        let mut kilobytes: u64 = 0;
        for line in listing.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            let [in_group, process, held, time] = words.as_slice() else { continue };
            let (Ok(in_group), Ok(process)) = (in_group.parse::<u32>(), process.parse::<u32>()) else {
                continue;
            };
            if in_group != group {
                continue;
            }
            kilobytes = kilobytes.saturating_add(held.parse::<u64>().unwrap_or(0));
            if let Some(seconds) = cpu_seconds(time) {
                let kept = self.cpu_by_process.entry(process).or_insert(0.0);
                *kept = kept.max(seconds);
            }
        }
        kilobytes / 1024
    }

    /// The CPU seconds the group's processes have used, those that have ended included.
    fn cpu_seconds(&self) -> f64 {
        self.cpu_by_process.values().sum()
    }
}

/// The seconds in a CPU time as `ps` writes it: `[[dd-]hh:]mm:ss.cc` on macOS, `[dd-]hh:mm:ss` on
/// Linux.
fn cpu_seconds(time: &str) -> Option<f64> {
    let (days, clock) = match time.split_once('-') {
        Some((days, clock)) => (days.parse::<f64>().ok()?, clock),
        None => (0.0, time),
    };
    let mut seconds = 0.0;
    for part in clock.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + seconds)
}

fn kill_group(group: u32) {
    let _ = Command::new("kill").args(["-9", &format!("-{group}")]).stderr(Stdio::null()).status();
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Command, Limits, Seen, cpu_seconds, limited};

    #[test]
    fn cpu_time_is_read_as_macos_and_linux_write_it() {
        assert_eq!(cpu_seconds("0:20.13"), Some(20.13));
        assert_eq!(cpu_seconds("1:02:03.50"), Some(3723.5));
        assert_eq!(cpu_seconds("00:01:15"), Some(75.0));
        assert_eq!(cpu_seconds("2-00:00:01"), Some(172_801.0));
        assert_eq!(cpu_seconds("TIME"), None);
    }

    #[test]
    fn a_process_that_has_ended_keeps_counting_the_cpu_it_used() {
        let mut seen = Seen::default();
        assert_eq!(seen.read("  7 7 2048 0:01.00\n  7 8 1024 0:02.00\n  9 9 999999 9:00.00\n", 7), 3);
        assert!((seen.cpu_seconds() - 3.0).abs() < 1e-9);
        // The tool that was 8 has ended, and 7 has used more since.
        assert_eq!(seen.read("  7 7 2048 0:01.50\n", 7), 2);
        assert!((seen.cpu_seconds() - 3.5).abs() < 1e-9);
    }

    #[test]
    fn a_busy_loop_two_processes_down_stops_at_its_cpu_limit() {
        let limits = Limits { cpu_seconds: 1.0, wall_seconds: 120 };
        let ran = limited(Command::new("sh").args(["-c", "sh -c 'while :; do :; done'; true"]), limits);
        let why = ran.err().unwrap_or_default();
        assert!(why.starts_with("it went past its limit of 1 seconds of CPU"), "{why}");
    }

    #[test]
    fn a_stuck_run_using_no_cpu_stops_at_its_wall_clock_limit() {
        let limits = Limits { cpu_seconds: 60.0, wall_seconds: 1 };
        let why = limited(Command::new("sleep").arg("60"), limits).err().unwrap_or_default();
        assert!(why.starts_with("it went past its wall-clock limit of 1 seconds, having used 0."), "{why}");
    }

    #[test]
    fn a_wait_that_uses_little_cpu_is_held_only_to_the_wall_clock_limit() {
        let limits = Limits { cpu_seconds: 1.0, wall_seconds: 60 };
        let ran = limited(Command::new("sh").args(["-c", "sleep 2; echo done"]), limits);
        assert!(ran.is_ok_and(|ran| ran.code == Some(0) && ran.stdout == b"done\n"));
    }
}
