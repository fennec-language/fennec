//! `fennec lsp`: an editor's session over standard input and output, run by the command
//! itself and by the build daemon.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A kit whose `main` uses a name that doesn't exist, and a folder for Fennec's own files.
fn broken_kit(name: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = std::env::temp_dir().join(format!("fennec-lsp-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("kit/src"))?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit/kit.toml"), manifest)?;
    std::fs::write(dir.join("kit/src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(nope)\n")?;
    Ok((std::fs::canonicalize(dir.join("kit"))?, dir.join("home")))
}

fn framed(message: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{message}", message.len())
}

/// Runs a whole session: opens `main.fen`, shuts down and exits. Gives back what the server
/// wrote, and whether it exited cleanly.
fn session(kit: &Path, home: &Path, daemon: bool) -> std::io::Result<(String, bool)> {
    let uri = fennec_lsp::uri_of(&kit.join("src/main.fen"));
    let source = "def main(stdout: Stdout):\\n    stdout.print(nope)\\n";
    let messages = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_owned(),
        format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"fennec","version":1,"text":"{source}"}}}}}}"#
        ),
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"exit"}"#.to_owned(),
    ];
    let mut command = Command::new(env!("CARGO_BIN_EXE_fennec"));
    command.arg("lsp").env("FENNEC_HOME", home).env("FENNEC_DAEMON_IDLE", "30");
    if daemon {
        command.env_remove("FENNEC_NO_DAEMON");
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
    let (kit, home) = broken_kit("in-process").unwrap();
    let (output, clean) = session(&kit, &home, false).unwrap();
    check(&output);
    assert!(clean);
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn the_language_server_runs_in_the_daemon() {
    let (kit, home) = broken_kit("daemon").unwrap();
    let (output, clean) = session(&kit, &home, true).unwrap();
    check(&output);
    assert!(clean);
    let status = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(["daemon", "status"])
        .env("FENNEC_HOME", &home)
        .env_remove("FENNEC_NO_DAEMON")
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(status.contains("1 request served"), "the session didn't run in the daemon: {status}");
    let _ = Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(["daemon", "stop"])
        .env("FENNEC_HOME", &home)
        .output();
    std::fs::remove_dir_all(kit.parent().unwrap()).unwrap();
}
