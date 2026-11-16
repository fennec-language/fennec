//! A failure stays fixed: every input the fuzzer has kept runs clean. A short fixed run of each
//! target checks the fuzzer itself still works, and that nothing it would soon find has crept in.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};

/// Whether a file beside the inputs is an output pinned for one.
fn pinned_output(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "out" || extension == "fails")
}

fn fuzz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fennec-fuzz"))
}

/// How a `fennec-fuzz run` ended, for a failing test to say: its exit code, or the signal that
/// stopped it from outside, the target it was in then, and all it wrote, standard error too.
fn ended(output: &std::process::Output) -> String {
    let printed = String::from_utf8_lossy(&output.stdout);
    let said = String::from_utf8_lossy(&output.stderr);
    // Each target says it's starting, and then what it ran: the last to start with nothing
    // after it is the one the run ended in.
    let last = printed.lines().rev().find_map(|line| line.strip_suffix(": starting"));
    let finished = |target: &str| {
        printed.lines().any(|line| line.starts_with(&format!("{target}: ")) && line.contains(" inputs, "))
    };
    let when = match last {
        Some(target) if !finished(target) => format!("while {target} was running"),
        Some(target) => format!("after {target} finished"),
        None => "before any target started".to_owned(),
    };
    format!("`fennec-fuzz run` ended with {} {when}\n--- printed\n{printed}--- said\n{said}", output.status)
}

#[test]
fn every_kept_input_runs_clean() {
    let found = Path::new(env!("CARGO_MANIFEST_DIR")).join("found");
    for folder in std::fs::read_dir(found).unwrap() {
        let folder = folder.unwrap().path();
        if !folder.is_dir() {
            continue;
        }
        let target = folder.file_name().unwrap().to_string_lossy().into_owned();
        for input in std::fs::read_dir(&folder).unwrap() {
            let input = input.unwrap().path();
            if pinned_output(&input) {
                continue;
            }
            let output = fuzz().args(["one", &target]).arg(&input).output().unwrap();
            assert!(
                output.status.success(),
                "{}: {}\n{}{}",
                input.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
}

/// Every target runs the same hundred inputs, however busy the machine is: the run has no time
/// limit, and an input counts as stuck only after two minutes of CPU time, far more than any takes.
#[test]
fn a_short_run_of_every_target_finds_nothing() {
    let dir = std::env::temp_dir().join(format!("fennec-fuzz-short-{}", std::process::id()));
    let output = fuzz()
        .args(["run", "--seed", "1", "--inputs", "100", "--stuck", "120", "--found"])
        .arg(&dir)
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{}", ended(&output));
    let targets = [
        "lexer",
        "parser",
        "formatter",
        "checker",
        "lengths",
        "csv",
        "json",
        "json-rules",
        "pack",
        "toml",
        "toml-rules",
        "http-client",
        "http-server",
        "postgres",
        "regex",
        "url",
        "runtime",
        "utf8",
    ];
    // Those that run natively are left out, saying so, where nothing can be built.
    let native = [
        "csv",
        "http-client",
        "http-server",
        "json",
        "json-rules",
        "pack",
        "postgres",
        "toml",
        "toml-rules",
        "url",
        "utf8",
    ];
    let left_out = printed.contains(
        "csv, http-client, http-server, json, json-rules, pack, postgres, toml, toml-rules, url, utf8: left out",
    );
    for target in targets {
        if left_out && native.contains(&target) {
            continue;
        }
        assert!(printed.contains(&format!("{target}: 100 inputs, 0 failures")), "{target}: {printed}");
    }
}

/// Each program the `llvm` target starts from builds both ways and prints what's pinned for it,
/// where LLVM's tools are here.
#[test]
fn every_program_to_start_from_prints_what_is_pinned() {
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/llvm");
    for input in std::fs::read_dir(seeds).unwrap() {
        let input = input.unwrap().path();
        if pinned_output(&input) {
            continue;
        }
        assert!(input.with_extension("out").exists(), "{} has no output pinned", input.display());
        let output = fuzz().args(["one", "llvm"]).arg(&input).output().unwrap();
        assert!(
            output.status.success(),
            "{}: {}\n{}",
            input.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A run stopped from outside says so, with how it ended and the target it was in, so a failure of
/// the test above shows whether the run crashed or was killed. Killed, it ends by SIGKILL on Unix,
/// and on Windows, which has no signals, with the exit code `TerminateProcess` gives it, 1.
#[test]
fn a_run_stopped_from_outside_says_where() {
    let mut child = fuzz()
        .args(["run", "--seed", "1", "--inputs", "300", "lexer"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut printed = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    printed.read_line(&mut first).unwrap();
    assert_eq!(first, "lexer: starting\n");
    child.kill().unwrap();
    let mut rest = Vec::new();
    printed.read_to_end(&mut rest).unwrap();
    let mut said = Vec::new();
    child.stderr.take().unwrap().read_to_end(&mut said).unwrap();
    let status = child.wait().unwrap();
    let stdout = [first.into_bytes(), rest].concat();
    let shown = ended(&std::process::Output { status, stdout, stderr: said });
    #[cfg(unix)]
    assert!(shown.contains("signal: 9 (SIGKILL)"), "{shown}");
    #[cfg(windows)]
    assert!(shown.contains("exit code: 1"), "{shown}");
    assert!(shown.contains("while lexer was running") || shown.contains("after lexer finished"), "{shown}");
}

#[test]
fn a_target_that_isnt_there_is_an_error() {
    let output = fuzz().args(["run", "no such target"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
