//! Keeping the store's cache under its cap when the daemon isn't what's running the commands.
//!
//! The daemon cleans the cache every five minutes while it's idle, but a command run with
//! `FENNEC_NO_DAEMON`, or one the daemon doesn't serve, never meets it. So any command, once no
//! cleanup has run for an hour, starts `fennec cache clean` on its own, which goes on after the
//! command is done, so the command never waits for it.

use std::process::{Command, ExitCode, Stdio};

use fennec_build::Cache;

/// Starts a cleanup in the background if one is due.
pub(crate) fn start_if_due() {
    let Some(cache) = crate::run::cache() else { return };
    if !cache.cleanup_due() {
        return;
    }
    let Ok(fennec) = std::env::current_exe() else { return };
    let started = Command::new(fennec)
        .args(["cache", "clean", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = started {
        // Waited for, so it isn't left a zombie by a command that runs a long time, like the
        // language server. A command that ends first leaves it to finish by itself.
        std::thread::spawn(move || child.wait());
    }
}

/// `fennec cache clean [--quiet]`: keeps the cache under its cap now, and clears out work folders
/// left behind, saying what it removed.
pub(crate) fn clean(quiet: bool) -> ExitCode {
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: there's no home folder to find the cache in");
        return ExitCode::FAILURE;
    };
    let cache = Cache::new(&home);
    let cap = Cache::cap();
    match cache.cleanup(cap) {
        Ok(cleanup) => {
            fennec_build::sweep_work(&home, fennec_build::WORK_KEPT);
            if !quiet {
                if cleanup.skipped {
                    println!("🦊 another cleanup of the cache is running");
                } else {
                    println!(
                        "🦊 cache: removed {} entries, {}; it holds {} of its {} cap",
                        cleanup.removed,
                        megabytes(cleanup.freed),
                        megabytes(cleanup.size),
                        megabytes(cap)
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: couldn't clean the cache in {}: {err}", home.join("cache").display());
            ExitCode::FAILURE
        }
    }
}

#[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal place")]
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}
