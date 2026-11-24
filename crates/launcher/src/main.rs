//! The launcher, installed as `fennec`. It hands a command to the toolchain it's for, when that's
//! installed already: the one the kit around this folder pins, the one `+0.4.2` or `--version
//! 0.4.2` names, or outside a kit the one for scripts. Everything else, its own commands, a
//! toolchain to install, a kit to pin the first time, and every error, it hands to `fennec-setup`,
//! beside it, with the arguments as they were, which does the same work in full and says what's
//! wrong (see `src/bin/fennec-setup/main.rs` for the commands). So it never fetches or writes.
//!
//! It's apart so that it imports little: on macOS binding each function a program imports costs
//! about 60 thousand instructions at every start, whether it's called or not, and a program's start
//! is most of what a handover costs. Here it reads the kit's files, hashes a manifest and execs.
#![expect(clippy::print_stderr, reason = "it says why it couldn't hand a command on")]

use std::path::PathBuf;
use std::process::ExitCode;

use fennec_launcher::{Asked, Launcher, TARGET, Web, beside_this, hand_off};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let asked = Asked::read(&args);
    let offline = asked.offline || std::env::var_os("FENNEC_OFFLINE").is_some();
    let handed = match ready(&asked) {
        Some(toolchain) => hand_off(&toolchain, &asked.command, offline),
        None => beside_this("fennec-setup").and_then(|setup| hand_off(&setup, &args, false)),
    };
    handed.unwrap_or_else(|why| {
        eprintln!("error: {why}");
        ExitCode::FAILURE
    })
}

/// The launcher's own commands, which `fennec-setup` does.
const ITS_OWN: [&str; 6] = ["toolchain", "default", "new", "update", "self", "--latest"];

/// The toolchain's `fennec` to hand the command to, when it's there to run now: found as
/// `fennec-setup` would, with every check, but never fetched, installed or pinned.
fn ready(asked: &Asked) -> Option<PathBuf> {
    // `new` goes there with a version named too, which tells a linked build the name it's run as.
    if asked
        .command
        .first()
        .is_some_and(|first| ITS_OWN.contains(&first.as_str()) && (asked.pick.is_none() || first == "new"))
    {
        return None;
    }
    let home = match std::env::var_os("FENNEC_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?)
            .join(".fennec"),
    };
    let launcher = Launcher {
        home,
        dist: String::new(),
        offline: true,
        trusted: Vec::new(),
        target: TARGET.to_owned(),
        web: Box::new(Nowhere),
    };
    launcher.ready(&std::env::current_dir().ok()?, asked.pick.as_ref())
}

/// No network: `fennec` never fetches.
struct Nowhere;

impl Web for Nowhere {
    fn fetch(&self, url: &str, _most: u64) -> Result<Vec<u8>, String> {
        Err(format!("`fennec` doesn't fetch {url}"))
    }

    fn redirect(&self, url: &str) -> Result<String, String> {
        Err(format!("`fennec` doesn't fetch {url}"))
    }
}
