//! Running a program and counting the instructions it runs. Unlike time, a count barely moves
//! when the machine is busy, and it doesn't depend on how fast the machine is. Where the CPU's
//! counters can be read, `perf` counts. Where they can't, as on CI's virtual machines, Valgrind
//! counts by simulating, about 50 times slower. macOS's `time` counts on a Mac.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A program to run: a command, its arguments, and a file for its standard input.
#[derive(Clone)]
pub struct Program {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub stdin: Option<PathBuf>,
}

/// One run: how long it took, and what it printed.
pub struct Run {
    pub took: Duration,
    pub stdout: String,
    pub stderr: String,
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

    /// Runs it, which has to succeed.
    pub fn run(&self) -> Result<Run, String> {
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
pub enum Counter {
    /// `perf stat`, writing its report to this file.
    Perf(PathBuf),
    Valgrind,
    /// macOS's `time -l`.
    AppleTime,
}

impl Counter {
    /// The best way to count on this machine, if there is one.
    pub fn find(out_dir: &Path) -> Option<Counter> {
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
    pub fn count(&self, program: &Program) -> Option<u64> {
        self.count_set_up(program, |_| {})
    }

    /// How many instructions a program runs, after `set_up` gives it the folder and environment
    /// it needs.
    pub fn count_set_up(&self, program: &Program, set_up: impl FnOnce(&mut Command)) -> Option<u64> {
        self.counted(program, None, 0, set_up)
    }

    /// `count_set_up`, for a program that exits with `code` when it does what it's meant to, like
    /// a linter that found what it reports.
    pub fn count_exiting(
        &self,
        program: &Program,
        code: i32,
        set_up: impl FnOnce(&mut Command),
    ) -> Option<u64> {
        self.counted(program, None, code, set_up)
    }

    /// How many instructions a program and every program it starts run, for one that compiles,
    /// links and starts what it built. `folder` takes Valgrind's report of each. None where they
    /// can't all be counted, as on a Mac, whose `time` counts only the program it starts.
    pub fn count_every_process(
        &self,
        program: &Program,
        folder: &Path,
        set_up: impl FnOnce(&mut Command),
    ) -> Option<u64> {
        self.counts_every_process().then(|| self.counted(program, Some(folder), 0, set_up)).flatten()
    }

    /// Whether `count_every_process` can count here.
    #[must_use]
    pub fn counts_every_process(&self) -> bool {
        !matches!(self, Counter::AppleTime)
    }

    /// `perf` counts the programs a program starts by itself. Valgrind is told to follow them
    /// when there's a `folder` to put their reports in: each process writes its own, since a
    /// program that reads what the ones it starts print would read Valgrind's report too.
    fn counted(
        &self,
        program: &Program,
        every: Option<&Path>,
        code: i32,
        set_up: impl FnOnce(&mut Command),
    ) -> Option<u64> {
        let input = program.input().ok()?;
        let reports = match (self, every) {
            (Counter::Valgrind, Some(folder)) => {
                let reports = folder.join("valgrind");
                let _ = std::fs::remove_dir_all(&reports);
                std::fs::create_dir_all(&reports).ok()?;
                Some(reports)
            }
            _ => None,
        };
        let mut command = match self {
            Counter::Perf(report) => {
                let mut perf = Command::new("perf");
                perf.args(["stat", "-x,", "-e", "instructions:u", "-o"]).arg(report).arg("--");
                perf
            }
            Counter::Valgrind => {
                let mut valgrind = Command::new("valgrind");
                valgrind.args(["--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null"]);
                if let Some(reports) = &reports {
                    valgrind.arg("--trace-children=yes").arg(format!("--log-file={}/%p", reports.display()));
                }
                valgrind
            }
            Counter::AppleTime => {
                let mut time = Command::new("/usr/bin/time");
                time.arg("-l");
                time
            }
        };
        set_up(&mut command);
        let output = command.arg(&program.command).args(&program.args).stdin(input).output().ok()?;
        if output.status.code() != Some(code) {
            return None;
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        match self {
            Counter::Perf(report) => instructions(&std::fs::read_to_string(report).ok()?),
            Counter::Valgrind if reports.is_some() => valgrind_total(reports.as_deref()?),
            Counter::Valgrind => valgrind_count(&stderr),
            Counter::AppleTime => apple_count(&stderr),
        }
    }
}

/// The cache misses a program makes: its data reads and writes that miss the first-level data
/// cache, and everything that misses the last level, data and instructions both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Misses {
    pub d1: u64,
    pub ll: u64,
}

/// The caches cachegrind simulates: 32 KiB first-level caches and an 8 MiB last level, with
/// 64-byte lines. They're fixed, so the counts don't depend on which machine runs them.
const CACHES: [&str; 3] = ["--I1=32768,8,64", "--D1=32768,8,64", "--LL=8388608,16,64"];

/// The cache misses a program makes on `CACHES`, from cachegrind's simulation, which is as
/// repeatable as its instruction count. None without Valgrind.
pub fn cache_misses(program: &Program) -> Option<Misses> {
    if !valgrind_available() {
        return None;
    }
    let input = program.input().ok()?;
    let output = Command::new("valgrind")
        .args(["--tool=cachegrind", "--cache-sim=yes", "--cachegrind-out-file=/dev/null"])
        .args(CACHES)
        .arg(&program.command)
        .args(&program.args)
        .stdin(input)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let report = String::from_utf8_lossy(&output.stderr);
    Some(Misses { d1: summary_count(&report, "D1  misses:")?, ll: summary_count(&report, "LL misses:")? })
}

/// The total on one of cachegrind's summary lines, `==1== D1  misses:   1,234  ( 1,000 rd + 234 wr)`.
fn summary_count(report: &str, label: &str) -> Option<u64> {
    let line = report.lines().find(|line| line.contains(label))?;
    let after = line.split_once(label)?.1;
    let total = after.split('(').next()?;
    let digits: String = total.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
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

/// The counts in every process's report in `folder`, added up. A process that ran another in its
/// place leaves its report without a count, and the one it ran counts what came after.
fn valgrind_total(folder: &Path) -> Option<u64> {
    let mut total = 0;
    let mut counted = 0;
    for entry in std::fs::read_dir(folder).ok()? {
        let report = std::fs::read_to_string(entry.ok()?.path()).ok()?;
        if let Some(count) = valgrind_count(&report) {
            total += count;
            counted += 1;
        }
    }
    (counted > 0).then_some(total)
}

/// After `count_every_process` counted with Valgrind, how many instructions each program it ran
/// took, by the name of the program, most first. Empty when it didn't count that way.
#[must_use]
pub fn counted_programs(folder: &Path) -> Vec<(String, u64)> {
    let mut programs: Vec<(String, u64)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder.join("valgrind")) else { return programs };
    for report in entries.flatten().filter_map(|entry| std::fs::read_to_string(entry.path()).ok()) {
        let (Some(name), Some(count)) = (program_name(&report), valgrind_count(&report)) else { continue };
        match programs.iter_mut().find(|(known, _)| *known == name) {
            Some((_, total)) => *total += count,
            None => programs.push((name, count)),
        }
    }
    programs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    programs
}

/// The name of the program in a Valgrind report, from `==123== Command: /path/to/opt -O1 ...`.
fn program_name(report: &str) -> Option<String> {
    let line = report.lines().find(|line| line.contains("Command: "))?;
    let path = line.split_once("Command: ")?.1.split_whitespace().next()?;
    Some(Path::new(path).file_name()?.to_string_lossy().into_owned())
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
pub fn median<T: Copy + Ord>(values: &mut [T], mean: fn(T, T) -> T) -> Option<T> {
    values.sort_unstable();
    let middle = values.len() / 2;
    let upper = *values.get(middle)?;
    if values.len() % 2 == 1 { Some(upper) } else { values.get(middle - 1).map(|&lower| mean(lower, upper)) }
}

#[cfg(test)]
mod tests {
    use super::{
        apple_count, counted_programs, instructions, median, summary_count, valgrind_count, valgrind_total,
    };

    #[test]
    fn perf_reports_give_their_instruction_count() {
        let report = "# started on Fri Oct 30 12:00:00 2026\n\n1234567890,,instructions:u,1000000,100.00,,\n";
        assert_eq!(instructions(report), Some(1_234_567_890));
        assert_eq!(instructions("<not supported>,,instructions:u,0,100.00,,\n"), None);
        assert_eq!(instructions(""), None);
    }

    #[test]
    fn cachegrind_reports_give_their_cache_misses() {
        let report = "==7== D1  misses:        12,345  (    10,000 rd   +     2,345 wr)\n\
                      ==7== LLd misses:         1,000  (       900 rd   +       100 wr)\n\
                      ==7== LL misses:          1,234  (     1,100 rd   +       134 wr)\n";
        assert_eq!(summary_count(report, "D1  misses:"), Some(12_345));
        assert_eq!(summary_count(report, "LL misses:"), Some(1_234));
        assert_eq!(summary_count(report, "LLi misses:"), None);
    }

    #[test]
    fn cachegrind_reports_give_their_instruction_count() {
        let report = "==4242== Cachegrind, a high-precision tracing profiler\n==4242== \n==4242== I   refs:      1,234,567\n";
        assert_eq!(valgrind_count(report), Some(1_234_567));
        assert_eq!(valgrind_count("==1== I refs: 89\n"), Some(89));
        assert_eq!(valgrind_count("==1== nothing here\n"), None);
    }

    #[test]
    fn every_process_report_with_a_count_is_added_up() {
        let folder = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-count-total-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(valgrind_total(&folder), None, "no reports, no count");
        std::fs::write(folder.join("1"), "==1== I   refs:      1,000\n").unwrap();
        std::fs::write(folder.join("2"), "==2== I   refs:      234\n").unwrap();
        // One that became another program leaves no count of its own.
        std::fs::write(folder.join("3"), "==3== Cachegrind, a high-precision tracing profiler\n").unwrap();
        assert_eq!(valgrind_total(&folder), Some(1_234));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn each_programs_count_is_added_up_by_its_name() {
        let folder = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-count-programs-{}", std::process::id())),
        );
        let reports = folder.join("valgrind");
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&reports).unwrap();
        let report = |pid: u32, command: &str, count: u64| {
            let text = format!("=={pid}== Command: {command}\n=={pid}== I   refs:      {count}\n");
            std::fs::write(reports.join(pid.to_string()), text).unwrap();
        };
        report(1, "/bin/llc -O0 a.ll", 30);
        report(2, "/home/fennec run --no-test", 10);
        report(3, "/bin/llc -O0 b.ll", 5);
        let programs = counted_programs(&folder);
        assert_eq!(programs, [("llc".to_owned(), 35), ("fennec".to_owned(), 10)]);
        let _ = std::fs::remove_dir_all(&folder);
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
