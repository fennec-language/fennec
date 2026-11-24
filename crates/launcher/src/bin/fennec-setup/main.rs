//! `fennec-setup`, beside the launcher: everything the launcher does but hand a command to a
//! toolchain that's installed already, which `fennec` does itself, and hands the rest here with its
//! arguments as they were. Kept apart, what `fennec` imports is little more than Rust's own start
//! does, and each import costs every start: on macOS, binding one costs about 60 thousand
//! instructions. So this program does, for `fennec`:
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
//! fennec self update                    replaces this launcher with the newest launcher release,
//!                                       which two of the release keys have to have signed
//! ```
//!
//! It reaches the network only through `fennec-fetch`, beside it, when something has to be fetched
//! (see `fennec_launcher::Helper`).
//!
//! `--offline`, or `FENNEC_OFFLINE`, never fetches, and the toolchain is told so too. `FENNEC_DIST` is where releases come from, a
//! mirror's address or a folder, `FENNEC_LAUNCHER_DIST` where launcher releases come from, and
//! `FENNEC_HOME` where they go.
#![expect(clippy::print_stdout, reason = "the launcher lists toolchains")]
#![expect(clippy::print_stderr, reason = "and says why it couldn't run one")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fennec_launcher::{Asked, Helper, LAUNCHER_RELEASES, Launcher, Pick, RELEASES, TARGET, hand_off};

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
            list(&launcher, &here()?);
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
        ["self", "update"] => {
            let launchers =
                std::env::var("FENNEC_LAUNCHER_DIST").unwrap_or_else(|_| LAUNCHER_RELEASES.to_owned());
            let running =
                std::env::current_exe().map_err(|err| format!("couldn't find the launcher: {err}"))?;
            print!("{}", launcher.self_update(&launchers, &running, env!("CARGO_PKG_VERSION"))?);
            Ok(ExitCode::SUCCESS)
        }
        ["self", ..] => Err("`fennec self` takes `update`".to_owned()),
        ["toolchain", ..] => {
            Err("`fennec toolchain` takes `list`, `install <version>`, `install --latest`, `remove <version>` or `link <name> <path>`"
                .to_owned())
        }
        _ => hand_off(&launcher.toolchain_for(&here()?)?, args, offline),
    }
}

/// Says the toolchains installed, and links, marking the one for scripts, set or the newest
/// installed, and the one the kit around `here` asks for.
fn list(launcher: &Launcher, here: &Path) {
    let (default, scripts, this_kit) =
        (launcher.default_version(), launcher.for_scripts(), launcher.kit_version(here));
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
}
