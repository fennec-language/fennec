//! Building the Rust twins, and timing runs. Each program runs several times, taking turns
//! with the others so they share whatever the machine is doing, and its median run counts.
//!
//! Instruction counts back up times: unlike time, they barely move when the machine is busy
//! or when code merely moves around in the binary. Where the CPU's counters can be read,
//! `perf` counts every run. Where they can't, as on CI's virtual machines, Valgrind counts
//! by simulating, about 50 times slower, so only to confirm a slowdown.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// One measured program: its median time and instruction count, and what it printed.
#[derive(Clone, Debug)]
pub(crate) struct Timing {
    pub(crate) median: Duration,
    /// The median count of instructions it ran, where `perf` can count them.
    pub(crate) instructions: Option<u64>,
    pub(crate) output: String,
    /// What it wrote to stderr, where the JIT says it fell back to the interpreter.
    pub(crate) notes: String,
}

/// A program to run: a command and its arguments.
pub(crate) struct Program {
    pub(crate) command: PathBuf,
    pub(crate) args: Vec<String>,
}

/// One run: how long it took, how many instructions it ran, and what it printed.
struct Run {
    took: Duration,
    instructions: Option<u64>,
    stdout: String,
    stderr: String,
}

impl Program {
    /// Runs the program, under `perf stat` when `count` names a file for its report.
    fn run(&self, count: Option<&Path>) -> Result<Run, String> {
        let mut command = match count {
            Some(report) => {
                let mut perf = Command::new("perf");
                perf.args(["stat", "-x,", "-e", "instructions:u", "-o"])
                    .arg(report)
                    .arg("--")
                    .arg(&self.command);
                perf
            }
            None => Command::new(&self.command),
        };
        let started = Instant::now();
        let output = command
            .args(&self.args)
            .output()
            .map_err(|err| format!("couldn't run {}: {err}", self.command.display()))?;
        let took = started.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(format!("{} {} failed:\n{stderr}", self.command.display(), self.args.join(" ")));
        }
        let instructions = count.and_then(|report| instructions(&std::fs::read_to_string(report).ok()?));
        Ok(Run { took, instructions, stdout, stderr })
    }
}

/// The instruction count in a `perf stat -x,` report: the first field of the line for the event.
fn instructions(report: &str) -> Option<u64> {
    report.lines().find(|line| line.contains("instructions"))?.split(',').next()?.trim().parse().ok()
}

/// Where `perf` writes its report, if it can count instructions on this machine. CI allows it
/// on Linux; elsewhere the suite goes by time alone.
pub(crate) fn perf_report(out_dir: &Path) -> Option<PathBuf> {
    let report = out_dir.join("perf.txt");
    let probe = Program { command: PathBuf::from("true"), args: Vec::new() };
    probe.run(Some(&report)).ok()?.instructions.map(|_| report)
}

/// Counts the instructions a program runs with Valgrind's cachegrind, if it's installed.
pub(crate) fn count_with_valgrind(program: &Program) -> Option<u64> {
    let output = Command::new("valgrind")
        .args(["--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null"])
        .arg(&program.command)
        .args(&program.args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    valgrind_count(&String::from_utf8_lossy(&output.stderr))
}

/// The count in cachegrind's summary line, `==123== I refs:      1,234,567`.
fn valgrind_count(report: &str) -> Option<u64> {
    let line = report.lines().find(|line| line.contains("I refs:") || line.contains("I   refs:"))?;
    let digits: String = line.rsplit(':').next()?.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether Valgrind is installed.
pub(crate) fn valgrind_available() -> bool {
    Command::new("valgrind").arg("--version").output().is_ok_and(|output| output.status.success())
}

/// The middle value: the mean of the middle two for an even count.
fn median<T: Copy + Ord>(values: &mut [T], mean: fn(T, T) -> T) -> Option<T> {
    values.sort_unstable();
    let middle = values.len() / 2;
    let upper = *values.get(middle)?;
    if values.len() % 2 == 1 { Some(upper) } else { values.get(middle - 1).map(|&lower| mean(lower, upper)) }
}

/// Runs each program `runs` times, taking turns, and keeps each one's median time and count.
pub(crate) fn time_together(
    programs: &[Program],
    runs: usize,
    count: Option<&Path>,
) -> Result<Vec<Timing>, String> {
    let mut all: Vec<Vec<Run>> = programs.iter().map(|_| Vec::with_capacity(runs)).collect();
    for _ in 0..runs {
        for (program, runs) in programs.iter().zip(all.iter_mut()) {
            runs.push(program.run(count)?);
        }
    }
    let mut timings = Vec::with_capacity(programs.len());
    for runs in all {
        let mut times: Vec<Duration> = runs.iter().map(|run| run.took).collect();
        let mut counts: Vec<u64> = runs.iter().filter_map(|run| run.instructions).collect();
        let Some(first) = runs.into_iter().next() else { continue };
        timings.push(Timing {
            median: median(&mut times, |a, b| (a + b) / 2).unwrap_or_default(),
            instructions: if counts.is_empty() { None } else { median(&mut counts, u64::midpoint) },
            output: first.stdout,
            notes: first.stderr,
        });
    }
    Ok(timings)
}

/// Compiles a Rust twin the way a release build would, for this machine's CPU.
pub(crate) fn build_twin(source: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let name = source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let binary = out_dir.join(name);
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["-C", "opt-level=3", "-C", "target-cpu=native", "-o"])
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
    use super::{instructions, median, valgrind_count};

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
    fn the_median_is_the_middle_run() {
        assert_eq!(median(&mut [5, 1, 9], u64::midpoint), Some(5));
        assert_eq!(median(&mut [4, 1, 9, 2], u64::midpoint), Some(3));
        assert_eq!(median(&mut Vec::<u64>::new(), u64::midpoint), None);
    }
}
