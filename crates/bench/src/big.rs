//! `fennec-bench big-codebase`: how the work of checking, building and testing a program grows
//! with its size. A program of each size is generated, the same each time from its seed (see the
//! `generate` module), and each step is measured on it:
//!
//! - `check`: `fennec check`, with nothing kept from before;
//! - `cold-dev`: a dev build, with nothing kept;
//! - `cold-release`: a release build, with nothing kept;
//! - `edit-dev`: a dev build again, after a one-line edit to a module in the middle of it;
//! - `edit-release`: a release build again, after that edit;
//! - `test`: `fennec test`, after that edit and the dev build.
//!
//! Each step gives its wall time and the most memory one of its processes held. Where they're
//! counted, it gives the instructions it took too, by process: `fennec` itself, LLVM's tools and
//! the linker, which is where the time goes. Between each size and the next, it gives how much
//! faster than the program each step grew: 1.00 is in step with it, and over that, faster than
//! linear, is a bug.
//!
//! `bench/big-codebase.txt` holds each step at each size to a budget, for each system it was
//! pinned on: its wall time, its memory and, where counted, its instructions, each with the slack
//! below. A step over a budget, or growing faster than the program by more than its slack, fails
//! the run. `--pin` writes what this run measured as this system's budgets, and keeps the others'.
//!
//! ```text
//! fennec-bench big-codebase [--fennec PATH] [--lines N,N,...] [--seed N] [--runs N]
//!     [--budgets PATH] [--pin] [--markdown PATH]
//! fennec-bench big-codebase generate DIR [--lines N] [--seed N]
//! ```

mod generate;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::build_times::{
    Counted, Invocation, Setup, ToolCount, billions, counted, load, median, over, seconds,
};

/// How far over its budget a step's wall time can go before it fails, in percent: a machine of
/// the same kind, as CI's runners are, can be this much slower on a bad night.
const WALL_SLACK_PERCENT: u128 = 50;

/// How far over its budget the most memory a step held can go before it fails, in percent.
const PEAK_SLACK_PERCENT: u128 = 25;

/// How far over its budget a step's instructions can go before it fails, in percent: a big
/// build's count moves more than a small one's from run to run, as LLVM's tools take turns.
const INSTRUCTIONS_SLACK_PERCENT: u128 = 5;

/// How much faster than the program a step can grow from one size to the next, as a factor in
/// hundredths, before it fails: 100 is in step, and a little over is noise.
const GROWTH_LIMIT: u128 = 125;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Check,
    ColdDev,
    ColdRelease,
    EditDev,
    EditRelease,
    Test,
}

impl Step {
    const ALL: [Step; 6] =
        [Step::Check, Step::ColdDev, Step::ColdRelease, Step::EditDev, Step::EditRelease, Step::Test];

    fn name(self) -> &'static str {
        match self {
            Step::Check => "check",
            Step::ColdDev => "cold-dev",
            Step::ColdRelease => "cold-release",
            Step::EditDev => "edit-dev",
            Step::EditRelease => "edit-release",
            Step::Test => "test",
        }
    }

    fn named(name: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|step| step.name() == name)
    }
}

/// One step at one size: the medians of its runs.
struct Found {
    lines: usize,
    step: Step,
    wall: Duration,
    peak_kb: Option<u64>,
    /// What each process took, each the median of its runs, when they're counted.
    by_tool: Option<Vec<ToolCount>>,
    /// What a build said it did.
    built: Option<Built>,
    load: f64,
}

impl Found {
    fn instructions(&self) -> Option<u64> {
        self.by_tool.as_ref().map(|by_tool| by_tool.iter().map(|tool| tool.instructions).sum())
    }
}

/// What a build says it did: how many of the program's functions it sent through LLVM, rather
/// than finding them in the cache, and how long it took compiling them and linking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Built {
    compiled: u64,
    total: u64,
    compiling: Duration,
    linking: Duration,
}

/// The line a build prints, `🦊 built … with LLVM: compiled 12 of 340 functions in 812 ms, linked
/// in 95 ms`, read back.
fn said_built(said: &str) -> Option<Built> {
    let rest = said.lines().find_map(|line| line.split_once("with LLVM: compiled ").map(|(_, rest)| rest))?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let [compiled, "of", total, "functions", "in", compiling, "ms,", "linked", "in", linking, "ms", ..] =
        words.as_slice()
    else {
        return None;
    };
    Some(Built {
        compiled: compiled.parse().ok()?,
        total: total.parse().ok()?,
        compiling: Duration::from_millis(compiling.parse().ok()?),
        linking: Duration::from_millis(linking.parse().ok()?),
    })
}

/// A step's budget at a size, on one system.
#[derive(Clone, Copy)]
struct Budget {
    wall: Duration,
    peak_kb: Option<u64>,
    instructions: Option<u64>,
}

/// What a run is asked for.
struct Options {
    fennec: PathBuf,
    sizes: Vec<usize>,
    seed: u64,
    runs: usize,
    budgets: PathBuf,
    pin: bool,
    markdown: Option<PathBuf>,
}

pub(crate) fn command(args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut args: Vec<String> = args.collect();
    let into = if args.first().is_some_and(|first| first == "generate") {
        args.remove(0);
        if args.first().is_none_or(|folder| folder.starts_with("--")) {
            return Err("`generate` needs a folder to write the program into".to_owned());
        }
        Some(PathBuf::from(args.remove(0)))
    } else {
        None
    };
    let options = options(args.into_iter())?;
    if let Some(into) = into {
        let lines = options.sizes.last().copied().unwrap_or(200_000);
        if into.exists() {
            return Err(format!("{} is there already", into.display()));
        }
        let made = generate::generate(&into, lines, options.seed)?;
        println!(
            "wrote {} lines in {} files, with {} tests, into {}",
            made.lines,
            made.files,
            made.tests,
            into.display()
        );
        return Ok(true);
    }
    measure_all(&options)
}

fn options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        fennec: PathBuf::from("target/release/fennec"),
        // As the nightly runs it. 50,000 lines and more ask for over 2,000 copies of generic
        // functions, which a build refuses until #1896.
        sizes: vec![20_000, 30_000],
        seed: 1,
        runs: 1,
        budgets: PathBuf::from("bench/big-codebase.txt"),
        pin: false,
        markdown: None,
    };
    while let Some(flag) = args.next() {
        if flag == "--pin" {
            options.pin = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("`{flag}` needs a value"))?;
        let number =
            |text: &str| text.parse::<usize>().map_err(|_| format!("`{flag}` needs a number, not `{text}`"));
        match flag.as_str() {
            "--fennec" => options.fennec = PathBuf::from(&value),
            "--lines" => options.sizes = value.split(',').map(number).collect::<Result<_, _>>()?,
            "--seed" => {
                options.seed =
                    value.parse().map_err(|_| format!("`{flag}` needs a number, not `{value}`"))?;
            }
            "--runs" => options.runs = number(&value)?,
            "--budgets" => options.budgets = PathBuf::from(&value),
            "--markdown" => options.markdown = Some(PathBuf::from(&value)),
            _ => return Err(format!("unknown option `{flag}`")),
        }
    }
    if options.runs == 0 {
        return Err("`--runs` needs at least one run".to_owned());
    }
    if options.sizes.is_empty() || options.sizes.contains(&0) {
        return Err("`--lines` needs sizes over 0".to_owned());
    }
    options.sizes.sort_unstable();
    options.sizes.dedup();
    Ok(options)
}

/// The system budgets are pinned for: CI's runners and a developer's machine differ, and so do
/// their linkers.
fn system() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Every step at every size, printed as it's measured, then how each grew, then what failed.
fn measure_all(options: &Options) -> Result<bool, String> {
    let work =
        std::fs::canonicalize(".").map_err(|err| err.to_string())?.join(crate::OUT_DIR).join("big-codebase");
    let setup = Setup::new(&options.fennec, work)?;
    let system = system();
    let pinned = read_budgets(&options.budgets)?;
    println!(
        "a big codebase on {} ({} cores), {system}, from seed {}, the median of {} run{}",
        crate::machine::cpu(),
        crate::machine::cores(),
        options.seed,
        options.runs,
        if options.runs == 1 { "" } else { "s" }
    );
    if !setup.counts() {
        println!("nothing here counts instructions, so there are only times and memory");
    }
    println!(
        "{:>7} {:<12} {:>9} {:>9} {:>14} {:>14} {:>13} {:>6}",
        "lines", "step", "wall", "memory", "instructions", "budget", "compiled", "load"
    );
    let mut found: Vec<Found> = Vec::new();
    let mut programs = Vec::new();
    let mut failures = Vec::new();
    for &lines in &options.sizes {
        // A step that fails at one size fails at every larger one, so the sizes stop there, and
        // what was measured is still reported, with the failure.
        let (program, measured) = match measure_size(&setup, options, lines) {
            Ok(measured) => measured,
            Err(err) => {
                eprintln!("{err}");
                let first: Vec<&str> = err.lines().filter(|line| !line.trim().is_empty()).take(3).collect();
                failures.push(format!("at {lines} lines: {}", first.join(" / ")));
                break;
            }
        };
        for step in &measured {
            let budget = budget_for(&pinned, &system, lines, step.step);
            print_line(step, budget);
        }
        programs.push(program);
        found.extend(measured);
    }
    let growth = growth(&found, &programs);
    print!("{}", growth_table(&growth));
    failures.extend(over_budget(&found, &pinned, &system));
    for (step, from, to, factor) in &growth {
        if *factor > GROWTH_LIMIT {
            failures.push(format!(
                "{} grew {} times faster than the program from {from} lines to {to}",
                step.name(),
                hundredths(*factor)
            ));
        }
    }
    for failure in &failures {
        eprintln!("error: {failure}");
    }
    if let Some(path) = &options.markdown {
        let text = markdown(options, &setup, &programs, &found, &growth, &failures);
        std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    if options.pin {
        write_budgets(&options.budgets, &pinned, &system, &found)?;
        println!("pinned these as {system}'s budgets in {}", options.budgets.display());
    }
    let _ = std::fs::remove_dir_all(setup.work());
    Ok(options.pin || failures.is_empty())
}

/// How big a generated program came out.
struct Program {
    /// The lines asked for, which name its size in the budgets.
    asked: usize,
    /// The lines it came out at, which its growth is measured against.
    lines: usize,
    files: usize,
    tests: usize,
}

/// Every step on a program of about `lines` lines, `runs` times, each time from nothing kept.
fn measure_size(setup: &Setup, options: &Options, lines: usize) -> Result<(Program, Vec<Found>), String> {
    let folder = setup.work().join(lines.to_string());
    let made = generate::generate(&folder.join("program"), lines, options.seed)?;
    let original =
        std::fs::read_to_string(&made.edited).map_err(|err| format!("{}: {err}", made.edited.display()))?;
    let mut runs: Vec<Vec<(Step, Counted, f64)>> = Vec::new();
    for at in 0..options.runs {
        std::fs::write(&made.edited, &original).map_err(|err| format!("{}: {err}", made.edited.display()))?;
        let homes = ["home-check", "home-dev", "home-release"].map(|name| folder.join(name));
        for home in &homes {
            let _ = std::fs::remove_dir_all(home);
        }
        let [check_home, dev_home, release_home] = homes.each_ref().map(PathBuf::as_path);
        let dev = folder.join("app-dev").to_string_lossy().into_owned();
        let release = folder.join("app-release").to_string_lossy().into_owned();
        let mut steps = Vec::new();
        for step in Step::ALL {
            if step == Step::EditDev {
                made.edit(at + 1)?;
            }
            let (home, args): (&Path, Vec<&str>) = match step {
                Step::Check => (check_home, vec!["check", "."]),
                Step::ColdDev | Step::EditDev => (dev_home, vec!["build", ".", "-o", dev.as_str()]),
                Step::ColdRelease | Step::EditRelease => {
                    (release_home, vec!["build", "--release", ".", "-o", release.as_str()])
                }
                Step::Test => (dev_home, vec!["test", "."]),
            };
            let load = load();
            let run =
                Invocation { args: &args, folder: &made.app, stdin: None, home, counting: true, own: false };
            let found = counted(setup, &run)
                .map_err(|err| format!("{err}\n(the {} step, at {lines} lines)", step.name()))?;
            steps.push((step, found, load));
        }
        runs.push(steps);
    }
    let program = Program { asked: lines, lines: made.lines, files: made.files, tests: made.tests };
    let _ = std::fs::remove_dir_all(&folder);
    let found = Step::ALL
        .into_iter()
        .map(|step| {
            let of_step: Vec<&(Step, Counted, f64)> =
                runs.iter().filter_map(|steps| steps.iter().find(|(at, _, _)| *at == step)).collect();
            medians(lines, step, &of_step)
        })
        .collect();
    Ok((program, found))
}

/// One step's medians over its runs: of its wall time, its memory, its load, what it says it
/// built, and what each tool took.
fn medians(lines: usize, step: Step, runs: &[&(Step, Counted, f64)]) -> Found {
    let walls: Vec<Duration> = runs.iter().map(|(_, found, _)| found.wall).collect();
    let mut loads: Vec<f64> = runs.iter().map(|(_, _, load)| *load).collect();
    loads.sort_by(f64::total_cmp);
    let mut peaks: Vec<u64> = runs.iter().filter_map(|(_, found, _)| found.peak_kb).collect();
    let peak_kb = if peaks.len() == runs.len() { median(&mut peaks) } else { None };
    let built: Vec<Built> = runs.iter().filter_map(|(_, found, _)| said_built(&found.said)).collect();
    let built = built.first().filter(|_| built.len() == runs.len()).map(|first| Built {
        compiling: middle(built.iter().map(|built| built.compiling).collect()),
        linking: middle(built.iter().map(|built| built.linking).collect()),
        ..*first
    });
    Found {
        lines,
        step,
        wall: middle(walls),
        peak_kb,
        by_tool: tool_medians(runs),
        built,
        load: loads.get(loads.len() / 2).copied().unwrap_or_default(),
    }
}

/// The middle duration, or the higher middle one.
fn middle(mut durations: Vec<Duration>) -> Duration {
    durations.sort();
    durations.get(durations.len() / 2).copied().unwrap_or_default()
}

/// Each tool's median instructions and time over the runs, when every run counted it.
fn tool_medians(runs: &[&(Step, Counted, f64)]) -> Option<Vec<ToolCount>> {
    let (_, first, _) = runs.first()?;
    first
        .by_tool
        .as_ref()?
        .iter()
        .map(|tool| {
            let of_tool: Vec<&ToolCount> = runs
                .iter()
                .filter_map(|(_, found, _)| {
                    found.by_tool.as_ref()?.iter().find(|other| other.name == tool.name)
                })
                .collect();
            if of_tool.len() != runs.len() {
                return None;
            }
            let mut counts: Vec<u64> = of_tool.iter().map(|tool| tool.instructions).collect();
            let took: Vec<Duration> = of_tool.iter().filter_map(|tool| tool.took).collect();
            Some(ToolCount {
                name: tool.name.clone(),
                runs: tool.runs,
                instructions: median(&mut counts)?,
                took: (took.len() == runs.len()).then(|| middle(took)),
            })
        })
        .collect()
}

fn budget_for(
    pinned: &[(String, usize, Step, Budget)],
    system: &str,
    lines: usize,
    step: Step,
) -> Option<Budget> {
    pinned
        .iter()
        .find(|(at, size, of, _)| at == system && *size == lines && *of == step)
        .map(|(_, _, _, budget)| *budget)
}

fn print_line(found: &Found, budget: Option<Budget>) {
    let instructions = found.instructions().map_or_else(|| "-".to_owned(), billions);
    let budget = budget
        .and_then(|budget| budget.instructions)
        .map_or_else(|| budget.map_or_else(|| "none".to_owned(), |budget| seconds(budget.wall)), billions);
    let compiled =
        found.built.map_or_else(|| "-".to_owned(), |built| format!("{}/{}", built.compiled, built.total));
    println!(
        "{:>7} {:<12} {:>9} {:>9} {instructions:>14} {budget:>14} {compiled:>13} {:>6.1}",
        found.lines,
        found.step.name(),
        seconds(found.wall),
        found.peak_kb.map_or_else(|| "-".to_owned(), megabytes),
        found.load
    );
}

fn megabytes(kb: u64) -> String {
    format!("{} MB", kb.div_ceil(1024))
}

/// A factor in hundredths, as `1.25`.
fn hundredths(factor: u128) -> String {
    format!("{}.{:02}", factor / 100, factor % 100)
}

/// What each step's work is, to compare across sizes: its instructions, where counted, which
/// barely move with the machine's load, or else its wall time in microseconds.
fn work_of(found: &Found) -> u128 {
    found.instructions().map_or_else(|| found.wall.as_micros(), u128::from)
}

/// How much faster than the program each step grew from each size to the next, in hundredths: its
/// work's growth over the growth of the lines the programs came out at, named by the sizes asked
/// for.
fn growth(found: &[Found], programs: &[Program]) -> Vec<(Step, usize, usize, u128)> {
    let mut grew = Vec::new();
    for step in Step::ALL {
        for pair in programs.windows(2) {
            let [from, to] = pair else { continue };
            let at = |asked| found.iter().find(|found| found.lines == asked && found.step == step);
            let (Some(small), Some(big)) = (at(from.asked), at(to.asked)) else { continue };
            let (small_work, big_work) = (work_of(small), work_of(big));
            if small_work == 0 || from.lines == 0 {
                continue;
            }
            // (big / small) / (to's lines / from's lines), in hundredths.
            let factor = big_work * 100 * from.lines as u128 / (small_work * to.lines as u128);
            grew.push((step, from.asked, to.asked, factor));
        }
    }
    grew
}

fn growth_table(growth: &[(Step, usize, usize, u128)]) -> String {
    let mut text = String::from("how much faster than the program each step grew (1.00 is in step):\n");
    for step in Step::ALL {
        let mut line = format!("  {:<12}", step.name());
        for (_, from, to, factor) in growth.iter().filter(|(at, ..)| *at == step) {
            let _ = write!(line, " {from}->{to}: {}", hundredths(*factor));
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Every step over a budget pinned for this system.
fn over_budget(found: &[Found], pinned: &[(String, usize, Step, Budget)], system: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for step in found {
        let Some(budget) = budget_for(pinned, system, step.lines, step.step) else { continue };
        let name = format!("{} at {} lines", step.step.name(), step.lines);
        if over(step.wall.as_millis(), budget.wall.as_millis(), WALL_SLACK_PERCENT) {
            failures.push(format!(
                "{name} took {}, over its budget of {} by more than {WALL_SLACK_PERCENT}%, with the load at {:.1}",
                seconds(step.wall),
                seconds(budget.wall),
                step.load
            ));
        }
        if let (Some(peak), Some(limit)) = (step.peak_kb, budget.peak_kb)
            && over(u128::from(peak), u128::from(limit), PEAK_SLACK_PERCENT)
        {
            failures.push(format!(
                "{name} held {}, over its budget of {} by more than {PEAK_SLACK_PERCENT}%",
                megabytes(peak),
                megabytes(limit)
            ));
        }
        if let (Some(instructions), Some(limit)) = (step.instructions(), budget.instructions)
            && over(u128::from(instructions), u128::from(limit), INSTRUCTIONS_SLACK_PERCENT)
        {
            failures.push(format!(
                "{name} took {} instructions, over its budget of {} by more than {INSTRUCTIONS_SLACK_PERCENT}%",
                billions(instructions),
                billions(limit)
            ));
        }
    }
    failures
}

/// The night's report: each step at each size, where its instructions went, how it grew, and what
/// failed.
fn markdown(
    options: &Options,
    setup: &Setup,
    programs: &[Program],
    found: &[Found],
    growth: &[(Step, usize, usize, u128)],
    failures: &[String],
) -> String {
    let mut text = String::from("## Big codebase\n\n");
    let _ = writeln!(
        text,
        "Generated from seed {} and measured on {} ({} cores, {}), the median of {} run{}. Each step \
         starts with nothing kept, but for the edits and the test, which follow a one-line edit to a \
         module in the middle.\n",
        options.seed,
        crate::machine::cpu(),
        crate::machine::cores(),
        system(),
        options.runs,
        if options.runs == 1 { "" } else { "s" }
    );
    for program in programs {
        let _ = writeln!(
            text,
            "- {} lines in {} files, with {} tests",
            program.lines, program.files, program.tests
        );
    }
    text.push_str("\n| lines | step | wall | memory | instructions |\n|---:|---|---:|---:|---:|\n");
    for step in found {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} |",
            step.lines,
            step.step.name(),
            seconds(step.wall),
            step.peak_kb.map_or_else(|| "-".to_owned(), megabytes),
            step.instructions().map_or_else(|| "-".to_owned(), billions)
        );
    }
    text.push_str(&built_table(found));
    if setup.counts() {
        text.push_str(&by_tool_table(found));
    }
    let _ = writeln!(
        text,
        "\nHow much faster than the program each step grew, from each size to the next, by {}: 1.00 is \
         in step with it, and over {} fails.\n",
        if setup.counts() { "instructions" } else { "wall time" },
        hundredths(GROWTH_LIMIT)
    );
    for step in Step::ALL {
        let factors: Vec<String> = growth
            .iter()
            .filter(|(at, ..)| *at == step)
            .map(|(_, from, to, factor)| format!("{from} to {to}: {}", hundredths(*factor)))
            .collect();
        let _ = writeln!(text, "- {}: {}", step.name(), factors.join(", "));
    }
    if failures.is_empty() {
        text.push_str("\nEvery step held to its budget.\n");
    } else {
        text.push_str("\nFailed:\n\n");
        for failure in failures {
            let _ = writeln!(text, "- {failure}");
        }
    }
    text
}

/// Where each step's instructions went, by process, with the time each kind of process took,
/// added up, as a Markdown table.
fn by_tool_table(found: &[Found]) -> String {
    let mut text = String::from(
        "\nWhere the work went, by process: how many times it ran, its instructions, and the time \
         its processes took, added up, which is more than the step took where they ran at once:\n\n",
    );
    let mut tools: Vec<&str> = Vec::new();
    for step in found {
        for tool in step.by_tool.iter().flatten() {
            if !tools.contains(&tool.name.as_str()) {
                tools.push(&tool.name);
            }
        }
    }
    let _ = writeln!(text, "| lines | step | {} |", tools.join(" | "));
    let _ = writeln!(text, "|---:|---|{}", "---:|".repeat(tools.len()));
    for step in found {
        let counts: Vec<String> = tools
            .iter()
            .map(|name| {
                step.by_tool.iter().flatten().find(|tool| tool.name == *name).map_or_else(
                    || "-".to_owned(),
                    |tool| match tool.took {
                        Some(took) => {
                            format!("{} runs, {}, {}", tool.runs, billions(tool.instructions), seconds(took))
                        }
                        None => billions(tool.instructions),
                    },
                )
            })
            .collect();
        let _ = writeln!(text, "| {} | {} | {} |", step.lines, step.step.name(), counts.join(" | "));
    }
    text
}

/// What each build sent through LLVM, of the program's functions, and how long it took compiling
/// and linking, as a Markdown table: after the edit, what it didn't compile again it reused.
fn built_table(found: &[Found]) -> String {
    let mut text = String::from(
        "\nWhat each build compiled through LLVM, of the program's functions, and how long it took \
         compiling and linking. After the one-line edit, the rest was reused from the cache:\n\n\
         | lines | step | compiled | of | compiling | linking |\n|---:|---|---:|---:|---:|---:|\n",
    );
    for step in found {
        let Some(built) = step.built else { continue };
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} | {} |",
            step.lines,
            step.step.name(),
            built.compiled,
            built.total,
            seconds(built.compiling),
            seconds(built.linking)
        );
    }
    text
}

/// The budgets in `path`: a line each, `system lines step wall-milliseconds memory-kb
/// instructions`, `-` for what wasn't measured, after comments. None yet is no budgets.
fn read_budgets(path: &Path) -> Result<Vec<(String, usize, Step, Budget)>, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(Vec::new()) };
    let mut budgets = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let maybe = |field: &str| -> Option<Option<u64>> {
            if field == "-" { Some(None) } else { field.parse().ok().map(Some) }
        };
        let fields: Vec<&str> = line.split_whitespace().collect();
        let parsed = match fields.as_slice() {
            [system, lines, step, wall, peak, instructions] => (|| {
                Some((
                    (*system).to_owned(),
                    lines.parse().ok()?,
                    Step::named(step)?,
                    Budget {
                        wall: Duration::from_millis(wall.parse().ok()?),
                        peak_kb: maybe(peak)?,
                        instructions: maybe(instructions)?,
                    },
                ))
            })(),
            _ => None,
        };
        budgets.push(parsed.ok_or_else(|| {
            format!(
                "{}:{}: `{line}` isn't `system lines step wall-milliseconds memory-kb instructions`",
                path.display(),
                number + 1
            )
        })?);
    }
    Ok(budgets)
}

/// Writes what was measured as `system`'s budgets, keeping every other system's.
fn write_budgets(
    path: &Path,
    pinned: &[(String, usize, Step, Budget)],
    system: &str,
    found: &[Found],
) -> Result<(), String> {
    let mut text = format!(
        "# What `fennec-bench big-codebase` holds each step to, pinned by `fennec-bench big-codebase --pin`:\n\
         # a system, a program's size in lines, a step, then the milliseconds it took, which a run may go\n\
         # over by {WALL_SLACK_PERCENT}%, the most memory one process held in KB, by {PEAK_SLACK_PERCENT}%, and the instructions\n\
         # it took, by {INSTRUCTIONS_SLACK_PERCENT}%, `-` where they weren't measured. linux-x86_64's are CI's runners,\n\
         # each night in `proofs.yml`.\n"
    );
    let _ = writeln!(
        text,
        "# {system} pinned on {} with {} cores.",
        crate::machine::cpu(),
        crate::machine::cores()
    );
    let mut lines: Vec<String> = pinned
        .iter()
        .filter(|(at, ..)| at != system)
        .map(|(at, size, step, budget)| budget_line(at, *size, *step, budget))
        .collect();
    for step in found {
        let budget = Budget { wall: step.wall, peak_kb: step.peak_kb, instructions: step.instructions() };
        lines.push(budget_line(system, step.lines, step.step, &budget));
    }
    for line in lines {
        text.push_str(&line);
    }
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

fn budget_line(system: &str, lines: usize, step: Step, budget: &Budget) -> String {
    let maybe = |value: Option<u64>| value.map_or_else(|| "-".to_owned(), |value| value.to_string());
    format!(
        "{system} {lines} {} {} {} {}\n",
        step.name(),
        budget.wall.as_millis(),
        maybe(budget.peak_kb),
        maybe(budget.instructions)
    )
}

#[cfg(test)]
mod tests {
    use super::{Built, Found, Program, Step, ToolCount, growth, read_budgets, said_built};
    use std::time::Duration;

    fn found(lines: usize, step: Step, instructions: u64) -> Found {
        Found {
            lines,
            step,
            wall: Duration::from_secs(1),
            peak_kb: None,
            by_tool: Some(vec![ToolCount { name: "fennec".to_owned(), runs: 1, instructions, took: None }]),
            built: None,
            load: 0.0,
        }
    }

    #[test]
    fn a_step_in_step_with_the_program_grows_by_one_and_a_quadratic_one_by_two() {
        let measured = [
            found(50, Step::Check, 1000),
            found(100, Step::Check, 2000),
            found(50, Step::Test, 1000),
            found(100, Step::Test, 4000),
        ];
        // The programs came out a little bigger than asked: 50 and 100 lines asked for, 55 and 110
        // made, still twice the size.
        let programs = [
            Program { asked: 50, lines: 55, files: 1, tests: 1 },
            Program { asked: 100, lines: 110, files: 1, tests: 1 },
        ];
        let grew = growth(&measured, &programs);
        assert_eq!(grew.iter().find(|(step, ..)| *step == Step::Check).map(|grew| grew.3), Some(100));
        assert_eq!(grew.iter().find(|(step, ..)| *step == Step::Test).map(|grew| grew.3), Some(200));
    }

    #[test]
    fn a_build_says_how_much_it_compiled_again() {
        let said = "🦊 built /tmp/app with LLVM: compiled 12 of 340 functions in 812 ms, linked in 95 ms\n";
        assert_eq!(
            said_built(said),
            Some(Built {
                compiled: 12,
                total: 340,
                compiling: Duration::from_millis(812),
                linking: Duration::from_millis(95)
            })
        );
        assert_eq!(said_built("4 passed, 0 failed\n"), None);
    }

    #[test]
    fn budgets_read_back_with_what_was_not_measured() {
        let folder = fennec_temp::Temp::folder("big-budgets").unwrap();
        let path = folder.path().join("budgets.txt");
        std::fs::write(&path, "# a comment\nlinux-x86_64 50000 cold-dev 61000 812000 -\n").unwrap();
        let budgets = read_budgets(&path).unwrap();
        assert_eq!(budgets.len(), 1);
        let (system, lines, step, budget) = &budgets[0];
        assert_eq!((system.as_str(), *lines, *step), ("linux-x86_64", 50_000, Step::ColdDev));
        assert_eq!(
            (budget.wall.as_millis(), budget.peak_kb, budget.instructions),
            (61_000, Some(812_000), None)
        );
        std::fs::write(&path, "linux-x86_64 50000 cold 61000\n").unwrap();
        assert!(read_budgets(&path).is_err());
    }
}
