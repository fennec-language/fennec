//! `fennec-bench parallel`: the kernels in `bench/parallel`, which use `parallel`, built through
//! LLVM with the runtime that has threads, beside rayon twins, by wall clock at 1, 2, 4 and 8
//! threads. Beside them are the same kernel without `parallel` and a plain Rust twin on one
//! thread, which say what running in parallel costs and how much it gives. Each side runs in
//! turn and its best run counts, as in `llvm_suite`.
//!
//! These aren't in the locked suite, which counts instructions a thread at a time. Nothing here
//! fails for being slow: only a kernel that prints something other than its twin does. It prints
//! one Markdown section, and with `--markdown PATH` writes it there too for the suite's report.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::llvm_suite::summary;

const THREADS: [usize; 4] = [1, 2, 4, 8];
const KERNEL: &str = "ecs";
const OUT_DIR: &str = "target/bench/parallel";

/// One program to time: how long each of its runs took, and which variable says its threads.
struct Side {
    command: PathBuf,
    threads: Option<(&'static str, usize)>,
    times: Vec<Duration>,
}

impl Side {
    fn new(command: PathBuf, threads: Option<(&'static str, usize)>) -> Side {
        Side { command, threads, times: Vec::new() }
    }

    /// Runs it once, giving back what it printed.
    fn run(&mut self) -> Result<String, String> {
        let mut command = Command::new(&self.command);
        if let Some((variable, threads)) = self.threads {
            command.env(variable, threads.to_string());
        }
        let started = Instant::now();
        let output =
            command.output().map_err(|err| format!("couldn't run {}: {err}", self.command.display()))?;
        self.times.push(started.elapsed());
        if !output.status.success() {
            return Err(format!(
                "{} failed: {}",
                self.command.display(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Its best run, in seconds, and whether the median was far from it.
    fn best(&self) -> (f64, bool) {
        let (best, _, spread) = summary(&self.times);
        (best, spread > crate::llvm_suite::NOISY)
    }
}

/// Builds a kernel through LLVM, with the runtime that has threads when `threads`.
fn built(fennec: &Path, source: &Path, executable: PathBuf, threads: bool) -> Result<PathBuf, String> {
    let mut command = Command::new(fennec);
    command.args(["build", "--llvm", "--release"]).arg(source).arg("-o").arg(&executable);
    if threads {
        command.env("FENNEC_THREADS_RUNTIME", "1");
    }
    let output = command.output().map_err(|err| format!("couldn't run {}: {err}", fennec.display()))?;
    if output.status.success() {
        return Ok(executable);
    }
    Err(format!("{} didn't build: {}", source.display(), String::from_utf8_lossy(&output.stderr)))
}

/// Builds the twins, with the suite's fixed CPU level, in their own project's locked versions.
fn twins(dir: &Path, out: &Path) -> Result<PathBuf, String> {
    let target = std::env::current_dir().map_err(|err| err.to_string())?.join(out).join("twin");
    let mut command = Command::new("cargo");
    command
        .args(["build", "--release", "--locked", "--quiet"])
        .current_dir(dir.join("twin"))
        .arg("--target-dir")
        .arg(&target);
    if let [flag, cpu] = crate::measure::twin_cpu() {
        command.env("RUSTFLAGS", format!("{flag} {cpu}"));
    }
    let output = command.output().map_err(|err| format!("couldn't run cargo: {err}"))?;
    if output.status.success() {
        return Ok(target.join("release"));
    }
    Err(format!("the twins didn't build: {}", String::from_utf8_lossy(&output.stderr)))
}

/// A time, with a mark when its runs were noisy.
fn shown(side: &Side) -> String {
    let (best, noisy) = side.best();
    format!("{:.0} ms{}", best * 1000.0, if noisy { " (noisy)" } else { "" })
}

/// The section for the report: `plain` is the two sides without `parallel`, Fennec's first, and
/// each of `threaded` is Fennec's and rayon's at a number of threads.
fn section(plain: &[Side; 2], threaded: &[[Side; 2]]) -> String {
    let [ours_plain, rust_plain] = plain;
    let (cpu, cores) = (crate::machine::cpu(), crate::machine::cores());
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let mut text =
        format!("## Parallel: `{KERNEL}` against rayon, on {os} {arch}, {cpu} with {cores} cores\n\n");
    text += "| threads | Fennec | speedup | rayon | speedup | Fennec's speed, % of rayon's |\n";
    text += "|---|---:|---:|---:|---:|---:|\n";
    let percent = |rust: f64, ours: f64| if ours > 0.0 { rust / ours * 100.0 } else { 0.0 };
    let _ = writeln!(
        text,
        "| none, without `parallel` | {} | | {} | | {:.0} |",
        shown(ours_plain),
        shown(rust_plain),
        percent(rust_plain.best().0, ours_plain.best().0)
    );
    let ones = threaded.first().map_or((0.0, 0.0), |[ours, rust]| (ours.best().0, rust.best().0));
    let ratio = |one: f64, many: f64| if many > 0.0 { one / many } else { 0.0 };
    for (count, [ours, rust]) in THREADS.iter().zip(threaded) {
        let (mine, theirs) = (ours.best().0, rust.best().0);
        let _ = writeln!(
            text,
            "| {count} | {} | {:.2}× | {} | {:.2}× | {:.0} |",
            shown(ours),
            ratio(ones.0, mine),
            shown(rust),
            ratio(ones.1, theirs),
            percent(theirs, mine)
        );
    }
    let _ = write!(
        text,
        "\nBest of each side's runs, by wall clock, taking turns. Without `parallel` is the same kernel on one thread, \
         against plain Rust with no rayon. On one thread, `parallel` costs Fennec {:.1}% and rayon {:.1}% over that. \
         Speedup is against the side's own time at 1 thread.\n",
        (ones.0 / ours_plain.best().0 - 1.0) * 100.0,
        (ones.1 / rust_plain.best().0 - 1.0) * 100.0
    );
    text
}

/// Runs the section. It fails only for a kernel that doesn't build or prints the wrong thing.
pub(crate) fn run(fennec: &Path, dir: &Path, runs: usize, markdown: Option<&Path>) -> Result<(), String> {
    let out = Path::new(OUT_DIR);
    std::fs::create_dir_all(out).map_err(|err| format!("couldn't make {}: {err}", out.display()))?;
    let threaded = built(fennec, &dir.join(format!("{KERNEL}.fen")), out.join(KERNEL), true)?;
    let plain = format!("{KERNEL}_plain");
    let plain = built(fennec, &dir.join(format!("{plain}.fen")), out.join(plain), false)?;
    let twin = twins(dir, out)?;
    let mut plain = [Side::new(plain, None), Side::new(twin.join(format!("{KERNEL}_plain")), None)];
    let mut sides: Vec<[Side; 2]> = THREADS
        .iter()
        .map(|&count| {
            [
                Side::new(threaded.clone(), Some(("FENNEC_THREADS", count))),
                Side::new(twin.join(KERNEL), Some(("RAYON_NUM_THREADS", count))),
            ]
        })
        .collect();
    for _ in 0..runs {
        let expected = plain.iter_mut().map(Side::run).collect::<Result<Vec<_>, _>>()?;
        let want = expected.last().cloned().unwrap_or_default();
        let printed = sides.iter_mut().flatten().map(Side::run).collect::<Result<Vec<_>, _>>()?;
        if let Some(wrong) = expected.iter().chain(&printed).find(|said| **said != want) {
            return Err(format!("`{KERNEL}` printed `{}`, and its twin `{}`", wrong.trim(), want.trim()));
        }
    }
    let text = section(&plain, &sides);
    println!("{text}");
    if let Some(path) = markdown {
        std::fs::write(path, &text).map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Side, section};
    use std::time::Duration;

    fn side(millis: u64) -> Side {
        let mut side = Side::new("program".into(), None);
        side.times = vec![Duration::from_millis(millis); 3];
        side
    }

    #[test]
    fn the_section_shows_each_side_s_speedup_and_fennec_against_rayon() {
        let plain = [side(90), side(100)];
        let threaded = [[side(100), side(110)], [side(50), side(44)]];
        let text = section(&plain, &threaded);
        assert!(text.contains("| none, without `parallel` | 90 ms | | 100 ms | | 111 |"), "{text}");
        assert!(text.contains("| 1 | 100 ms | 1.00× | 110 ms | 1.00× | 110 |"), "{text}");
        assert!(text.contains("| 2 | 50 ms | 2.00× | 44 ms | 2.50× | 88 |"), "{text}");
        assert!(text.contains("costs Fennec 11.1% and rayon 10.0%"), "{text}");
    }

    #[test]
    fn a_side_whose_median_run_is_far_from_its_best_is_called_noisy() {
        let mut noisy = side(100);
        noisy.times = [100, 120, 130].map(Duration::from_millis).to_vec();
        assert_eq!(noisy.best(), (0.1, true));
        assert_eq!(side(100).best(), (0.1, false));
    }
}
