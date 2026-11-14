//! `fennec-oracle`: runs a Fennec program the oracle's way, compares it with the compiled
//! program, or reports how much of the examples it covers.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use fennec_oracle::{Ended, Outcome, report};

const USAGE: &str = "usage:
  fennec-oracle run <program.fen>                    run it the oracle's way
  fennec-oracle compare <program.fen> [--fennec PATH]  hold the compiled program, debug and release, to the oracle
  fennec-oracle coverage [examples dir]              run the oracle over the examples, and say what it covers

exit codes: run gives the program's own; compare gives 0 when they agree and 1 when they differ;
3 means the oracle doesn't model something the program uses";

/// How long a compiled program, or a build of one, may take.
const CHILD_SECONDS: u64 = 60;

/// How much memory a compiled program, with what it starts, may hold.
const CHILD_MEGABYTES: u64 = 4000;

/// The exit code for a program the oracle can't run.
const UNSUPPORTED: u8 = 3;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["run", path] => run(path),
        ["compare", path] => compare(path, None),
        ["compare", path, "--fennec", fennec] | ["compare", "--fennec", fennec, path] => {
            compare(path, Some(fennec))
        }
        ["coverage"] => coverage("examples"),
        ["coverage", dir] => coverage(dir),
        _ => {
            say_err(USAGE);
            ExitCode::from(2)
        }
    }
}

fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn say_err(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{text}");
}

fn run(path: &str) -> ExitCode {
    let outcome = fennec_oracle::run_path(Path::new(path), path);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&outcome.stdout);
    let _ = out.flush();
    match outcome.ended {
        Ended::Exited(code) => ExitCode::from(code),
        Ended::Stopped(said) => {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(said.as_bytes());
            ExitCode::from(1)
        }
        Ended::Unsupported(why) => {
            say_err(&format!("unsupported: {why}"));
            ExitCode::from(UNSUPPORTED)
        }
    }
}

fn coverage(dir: &str) -> ExitCode {
    let examples = match report::examples(Path::new(dir)) {
        Ok(examples) => examples,
        Err(err) => {
            say_err(&format!("couldn't read {dir}: {err}"));
            return ExitCode::from(2);
        }
    };
    let mut judged = Vec::with_capacity(examples.len());
    for example in &examples {
        let verdict = report::judge(example);
        say(&report::line(example, &verdict));
        judged.push(verdict);
    }
    say(&report::summary(&examples, &judged));
    if judged.iter().any(|verdict| matches!(verdict, report::Verdict::Differs(_))) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// What a compiled run gave: its exit code, or how it ended otherwise, and what it printed.
struct Compiled {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn compare(path: &str, fennec: Option<&str>) -> ExitCode {
    let outcome = fennec_oracle::run_path(Path::new(path), path);
    let Some((code, _)) = report::oracle_side(&outcome) else {
        if let Ended::Unsupported(why) = &outcome.ended {
            say_err(&format!("unsupported: {why}"));
        }
        return ExitCode::from(UNSUPPORTED);
    };
    let fennec = fennec.map_or_else(default_fennec, PathBuf::from);
    let sorted = varies(path);
    let mut agree = true;
    let debug = limited(Command::new(&fennec).args(["run", "--no-test", path]));
    agree &= judged("debug", &outcome, code, debug, sorted);
    let built = std::env::temp_dir().join(format!("fennec-oracle-{}", std::process::id()));
    let shown_built = built.to_string_lossy().into_owned();
    let release = limited(Command::new(&fennec).args(["build", "--release", path, "-o", &shown_built]))
        .and_then(|build| {
            if build.code == Some(0) {
                limited(&mut Command::new(&built))
            } else {
                Err(format!("the release build failed:\n{}", String::from_utf8_lossy(&build.stderr)))
            }
        });
    let _ = std::fs::remove_file(&built);
    agree &= judged("release", &outcome, code, release, sorted);
    if agree { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// Holds one compiled run to the oracle's, saying how they differ if they do.
fn judged(
    which: &str,
    outcome: &Outcome,
    code: u8,
    compiled: Result<Compiled, String>,
    sorted: bool,
) -> bool {
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(why) => {
            say(&format!("{which}: couldn't run the compiled program: {why}\n"));
            return false;
        }
    };
    let ours = report::as_compared(&outcome.stdout, sorted);
    let theirs = report::as_compared(&compiled.stdout, sorted);
    let mut same = true;
    if ours != theirs {
        say(&format!(
            "{which}: standard output differs (- compiled, + oracle)\n{}",
            report::diff(&theirs, &ours)
        ));
        same = false;
    }
    if compiled.code != Some(i32::from(code)) {
        let theirs =
            compiled.code.map_or_else(|| "no exit code".to_owned(), |code| format!("exit code {code}"));
        say(&format!(
            "{which}: the compiled program ended with {theirs}, the oracle with exit code {code}\n{}",
            String::from_utf8_lossy(&compiled.stderr)
        ));
        same = false;
    }
    if same {
        say(&format!("{which}: agrees\n"));
    }
    same
}

/// Whether a program's lines may come in another order each run, as `determinism-varies.txt` at
/// the checkout's root lists them, one path a line.
fn varies(path: &str) -> bool {
    let mut dir = std::env::current_dir().ok();
    while let Some(at) = dir {
        let listed = at.join("determinism-varies.txt");
        if let Ok(text) = std::fs::read_to_string(&listed) {
            let wanted = Path::new(path);
            return text.lines().map(str::trim).any(|line| !line.is_empty() && wanted.ends_with(line));
        }
        if at.join("Cargo.toml").is_file() && at.join("examples").is_dir() {
            return false;
        }
        dir = at.parent().map(Path::to_path_buf);
    }
    false
}

/// The `fennec` built beside this, or the one on the path.
fn default_fennec() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("fennec")));
    beside.filter(|fennec| fennec.is_file()).unwrap_or_else(|| PathBuf::from("fennec"))
}

/// Runs a command in a process group of its own, with empty input, stopping the whole group past
/// the time or memory limit.
fn limited(command: &mut Command) -> Result<Compiled, String> {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("couldn't start it: {err}"))?;
    let group = child.id();
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);
    let started = Instant::now();
    let mut stopped = None;
    let mut polls: u32 = 0;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => return Err(err.to_string()),
        }
        if started.elapsed() > Duration::from_secs(CHILD_SECONDS) {
            stopped = Some(format!("it ran past {CHILD_SECONDS} seconds"));
        } else if polls.is_multiple_of(10) && group_megabytes(group) > CHILD_MEGABYTES {
            stopped = Some(format!("it held more than {CHILD_MEGABYTES} MB"));
        }
        if stopped.is_some() {
            kill_group(group);
            let _ = child.kill();
            break child.wait().map_err(|err| err.to_string())?;
        }
        polls = polls.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(20));
    };
    kill_group(group);
    if let Some(why) = stopped {
        return Err(why);
    }
    let stdout = stdout.map(|reader| reader.join().unwrap_or_default()).unwrap_or_default();
    let stderr = stderr.map(|reader| reader.join().unwrap_or_default()).unwrap_or_default();
    Ok(Compiled { code: status.code(), stdout, stderr })
}

fn read_all(mut from: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = from.read_to_end(&mut all);
        all
    })
}

/// How many megabytes a process group holds, as `ps` says.
fn group_megabytes(group: u32) -> u64 {
    let Ok(listed) = Command::new("ps").args(["-o", "rss=", "-g", &group.to_string()]).output() else {
        return 0;
    };
    let kilobytes: u64 = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .sum();
    kilobytes / 1024
}

fn kill_group(group: u32) {
    let _ = Command::new("kill").args(["-9", &format!("-{group}")]).stderr(Stdio::null()).status();
}
