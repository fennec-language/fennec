//! `fennec lsp`: an editor's session over standard input and output, run by the command
//! itself and by the build daemon.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// A kit whose `main` uses a name that doesn't exist, and a folder for Fennec's own files, in a
/// folder that holds both and goes when the first is dropped.
fn broken_kit(name: &str) -> std::io::Result<(fennec_temp::Temp, PathBuf, PathBuf)> {
    let dir = std::env::temp_dir().join(format!("fennec-lsp-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("kit/src"))?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit/kit.toml"), manifest)?;
    std::fs::write(dir.join("kit/src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(nope)\n")?;
    let kit = std::fs::canonicalize(dir.join("kit"))?;
    Ok((fennec_temp::Temp::at(dir.clone()), kit, dir.join("home")))
}

fn framed(message: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{message}", message.len())
}

/// Runs a whole session: opens `main.fen`, shuts down and exits. Gives back what the server
/// wrote, and whether it exited cleanly.
fn session(kit: &Path, home: &Path, daemon: bool) -> std::io::Result<(String, bool)> {
    session_with(kit, home, daemon, "def main(stdout: Stdout):\\n    stdout.print(nope)\\n")
}

/// A whole session that opens `main.fen` holding `source`, written as it goes in JSON.
fn session_with(kit: &Path, home: &Path, daemon: bool, source: &str) -> std::io::Result<(String, bool)> {
    let uri = fennec_lsp::uri_of(&kit.join("src/main.fen"));
    let messages = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_owned(),
        format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"fennec","version":1,"text":"{source}"}}}}}}"#
        ),
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"exit"}"#.to_owned(),
    ];
    let mut command = crate::executables::fennec_command();
    command.arg("lsp").env("FENNEC_HOME", home);
    if daemon {
        #[cfg(unix)]
        command.env("FENNEC_DAEMON_IDLE", crate::executables::DAEMON_IDLE);
        command.env_remove("FENNEC_NO_DAEMON").env("FENNEC_DAEMONS", crate::executables::daemons_of(home));
    } else {
        command.env("FENNEC_NO_DAEMON", "1");
    }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(messages.iter().map(|message| framed(message)).collect::<String>().as_bytes())?;
    }
    let output = child.wait_with_output()?;
    Ok((String::from_utf8_lossy(&output.stdout).into_owned(), output.status.success()))
}

fn check(output: &str) {
    assert!(output.contains(r#""id":1,"result":{"capabilities""#), "{output}");
    assert!(output.contains("textDocument/publishDiagnostics") && output.contains("nope"), "{output}");
    assert!(output.contains(r#""id":2,"result":null"#), "{output}");
}

#[test]
fn the_language_server_runs_in_process_without_a_daemon() {
    let (_folder, kit, home) = broken_kit("in-process").unwrap();
    let (output, clean) = session(&kit, &home, false).unwrap();
    check(&output);
    assert!(clean);
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn the_language_server_runs_in_the_daemon() {
    let (_folder, kit, home) = broken_kit("daemon").unwrap();
    let _stopped = crate::executables::DaemonsStopped { home: &home };
    let (output, clean) = session(&kit, &home, true).unwrap();
    check(&output);
    assert!(clean);
    let status = crate::executables::fennec_command()
        .args(["daemon", "status"])
        .env("FENNEC_HOME", &home)
        .env_remove("FENNEC_NO_DAEMON")
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(status.contains("1 request served"), "the session didn't run in the daemon: {status}");
    let _ = crate::executables::fennec_command().args(["daemon", "stop"]).env("FENNEC_HOME", &home).output();
    // It goes, and takes its note with it.
    let noted = || std::fs::read_dir(crate::executables::daemons_of(&home)).map_or(0, Iterator::count);
    let gone = (0..250).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        noted() == 0
    });
    assert!(gone, "the daemon didn't stop");
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// A program a million brackets deep, which ran a parser out of stack, as JSON text.
fn too_deep() -> String {
    format!("def main(stdout: Stdout):\\n    x = {}1{}\\n", "(".repeat(1_000_000), ")".repeat(1_000_000))
}

/// The start of what a server wrote, which is a million brackets long when it echoes the program.
fn start(output: &str) -> String {
    output.chars().take(2000).collect()
}

/// An editor's session lives as long as the editor: a program nested too deep gets the error, and
/// the server goes on to answer what comes after it.
fn refused(output: &str) {
    assert!(
        output.contains("textDocument/publishDiagnostics")
            && output.contains("this nests more than 1000 deep"),
        "{}",
        start(output)
    );
    assert!(output.contains(r#""id":2,"result":null"#), "{}", start(output));
}

#[test]
fn a_program_nested_too_deep_is_refused_in_process() {
    let (_folder, kit, home) = broken_kit("deep-in-process").unwrap();
    let (output, clean) = session_with(&kit, &home, false, &too_deep()).unwrap();
    refused(&output);
    assert!(clean);
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

/// The build daemon serves every later build and editor, so it gives the error and goes on: the
/// next session it serves answers as ever.
#[cfg(unix)]
#[test]
fn a_program_nested_too_deep_leaves_the_daemon_serving() {
    let (_folder, kit, home) = broken_kit("deep-daemon").unwrap();
    let _stopped = crate::executables::DaemonsStopped { home: &home };
    let (output, clean) = session_with(&kit, &home, true, &too_deep()).unwrap();
    refused(&output);
    assert!(clean);
    let (output, clean) = session(&kit, &home, true).unwrap();
    check(&output);
    assert!(clean);
    let status = crate::executables::fennec_command()
        .args(["daemon", "status"])
        .env("FENNEC_HOME", &home)
        .env_remove("FENNEC_NO_DAEMON")
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(status.contains("2 requests served"), "both sessions didn't run in one daemon: {status}");
    let _ = crate::executables::fennec_command().args(["daemon", "stop"]).env("FENNEC_HOME", &home).output();
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}
