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

/// How far a side's median run may be from its best, as a share of the best, before the kernel
/// is called noisy.
const NOISY: f64 = 0.05;

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
}

/// How a kernel did against one of its other twins.
#[derive(Default)]
struct Versus {
    /// The twin's best whole run, in seconds.
    best: Option<f64>,
    /// The whole run's best time, as a percentage of the twin's speed.
    time: Option<f64>,
    /// Roughly the kernel's: the best time with each side's startup taken off.
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
fn summary(times: &[Duration]) -> (f64, f64, f64) {
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

/// Runs a kernel and a twin `runs` times each, taking turns and swapping which goes first,
/// and gives back each side's times, or why they can't be compared.
fn race(
    kernel: &Kernel,
    ours: &Program,
    theirs: &Program,
    runs: usize,
) -> Result<(Vec<Duration>, Vec<Duration>), String> {
    let (mut mine, mut twins) = (Vec::with_capacity(runs), Vec::with_capacity(runs));
    for round in 0..runs {
        let ran = if round % 2 == 0 {
            (ours.run(), theirs.run())
        } else {
            let twin = theirs.run();
            (ours.run(), twin)
        };
        match ran {
            (Ok(ours), Ok(twin)) if kernel.output_hash(&ours.stdout) == kernel.output_hash(&twin.stdout) => {
                mine.push(ours.took);
                twins.push(twin.took);
            }
            (Ok(_), Ok(_)) => return Err(MISMATCH.to_owned()),
            (Err(message), _) | (_, Err(message)) => {
                return Err(message.lines().next().unwrap_or("it failed").to_owned());
            }
        }
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
        (summary(&ours), summary(&theirs));
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
        Ok((ours, theirs)) => row.dev = speed(summary(&theirs).0, summary(&ours).0),
        Err(problem) => row.problem = Some(format!("dev build: {problem}")),
    }
}

/// A kernel against its twin in `language`, if it has one and the language is installed, with
/// `startups`, ours then theirs, taken off for the kernel's own time.
/// What every race of one kernel shares: its LLVM build, where twins are built, and how many
/// runs each side gets.
struct Racing<'r> {
    llvm: &'r Program,
    out_dir: &'r Path,
    runs: usize,
}

fn versus(
    kernel: &Kernel,
    source: &str,
    racing: &Racing<'_>,
    language: Language,
    startups: (Option<f64>, Option<f64>),
) -> Versus {
    let Racing { llvm, out_dir, runs } = *racing;
    let Some(twin) = language.twin_of(source) else { return Versus::default() };
    let raced = language.build(&twin, out_dir).and_then(|program| race(kernel, llvm, &program, runs));
    match raced {
        Ok((ours, theirs)) => {
            let (ours, theirs) = (summary(&ours).0, summary(&theirs).0);
            let kernel = match startups {
                (Some(our_startup), Some(their_startup)) => {
                    crate::startup::kernel_speed(ours, our_startup, theirs, their_startup)
                }
                _ => None,
            };
            Versus { best: Some(theirs), time: speed(theirs, ours), kernel, problem: None }
        }
        Err(problem) => Versus { best: None, time: None, kernel: None, problem: Some(problem) },
    }
}

/// The geometric mean of what's there.
fn geomean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values
        .filter(|value| *value > 0.0)
        .fold((0.0, 0_i32), |(sum, count), value| (sum + value.ln(), count + 1));
    (count > 0).then(|| (sum / f64::from(count)).exp())
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
    let mut text = format!("## LLVM against Rust, Go and Java on {}\n\n", platform());
    text.push_str("| kernel | instructions % | best time % | median time % | spread | dev best time % | release build | dev build |");
    for language in Language::ALL {
        let _ = write!(text, " {0} kernel % ≈ | {0} whole run % |", language.name());
    }
    text.push_str(" |\n|---|---:|---:|---:|---:|---:|---:|---:|");
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
            "| {} | {} | {} | {} | {spread} | {} | {} | {} |{others} {note} |",
            row.name,
            percent(row.instructions),
            percent(row.time),
            percent(row.median),
            percent(row.dev),
            millis(row.builds.0),
            millis(row.builds.1)
        );
    }
    text.push_str(&means(rows, counter, startups));
    text.push_str(
        "\n\nEach is the overflow-checked Rust twin's count or time over LLVM's, so 100 matches Rust and \
         higher is faster. Times are each side's best and median of runs that take turns. Spread is \
         how far a median run is from the best, on the side where it's farther. Dev is the build \
         `fennec build --llvm` makes without `--release`, cached a function at a time, raced \
         against the twin the same way. Each build's time is from an empty Fennec home, so no \
         cache helps it.\n\nThe Go and Java \
         columns compare best times the same way. Whole run is each program from start to exit. \
         Kernel ≈ takes each language's startup off its whole run first: startup is the best run of \
         a hello-world in that language, so this is only roughly the kernel's time, and it's left out \
         where either side's kernel took less time than its startup, which leaves mostly noise. \
         Java's whole runs include the JVM starting and its JIT warming up, and its kernel times \
         still include the warming up.\n",
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
    let _ = write!(
        text,
        "\nGeometric mean: {}% by instructions, {}% by best time, {}% by median time, and the dev build {}% by best time.",
        percent(instructions),
        percent(time),
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
                    " Against {}: roughly {}% by kernel time, {mean:.0}% by whole run.",
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

/// Each side's best whole run in milliseconds, which less its language's startup is roughly
/// what its kernel took.
fn best_ms(row: &Row) -> Json {
    let ms = |seconds: Option<f64>| number(seconds.map(|seconds| seconds * 1000.0));
    let mut fields = vec![("fennec".to_owned(), ms(row.best.0)), ("rust".to_owned(), ms(row.best.1))];
    for (language, versus) in Language::ALL.iter().zip(&row.others) {
        fields.push((language.key().to_owned(), ms(versus.best)));
    }
    Json::Object(fields)
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
            ("best_ms", best_ms(row)),
            ("dev", number(row.dev)),
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
        ("commit", Json::string(std::env::var("GITHUB_SHA").unwrap_or_default())),
        ("instructions", number(geomean(rows.iter().filter_map(|row| row.instructions)))),
        ("time", number(geomean(rows.iter().filter_map(|row| row.time)))),
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
    let out_dir = std::env::temp_dir().join(format!("fennec-llvm-suite-{}", std::process::id()));
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
        let executable = out_dir.join(format!("{}.llvm", kernel.name));
        let home = |which: &str| out_dir.join(format!("home-{}-{which}", kernel.name));
        let release = build(fennec, source, &executable, true, Some(&home("release")));
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
                    dev_build(&mut row, kernel, (fennec, source), &twin, &home("dev"), (&out_dir, runs));
                    row.others = installed
                        .iter()
                        .zip(&startups.others)
                        .map(|(language, their_startup)| match language {
                            Some(language) => {
                                let both = (startups.fennec, *their_startup);
                                let racing = Racing { llvm: &llvm, out_dir: &out_dir, runs };
                                versus(kernel, source, &racing, *language, both)
                            }
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

    use super::{geomean, summary};

    #[test]
    fn the_best_run_counts_and_the_spread_is_how_far_the_median_is_from_it() {
        let times: Vec<Duration> = [10, 11, 12, 13, 40].iter().map(|ms| Duration::from_millis(*ms)).collect();
        let (best, median, spread) = summary(&times);
        assert!((best - 0.010).abs() < 1e-9);
        assert!((median - 0.012).abs() < 1e-9);
        assert!((spread - 0.2).abs() < 1e-9, "{spread}");
    }

    #[test]
    fn the_geometric_mean_skips_what_wasnt_measured() {
        assert!((geomean([50.0, 200.0].into_iter()).unwrap_or(0.0) - 100.0).abs() < 1e-9);
        assert_eq!(geomean(std::iter::empty()), None);
    }
}
