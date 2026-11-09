//! A release's dry run: installs a toolchain archive into a Fennec home through the launcher's own
//! install, as a person's launcher would install a signed release, and prints where its `fennec`
//! is. Nobody has signed it, so it's signed here with a key made for this run and thrown away,
//! which only this run trusts: the launcher people install trusts the release keys alone.
//!
//! ```text
//! cargo run -p fennec-launcher --example dry_run -- <archive> <dist folder> <Fennec home>
//! ```
//!
//! The archive can have any name; it's put in the dist folder as this build's version's release
//! for this platform. `.github/scripts/release-dry-run.sh` runs this, then builds and runs every
//! example from what it installed.

#![expect(clippy::print_stdout, reason = "it says where the installed `fennec` is")]
#![expect(clippy::print_stderr, reason = "and why it couldn't install it")]

use std::path::PathBuf;
use std::process::ExitCode;

#[path = "../tests/common/signing.rs"]
mod signing;

fn main() -> ExitCode {
    match installed() {
        Ok(fennec) => {
            println!("{}", fennec.display());
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

fn installed() -> Result<PathBuf, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [archive, dist, home] = args.as_slice() else {
        return Err("usage: dry_run <archive> <dist folder> <Fennec home>".to_owned());
    };
    let version = env!("CARGO_PKG_VERSION");
    let target = fennec_launcher::TARGET;
    let zip = std::path::Path::new(archive)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    let extension = if zip { "zip" } else { "tar.gz" };
    let bytes = std::fs::read(archive).map_err(|err| format!("couldn't read {archive}: {err}"))?;
    // Laid out as the release page is: each version's files in a folder named for its tag.
    let folder = PathBuf::from(dist).join(format!("v{version}"));
    std::fs::create_dir_all(&folder).map_err(|err| format!("couldn't make {}: {err}", folder.display()))?;
    let file = format!("fennec-v{version}-{target}.{extension}");
    write(&folder.join(&file), &bytes)?;
    let manifest =
        fennec_dist::Manifest::of_archives(fennec_dist::Kind::Toolchain, version, &[(file, bytes)])?.write();
    let name = format!("fennec-v{version}.manifest");
    write(&folder.join(&name), manifest.as_bytes())?;
    let (pair, public) = signing::made_key()?;
    let signature = signing::sign(&pair, "fennec-toolchain", manifest.as_bytes());
    write(&folder.join(format!("{name}.sig")), signature.as_bytes())?;
    let launcher = fennec_launcher::Launcher {
        home: PathBuf::from(home),
        dist: dist.clone(),
        offline: false,
        trusted: vec![public],
        target: target.to_owned(),
    };
    launcher.exactly(version)
}

fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|err| format!("couldn't write {}: {err}", path.display()))
}
