//! `fennec-dist`, for making releases rather than using them:
//!
//! ```text
//! fennec-dist manifest <toolchain|launcher> <version> <archive>...
//! fennec-dist verify <toolchain|launcher> <manifest> <signatures> [<public keys>]
//! ```
//!
//! `manifest` writes a release's manifest, from the archives CI built, to standard output. That's
//! what gets signed, offline: `ssh-keygen -Y sign -f <key> -n fennec-toolchain <manifest>`.
//! `verify` says whether a manifest has the signatures it needs, from the keys built in, or from a
//! file of OpenSSH public keys, one a line, to try keys that aren't built in yet.

#![expect(clippy::print_stdout, reason = "the command prints a manifest or a verdict")]
#![expect(clippy::print_stderr, reason = "and says why it couldn't")]

use std::process::ExitCode;

use fennec_dist::{Kind, Manifest, PublicKey};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["manifest", kind, version, archives @ ..] if !archives.is_empty() => {
            manifest(kind, version, archives)
        }
        ["verify", kind, manifest, signatures] => verify(kind, manifest, signatures, None),
        ["verify", kind, manifest, signatures, keys] => verify(kind, manifest, signatures, Some(keys)),
        _ => Err("usage: fennec-dist manifest <toolchain|launcher> <version> <archive>...\n       \
                  fennec-dist verify <toolchain|launcher> <manifest> <signatures> [<public keys>]"
            .to_owned()),
    };
    match done {
        Ok(said) => {
            print!("{said}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

fn kind(word: &str) -> Result<Kind, String> {
    Kind::named(word).ok_or_else(|| format!("`{word}` isn't `toolchain` or `launcher`"))
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|err| format!("couldn't read {path}: {err}"))
}

fn manifest(kind_word: &str, version: &str, archives: &[&str]) -> Result<String, String> {
    let mut read_in = Vec::new();
    for path in archives {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{path} has no file name"))?;
        read_in.push((name, read(path)?));
    }
    Ok(Manifest::of_archives(kind(kind_word)?, version, &read_in)?.write())
}

fn verify(kind_word: &str, manifest: &str, signatures: &str, keys: Option<&str>) -> Result<String, String> {
    let kind = kind(kind_word)?;
    let text = read(manifest)?;
    let signatures =
        String::from_utf8(read(signatures)?).map_err(|_| "the signatures aren't text".to_owned())?;
    let trusted = match keys {
        Some(keys) => {
            let keys = String::from_utf8(read(keys)?).map_err(|_| "the keys aren't text".to_owned())?;
            keys.lines()
                .filter(|line| !line.trim().is_empty())
                .map(PublicKey::parse)
                .collect::<Result<Vec<_>, _>>()?
        }
        None => fennec_dist::release_keys()?,
    };
    let release = fennec_dist::verified_by(&text, &signatures, kind, &trusted)?;
    Ok(format!("fennec {} {}: signed as it needs to be\n", kind_word, release.version))
}
