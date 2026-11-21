//! std's private parts, tested from outside std. The programs in `fixtures/std_private/` import
//! what std keeps for its own code, as Postgres's wire protocol, which only `fennec-std-tests`, the
//! repository's test build of the command, lets them do. Each prints what its `.out` says, and its
//! tests pass. `fennec` itself refuses those imports, whatever's set.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::executables::{FENNEC_DEADLINE, Within as _, runtime_built};

/// `fennec-std-tests`, run as a test runs `fennec`: building in its own process.
fn std_tests_command() -> Command {
    runtime_built();
    let mut command = Command::new(env!("CARGO_BIN_EXE_fennec-std-tests"));
    command.env("FENNEC_NO_DAEMON", "1");
    command
}

/// The programs that test std's private parts, each with the `.out` it prints.
fn programs() -> std::io::Result<Vec<PathBuf>> {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/std_private");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Each program that tests std's private parts prints what its `.out` says, through LLVM, and
/// its tests pass.
#[test]
fn std_private_parts_print_what_they_should_and_pass_their_tests() -> std::io::Result<()> {
    if !crate::llvm::ready() {
        return Ok(());
    }
    let programs = programs()?;
    assert!(programs.len() >= 5, "only {} programs test std's private parts", programs.len());
    let mut wrong = Vec::new();
    for path in &programs {
        let shown = path.to_string_lossy();
        let should = std::fs::read_to_string(path.with_extension("out"))?;
        let ran = std_tests_command().args(["run", "--no-test", &shown]).output_before(FENNEC_DEADLINE)?;
        let printed = String::from_utf8_lossy(&ran.stdout);
        if !ran.status.success() || printed != should {
            let said = String::from_utf8_lossy(&ran.stderr);
            wrong.push(format!("{}: printed {printed:?}, not {should:?}, and said {said}", path.display()));
        }
        let tested = std_tests_command().args(["test", "--all", &shown]).output_before(FENNEC_DEADLINE)?;
        if !tested.status.success() {
            let said = [tested.stdout, tested.stderr].concat();
            wrong.push(format!("{}: its tests failed:\n{}", path.display(), String::from_utf8_lossy(&said)));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}

/// `fennec` refuses to import a name std keeps for its own code, with `FENNEC_STD_PRIVATE` set
/// as well: nothing it reads lets a program in, and only the test build's own command,
/// `fennec-std-tests`, imports one.
#[test]
fn the_fennec_command_never_imports_what_std_keeps_for_itself() -> std::io::Result<()> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-std-private-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("private.fen");
    std::fs::write(
        &program,
        "from std.string import quoted_input\n\ndef main(stdout: Stdout):\n    stdout.print(quoted_input(\"x\"))\n    return\n",
    )?;
    let shown = program.to_string_lossy();
    let refused = crate::executables::fennec_command()
        .args(["check", &shown])
        .env("FENNEC_STD_PRIVATE", "1")
        .output_within()?;
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "fennec let a program import a private name: {said}");
    assert!(said.contains("`std.string` has nothing called `quoted_input`"), "{said}");
    let checked = std_tests_command().args(["check", &shown]).output_within()?;
    let said = String::from_utf8_lossy(&checked.stderr);
    assert!(checked.status.success(), "fennec-std-tests refused the import: {said}");
    Ok(())
}

/// `fennec-std-tests` works in its own process, with the build daemon allowed: it never starts one.
/// One it started would share `fennec`'s, keyed by the same build, and what a daemon keeps from
/// one command to the next would then reach `fennec`. `fennec` itself, run the same way, does start
/// one, so a start would be seen.
#[cfg(unix)]
#[test]
fn fennec_std_tests_starts_no_daemon() -> std::io::Result<()> {
    if !crate::llvm::ready() {
        return Ok(());
    }
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-std-tests-daemon-{}", std::process::id())),
    );
    let home = dir.join("home");
    std::fs::create_dir_all(&home)?;
    let _stopped = crate::executables::DaemonsStopped { home: &home };
    let program = dir.join("hello.fen");
    std::fs::write(&program, "def main(stdout: Stdout):\n    stdout.print(\"hi\")\n    return\n")?;
    let shown = program.to_string_lossy();
    // As a test's own daemons are allowed to start, in a home in the temp folder.
    let allowed = |mut command: Command| {
        command
            .env("FENNEC_HOME", &home)
            .env("FENNEC_DAEMONS", crate::executables::daemons_of(&home))
            .env("FENNEC_DAEMON_IDLE", crate::executables::DAEMON_IDLE)
            .env_remove("FENNEC_NO_DAEMON")
            .env_remove("CI");
        command
    };
    let status = || -> std::io::Result<String> {
        let said =
            allowed(crate::executables::fennec_command()).args(["daemon", "status"]).output_within()?;
        Ok(String::from_utf8_lossy(&said.stdout).into_owned())
    };
    let ran = allowed(crate::executables::fennec_command())
        .args(["run", "--no-test", &shown])
        .output_before(FENNEC_DEADLINE)?;
    assert!(ran.status.success() && ran.stdout == b"hi\n", "{}", String::from_utf8_lossy(&ran.stderr));
    let running = status()?;
    assert!(running.starts_with("running:"), "fennec started no daemon, so none would be seen: {running}");
    allowed(crate::executables::fennec_command()).args(["daemon", "stop"]).output_within()?;
    let mut stopped = status()?;
    for _ in 0..50 {
        if stopped == "not running\n" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        stopped = status()?;
    }
    assert_eq!(stopped, "not running\n", "fennec's daemon didn't stop");
    let ran =
        allowed(std_tests_command()).args(["run", "--no-test", &shown]).output_before(FENNEC_DEADLINE)?;
    assert!(ran.status.success() && ran.stdout == b"hi\n", "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(status()?, "not running\n", "fennec-std-tests started a daemon");
    Ok(())
}
