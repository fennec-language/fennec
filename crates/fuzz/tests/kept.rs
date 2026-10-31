//! A failure stays fixed: every input the fuzzer has kept runs clean. A short fixed run of each
//! target checks the fuzzer itself still works, and that nothing it would soon find has crept in.

use std::path::Path;
use std::process::Command;

fn fuzz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fennec-fuzz"))
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
            let status = fuzz().args(["one", &target]).arg(&input).status().unwrap();
            assert!(status.success(), "{}: {status}", input.display());
        }
    }
}

#[test]
fn a_short_run_of_every_target_finds_nothing() {
    let dir = std::env::temp_dir().join(format!("fennec-fuzz-short-{}", std::process::id()));
    let output =
        fuzz().args(["run", "--seconds", "120", "--inputs", "100", "--found"]).arg(&dir).output().unwrap();
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("lexer: 100 inputs, 0 failures"), "{printed}");
    assert!(printed.contains("utf8: 100 inputs, 0 failures"), "{printed}");
}

#[test]
fn a_target_that_isnt_there_is_an_error() {
    let output = fuzz().args(["run", "no such target"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
