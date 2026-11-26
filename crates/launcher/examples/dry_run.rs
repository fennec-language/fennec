//! A release's dry run: installs a launcher archive and a toolchain archive into a Fennec home
//! through the launcher's own code, as a person's launcher would install signed releases, and
//! prints where the toolchain's `fennec` is. Nobody has signed them, so they're signed here with
//! keys made for this run and thrown away, which only this run trusts: the launcher people install
//! trusts the release keys alone.
//!
//! ```text
//! cargo run -p fennec-launcher --example dry_run -- <toolchain archive> <launcher archive> <dist folder> <Fennec home>
//! ```
//!
//! The archives can have any names. Each is put in the dist folder as this build's version's
//! release for this platform, laid out as the bucket is: `toolchain/` and `launcher/`, each with
//! its `latest` and a folder for the version holding the archive, its manifest and the manifest's
//! signatures. The launcher goes in as `fennec self update` puts one in, over an older one in
//! `bin`, and the toolchain as a kit's first run installs it. `.github/scripts/release-dry-run.sh`
//! runs this, then uses what it installed for everything a person would.

#![expect(clippy::print_stdout, reason = "it says where the installed `fennec` is")]
#![expect(clippy::print_stderr, reason = "and what it installed, or why it couldn't")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fennec_dist::Kind;
use ring::signature::Ed25519KeyPair;

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
    let [toolchain, launcher, dist, home] = args.as_slice() else {
        return Err(
            "usage: dry_run <toolchain archive> <launcher archive> <dist folder> <Fennec home>".to_owned()
        );
    };
    let (dist, home) = (Path::new(dist), PathBuf::from(home));
    let toolchains = dist.join("toolchain");
    let launchers = dist.join("launcher");
    // A toolchain's manifest needs one signature, a launcher's two.
    let (first, one) = signing::made_key()?;
    let (second, two) = signing::made_key()?;
    released(Kind::Toolchain, Path::new(toolchain), &toolchains, &[&first])?;
    released(Kind::Launcher, Path::new(launcher), &launchers, &[&first, &second])?;
    let installer = fennec_launcher::Launcher {
        home: home.clone(),
        dist: toolchains.to_string_lossy().into_owned(),
        offline: false,
        trusted: vec![one, two],
        target: fennec_launcher::TARGET.to_owned(),
        // Its releases are in a folder, so nothing is fetched over the network.
        web: Box::new(fennec_launcher::Helper::beside_this()?),
    };
    // An older launcher in `bin`, as an installer leaves one, for `self update` to replace.
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).map_err(|err| format!("couldn't make {}: {err}", bin.display()))?;
    for program in fennec_launcher::PROGRAMS {
        write(&bin.join(format!("{program}{}", std::env::consts::EXE_SUFFIX)), b"older")?;
    }
    let running = bin.join(format!("fennec-setup{}", std::env::consts::EXE_SUFFIX));
    eprint!("{}", installer.self_update(&launchers.to_string_lossy(), &running, "0.0.0")?);
    installer.exactly(env!("CARGO_PKG_VERSION"))
}

/// Puts `archive` in `mirror` as this version's release for this platform, signed by each of
/// `signers`, one signature after another, and names it the newest.
fn released(kind: Kind, archive: &Path, mirror: &Path, signers: &[&Ed25519KeyPair]) -> Result<(), String> {
    let version = env!("CARGO_PKG_VERSION");
    let (name, namespace) = match kind {
        Kind::Toolchain => ("fennec", "fennec-toolchain"),
        Kind::Launcher => ("fennec-launcher", "fennec-launcher"),
    };
    let zip = archive.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    let extension = if zip { "zip" } else { "tar.gz" };
    let bytes =
        std::fs::read(archive).map_err(|err| format!("couldn't read {}: {err}", archive.display()))?;
    let folder = mirror.join(format!("v{version}"));
    std::fs::create_dir_all(&folder).map_err(|err| format!("couldn't make {}: {err}", folder.display()))?;
    let file = format!("{name}-v{version}-{}.{extension}", fennec_launcher::TARGET);
    write(&folder.join(&file), &bytes)?;
    let manifest = fennec_dist::Manifest::of_archives(kind, version, &[(file, bytes)])?.write();
    let manifest_name = format!("{name}-v{version}.manifest");
    write(&folder.join(&manifest_name), manifest.as_bytes())?;
    let signatures: String =
        signers.iter().map(|pair| signing::sign(pair, namespace, manifest.as_bytes())).collect();
    write(&folder.join(format!("{manifest_name}.sig")), signatures.as_bytes())?;
    write(&mirror.join("latest"), format!("{version}\n").as_bytes())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|err| format!("couldn't write {}: {err}", path.display()))
}
