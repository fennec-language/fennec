//! `fennec-bench llvm-suite`: every kernel built by `fennec build --llvm --release` beside its
//! overflow-checked Rust twin, the way the LLVM lane compares them. Each is counted where
//! instructions can be counted, and timed on whatever machine this runs on: it and its twin run
//! ten times each, taking turns and swapping which goes first, and each side's best run counts,
//! which filters out other work on a shared machine. The medians stay beside them, so noise
//! shows. Speeds are a percentage of the twin's: 100 matches Rust, and higher is faster.
//!
//! It prints one Markdown table, and with `--json PATH` writes the same as one JSON object for a
//! history of runs. Each kernel's spread is shown too: how far a median run is from the best,
//! which is the noise the best run hides. One with a wide spread is called noisy. Nothing here
//! fails for being slow: only a kernel that prints something other than its twin does.
//!
//! A kernel with Go and Java twins, where those are installed, is timed against them as well,
//! the same way, so the table says where Fennec stands against each. They're for showing:
//! Rust is the twin the LLVM backend is held to.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fennec_lsp::Json;

use crate::kernels::Kernel;
use crate::measure::{Counter, Overflow, Program};
use crate::other_twins::Language;
use crate::startup::Startups;
use fennec_count::Misses;

/// How far a side's median run may be from its best, as a share of the best, before the kernel
/// is called noisy.
pub(crate) const NOISY: f64 = 0.05;

/// What one kernel came to.
struct Row {
    name: String,
    /// Instructions, best time and median time as a percentage of the twin's speed, when they
    /// could be measured.
    instructions: Option<f64>,
    time: Option<f64>,
    median: Option<f64>,
    /// How far the median run is from the best on whichever side it's farther, as a share of the
    /// best.
    spread: f64,
    /// Why it has no numbers: it doesn't build, or it prints the wrong thing.
    problem: Option<String>,
    /// Against each of `Language::ALL`'s twins: best time as a percentage of that twin's speed,
    /// or why there's none.
    others: Vec<Versus>,
    /// The best whole runs, in seconds: the LLVM build's and the Rust twin's.
    best: (Option<f64>, Option<f64>),
    /// The cached dev build's best time as a percentage of the twin's speed.
    dev: Option<f64>,
    /// How long the release and dev builds took, each from an empty Fennec home, in seconds.
    builds: (Option<f64>, Option<f64>),
    /// The LLVM build's and the Rust twin's cache misses, where Valgrind can simulate them.
    misses: (Option<Misses>, Option<Misses>),
    /// The kernel's own best time, in seconds, as the LLVM build and the Rust twin measured it,
    /// and the LLVM build's as a percentage of the twin's speed.
    kernel_time: (Option<f64>, Option<f64>),
    kernel: Option<f64>,
}

/// How a kernel did against one of its other twins.
#[derive(Default)]
struct Versus {
    /// The twin's best whole run, and its kernel's best own time, in seconds.
    best: Option<f64>,
    kernel_best: Option<f64>,
    /// The whole run's best time, as a percentage of the twin's speed.
    time: Option<f64>,
    /// The kernel's own best time, as each program measured it, as a percentage of the twin's.
    kernel: Option<f64>,
    problem: Option<String>,
}

impl Row {
    fn new(name: &str) -> Row {
        Row {
            name: name.to_owned(),
            instructions: None,
            time: None,
            median: None,
            spread: 0.0,
            problem: None,
            others: Language::ALL.iter().map(|_| Versus::default()).collect(),
            best: (None, None),
            dev: None,
            builds: (None, None),
            misses: (None, None),
            kernel_time: (None, None),
            kernel: None,
        }
    }
}

/// Times as fractions of a second, sorted.
fn sorted(times: &[Duration]) -> Vec<f64> {
    let mut seconds: Vec<f64> = times.iter().map(Duration::as_secs_f64).collect();
    seconds.sort_by(f64::total_cmp);
    seconds
}

/// The best of some times, their median, and their spread: how far the median is from the best,
/// as a share of the best.
pub(crate) fn summary(times: &[Duration]) -> (f64, f64, f64) {
    let seconds = sorted(times);
    let best = seconds.first().copied().unwrap_or(0.0);
    let middle = seconds.get(seconds.len() / 2).copied().unwrap_or(0.0);
    (best, middle, if best > 0.0 { (middle - best) / best } else { 0.0 })
}

/// The twin's time over ours, as a percentage.
fn speed(rust: f64, mine: f64) -> Option<f64> {
    (mine > 0.0).then(|| rust / mine * 100.0)
}

/// Builds a kernel through LLVM into `out_dir`, or says why it doesn't build.
pub(crate) fn built(fennec: &Path, source: &str, out_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let executable = out_dir.join(format!("{name}.llvm"));
    build(fennec, source, &executable, true, None).map(|_| executable)
}

/// Builds a kernel through LLVM to `executable`, a release build or the cached dev one, and
/// says how long that took. With `home`, it builds with a Fennec home of its own, empty, so
/// nothing another kernel's build cached can speed it up.
fn build(
    fennec: &Path,
    source: &str,
    executable: &Path,
    release: bool,
    home: Option<&Path>,
) -> Result<f64, String> {
    let mut command = Command::new(fennec);
    command
        .args(["build", "--llvm"])
        .args(release.then_some("--release"))
        .args([source, "-o"])
        .arg(executable);
    if let Some(home) = home {
        command.env("FENNEC_HOME", home);
    }
    let started = Instant::now();
    let output = command.output().map_err(|err| format!("couldn't run {}: {err}", fennec.display()))?;
    let took = started.elapsed().as_secs_f64();
    if output.status.success() {
        return Ok(took);
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(said.lines().next().unwrap_or("it didn't build").trim_start_matches("error: ").to_owned())
}

/// The header of every kernel's `main`, which is the last thing in its file.
const KERNEL_MAIN: &str = "def main(stdout: Stdout):\n";

/// A kernel that takes `--time` as its twins do, written to `out_dir`: its `main` also takes
/// the clock and the arguments, notes when it starts, and with any argument prints
/// `kernel-ns N` last, from the clock. It's added here rather than written in the kernel, so
/// the kernel stays exactly the code its targets hold it to. None for a kernel whose `main` isn't
/// written that way, or isn't last, which is then timed only as a whole run.
fn timed_copy(source: &str, out_dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(source).ok()?;
    let at = text.find(KERNEL_MAIN)?;
    let body = text.get(at + KERNEL_MAIN.len()..)?;
    if body.lines().any(|line| !line.is_empty() && !line.starts_with(' ')) {
        return None;
    }
    let timed = format!(
        "{}def main(stdout: Stdout, clock: Clock, args: Args):\n    started = clock.now()\n{}\n    if args.len() > 0:\n        stdout.print(\"kernel-ns\", (clock.now() - started).nanos)\n",
        text.get(..at)?,
        body.trim_end()
    );
    let path = out_dir.join(format!("{name}.timed.fen"));
    std::fs::write(&path, timed).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// What one side's runs took: each whole run, and the kernel's own time each run said.
#[derive(Default)]
struct Times {
    whole: Vec<Duration>,
    kernel: Vec<Duration>,
}

impl Times {
    /// The best whole run, in seconds.
    fn best(&self) -> f64 {
        summary(&self.whole).0
    }

    /// The best of the kernel's own times, in seconds, if every run said one.
    fn best_kernel(&self) -> Option<f64> {
        (!self.kernel.is_empty() && self.kernel.len() == self.whole.len()).then(|| summary(&self.kernel).0)
    }
}

/// A program run with `--time`, which makes each kernel and twin print its kernel's own time
/// last, measured with a monotonic clock, as `kernel-ns N`.
fn timing(program: &Program) -> Program {
    let mut args = program.args.clone();
    args.push("--time".to_owned());
    Program { command: program.command.clone(), args, stdin: program.stdin.clone() }
}

/// What a program printed without the `kernel-ns N` line, and the time that line says.
fn kernel_time(stdout: &str) -> (String, Option<Duration>) {
    let mut kept = String::with_capacity(stdout.len());
    let mut took = None;
    for line in stdout.split_inclusive('\n') {
        match line.trim_end().strip_prefix("kernel-ns ") {
            Some(nanos) => took = nanos.trim().parse().ok().map(Duration::from_nanos),
            None => kept.push_str(line),
        }
    }
    (kept, took)
}

/// Runs a kernel and a twin `runs` times each, taking turns and swapping which goes first,
/// each with `--time`, and gives back what each side took, or why they can't be compared.
fn race(kernel: &Kernel, ours: &Program, theirs: &Program, runs: usize) -> Result<(Times, Times), String> {
    let (ours, theirs) = (timing(ours), timing(theirs));
    let (mut mine, mut twins) = (Times::default(), Times::default());
    for round in 0..runs {
        let ran = if round % 2 == 0 {
            (ours.run(), theirs.run())
        } else {
            let twin = theirs.run();
            (ours.run(), twin)
        };
        let (ours_ran, twin_ran) = match ran {
            (Ok(ours), Ok(twin)) => (ours, twin),
            (Err(message), _) | (_, Err(message)) => {
                return Err(message.lines().next().unwrap_or("it failed").to_owned());
            }
        };
        let ((our_output, our_kernel), (their_output, their_kernel)) =
            (kernel_time(&ours_ran.stdout), kernel_time(&twin_ran.stdout));
        if kernel.output_hash(&our_output) != kernel.output_hash(&their_output) {
            return Err(MISMATCH.to_owned());
        }
        mine.whole.push(ours_ran.took);
        twins.whole.push(twin_ran.took);
        mine.kernel.extend(our_kernel);
        twins.kernel.extend(their_kernel);
    }
    Ok((mine, twins))
}

/// What a kernel that prints something other than its twin is said to do.
const MISMATCH: &str = "prints something other than its twin";

/// Runs a kernel and its Rust twin `runs` times each, taking turns and swapping which goes
/// first, and compares them.
fn measured(kernel: &Kernel, llvm: &Program, twin: &Program, counter: Option<&Counter>, runs: usize) -> Row {
    let mut row = Row::new(&kernel.name);
    let (ours, theirs) = match race(kernel, llvm, twin, runs) {
        Ok(times) => times,
        Err(problem) => {
            row.problem = Some(problem);
            return row;
        }
    };
    let ((my_best, my_median, my_spread), (rust_best, rust_median, rust_spread)) =
        (summary(&ours.whole), summary(&theirs.whole));
    row.kernel_time = (ours.best_kernel(), theirs.best_kernel());
    if let (Some(mine), Some(rust)) = row.kernel_time {
        row.kernel = speed(rust, mine);
    }
    row.time = speed(rust_best, my_best);
    row.best = (Some(my_best), Some(rust_best));
    row.median = speed(rust_median, my_median);
    row.spread = my_spread.max(rust_spread);
    if let Some(counter) = counter
        && let (Some(mine), Some(rust)) = (counter.count(llvm), counter.count(twin))
    {
        #[expect(clippy::cast_precision_loss, reason = "counts are far below 2^52")]
        let ratio = rust as f64 / mine.max(1) as f64;
        row.instructions = Some(ratio * 100.0);
    }
    row.misses = (fennec_count::cache_misses(llvm), fennec_count::cache_misses(twin));
    row
}

/// The kernel built the cached dev way, timed building from an empty `home` and raced against
/// its Rust twin. A dev build that doesn't build, or prints something else, is noted.
fn dev_build(
    row: &mut Row,
    kernel: &Kernel,
    (fennec, source): (&Path, &str),
    twin: &Program,
    home: &Path,
    (out_dir, runs): (&Path, usize),
) {
    let executable = out_dir.join(format!("{}.dev", kernel.name));
    let raced = build(fennec, source, &executable, false, Some(home)).and_then(|took| {
        row.builds.1 = Some(took);
        race(kernel, &Program { command: executable, args: Vec::new(), stdin: None }, twin, runs)
    });
    match raced {
        Ok((ours, theirs)) => row.dev = speed(theirs.best(), ours.best()),
        Err(problem) => row.problem = Some(format!("dev build: {problem}")),
    }
}

/// What every race of one kernel shares: its LLVM build, where twins are built, and how many
/// runs each side gets.
struct Racing<'r> {
    llvm: &'r Program,
    out_dir: &'r Path,
    runs: usize,
}

/// A kernel against its twin in `language`, if it has one and the language is installed.
fn versus(kernel: &Kernel, source: &str, racing: &Racing<'_>, language: Language) -> Versus {
    let Racing { llvm, out_dir, runs } = *racing;
    let Some(twin) = language.twin_of(source) else { return Versus::default() };
    let raced = language.build(&twin, out_dir).and_then(|program| race(kernel, llvm, &program, runs));
    match raced {
        Ok((ours, theirs)) => {
            let kernel = match (ours.best_kernel(), theirs.best_kernel()) {
                (Some(ours), Some(theirs)) => speed(theirs, ours),
                _ => None,
            };
            Versus {
                best: Some(theirs.best()),
                kernel_best: theirs.best_kernel(),
                time: speed(theirs.best(), ours.best()),
                kernel,
                problem: None,
            }
        }
        Err(problem) => Versus { problem: Some(problem), ..Versus::default() },
    }
}

/// The geometric mean of what's there.
fn geomean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values
        .filter(|value| *value > 0.0)
        .fold((0.0, 0_i32), |(sum, count), value| (sum + value.ln(), count + 1));
    (count > 0).then(|| (sum / f64::from(count)).exp())
}

/// A count, short: `1.2M`, `34.5k` or `678`.
fn short(count: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "shown to one decimal place")]
    let value = count as f64;
    if count >= 1_000_000 {
        format!("{:.1}M", value / 1e6)
    } else if count >= 1_000 {
        format!("{:.1}k", value / 1e3)
    } else {
        count.to_string()
    }
}

/// One kind of cache miss for the LLVM build and the twin, as `ours / Rust's`.
fn both(misses: (Option<Misses>, Option<Misses>), kind: fn(Misses) -> u64) -> String {
    match misses {
        (Some(ours), Some(rust)) => format!("{} / {}", short(kind(ours)), short(kind(rust))),
        _ => "-".to_owned(),
    }
}

/// Seconds as whole milliseconds, for build times.
fn millis(seconds: Option<f64>) -> String {
    seconds.map_or_else(|| "-".to_owned(), |seconds| format!("{:.0} ms", seconds * 1000.0))
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.0}"))
}

/// The machine this runs on, like `x86_64-linux`.
fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The slowest kernel by time, with its speed.
fn slowest(rows: &[Row]) -> Option<(&str, f64)> {
    rows.iter().filter_map(|row| Some((row.name.as_str(), row.time?))).min_by(|a, b| a.1.total_cmp(&b.1))
}

/// The table, then the geometric means and the slowest kernel.
fn table(rows: &[Row], counter: Option<&Counter>, startups: &Startups) -> String {
    let mut text = format!(
        "## LLVM against Rust, Go and Java on {}, {} with {} cores\n\n",
        platform(),
        crate::machine::cpu(),
        crate::machine::cores()
    );
    text.push_str("| kernel | instructions % | D1 misses, ours / Rust's | LL misses, ours / Rust's | best time % | kernel time % | median time % | spread | dev best time % | release build | dev build |");
    for language in Language::ALL {
        let _ = write!(text, " {0} kernel time % | {0} whole run % |", language.name());
    }
    text.push_str(" |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    text.push_str(&"---:|---:|".repeat(Language::ALL.len()));
    text.push_str("---|\n");
    for row in rows {
        let mut notes: Vec<String> = match &row.problem {
            Some(problem) => vec![problem.clone()],
            None if row.spread > NOISY => vec!["noisy".to_owned()],
            None => Vec::new(),
        };
        for (language, versus) in Language::ALL.iter().zip(&row.others) {
            if let Some(problem) = &versus.problem {
                notes.push(format!("{}: {problem}", language.name()));
            }
        }
        let note = notes.join("; ").replace('|', "\\|");
        let mut others = String::new();
        for versus in &row.others {
            let _ = write!(others, " {} | {} |", percent(versus.kernel), percent(versus.time));
        }
        let spread =
            if row.problem.is_some() { "-".to_owned() } else { format!("{:.0}%", row.spread * 100.0) };
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} | {} | {} | {spread} | {} | {} | {} |{others} {note} |",
            row.name,
            percent(row.instructions),
            both(row.misses, |misses| misses.d1),
            both(row.misses, |misses| misses.ll),
            percent(row.time),
            percent(row.kernel),
            percent(row.median),
            percent(row.dev),
            millis(row.builds.0),
            millis(row.builds.1)
        );
    }
    text.push_str(&means(rows, counter, startups));
    text.push_str(
        "\n\nEach is the overflow-checked Rust twin's count or time over LLVM's, so 100 matches Rust and \
         higher is faster. D1 and LL misses are counts, not percentages: each side's misses in the \
         first-level data cache, and in the last level, data and instructions both, from cachegrind \
         simulating fixed caches (32 KiB first level, 8 MiB last level, 64-byte lines), as \
         repeatable as its instruction counts. Times are each side's best and median of runs that \
         take turns. Spread is \
         how far a median run is from the best, on the side where it's farther. Dev is the build \
         `fennec build --llvm` makes without `--release`, cached a function at a time, raced \
         against the twin the same way. Each build's time is from an empty Fennec home, so no \
         cache helps it.\n\nThe Go and Java \
         columns compare best times the same way. Kernel time is what each program measured \
         itself with a monotonic clock, from the start of its work to its last line, when run with \
         `--time`; Java's is its second run, after one run to warm its JIT up. Whole run is each \
         program from start to exit, startup included; Java's includes the JVM starting and its \
         JIT warming up. Startup is the best run of a hello-world in each language.\n",
    );
    text
}

/// The geometric means, the slowest kernel, and each language's startup, after the table.
fn means(rows: &[Row], counter: Option<&Counter>, startups: &Startups) -> String {
    let mut text = String::new();
    let instructions = geomean(rows.iter().filter_map(|row| row.instructions));
    let time = geomean(rows.iter().filter_map(|row| row.time));
    let median = geomean(rows.iter().filter_map(|row| row.median));
    let dev = geomean(rows.iter().filter_map(|row| row.dev));
    let kernel = geomean(rows.iter().filter_map(|row| row.kernel));
    let _ = write!(
        text,
        "\nGeometric mean: {}% by instructions, {}% by best time, {}% by kernel time, {}% by median time, and the dev build {}% by best time.",
        percent(instructions),
        percent(time),
        percent(kernel),
        percent(median),
        percent(dev)
    );
    if let Some((name, speed)) = slowest(rows) {
        let _ = write!(text, " Slowest: {name}, at {speed:.0}% of its twin's speed.");
    }
    for (at, language) in Language::ALL.iter().enumerate() {
        let mean = geomean(rows.iter().filter_map(|row| row.others.get(at)?.time));
        let kernels = geomean(rows.iter().filter_map(|row| row.others.get(at)?.kernel));
        match mean {
            Some(mean) => {
                let _ = write!(
                    text,
                    " Against {}: {}% by kernel time, {mean:.0}% by whole run.",
                    language.name(),
                    percent(kernels)
                );
            }
            None if !language.installed() => {
                let _ = write!(text, " {} isn't installed here.", language.name());
            }
            None => {}
        }
    }
    if counter.is_none() {
        text.push_str(" Nothing could count instructions here.");
    }
    let mut started = vec![("Fennec", startups.fennec), ("Rust", startups.rust)];
    started.extend(Language::ALL.iter().map(|language| language.name()).zip(startups.others.iter().copied()));
    let started: Vec<String> = started
        .iter()
        .filter_map(|(name, seconds)| Some(format!("{name} {:.1} ms", (*seconds)? * 1000.0)))
        .collect();
    if !started.is_empty() {
        let _ = write!(text, " Startup, a hello-world's best run: {}.", started.join(", "));
    }
    text
}

fn number(value: Option<f64>) -> Json {
    value.map_or(Json::Null, |value| Json::Number((value * 10.0).round() / 10.0))
}

/// Each side's best in milliseconds, for the history: its whole run with `whole`, or its
/// kernel's own time.
fn best_ms(row: &Row, whole: bool) -> Json {
    let ms = |seconds: Option<f64>| number(seconds.map(|seconds| seconds * 1000.0));
    let (fennec, rust) = if whole { row.best } else { row.kernel_time };
    let mut fields = vec![("fennec".to_owned(), ms(fennec)), ("rust".to_owned(), ms(rust))];
    for (language, versus) in Language::ALL.iter().zip(&row.others) {
        fields.push((language.key().to_owned(), ms(if whole { versus.best } else { versus.kernel_best })));
    }
    Json::Object(fields)
}

/// The cache misses, for the history: `{"fennec": {"d1": …, "ll": …}, "rust": {…}}`, or null
/// where they weren't simulated.
fn misses_json(misses: (Option<Misses>, Option<Misses>)) -> Json {
    let side = |misses: Option<Misses>| {
        misses.map_or(Json::Null, |misses| {
            #[expect(clippy::cast_precision_loss, reason = "miss counts are far below 2^52")]
            let (d1, ll) = (misses.d1 as f64, misses.ll as f64);
            Json::object([("d1", Json::Number(d1)), ("ll", Json::Number(ll))])
        })
    };
    match misses {
        (None, None) => Json::Null,
        (ours, rust) => Json::object([("fennec", side(ours)), ("rust", side(rust))]),
    }
}

/// Each language's startup in milliseconds, for the history.
fn startup_json(startups: &Startups) -> Json {
    let ms = |seconds: Option<f64>| number(seconds.map(|seconds| seconds * 1000.0));
    let mut fields = vec![("fennec".to_owned(), ms(startups.fennec)), ("rust".to_owned(), ms(startups.rust))];
    for (language, seconds) in Language::ALL.iter().zip(&startups.others) {
        fields.push((language.key().to_owned(), ms(*seconds)));
    }
    Json::Object(fields)
}

/// The run as one JSON object, for a history of runs.
fn json(rows: &[Row], startups: &Startups) -> Json {
    let kernels = rows.iter().map(|row| {
        let fields = Json::object([
            ("name", Json::string(row.name.as_str())),
            ("instructions", number(row.instructions)),
            ("time", number(row.time)),
            ("median", number(row.median)),
            ("spread", number(Some(row.spread * 100.0))),
            ("noisy", Json::Bool(row.problem.is_none() && row.spread > NOISY)),
            ("problem", row.problem.as_ref().map_or(Json::Null, |problem| Json::string(problem.as_str()))),
            ("best_ms", best_ms(row, true)),
            ("kernel", number(row.kernel)),
            ("kernel_ms", best_ms(row, false)),
            ("dev", number(row.dev)),
            ("misses", misses_json(row.misses)),
            ("release_build_ms", number(row.builds.0.map(|seconds| seconds * 1000.0))),
            ("dev_build_ms", number(row.builds.1.map(|seconds| seconds * 1000.0))),
        ]);
        let Json::Object(mut fields) = fields else { return fields };
        for (language, versus) in Language::ALL.iter().zip(&row.others) {
            fields.push((language.key().to_owned(), number(versus.time)));
            fields.push((format!("{}_kernel", language.key()), number(versus.kernel)));
            let problem =
                versus.problem.as_ref().map_or(Json::Null, |problem| Json::string(problem.as_str()));
            fields.push((format!("{}_problem", language.key()), problem));
        }
        Json::Object(fields)
    });
    Json::object([
        ("platform", Json::string(platform())),
        ("cpu", Json::string(crate::machine::cpu())),
        ("cores", Json::number(crate::machine::cores())),
        ("commit", Json::string(std::env::var("GITHUB_SHA").unwrap_or_default())),
        ("instructions", number(geomean(rows.iter().filter_map(|row| row.instructions)))),
        ("time", number(geomean(rows.iter().filter_map(|row| row.time)))),
        ("kernel", number(geomean(rows.iter().filter_map(|row| row.kernel)))),
        ("median", number(geomean(rows.iter().filter_map(|row| row.median)))),
        ("slowest", slowest(rows).map_or(Json::Null, |(name, _)| Json::string(name))),
        ("dev", number(geomean(rows.iter().filter_map(|row| row.dev)))),
        ("go", number(geomean(rows.iter().filter_map(|row| row.others.first()?.time)))),
        ("java", number(geomean(rows.iter().filter_map(|row| row.others.get(1)?.time)))),
        ("go_kernel", number(geomean(rows.iter().filter_map(|row| row.others.first()?.kernel)))),
        ("java_kernel", number(geomean(rows.iter().filter_map(|row| row.others.get(1)?.kernel)))),
        ("startup_ms", startup_json(startups)),
        ("kernels", Json::Array(kernels.collect())),
    ])
}

/// Measures every kernel that's one file with a twin of its own. Fails only when a kernel prints
/// something other than its twin.
pub(crate) fn run(
    fennec: &Path,
    kernels_dir: &Path,
    runs: usize,
    json_path: Option<&Path>,
) -> Result<bool, String> {
    let out_dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-llvm-suite-{}", std::process::id())));
    std::fs::create_dir_all(&out_dir).map_err(|err| format!("couldn't make {}: {err}", out_dir.display()))?;
    let counter = Counter::find(&out_dir);
    let kernels = crate::kernels::find(kernels_dir, &out_dir, Path::new("selfhost"))?;
    let installed: Vec<Option<Language>> =
        Language::ALL.iter().map(|language| language.installed().then_some(*language)).collect();
    let startup_dir = kernels_dir.parent().unwrap_or(kernels_dir).join("startup");
    let startups = crate::startup::measure(fennec, &startup_dir, &out_dir, &installed, runs);
    let mut rows = Vec::new();
    for kernel in &kernels {
        let Some(source) = kernel.single_file() else { continue };
        // Built from a copy that times its own work, as the twins do; its twins are found
        // beside the kernel itself.
        let timed = timed_copy(source, &out_dir, &kernel.name);
        let built_from = timed.as_deref().unwrap_or(source);
        let executable = out_dir.join(format!("{}.llvm", kernel.name));
        let home = |which: &str| out_dir.join(format!("home-{}-{which}", kernel.name));
        let release = build(fennec, built_from, &executable, true, Some(&home("release")));
        let row = match release.map(|took| (executable, took)) {
            Err(problem) => {
                let mut row = Row::new(&kernel.name);
                row.problem = Some(format!("doesn't build: {problem}"));
                row
            }
            Ok((executable, release_build)) => {
                let twin = kernel.twin(fennec, &out_dir, Overflow::Checked)?;
                let llvm = Program { command: executable, args: Vec::new(), stdin: None };
                let mut row = measured(kernel, &llvm, &twin, counter.as_ref(), runs);
                row.builds.0 = Some(release_build);
                if row.problem.is_none() {
                    dev_build(&mut row, kernel, (fennec, built_from), &twin, &home("dev"), (&out_dir, runs));
                    let racing = Racing { llvm: &llvm, out_dir: &out_dir, runs };
                    row.others = installed
                        .iter()
                        .map(|language| match language {
                            Some(language) => versus(kernel, source, &racing, *language),
                            None => Versus::default(),
                        })
                        .collect();
                }
                row
            }
        };
        rows.push(row);
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    let text = table(&rows, counter.as_ref(), &startups);
    print!("{text}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        crate::append(Path::new(&summary), &text)
            .map_err(|err| format!("couldn't write the summary: {err}"))?;
    }
    if let Some(path) = json_path {
        std::fs::write(path, format!("{}\n", json(&rows, &startups)))
            .map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(!rows.iter().any(|row| row.problem.as_deref() == Some(MISMATCH)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{geomean, kernel_time, summary, timed_copy};

    #[test]
    fn the_best_run_counts_and_the_spread_is_how_far_the_median_is_from_it() {
        let times: Vec<Duration> = [10, 11, 12, 13, 40].iter().map(|ms| Duration::from_millis(*ms)).collect();
        let (best, median, spread) = summary(&times);
        assert!((best - 0.010).abs() < 1e-9);
        assert!((median - 0.012).abs() < 1e-9);
        assert!((spread - 0.2).abs() < 1e-9, "{spread}");
    }

    #[test]
    fn a_kernels_own_time_comes_off_what_it_printed() {
        let (printed, took) = kernel_time("9227465\nkernel-ns 1500\n");
        assert_eq!(printed, "9227465\n");
        assert_eq!(took, Some(Duration::from_nanos(1500)));
        assert_eq!(kernel_time("1 2\n"), ("1 2\n".to_owned(), None), "run without --time");
    }

    #[test]
    fn a_kernel_is_built_from_a_copy_that_times_its_main() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-timed-copy-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let kernel = dir.join("k.fen");
        std::fs::write(
            &kernel,
            "def f() -> int:\n    1\n\ndef main(stdout: Stdout):\n    stdout.print(f())\n",
        )
        .unwrap();
        let copy = timed_copy(kernel.to_str().unwrap(), &dir, "k").unwrap();
        assert_eq!(
            std::fs::read_to_string(copy).unwrap(),
            "def f() -> int:\n    1\n\ndef main(stdout: Stdout, clock: Clock, args: Args):\n    started = clock.now()\n    \
             stdout.print(f())\n    if args.len() > 0:\n        stdout.print(\"kernel-ns\", (clock.now() - started).nanos)\n"
        );
        std::fs::write(
            &kernel,
            "def main(stdout: Stdout):\n    stdout.print(1)\n\ndef later() -> int:\n    2\n",
        )
        .unwrap();
        assert_eq!(timed_copy(kernel.to_str().unwrap(), &dir, "k"), None, "main isn't last");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_geometric_mean_skips_what_wasnt_measured() {
        assert!((geomean([50.0, 200.0].into_iter()).unwrap_or(0.0) - 100.0).abs() < 1e-9);
        assert_eq!(geomean(std::iter::empty()), None);
    }
}
