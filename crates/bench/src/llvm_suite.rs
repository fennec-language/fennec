//! `fennec-bench llvm-suite`: every kernel built by `fennec build --llvm --release` beside its
//! overflow-checked Rust twin, the way the LLVM lane compares them. Each is counted where
//! instructions can be counted, and timed on whatever machine this runs on, taking turns with
//! its twin. Speeds are a percentage of the twin's: 100 matches Rust, and higher is faster.
//!
//! It prints one Markdown table, and with `--json PATH` writes the same as one JSON object for a
//! history of runs. Time on a shared machine wanders, so each kernel's spread is shown, and one
//! that wanders too far is called noisy. Nothing here fails for being slow: only a kernel that
//! prints something other than its twin does.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fennec_lsp::Json;

use crate::kernels::Kernel;
use crate::measure::{Counter, Overflow, Program};

/// How far a kernel's times may spread, as a share of the median, before it's called noisy.
const NOISY: f64 = 0.10;

/// What one kernel came to.
struct Row {
    name: String,
    /// Instructions and time as a percentage of the twin's speed, when they could be measured.
    instructions: Option<f64>,
    time: Option<f64>,
    /// The spread of the slower of the two's middle runs, as a share of its median.
    spread: f64,
    /// Why it has no numbers: it doesn't build, or it prints the wrong thing.
    problem: Option<String>,
}

/// Times as fractions of a second, sorted.
fn sorted(times: &[Duration]) -> Vec<f64> {
    let mut seconds: Vec<f64> = times.iter().map(Duration::as_secs_f64).collect();
    seconds.sort_by(f64::total_cmp);
    seconds
}

/// The median of sorted times, and their spread: the range of all but the fastest and slowest
/// run, once there are five or more, over the median.
fn median_and_spread(times: &[Duration]) -> (f64, f64) {
    let seconds = sorted(times);
    let middle = seconds.get(seconds.len() / 2).copied().unwrap_or(0.0);
    let kept =
        if seconds.len() >= 5 { seconds.get(1..seconds.len() - 1).unwrap_or(&seconds) } else { &seconds };
    let range = kept.last().copied().unwrap_or(0.0) - kept.first().copied().unwrap_or(0.0);
    (middle, if middle > 0.0 { range / middle } else { 0.0 })
}

/// Builds a kernel through LLVM into `out_dir`, or says why it doesn't build.
fn built(fennec: &Path, source: &str, out_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let executable = out_dir.join(format!("{name}.llvm"));
    let output = Command::new(fennec)
        .args(["build", "--llvm", "--release", source, "-o"])
        .arg(&executable)
        .output()
        .map_err(|err| format!("couldn't run {}: {err}", fennec.display()))?;
    if output.status.success() {
        return Ok(executable);
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(said.lines().next().unwrap_or("it didn't build").trim_start_matches("error: ").to_owned())
}

/// Runs a kernel and its twin `runs` times each, taking turns, and compares them.
fn measured(kernel: &Kernel, llvm: &Program, twin: &Program, counter: Option<&Counter>, runs: usize) -> Row {
    let mut row =
        Row { name: kernel.name.clone(), instructions: None, time: None, spread: 0.0, problem: None };
    let (mut ours, mut theirs) = (Vec::with_capacity(runs), Vec::with_capacity(runs));
    for _ in 0..runs {
        match (llvm.run(), twin.run()) {
            (Ok(mine), Ok(rust)) if kernel.output_hash(&mine.stdout) == kernel.output_hash(&rust.stdout) => {
                ours.push(mine.took);
                theirs.push(rust.took);
            }
            (Ok(_), Ok(_)) => {
                row.problem = Some("prints something other than its twin".to_owned());
                return row;
            }
            (Err(message), _) | (_, Err(message)) => {
                row.problem = Some(message.lines().next().unwrap_or("it failed").to_owned());
                return row;
            }
        }
    }
    let ((mine, my_spread), (rust, rust_spread)) = (median_and_spread(&ours), median_and_spread(&theirs));
    row.time = (mine > 0.0).then(|| rust / mine * 100.0);
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

/// The geometric mean of what's there.
fn geomean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values
        .filter(|value| *value > 0.0)
        .fold((0.0, 0_i32), |(sum, count), value| (sum + value.ln(), count + 1));
    (count > 0).then(|| (sum / f64::from(count)).exp())
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
fn table(rows: &[Row], counter: Option<&Counter>) -> String {
    let mut text = format!("## LLVM against Rust on {}\n\n", platform());
    text.push_str("| kernel | instructions % | time % | spread | |\n|---|---:|---:|---:|---|\n");
    for row in rows {
        let note = match &row.problem {
            Some(problem) => problem.replace('|', "\\|"),
            None if row.spread > NOISY => "noisy".to_owned(),
            None => String::new(),
        };
        let spread =
            if row.problem.is_some() { "-".to_owned() } else { format!("{:.0}%", row.spread * 100.0) };
        let _ = writeln!(
            text,
            "| {} | {} | {} | {spread} | {note} |",
            row.name,
            percent(row.instructions),
            percent(row.time)
        );
    }
    let instructions = geomean(rows.iter().filter_map(|row| row.instructions));
    let time = geomean(rows.iter().filter_map(|row| row.time));
    let _ = write!(
        text,
        "\nGeometric mean: {}% by instructions, {}% by time.",
        percent(instructions),
        percent(time)
    );
    if let Some((name, speed)) = slowest(rows) {
        let _ = write!(text, " Slowest: {name}, at {speed:.0}% of its twin's speed.");
    }
    if counter.is_none() {
        text.push_str(" Nothing could count instructions here.");
    }
    text.push_str(
        "\n\nEach is the overflow-checked Rust twin's count or median time over LLVM's, so 100 matches \
         Rust and higher is faster. Spread is the range of the middle runs over the median.\n",
    );
    text
}

fn number(value: Option<f64>) -> Json {
    value.map_or(Json::Null, |value| Json::Number((value * 10.0).round() / 10.0))
}

/// The run as one JSON object, for a history of runs.
fn json(rows: &[Row]) -> Json {
    let kernels = rows.iter().map(|row| {
        Json::object([
            ("name", Json::string(row.name.as_str())),
            ("instructions", number(row.instructions)),
            ("time", number(row.time)),
            ("spread", number(Some(row.spread * 100.0))),
            ("noisy", Json::Bool(row.problem.is_none() && row.spread > NOISY)),
            ("problem", row.problem.as_ref().map_or(Json::Null, |problem| Json::string(problem.as_str()))),
        ])
    });
    Json::object([
        ("platform", Json::string(platform())),
        ("commit", Json::string(std::env::var("GITHUB_SHA").unwrap_or_default())),
        ("instructions", number(geomean(rows.iter().filter_map(|row| row.instructions)))),
        ("time", number(geomean(rows.iter().filter_map(|row| row.time)))),
        ("slowest", slowest(rows).map_or(Json::Null, |(name, _)| Json::string(name))),
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
    let mut rows = Vec::new();
    for kernel in &kernels {
        let Some(source) = kernel.single_file() else { continue };
        let row = match built(fennec, source, &out_dir, &kernel.name) {
            Err(problem) => Row {
                name: kernel.name.clone(),
                instructions: None,
                time: None,
                spread: 0.0,
                problem: Some(format!("doesn't build: {problem}")),
            },
            Ok(executable) => {
                let twin = kernel.twin(fennec, &out_dir, Overflow::Checked)?;
                let llvm = Program { command: executable, args: Vec::new(), stdin: None };
                measured(kernel, &llvm, &twin, counter.as_ref(), runs)
            }
        };
        rows.push(row);
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    let text = table(&rows, counter.as_ref());
    print!("{text}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        crate::append(Path::new(&summary), &text)
            .map_err(|err| format!("couldn't write the summary: {err}"))?;
    }
    if let Some(path) = json_path {
        std::fs::write(path, format!("{}\n", json(&rows)))
            .map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(!rows.iter().any(|row| row.problem.as_deref() == Some("prints something other than its twin")))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{geomean, median_and_spread};

    #[test]
    fn the_spread_leaves_out_the_fastest_and_slowest_run() {
        let times: Vec<Duration> = [10, 11, 12, 13, 40].iter().map(|ms| Duration::from_millis(*ms)).collect();
        let (median, spread) = median_and_spread(&times);
        assert!((median - 0.012).abs() < 1e-9);
        assert!((spread - 2.0 / 12.0).abs() < 1e-9, "{spread}");
    }

    #[test]
    fn the_geometric_mean_skips_what_wasnt_measured() {
        assert!((geomean([50.0, 200.0].into_iter()).unwrap_or(0.0) - 100.0).abs() < 1e-9);
        assert_eq!(geomean(std::iter::empty()), None);
    }
}
