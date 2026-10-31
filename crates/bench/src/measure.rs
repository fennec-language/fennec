//! Timing runs and counting instructions. Each program runs several times, taking turns with
//! the others so they share whatever the machine is doing, and its median run counts.
//!
//! Instruction counts are what's compared with the targets: unlike time, they barely move when
//! the machine is busy, and they don't depend on how fast it is. Where the CPU's counters can
//! be read, `perf` counts. Where they can't, as on CI's virtual machines, Valgrind counts by
//! simulating, about 50 times slower. macOS's `time` counts on a Mac.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One measured program: its median time, and what it printed.
#[derive(Clone, Debug)]
pub(crate) struct Timing {
    pub(crate) median: Duration,
    pub(crate) output: String,
    /// What it wrote to stderr, where the JIT says it fell back to the interpreter.
    pub(crate) notes: String,
}

/// A program to run: a command, its arguments, and a file for its standard input.
pub(crate) struct Program {
    pub(crate) command: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) stdin: Option<PathBuf>,
}

/// One run: how long it took, and what it printed.
struct Run {
    took: Duration,
    stdout: String,
    stderr: String,
}

impl Program {
    fn input(&self) -> Result<Stdio, String> {
        match &self.stdin {
            Some(path) => std::fs::File::open(path)
                .map(Stdio::from)
                .map_err(|err| format!("couldn't read {}: {err}", path.display())),
            None => Ok(Stdio::null()),
        }
    }

    fn run(&self) -> Result<Run, String> {
        let input = self.input()?;
        let started = Instant::now();
        let output = Command::new(&self.command)
            .args(&self.args)
            .stdin(input)
            .output()
            .map_err(|err| format!("couldn't run {}: {err}", self.command.display()))?;
        let took = started.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(format!("{} {} failed:\n{stderr}", self.command.display(), self.args.join(" ")));
        }
        Ok(Run { took, stdout, stderr })
    }
}

/// What can count instructions on this machine.
#[derive(Clone, Debug)]
pub(crate) enum Counter {
    /// `perf stat`, writing its report to this file.
    Perf(PathBuf),
    Valgrind,
    /// macOS's `time -l`.
    AppleTime,
}

impl Counter {
    /// The best way to count on this machine, if there is one.
    pub(crate) fn find(out_dir: &Path) -> Option<Counter> {
        let report = out_dir.join("perf.txt");
        let perf = Counter::Perf(report);
        let probe = Program { command: PathBuf::from("true"), args: Vec::new(), stdin: None };
        if perf.count(&probe).is_some() {
            return Some(perf);
        }
        if valgrind_available() {
            return Some(Counter::Valgrind);
        }
        cfg!(target_os = "macos").then_some(Counter::AppleTime)
    }

    /// How many instructions a program runs.
    pub(crate) fn count(&self, program: &Program) -> Option<u64> {
        let input = program.input().ok()?;
        let mut command = match self {
            Counter::Perf(report) => {
                let mut perf = Command::new("perf");
                perf.args(["stat", "-x,", "-e", "instructions:u", "-o"]).arg(report).arg("--");
                perf
            }
            Counter::Valgrind => {
                let mut valgrind = Command::new("valgrind");
                valgrind.args(["--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null"]);
                valgrind
            }
            Counter::AppleTime => {
                let mut time = Command::new("/usr/bin/time");
                time.arg("-l");
                time
            }
        };
        let output = command.arg(&program.command).args(&program.args).stdin(input).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        match self {
            Counter::Perf(report) => instructions(&std::fs::read_to_string(report).ok()?),
            Counter::Valgrind => valgrind_count(&stderr),
            Counter::AppleTime => apple_count(&stderr),
        }
    }
}

/// The instruction count in a `perf stat -x,` report: the first field of the line for the event.
fn instructions(report: &str) -> Option<u64> {
    report.lines().find(|line| line.contains("instructions"))?.split(',').next()?.trim().parse().ok()
}

/// The count in cachegrind's summary line, `==123== I refs:      1,234,567`.
fn valgrind_count(report: &str) -> Option<u64> {
    let line = report.lines().find(|line| line.contains("I refs:") || line.contains("I   refs:"))?;
    let digits: String = line.rsplit(':').next()?.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The count in macOS's `time -l` report, `  1234567  instructions retired`.
fn apple_count(report: &str) -> Option<u64> {
    let line = report.lines().find(|line| line.contains("instructions retired"))?;
    line.split_whitespace().next()?.parse().ok()
}

/// Whether Valgrind is installed.
fn valgrind_available() -> bool {
    Command::new("valgrind").arg("--version").output().is_ok_and(|output| output.status.success())
}

/// The middle value, or the mean of the middle two.
fn median<T: Copy + Ord>(values: &mut [T], mean: fn(T, T) -> T) -> Option<T> {
    values.sort_unstable();
    let middle = values.len() / 2;
    let upper = *values.get(middle)?;
    if values.len() % 2 == 1 { Some(upper) } else { values.get(middle - 1).map(|&lower| mean(lower, upper)) }
}

/// Runs each program `runs` times, taking turns, and keeps each one's median time and count.
pub(crate) fn time_together(programs: &[Program], runs: usize) -> Result<Vec<Timing>, String> {
    let mut all: Vec<Vec<Run>> = programs.iter().map(|_| Vec::with_capacity(runs)).collect();
    for _ in 0..runs {
        for (program, runs) in programs.iter().zip(all.iter_mut()) {
            runs.push(program.run()?);
        }
    }
    let mut timings = Vec::with_capacity(programs.len());
    for runs in all {
        let mut times: Vec<Duration> = runs.iter().map(|run| run.took).collect();
        let Some(first) = runs.into_iter().next() else { continue };
        timings.push(Timing {
            median: median(&mut times, |a, b| (a + b) / 2).unwrap_or_default(),
            output: first.stdout,
            notes: first.stderr,
        });
    }
    Ok(timings)
}

/// Compiles a Rust twin the way a release build does, for the architecture's baseline CPU, so
/// its count doesn't depend on which machine pinned it.
pub(crate) fn build_twin(source: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let name = source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let binary = out_dir.join(name);
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["-C", "opt-level=3", "-o"])
        .arg(&binary)
        .arg(source)
        .output()
        .map_err(|err| format!("couldn't run rustc: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{} didn't compile:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::{apple_count, instructions, median, valgrind_count};

    #[test]
    fn perf_reports_give_their_instruction_count() {
        let report = "# started on Fri Oct 30 12:00:00 2026\n\n1234567890,,instructions:u,1000000,100.00,,\n";
        assert_eq!(instructions(report), Some(1_234_567_890));
        assert_eq!(instructions("<not supported>,,instructions:u,0,100.00,,\n"), None);
        assert_eq!(instructions(""), None);
    }

    #[test]
    fn cachegrind_reports_give_their_instruction_count() {
        let report = "==4242== Cachegrind, a high-precision tracing profiler\n==4242== \n==4242== I   refs:      1,234,567\n";
        assert_eq!(valgrind_count(report), Some(1_234_567));
        assert_eq!(valgrind_count("==1== I refs: 89\n"), Some(89));
        assert_eq!(valgrind_count("==1== nothing here\n"), None);
    }

    #[test]
    fn macos_reports_give_their_instruction_count() {
        assert_eq!(apple_count("  123  page reclaims\n   456789  instructions retired\n"), Some(456_789));
        assert_eq!(apple_count("nothing here\n"), None);
    }

    #[test]
    fn the_median_is_the_middle_run() {
        assert_eq!(median(&mut [5, 1, 9], u64::midpoint), Some(5));
        assert_eq!(median(&mut [4, 1, 9, 2], u64::midpoint), Some(3));
        assert_eq!(median(&mut Vec::<u64>::new(), u64::midpoint), None);
    }
}
