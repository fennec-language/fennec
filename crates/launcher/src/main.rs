//! The launcher, installed as `fennec`:
//!
//! ```text
//! fennec [--offline] <command>...       runs the command with the kit's toolchain
//! fennec +<version> <command>...        runs it with that version, in a kit or not, this once
//! fennec --version <version> <command>...  the version for a script outside a kit
//! fennec default [<version>]            says or sets the version for scripts outside a kit; with
//!                                       none set, they run with the newest stable one installed
//! fennec new <path>                     a new kit, made by the version for scripts, which it pins
//! fennec toolchain list                 the toolchains installed, the one for scripts, and this kit's
//! fennec toolchain install <version>    or `--latest`, the newest release
//! fennec toolchain remove <version>     or unlinks a linked build, leaving it where it is
//! fennec toolchain link <name> <path>   runs the `fennec` in a folder of your own, like a checkout's
//!                                       `target/release`, as `<name>`: `fennec = "dev"` in a kit
//! fennec update [--latest]              pins the kit to the version kit.toml asks for, or the newest,
//!                                       then has that toolchain pin the kit's dependencies
//! fennec --latest                       the newest release, the only thing that asks
//! ```
//!
//! It hands a command over in about the time a program takes to start: it reads the kit's files and
//! runs the toolchain, and reaches the network only through `fennec-fetch`, beside it, when
//! something has to be fetched (see `fennec_launcher::Helper`).
//!
//! `--offline`, or `FENNEC_OFFLINE`, never fetches, and the toolchain is told so too. `FENNEC_DIST` is where releases come from, a
//! mirror's address or a folder, and `FENNEC_HOME` where they go.
#![expect(clippy::print_stdout, reason = "the launcher lists toolchains")]
#![expect(clippy::print_stderr, reason = "and says why it couldn't run one")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fennec_launcher::{Asked, Helper, Launcher, Pick, RELEASES, TARGET};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match launched(Asked::read(&args)) {
        Ok(code) => code,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

fn launched(asked: Asked) -> Result<ExitCode, String> {
    let offline = asked.offline || std::env::var_os("FENNEC_OFFLINE").is_some();
    let args = asked.command.as_slice();
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
        web: Box::new(Helper::beside_this()?),
    };
    let here = || std::env::current_dir().map_err(|err| format!("couldn't tell which folder this is: {err}"));
    match asked.pick {
        Some(Pick::JustThisOnce(version)) => return hand_off(&launcher.exactly(&version)?, args, offline),
        Some(Pick::OutsideAKit(version)) => {
            return hand_off(&launcher.outside_a_kit(&here()?, &version)?, args, offline);
        }
        None => {}
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["toolchain", "list"] => {
            // Which runs for scripts, set or the newest installed, and which this kit asks for.
            let (default, scripts, this_kit) =
                (launcher.default_version(), launcher.for_scripts(), launcher.kit_version(&here()?));
            let mark = |name: &str| {
                let mut marks = Vec::new();
                if scripts.as_deref() == Some(name) {
                    marks.push(if default.is_some() { "default" } else { "for scripts" });
                }
                if this_kit.as_deref() == Some(name) {
                    marks.push("this kit");
                }
                if marks.is_empty() { String::new() } else { format!(" ({})", marks.join(", ")) }
            };
            for version in launcher.installed() {
                println!("{version}{}", mark(&version));
            }
            for (name, folder) in launcher.links() {
                println!("{name} → {}{}", folder.display(), mark(&name));
            }
            Ok(ExitCode::SUCCESS)
        }
        ["toolchain", "link", name, path] => launcher.link(name, Path::new(path)).map(|()| ExitCode::SUCCESS),
        ["toolchain", "install", "--latest"] => {
            let version = launcher.latest()?;
            launcher.ensure(&version, None)?;
            println!("{version}");
            Ok(ExitCode::SUCCESS)
        }
        ["toolchain", "install", version] => launcher.ensure(version, None).map(|_| ExitCode::SUCCESS),
        ["toolchain", "remove", version] => launcher.remove(version).map(|()| ExitCode::SUCCESS),
        ["default", version] => launcher.set_default(version).map(|()| ExitCode::SUCCESS),
        ["default"] => {
            println!("{}", launcher.default_version().ok_or(fennec_launcher::NO_DEFAULT)?);
            Ok(ExitCode::SUCCESS)
        }
        // A new kit is made outside any kit, so by the version for scripts, which the new
        // `kit.toml` then asks for.
        ["new", ..] => {
            let version = launcher.for_scripts().ok_or(fennec_launcher::NO_TOOLCHAIN)?;
            hand_off(&launcher.exactly(&version)?, args, offline)
        }
        ["--latest"] => {
            println!("{}", launcher.latest()?);
            Ok(ExitCode::SUCCESS)
        }
        ["update"] | ["update", "--latest"] => {
            let here = here()?;
            print!("{}", launcher.update(&here, words.len() == 2)?);
            hand_off(&launcher.toolchain_for(&here)?, &["update".to_owned()], offline)
        }
        ["toolchain", ..] => {
            Err("`fennec toolchain` takes `list`, `install <version>`, `install --latest`, `remove <version>` or `link <name> <path>`"
                .to_owned())
        }
        _ => hand_off(&launcher.toolchain_for(&here()?)?, args, offline),
    }
}

/// Runs the toolchain's `fennec` in place of this process, with the same arguments, called `fennec`
/// as the launcher was: signals, the exit code and every stream go straight to it.
#[cfg(unix)]
fn hand_off(command: &Path, args: &[String], offline: bool) -> Result<ExitCode, String> {
    use std::os::unix::process::CommandExt as _;
    let err = toolchain(command, args, offline).arg0("fennec").exec();
    Err(format!("couldn't run {}: {err}", command.display()))
}

/// Runs the toolchain's `fennec`, with the same arguments, and exits as it does. Ctrl-C reaches
/// every program in the console, so the launcher lets it go by and waits: the toolchain decides
/// what it means, and the launcher exits as it does, rather than leaving it running behind a prompt.
#[cfg(not(unix))]
fn hand_off(command: &Path, args: &[String], offline: bool) -> Result<ExitCode, String> {
    ctrlc::set_handler(|| {})
        .map_err(|err| format!("couldn't let Ctrl-C through to the toolchain: {err}"))?;
    let status = toolchain(command, args, offline)
        .status()
        .map_err(|err| format!("couldn't run {}: {err}", command.display()))?;
    Ok(status.code().and_then(|code| u8::try_from(code).ok()).map_or(ExitCode::FAILURE, ExitCode::from))
}

/// The toolchain's `fennec` with the arguments, told to fetch nothing when the launcher was.
fn toolchain(command: &Path, args: &[String], offline: bool) -> std::process::Command {
    let mut toolchain = std::process::Command::new(command);
    toolchain.args(args);
    if offline {
        toolchain.env("FENNEC_OFFLINE", "1");
    }
    toolchain
}
