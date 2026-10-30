//! `fennec lsp`: the language server, which an editor starts and talks to over standard
//! input and output. The session runs in the build daemon, so every editor shares one engine:
//! this command only carries messages between the editor and the daemon. Without a daemon,
//! it runs the session itself.

use std::process::ExitCode;

pub(crate) fn lsp() -> ExitCode {
    #[cfg(unix)]
    if let Some(connection) =
        fennec_build::fennec_home().and_then(|home| crate::daemon::language_server(&home))
    {
        return carry(connection);
    }
    crate::big_stack(|| {
        let served = fennec_lsp::serve(&mut std::io::stdin().lock(), &mut std::io::stdout().lock());
        if served.unwrap_or(false) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
    })
}

/// Carries the editor's messages to the daemon and its answers back, until either side is
/// done.
#[cfg(unix)]
fn carry(connection: std::os::unix::net::UnixStream) -> ExitCode {
    let Ok(mut to_daemon) = connection.try_clone() else { return ExitCode::FAILURE };
    // The editor's side ends when the editor closes standard input; the daemon then sees the
    // end of the session too. The thread is never waited for: it may be blocked reading from
    // an editor that's already gone.
    let _editor = std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut to_daemon);
        let _ = to_daemon.shutdown(std::net::Shutdown::Write);
    });
    if forward(connection, &mut std::io::stdout().lock()).is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Copies everything from the daemon to the editor, flushing as it goes: the editor waits
/// for each message, and standard output only flushes by itself at a newline.
#[cfg(unix)]
fn forward(mut from: impl std::io::Read, to: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = from.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else { return Ok(()) };
        to.write_all(chunk)?;
        to.flush()?;
    }
}
