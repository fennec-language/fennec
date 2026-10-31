//! `fennec-bench`: the benchmark suite. Each kernel in `bench/kernels/` is measured on its own,
//! against a target pinned in `bench/targets/`: how many instructions its work should take.
//! The table shows how long each kernel takes to start and to do its work, its speed as a
//! percentage of its target, and how it compares with the previous commit's `fennec`.
//!
//! A run fails if a kernel prints the wrong thing, or if one marked as enforced drops below the
//! JIT's floor, or got slower than before: by its median time and, where they're counted, its
//! instructions too.
//!
//! `fennec-bench pin` measures the targets again, from the twins that sit beside the kernels.
//! Everyday runs don't build or run the twins.
//!
//! ```text
//! fennec-bench [--fennec PATH] [--previous PATH] [--runs N] [--kernels DIR] [--targets DIR] [--report-only]
//! fennec-bench pin [--fennec PATH] [--kernels DIR] [--targets DIR]
//! fennec-bench budgets [--fennec PATH] [--runs N] [--files N]
//! ```
//!
//! `budgets` measures something else: see the `budgets` module.
#![expect(clippy::print_stdout, reason = "the suite prints its table")]
#![expect(clippy::print_stderr, reason = "the suite reports failures")]

mod budgets;
mod kernels;
mod measure;
mod report;
mod targets;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kernels::Kernel;
use measure::{Counter, time_together};
use report::Result;
use targets::{Target, Targets};

struct Options {
    fennec: PathBuf,
    previous: Option<PathBuf>,
    runs: usize,
    kernels: PathBuf,
    targets: PathBuf,
    /// Slowdowns are warnings, not failures, until the signal is proven stable on CI. A kernel
    /// below an enforced floor always fails.
    report_only: bool,
}

/// What a run found: every kernel's result, the kernels that got slower, and the errors,
/// like a kernel that printed the wrong thing or dropped below its floor.
struct Found {
    results: Vec<Result>,
    slowdowns: Vec<String>,
    errors: Vec<String>,
}

const OUT_DIR: &str = "target/bench";

/// How many times a twin is timed when it's pinned.
const PIN_RUNS: usize = 7;

/// `fennec-bench budgets`: fails when an enforced budget isn't met.
fn budgets(mut args: impl Iterator<Item = String>) -> std::result::Result<bool, String> {
    let (mut fennec, mut runs, mut files) = (PathBuf::from("target/release/fennec"), 7, 140);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("`{flag}` needs a value"))?;
        let number = || value.parse().map_err(|_| format!("`{flag}` needs a number, not `{value}`"));
        match flag.as_str() {
            "--fennec" => fennec = PathBuf::from(&value),
            "--runs" => runs = number()?,
            "--files" => files = number()?,
            _ => return Err(format!("unknown option `{flag}`")),
        }
    }
    budgets::run(&fennec, runs, files)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|first| first == "budgets") {
        return match budgets(args.into_iter().skip(1)) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::FAILURE
            }
        };
    }
    let pinning = args.first().is_some_and(|first| first == "pin");
    if pinning {
        args.remove(0);
    }
    let ran = options(args.into_iter()).and_then(|options| {
        if pinning {
            return pin(&options).map(|()| ExitCode::SUCCESS);
        }
        suite(&options).map(|found| report(&options, found))
    });
    ran.unwrap_or_else(|message| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    })
}

/// Prints what a run found, and says whether it passed.
fn report(options: &Options, mut found: Found) -> ExitCode {
    let table = report::table(&found.results);
    print!("{table}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let text = format!("## Benchmarks\n\n{table}");
        if let Err(err) = append(Path::new(&summary), &text) {
            found.errors.push(format!("couldn't write the summary: {err}"));
        }
    }
    let on_github = std::env::var_os("GITHUB_ACTIONS").is_some();
    for slowdown in &found.slowdowns {
        if options.report_only {
            // A warning annotation shows on the pull request without failing it.
            if on_github {
                println!("::warning title=Benchmark slowdown::{slowdown}");
            }
            eprintln!("warning: {slowdown}");
        } else {
            eprintln!("error: {slowdown}");
        }
    }
    for result in found.results.iter().filter(|result| result.meets_floor()) {
        // A second twin's row is for comparison, and is never held to the floor.
        if !result.target.as_ref().is_some_and(|target| target.enforced) && !result.kernel.contains('.') {
            println!(
                "{} has reached the floor: mark it enforced in {}",
                result.kernel,
                options.targets.display()
            );
        }
    }
    for result in found.results.iter().filter(|result| result.not_native.is_some_and(|count| count > 0)) {
        let (kernel, count) = (&result.kernel, result.not_native.unwrap_or(0));
        // What falls off the fast path prints nothing when it runs, so the suite says so.
        if on_github {
            println!("::warning title=Not native::{kernel} has {count} places that aren't native code");
        }
        eprintln!("warning: {kernel} has {count} places that aren't native code: see `fennec dev fallbacks`");
    }
    for error in &found.errors {
        eprintln!("error: {error}");
    }
    let failed = !found.errors.is_empty() || !options.report_only && !found.slowdowns.is_empty();
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(text.as_bytes())
}

fn options(mut args: impl Iterator<Item = String>) -> std::result::Result<Options, String> {
    let mut options = Options {
        fennec: PathBuf::from("target/release/fennec"),
        previous: None,
        runs: 7,
        kernels: PathBuf::from("bench/kernels"),
        targets: PathBuf::from("bench/targets"),
        report_only: false,
    };
    while let Some(flag) = args.next() {
        if flag == "--report-only" {
            options.report_only = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("`{flag}` needs a value"))?;
        match flag.as_str() {
            "--fennec" => options.fennec = PathBuf::from(value),
            "--previous" => options.previous = Some(PathBuf::from(value)),
            "--kernels" => options.kernels = PathBuf::from(value),
            "--targets" => options.targets = PathBuf::from(value),
            "--runs" => {
                options.runs =
                    value.parse().map_err(|_| format!("`--runs` needs a number, not `{value}`"))?;
            }
            _ => return Err(format!("unknown option `{flag}`")),
        }
    }
    Ok(options)
}

fn prepared(options: &Options) -> std::result::Result<(PathBuf, Vec<Kernel>), String> {
    let out_dir = PathBuf::from(OUT_DIR);
    std::fs::create_dir_all(&out_dir).map_err(|err| format!("couldn't make {}: {err}", out_dir.display()))?;
    let kernels = kernels::find(&options.kernels, &out_dir, Path::new("selfhost"))?;
    Ok((out_dir, kernels))
}

/// `fennec-bench pin`: counts each kernel's work in its twin, and writes the targets for this
/// architecture. Kernels keep their ratchets. A kernel has to print what its twin prints.
fn pin(options: &Options) -> std::result::Result<(), String> {
    let (out_dir, kernels) = prepared(options)?;
    let counter = Counter::find(&out_dir).ok_or("nothing can count instructions on this machine")?;
    let path = targets::path(&options.targets);
    let before = targets::read(&path)?;
    let mut pinned = Targets::new();
    for kernel in &kernels {
        eprintln!("pinning {}", kernel.name);
        let twin = kernel.twin(&options.fennec, &out_dir)?;
        let printed = time_together(&[twin, kernel.run(&options.fennec)], 1)?;
        let [twin_timing, ours] = printed.as_slice() else { return Err("nothing was run".to_owned()) };
        if kernel.output_hash(&ours.output) != kernels::hash(&twin_timing.output) {
            return Err(format!(
                "{} printed\n{}but its twin printed\n{}",
                kernel.name, ours.output, twin_timing.output
            ));
        }
        let twin = kernel.twin(&options.fennec, &out_dir)?;
        let instructions = counter.count(&twin).ok_or_else(|| format!("couldn't count {}", kernel.name))?;
        let timed = time_together(&[twin], PIN_RUNS)?;
        let micros =
            timed.first().map_or(0, |timing| u64::try_from(timing.median.as_micros()).unwrap_or(u64::MAX));
        let enforced = before.get(&kernel.name).is_some_and(|target| target.enforced);
        let output = kernel.output_hash(&ours.output);
        let source = kernel.source_hash()?;
        pinned.insert(kernel.name.clone(), Target { instructions, micros, output, source, enforced });
    }
    targets::write(&path, &pinned, &measure::rustc_version()?)?;
    println!("pinned {} kernels in {}", pinned.len(), path.display());
    Ok(())
}

/// Runs every kernel.
fn suite(options: &Options) -> std::result::Result<Found, String> {
    let (out_dir, kernels) = prepared(options)?;
    let targets = targets::read(&targets::path(&options.targets))?;
    if targets.is_empty() {
        eprintln!("no targets for {} yet: `fennec-bench pin` measures them", std::env::consts::ARCH);
    }
    let counter = Counter::find(&out_dir);
    eprintln!(
        "{}",
        match &counter {
            Some(Counter::Perf(_)) => "counting instructions with perf",
            Some(Counter::Valgrind) => "counting instructions with Valgrind",
            Some(Counter::AppleTime) => "counting instructions with time",
            None => "nothing can count instructions here, so nothing can be compared with its target",
        }
    );
    let mut found =
        Found { results: Vec::new(), slowdowns: Vec::new(), errors: lock_problems(&kernels, &targets)? };
    for kernel in &kernels {
        eprintln!("running {}", kernel.name);
        let target = targets.get(&kernel.name);
        match run_kernel(kernel, options, options.runs, counter.as_ref(), target) {
            Ok(result) => {
                found.slowdowns.extend(result.failure());
                found.errors.extend(result.below_floor());
                found.results.push(result);
            }
            Err(message) => found.errors.push(format!("{}: {message}", kernel.name)),
        }
    }
    Ok(found)
}

/// What breaks the lock on the suite: a kernel that isn't what was pinned, or one that's come or
/// gone. With no targets for this architecture, there's nothing to hold them to.
fn lock_problems(kernels: &[Kernel], targets: &Targets) -> std::result::Result<Vec<String>, String> {
    let mut problems = Vec::new();
    if targets.is_empty() {
        return Ok(problems);
    }
    let advice = "the suite is locked: pin it again, and say why in the pull request";
    for kernel in kernels {
        match targets.get(&kernel.name) {
            None => problems.push(format!("{} has no target; {advice}", kernel.name)),
            Some(target) if target.source != kernel.source_hash()? => {
                problems.push(format!("{} isn't what was pinned; {advice}", kernel.name));
            }
            Some(_) => {}
        }
    }
    for name in targets.keys().filter(|name| !kernels.iter().any(|kernel| kernel.name == **name)) {
        problems.push(format!("{name} has a target but no kernel; {advice}"));
    }
    Ok(problems)
}

/// Times and counts one kernel, against the previous `fennec` if there's one that can still
/// run it. A slowdown is confirmed by counting the previous run's instructions, or, with
/// nothing to count them, by measuring again for three times as long.
fn run_kernel(
    kernel: &Kernel,
    options: &Options,
    runs: usize,
    counter: Option<&Counter>,
    target: Option<&Target>,
) -> std::result::Result<Result, String> {
    let mut programs = vec![kernel.run(&options.fennec), kernel.startup(&options.fennec)];
    let previous = options.previous.as_deref().filter(|previous| {
        // A previous `fennec` that can't run a new kernel just isn't compared on it.
        time_together(&[kernel.run(previous)], 1).is_ok()
    });
    programs.extend(previous.map(|previous| kernel.run(previous)));
    let timings = time_together(&programs, runs)?;
    let [ours, startup, others @ ..] = timings.as_slice() else { return Err("nothing was timed".to_owned()) };
    let printed = kernel.output_hash(&ours.output);
    if let Some(target) = target.filter(|target| target.output != printed) {
        return Err(format!(
            "it printed something else than its target expects ({printed}, not {}):\n{}",
            target.output, ours.output
        ));
    }
    let previous_output = others.first().filter(|previous| previous.output == ours.output);
    let mut result = Result {
        kernel: kernel.name.clone(),
        total: ours.median,
        startup: startup.median,
        previous: previous_output.map(|previous| previous.median),
        instructions: counter.and_then(|counter| counter.count(&kernel.run(&options.fennec))),
        startup_instructions: counter.and_then(|counter| counter.count(&kernel.startup(&options.fennec))),
        previous_instructions: None,
        not_native: kernel.not_native(&options.fennec),
        target: target.cloned(),
        fell_back: ours.notes.contains("using the interpreter"),
    };
    if result.failure().is_some() {
        match (counter, previous) {
            (Some(counter), Some(previous)) => {
                eprintln!("counting {} on the previous commit", kernel.name);
                result.previous_instructions = counter.count(&kernel.run(previous));
            }
            _ if runs == options.runs => {
                eprintln!("measuring {} again", kernel.name);
                return run_kernel(kernel, options, runs * 3, counter, target);
            }
            _ => {}
        }
    }
    Ok(result)
}
