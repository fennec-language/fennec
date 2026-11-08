//! The launcher, installed as `fennec`:
//!
//! ```text
//! fennec [--offline] <command>...       runs the command with the kit's toolchain
//! fennec toolchain list                 the toolchains installed, and the default
//! fennec toolchain install <version>
//! fennec toolchain remove <version>
//! fennec toolchain default <version>    the one run outside a kit
//! fennec update [--latest]              pins the kit to the version kit.toml asks for, or the newest,
//!                                       then has that toolchain pin the kit's dependencies
//! fennec --latest                       the newest release, the only thing that asks
//! ```
//!
//! `--offline`, or `FENNEC_OFFLINE`, never fetches, and the toolchain is told so too. `FENNEC_DIST` is where releases come from, a
//! mirror's address or a folder, and `FENNEC_HOME` where they go.
#![expect(clippy::print_stdout, reason = "the launcher lists toolchains")]
#![expect(clippy::print_stderr, reason = "and says why it couldn't run one")]

use std::path::PathBuf;
use std::process::ExitCode;

use fennec_launcher::{Launcher, RELEASES, TARGET};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let offline = args.first().is_some_and(|first| first == "--offline");
    if offline {
        args.remove(0);
    }
    match launched(&args, offline || std::env::var_os("FENNEC_OFFLINE").is_some()) {
        Ok(code) => code,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

fn launched(args: &[String], offline: bool) -> Result<ExitCode, String> {
    let home = match std::env::var_os("FENNEC_HOME") {
        Some(home) => PathBuf::from(home),
        None => std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|home| PathBuf::from(home).join(".fennec"))
            .ok_or("couldn't find a home folder; set FENNEC_HOME")?,
    };
    let launcher = Launcher {
        home,
        dist: std::env::var("FENNEC_DIST").unwrap_or_else(|_| RELEASES.to_owned()),
        offline,
        trusted: fennec_dist::release_keys()?,
        target: TARGET.to_owned(),
    };
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["toolchain", "list"] => {
            let default = launcher.default_version();
            for version in launcher.installed() {
                let mark = if default.as_deref() == Some(version.as_str()) { " (default)" } else { "" };
                println!("{version}{mark}");
            }
            Ok(ExitCode::SUCCESS)
        }
        ["toolchain", "install", version] => launcher.ensure(version, None).map(|_| ExitCode::SUCCESS),
        ["toolchain", "remove", version] => launcher.remove(version).map(|()| ExitCode::SUCCESS),
        ["toolchain", "default", version] => launcher.set_default(version).map(|()| ExitCode::SUCCESS),
        ["--latest"] => {
            println!("{}", launcher.latest()?);
            Ok(ExitCode::SUCCESS)
        }
        ["update"] | ["update", "--latest"] => {
            let here = std::env::current_dir()
                .map_err(|err| format!("couldn't tell which folder this is: {err}"))?;
            print!("{}", launcher.update(&here, words.len() == 2)?);
            hand_off(&launcher.toolchain_for(&here)?, &["update".to_owned()], offline)
        }
        ["toolchain", ..] => Err(
            "`fennec toolchain` takes `list`, `install <version>`, `remove <version>` or `default <version>`"
                .to_owned(),
        ),
        _ => {
            let here = std::env::current_dir()
                .map_err(|err| format!("couldn't tell which folder this is: {err}"))?;
            hand_off(&launcher.toolchain_for(&here)?, args, offline)
        }
    }
}

/// Runs the toolchain's `fennec` in place of this process, with the same arguments.
#[cfg(unix)]
fn hand_off(command: &std::path::Path, args: &[String], offline: bool) -> Result<ExitCode, String> {
    use std::os::unix::process::CommandExt as _;
    let err = toolchain(command, args, offline).exec();
    Err(format!("couldn't run {}: {err}", command.display()))
}

/// Runs the toolchain's `fennec`, with the same arguments, and exits as it does.
#[cfg(not(unix))]
fn hand_off(command: &std::path::Path, args: &[String], offline: bool) -> Result<ExitCode, String> {
    let status = toolchain(command, args, offline)
        .status()
        .map_err(|err| format!("couldn't run {}: {err}", command.display()))?;
    Ok(status.code().and_then(|code| u8::try_from(code).ok()).map_or(ExitCode::FAILURE, ExitCode::from))
}

/// The toolchain's `fennec` with the arguments, told to fetch nothing when the launcher was.
fn toolchain(command: &std::path::Path, args: &[String], offline: bool) -> std::process::Command {
    let mut toolchain = std::process::Command::new(command);
    toolchain.args(args);
    if offline {
        toolchain.env("FENNEC_OFFLINE", "1");
    }
    toolchain
}
