//! `fennec-bench build-times`: how much work Fennec does to build, check and start the
//! scoreboard's real programs and the self-hosted front end. Each step is measured as the median
//! of several runs:
//!
//! - `cold`: a release build in a Fennec folder of its own, made new for each run;
//! - `warm`: a dev build again, after a function nothing calls is added on one line at the end;
//! - `check`: `fennec check` after another line like that;
//! - `run`: `fennec run` on a program built already, which finds it in the store.
//!
//! Each run's work is counted as instructions retired, which barely move when the machine is busy,
//! across every process a step starts: the compiler, LLVM's tools and the linker. The budget in
//! `bench/build-times.txt` holds each step to its count, and a step over it fails the run. Wall
//! time is shown, with the machine's load when it was measured, and only warns.
//!
//! A run's count is everything but the program itself: with `FENNEC_RUN_WAITS`, `fennec run`
//! starts the program as a process of its own and waits for it, and only `fennec` is counted. Its
//! wall time is the whole run, the program's included.
//!
//! On a Mac, `time -l` counts only the process it starts, so each of LLVM's tools, `cc` and the
//! linker are started through a script that counts it too, and the counts are added up. On
//! Linux, `perf stat` counts every process. Where neither can count, there are only times.
//!
//! ```text
//! fennec-bench build-times [--fennec PATH] [--runs N] [--budgets PATH] [--pin]
//! ```
//!
//! `--pin` writes what this run measured as the budgets.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The scoreboard's real programs, in `bench/kernels`.
const REAL_PROGRAMS: &[&str] = &[
    "game_tick",
    "chat_server",
    "api_handler",
    "log_crunch",
    "pathfind",
    "physics",
    "grep_tool",
    "data_pipeline",
];

/// How far over its budget a step's count can go before it fails, in percent: counts of one
/// build differ by half a percent from run to run.
const SLACK_PERCENT: u128 = 3;

/// How far over its budget a step's wall time goes before it warns, in percent.
const WALL_SLACK_PERCENT: u128 = 50;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Cold,
    Warm,
    Check,
    Run,
}

impl Step {
    const ALL: [Step; 4] = [Step::Cold, Step::Warm, Step::Check, Step::Run];

    fn name(self) -> &'static str {
        match self {
            Step::Cold => "cold",
            Step::Warm => "warm",
            Step::Check => "check",
            Step::Run => "run",
        }
    }

    fn named(name: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|step| step.name() == name)
    }
}

/// One program measured: a kernel's file, or the self-hosted front end's kit, copied into a folder
/// of the run's own, so a change to it changes nothing in the repository.
struct Measured {
    name: String,
    /// What's built: the copied file or kit.
    path: PathBuf,
    /// The file a change is added to.
    changed: PathBuf,
    /// Where it runs from: the folder it came from, whose files it may read.
    folder: PathBuf,
    /// What it reads on standard input.
    stdin: Option<PathBuf>,
}

/// One step's measurement: the median of its runs.
struct Found {
    instructions: Option<u64>,
    wall: Duration,
    load: f64,
}

/// A step's budget: its count, and the wall time it took when it was pinned.
#[derive(Clone, Copy)]
struct Budget {
    instructions: u64,
    wall: Duration,
}

/// What every run uses: the `fennec` measured, how its work is counted, and the folder runs work
/// in.
struct Setup {
    fennec: PathBuf,
    counter: Counter,
    work: PathBuf,
}

/// How work is counted here.
enum Counter {
    /// macOS's `time -l`, with the scripts in this folder standing for LLVM's tools, `cc` and the
    /// linker, each writing its own count to the file in `BUILD_TIMES_COUNTS`.
    Apple {
        tools: PathBuf,
    },
    /// `perf stat`, which counts every process.
    Perf,
    None,
}

pub(crate) fn command(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut fennec = PathBuf::from("target/release/fennec");
    let mut runs = 5;
    let mut budgets = PathBuf::from("bench/build-times.txt");
    let mut pin = false;
    while let Some(flag) = args.next() {
        if flag == "--pin" {
            pin = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("`{flag}` needs a value"))?;
        match flag.as_str() {
            "--fennec" => fennec = PathBuf::from(&value),
            "--runs" => {
                runs = value.parse().map_err(|_| format!("`{flag}` needs a number, not `{value}`"))?;
            }
            "--budgets" => budgets = PathBuf::from(&value),
            _ => return Err(format!("unknown option `{flag}`")),
        }
    }
    if runs == 0 {
        return Err("`--runs` needs at least one run".to_owned());
    }
    let fennec = std::fs::canonicalize(&fennec).map_err(|err| format!("{}: {err}", fennec.display()))?;
    let work =
        std::fs::canonicalize(".").map_err(|err| err.to_string())?.join(crate::OUT_DIR).join("build-times");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    let counter = counter(&fennec, &work)?;
    let programs = programs(&work)?;
    let setup = Setup { fennec, counter, work };
    let pinned = read_budgets(&budgets)?;
    println!(
        "build times on {} ({} cores), the median of {runs} runs",
        crate::machine::cpu(),
        crate::machine::cores()
    );
    if matches!(setup.counter, Counter::None) {
        println!("nothing here counts instructions, so there are only times, and no budget is held");
    }
    println!(
        "{:<14} {:<6} {:>14} {:>14} {:>9} {:>6}",
        "program", "step", "instructions", "budget", "wall", "load"
    );
    let (measured, failures, warnings) = measure_all(&setup, &programs, &pinned, runs)?;
    println!("a run's count is fennec's own work, but its wall time is the program's run as well");
    for warning in &warnings {
        println!("warning: slower than when pinned: {warning}");
    }
    for failure in &failures {
        eprintln!("error: {failure}");
    }
    if pin {
        write_budgets(&budgets, &measured)?;
        println!("pinned these as the budgets in {}", budgets.display());
    }
    let _ = std::fs::remove_dir_all(&setup.work);
    Ok(pin || failures.is_empty())
}

/// Every step of every program, each line printed as it's measured, with what went over its
/// budget, which fails, and what took longer than when pinned, which warns.
#[expect(clippy::type_complexity, reason = "what was measured, and the failures and warnings found in it")]
fn measure_all(
    setup: &Setup,
    programs: &[Measured],
    pinned: &[(String, Step, Budget)],
    runs: usize,
) -> Result<(Vec<(String, Step, Found)>, Vec<String>, Vec<String>), String> {
    let mut measured = Vec::new();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    for program in programs {
        for step in Step::ALL {
            let found = measure(setup, program, step, runs)?;
            let budget = pinned
                .iter()
                .find(|(name, at, _)| *name == program.name && *at == step)
                .map(|(_, _, budget)| *budget);
            print_line(&program.name, step, &found, budget);
            if let (Some(budget), Some(instructions)) = (budget, found.instructions)
                && over(u128::from(instructions), u128::from(budget.instructions), SLACK_PERCENT)
            {
                failures.push(format!(
                    "{} {}: {} instructions, over its budget of {}",
                    program.name,
                    step.name(),
                    billions(instructions),
                    billions(budget.instructions)
                ));
            }
            if let Some(budget) = budget
                && over(found.wall.as_millis(), budget.wall.as_millis(), WALL_SLACK_PERCENT)
            {
                warnings.push(format!(
                    "{} {}: {} against {} when pinned, with the load at {:.1}",
                    program.name,
                    step.name(),
                    seconds(found.wall),
                    seconds(budget.wall),
                    found.load
                ));
            }
            measured.push((program.name.clone(), step, found));
        }
    }
    Ok((measured, failures, warnings))
}

/// Whether `found` is more than `percent` over `budget`.
fn over(found: u128, budget: u128, percent: u128) -> bool {
    found * 100 > budget * (100 + percent)
}

fn print_line(name: &str, step: Step, found: &Found, budget: Option<Budget>) {
    let instructions = found.instructions.map_or_else(|| "-".to_owned(), billions);
    let budget = budget.map_or_else(|| "none".to_owned(), |budget| billions(budget.instructions));
    println!(
        "{name:<14} {:<6} {instructions:>14} {budget:>14} {:>9} {:>6.1}",
        step.name(),
        seconds(found.wall),
        found.load
    );
}

/// A count in billions, to three places.
fn billions(count: u64) -> String {
    let millions = count / 1_000_000;
    format!("{}.{:03} G", millions / 1000, millions % 1000)
}

fn seconds(wall: Duration) -> String {
    format!("{:.2} s", wall.as_secs_f64())
}

/// The real programs and the self-hosted front end, each copied into `work`.
fn programs(work: &Path) -> Result<Vec<Measured>, String> {
    let kernels = std::fs::canonicalize("bench/kernels").map_err(|err| format!("bench/kernels: {err}"))?;
    let mut programs = Vec::new();
    for name in REAL_PROGRAMS {
        let source = kernels.join(format!("{name}.fen"));
        let folder = work.join(name);
        std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        let path = folder.join(format!("{name}.fen"));
        std::fs::copy(&source, &path).map_err(|err| format!("{}: {err}", source.display()))?;
        programs.push(Measured {
            name: (*name).to_owned(),
            changed: path.clone(),
            path,
            folder: kernels.clone(),
            stdin: None,
        });
    }
    let selfhost = std::fs::canonicalize("selfhost").map_err(|err| format!("selfhost: {err}"))?;
    let kit = work.join("selfhost");
    copy_folder(&selfhost, &kit)?;
    // What it's given: its own main file to parse, as `fennec-bench` gives it the kernels.
    let input = work.join("selfhost.input");
    std::fs::write(&input, format!("tree {}\n", selfhost.join("src/main.fen").display()))
        .map_err(|err| format!("{}: {err}", input.display()))?;
    programs.push(Measured {
        name: "selfhost".to_owned(),
        changed: kit.join("src/main.fen"),
        path: kit,
        folder: selfhost,
        stdin: Some(input),
    });
    Ok(programs)
}

/// A copy of a kit's files, leaving out what a build of it left there.
fn copy_folder(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|err| format!("{}: {err}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|err| format!("{}: {err}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let name = entry.file_name();
        if name == "target" || name == ".fennec" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_folder(&path, &to.join(&name))?;
        } else {
            std::fs::copy(&path, to.join(&name)).map_err(|err| format!("{}: {err}", path.display()))?;
        }
    }
    Ok(())
}

/// Measures one step of one program, `runs` times, giving the medians.
fn measure(setup: &Setup, program: &Measured, step: Step, runs: usize) -> Result<Found, String> {
    let work = &setup.work;
    let home = work.join(format!("home-{}", program.name));
    let built = work.join(format!("{}.built", program.name));
    let path = program.path.to_string_lossy().into_owned();
    let built_text = built.to_string_lossy().into_owned();
    // A warm step starts from a Fennec folder that has built the program once.
    if step == Step::Warm {
        let _ = std::fs::remove_dir_all(&home);
        ran(setup, false, program, &home, &["build", &path, "-o", &built_text], false)?;
    }
    let mut counts = Vec::new();
    let mut walls = Vec::new();
    let mut loads = Vec::new();
    for at in 0..runs {
        let args: Vec<&str> = match step {
            Step::Cold => {
                let _ = std::fs::remove_dir_all(&home);
                vec!["build", "--release", &path, "-o", &built_text]
            }
            Step::Warm => {
                change(&program.changed, step, at)?;
                vec!["build", &path, "-o", &built_text]
            }
            Step::Check => {
                change(&program.changed, step, at)?;
                vec!["check", &path]
            }
            Step::Run => {
                // Built first, untimed, so each timed run finds it in the store.
                if at == 0 {
                    ran(setup, false, program, &home, &["build", &path, "-o", &built_text], false)?;
                }
                vec!["run", "--no-test", &path]
            }
        };
        loads.push(load());
        let (count, wall) = ran(setup, true, program, &home, &args, step == Step::Run)?;
        counts.extend(count);
        walls.push(wall);
    }
    walls.sort();
    loads.sort_by(f64::total_cmp);
    Ok(Found {
        instructions: if counts.len() == runs { median(&mut counts) } else { None },
        wall: walls.get(walls.len() / 2).copied().unwrap_or_default(),
        load: loads.get(loads.len() / 2).copied().unwrap_or_default(),
    })
}

/// The middle count, or the lower middle one, of some counts.
fn median(counts: &mut [u64]) -> Option<u64> {
    counts.sort_unstable();
    counts.get(counts.len().checked_sub(1)? / 2).copied()
}

/// Adds a line to a program's file, a function nothing calls, so the next build or check has
/// something new to read, and no two are the same.
fn change(file: &Path, step: Step, at: usize) -> Result<(), String> {
    use std::io::Write as _;
    let mut opened = std::fs::OpenOptions::new()
        .append(true)
        .open(file)
        .map_err(|err| format!("{}: {err}", file.display()))?;
    writeln!(opened, "\ndef changed_for_build_times_{}_{at}() -> int: return {at}", step.name())
        .map_err(|err| format!("{}: {err}", file.display()))
}

/// Runs `fennec` with `args` for `program`, in its Fennec folder `home`, giving how many
/// instructions it took, when `counting` and they can be counted, and how long. `own` counts only
/// `fennec` itself, not the program a run starts.
fn ran(
    setup: &Setup,
    counting: bool,
    program: &Measured,
    home: &Path,
    args: &[&str],
    own: bool,
) -> Result<(Option<u64>, Duration), String> {
    let (fennec, work) = (&setup.fennec, &setup.work);
    let counter = if counting { &setup.counter } else { &Counter::None };
    let report = work.join("count.txt");
    let children = work.join("children.txt");
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::write(&children, "");
    let mut command = match counter {
        Counter::Apple { .. } => {
            let mut time = Command::new("/usr/bin/time");
            time.arg("-l").arg("-o").arg(&report).arg(fennec);
            time
        }
        Counter::Perf => {
            let mut perf = Command::new("perf");
            perf.args(["stat", "-x,", "-e", "instructions:u", "-o"]).arg(&report);
            if own {
                perf.arg("--no-inherit");
            }
            perf.arg("--").arg(fennec);
            perf
        }
        Counter::None => Command::new(fennec),
    };
    command
        .args(args)
        .current_dir(&program.folder)
        .env("FENNEC_HOME", home)
        // The daemon would do the work in a process of its own, out of what's counted.
        .env("FENNEC_NO_DAEMON", "1")
        .env("FENNEC_RUN_WAITS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command.stdin(match &program.stdin {
        Some(path) => {
            std::fs::File::open(path).map(Stdio::from).map_err(|err| format!("{}: {err}", path.display()))?
        }
        None => Stdio::null(),
    });
    if let (Counter::Apple { tools }, false) = (counter, own) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![tools.clone()];
        paths.extend(std::env::split_paths(&path));
        command
            .env("PATH", std::env::join_paths(paths).map_err(|err| err.to_string())?)
            .env("FENNEC_LLVM_BIN", tools)
            .env("BUILD_TIMES_COUNTS", &children);
    }
    let started = Instant::now();
    let output = command.output().map_err(|err| format!("couldn't run {}: {err}", fennec.display()))?;
    let wall = started.elapsed();
    if !output.status.success() {
        return Err(format!(
            "`fennec {}` for {} failed:\n{}",
            args.join(" "),
            program.name,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let count = match counter {
        Counter::Apple { .. } => {
            let own = apple_counts(&std::fs::read_to_string(&report).unwrap_or_default());
            let started = apple_counts(&std::fs::read_to_string(&children).unwrap_or_default());
            own.map(|own| own + started.unwrap_or(0))
        }
        Counter::Perf => perf_count(&std::fs::read_to_string(&report).unwrap_or_default()),
        Counter::None => None,
    };
    Ok((count, wall))
}

/// Every `instructions retired` count in a `time -l` report, added up, or None if there's none.
fn apple_counts(report: &str) -> Option<u64> {
    let counts: Vec<u64> = report
        .lines()
        .filter(|line| line.contains("instructions retired"))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect();
    (!counts.is_empty()).then(|| counts.iter().sum())
}

/// The count in a `perf stat -x,` report: the first field of its line for the event.
fn perf_count(report: &str) -> Option<u64> {
    report.lines().find(|line| line.contains("instructions"))?.split(',').next()?.trim().parse().ok()
}

/// The machine's load over the last minute.
fn load() -> f64 {
    let text = if cfg!(target_os = "macos") {
        Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).replace(['{', '}'], ""))
            .unwrap_or_default()
    } else {
        std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
    };
    text.split_whitespace().next().and_then(|first| first.parse().ok()).unwrap_or(0.0)
}

/// How work is counted on this machine. On a Mac, the counting scripts are written into `work`.
fn counter(fennec: &Path, work: &Path) -> Result<Counter, String> {
    if cfg!(target_os = "macos") {
        let tools = work.join("tools");
        counting_tools(fennec, &tools)?;
        return Ok(Counter::Apple { tools });
    }
    let probe = Command::new("perf")
        .args(["stat", "-x,", "-e", "instructions:u", "--", "true"])
        .output()
        .is_ok_and(|output| {
            output.status.success() && perf_count(&String::from_utf8_lossy(&output.stderr)).is_some()
        });
    Ok(if probe { Counter::Perf } else { Counter::None })
}

/// A folder standing for LLVM's: `opt`, `llc` and `llvm-link` each start the real one through
/// `time -l`, writing its count to the file in `BUILD_TIMES_COUNTS`, and the rest are links to the
/// real ones. `cc` and `ld` do the same for the C compiler and the linker it starts, which `-B`
/// has it find here.
fn counting_tools(fennec: &Path, tools: &Path) -> Result<(), String> {
    let real = llvm_folder(fennec)?;
    std::fs::create_dir_all(tools).map_err(|err| format!("{}: {err}", tools.display()))?;
    let counted = |name: &str, command: &str| -> Result<(), String> {
        let script = tools.join(name);
        let text =
            format!("#!/bin/sh\nexec /usr/bin/time -l -a -o \"$BUILD_TIMES_COUNTS\" {command} \"$@\"\n");
        std::fs::write(&script, text).map_err(|err| format!("{}: {err}", script.display()))?;
        executable(&script)
    };
    for name in ["opt", "llc", "llvm-link"] {
        counted(name, &quoted(&real.join(name)))?;
    }
    let entries = std::fs::read_dir(&real).map_err(|err| format!("{}: {err}", real.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !tools.join(&name).exists() {
            link(&entry.path(), &tools.join(&name))?;
        }
    }
    let ld = Command::new("xcrun")
        .args(["-f", "ld"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .map_err(|err| format!("couldn't find the linker with `xcrun`: {err}"))?;
    counted("ld", &quoted(Path::new(&ld)))?;
    counted("cc", &format!("/usr/bin/cc -B {}", quoted(tools)))
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(from, to).map_err(|err| format!("{}: {err}", to.display()))
}

#[cfg(not(unix))]
fn link(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map(|_| ()).map_err(|err| format!("{}: {err}", to.display()))
}

#[cfg(unix)]
fn executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(not(unix))]
fn executable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// A path in single quotes, for a shell script.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Where `fennec` finds LLVM's tools: `FENNEC_LLVM_BIN`, the folder a release has beside it, or
/// the Rust toolchain's `llvm-tools`.
fn llvm_folder(fennec: &Path) -> Result<PathBuf, String> {
    if let Some(folder) = std::env::var_os("FENNEC_LLVM_BIN") {
        return Ok(PathBuf::from(folder));
    }
    let shipped = fennec.with_file_name("llvm").join("bin");
    if shipped.join("opt").is_file() {
        return Ok(shipped);
    }
    let rustc = |arg: &str| {
        Command::new("rustc")
            .args(["--print", arg])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .map_err(|err| format!("couldn't run `rustc` to find LLVM's tools: {err}"))
    };
    let folder = Path::new(&rustc("sysroot")?).join("lib/rustlib").join(rustc("host-tuple")?).join("bin");
    if folder.join("opt").is_file() {
        Ok(folder)
    } else {
        Err(format!("LLVM's tools aren't in {}: run `rustup component add llvm-tools`", folder.display()))
    }
}

/// The budgets in `path`: a line each, `program step instructions wall-milliseconds`, after
/// comments. None yet is no budgets.
fn read_budgets(path: &Path) -> Result<Vec<(String, Step, Budget)>, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(Vec::new()) };
    let mut budgets = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let parsed = match fields.as_slice() {
            [name, step, instructions, wall] => Step::named(step)
                .zip(instructions.parse().ok())
                .zip(wall.parse().ok())
                .map(|((step, instructions), wall)| {
                    ((*name).to_owned(), step, Budget { instructions, wall: Duration::from_millis(wall) })
                }),
            _ => None,
        };
        budgets.push(parsed.ok_or_else(|| {
            format!(
                "{}:{}: `{line}` isn't `program step instructions wall-milliseconds`",
                path.display(),
                number + 1
            )
        })?);
    }
    Ok(budgets)
}

fn write_budgets(path: &Path, measured: &[(String, Step, Found)]) -> Result<(), String> {
    let mut text = String::from(
        "# What `fennec-bench build-times` holds each step to, pinned by `fennec-bench build-times --pin`:\n\
         # a program, a step, the instructions it took, which a run may go over by 3% at most, and\n\
         # the milliseconds it took, which only warns when a run takes half again as long.\n",
    );
    // Counts barely move from machine to machine, but times do, and so can the linker's work.
    let _ = writeln!(text, "# Pinned on {} with {} cores.", crate::machine::cpu(), crate::machine::cores());
    for (name, step, found) in measured {
        let Some(instructions) = found.instructions else { continue };
        let _ = writeln!(text, "{name} {} {instructions} {}", step.name(), found.wall.as_millis());
    }
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{apple_counts, billions, median, over, perf_count};

    #[test]
    fn a_count_over_its_budget_by_more_than_the_slack_is_over() {
        assert!(!over(103, 100, 3));
        assert!(over(104, 100, 3));
        assert_eq!(billions(10_586_752_110), "10.586 G");
        assert_eq!(billions(45_000_000), "0.045 G");
    }

    #[test]
    fn the_counts_of_every_process_in_a_report_are_added_up() {
        let report = "  1.0 real 0.5 user\n  4000  instructions retired\n  9  cycles elapsed\n  2.0 real\n  600  instructions retired\n";
        assert_eq!(apple_counts(report), Some(4600));
        assert_eq!(apple_counts("  1.0 real\n"), None);
    }

    #[test]
    fn perf_reports_give_their_count() {
        assert_eq!(perf_count("# started on\n\n123456,,instructions:u,100.00,,\n"), Some(123_456));
        assert_eq!(perf_count("<not counted>,,instructions:u\n"), None);
    }

    #[test]
    fn the_median_is_the_middle_or_the_lower_middle() {
        assert_eq!(median(&mut [5, 1, 3]), Some(3));
        assert_eq!(median(&mut [4, 1, 3, 2]), Some(2));
        assert_eq!(median(&mut []), None);
    }
}
