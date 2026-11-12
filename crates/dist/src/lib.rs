//! What a Fennec release is, and who vouches for it.
//!
//! A release is a manifest: one line naming what it is and its version, then a line for each
//! platform's archive, with its file's name, size and BLAKE3 hash.
//!
//! ```text
//! fennec toolchain 0.4.2
//! aarch64-apple-darwin fennec-0.4.2-aarch64-apple-darwin.tar.gz 51234567 1f0c…
//! x86_64-unknown-linux-gnu fennec-0.4.2-x86_64-unknown-linux-gnu.tar.gz 50999104 9ab4…
//! ```
//!
//! CI writes the manifest from the archives it built. It's signed offline, with `ssh-keygen -Y
//! sign` and an Ed25519 key on a hardware key, so a signature can't be made without someone
//! touching it. The launcher takes a manifest only with enough good signatures from the keys
//! built into it, and an archive only when its size and hash are the manifest's. So nothing on the
//! way, a mirror, a CDN or the release page, can change what's installed.
//!
//! A toolchain needs one signature, and a new launcher two, from different keys, so one stolen
//! key can't change the launcher, which is what checks everything else. Each kind is signed in
//! its own namespace, so a signature for one never counts for the other.

mod sshsig;
mod trust;

pub use trust::{kit_root, trusted, write_kit_file};

use std::fmt::Write as _;

pub use sshsig::PublicKey;

/// The keys that sign releases, as OpenSSH public key lines. There are none yet: until there are,
/// nothing verifies, so nothing can be installed.
const RELEASE_KEYS: &[&str] = &[];

/// What a release is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A version of Fennec: the compiler and everything that comes with it.
    Toolchain,
    /// The launcher, which runs the toolchain each kit asks for.
    Launcher,
}

impl Kind {
    /// The kind a word names, as a manifest's first line does.
    #[must_use]
    pub fn named(word: &str) -> Option<Kind> {
        [Kind::Toolchain, Kind::Launcher].into_iter().find(|kind| kind.word() == word)
    }

    /// How a manifest's first line names it.
    fn word(self) -> &'static str {
        match self {
            Kind::Toolchain => "toolchain",
            Kind::Launcher => "launcher",
        }
    }

    /// The namespace its signatures are made in, which `ssh-keygen -Y sign -n` takes.
    #[must_use]
    pub fn namespace(self) -> &'static str {
        match self {
            Kind::Toolchain => "fennec-toolchain",
            Kind::Launcher => "fennec-launcher",
        }
    }

    /// How many of the keys that sign releases have to sign one of this kind.
    #[must_use]
    pub fn needs(self) -> usize {
        match self {
            Kind::Toolchain => 1,
            Kind::Launcher => 2,
        }
    }
}

/// The keys built in that sign releases.
///
/// # Errors
///
/// When one of them isn't an Ed25519 key, which is this build's mistake.
pub fn release_keys() -> Result<Vec<PublicKey>, String> {
    RELEASE_KEYS.iter().map(|line| PublicKey::parse(line)).collect()
}

/// A release: what it is, its version, and each platform's archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub kind: Kind,
    pub version: String,
    pub archives: Vec<Archive>,
}

/// One platform's archive in a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    /// The platform, named as the build names its target, like `aarch64-apple-darwin`.
    pub target: String,
    /// Its file's name, with nothing that could reach another folder.
    pub file: String,
    pub size: u64,
    pub hash: fennec_blake3::Hash,
}

impl Archive {
    /// Whether `bytes` are this archive.
    ///
    /// # Errors
    ///
    /// When their size or hash isn't the manifest's.
    pub fn check(&self, bytes: &[u8]) -> Result<(), String> {
        if u64::try_from(bytes.len()).ok() != Some(self.size) {
            return Err(format!(
                "{} is {} bytes, and the release says {}",
                self.file,
                bytes.len(),
                self.size
            ));
        }
        let hash = fennec_blake3::hash(bytes);
        if hash != self.hash {
            return Err(format!(
                "{} has the hash {}, and the release says {}",
                self.file,
                hash.to_hex(),
                self.hash.to_hex()
            ));
        }
        Ok(())
    }
}

impl Manifest {
    /// Reads a manifest. It has to be exactly as `write` writes one.
    ///
    /// # Errors
    ///
    /// When it isn't, saying where.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut lines = text.split_terminator('\n');
        let first = lines.next().ok_or("the manifest is empty")?;
        let (kind, version) = match first.split(' ').collect::<Vec<_>>().as_slice() {
            ["fennec", "toolchain", version] => (Kind::Toolchain, *version),
            ["fennec", "launcher", version] => (Kind::Launcher, *version),
            _ => {
                return Err(format!(
                    "the manifest starts `{first}`, not `fennec toolchain` or `fennec launcher`"
                ));
            }
        };
        if !plain(version, "+") || !version.starts_with(|first: char| first.is_ascii_digit()) {
            return Err(format!("`{version}` isn't a version"));
        }
        let mut archives: Vec<Archive> = Vec::new();
        for line in lines {
            let [target, file, size, hash] = line.split(' ').collect::<Vec<_>>()[..] else {
                return Err(format!("`{line}` isn't a platform, a file, its size and its hash"));
            };
            let size = size.parse().ok().filter(|_| size.bytes().all(|byte| byte.is_ascii_digit()));
            let hash = fennec_blake3::Hash::from_hex(hash).filter(|_| hash.len() == 64);
            let (Some(size), Some(hash)) = (size, hash) else {
                return Err(format!("`{line}` has no size or hash that can be read"));
            };
            if !plain(target, "_") || !plain(file, "_+") || file.starts_with('.') {
                return Err(format!("`{line}` names a platform or a file that isn't plain"));
            }
            if archives.iter().any(|archive| archive.target == target || archive.file == file) {
                return Err(format!("`{line}` names a platform or a file twice"));
            }
            archives.push(Archive { target: target.to_owned(), file: file.to_owned(), size, hash });
        }
        if archives.is_empty() {
            return Err("the manifest has no archives".to_owned());
        }
        let manifest = Manifest { kind, version: version.to_owned(), archives };
        if manifest.write() != text {
            return Err("the manifest isn't written the one way a manifest is".to_owned());
        }
        Ok(manifest)
    }

    /// The manifest's text, which is what's signed.
    #[must_use]
    pub fn write(&self) -> String {
        let mut text = format!("fennec {} {}\n", self.kind.word(), self.version);
        for Archive { target, file, size, hash } in &self.archives {
            // Writing to a `String` can't fail.
            let _ = writeln!(text, "{target} {file} {size} {}", hash.to_hex());
        }
        text
    }

    /// The manifest of a release made of `archives`, each a file's name and its bytes. Each file is
    /// named for its release and platform, as CI names them: `fennec-v0.4.2-aarch64-apple-darwin.tar.gz`
    /// for a toolchain, and `fennec-launcher-v1.0.0-...` for the launcher, with or without the `v`.
    ///
    /// # Errors
    ///
    /// When a file isn't named that way, or the manifest wouldn't read back as written.
    pub fn of_archives(
        kind: Kind,
        version: &str,
        archives: &[(String, Vec<u8>)],
    ) -> Result<Manifest, String> {
        let prefix = match kind {
            Kind::Toolchain => "fennec-",
            Kind::Launcher => "fennec-launcher-",
        };
        let mut made = Vec::new();
        for (file, bytes) in archives {
            let target = [format!("{prefix}v{version}-"), format!("{prefix}{version}-")]
                .iter()
                .find_map(|start| file.strip_prefix(start.as_str()))
                .and_then(|rest| rest.strip_suffix(".tar.gz").or_else(|| rest.strip_suffix(".zip")))
                .ok_or_else(|| {
                    format!("{file} isn't named `{prefix}v{version}-<platform>.tar.gz` or `.zip`")
                })?;
            let size = u64::try_from(bytes.len()).map_err(|_| format!("{file} is too big"))?;
            made.push(Archive {
                target: target.to_owned(),
                file: file.clone(),
                size,
                hash: fennec_blake3::hash(bytes),
            });
        }
        let manifest = Manifest { kind, version: version.to_owned(), archives: made };
        // What's written has to read back, so a name CI got wrong fails here, not on someone's machine.
        Manifest::parse(&manifest.write())?;
        Ok(manifest)
    }

    /// The archive for a platform, if the release has one.
    #[must_use]
    pub fn archive(&self, target: &str) -> Option<&Archive> {
        self.archives.iter().find(|archive| archive.target == target)
    }
}

/// Whether a word is ASCII letters, digits, dots, dashes and the characters `also` gives.
fn plain(word: &str, also: &str) -> bool {
    !word.is_empty()
        && word.len() <= 128
        && word.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || also.contains(ch))
}

/// The manifest `text`, if it's a release of `kind` that enough of the release keys built in have
/// signed, in `signatures`: what `ssh-keygen -Y sign` wrote for each key, one after another.
///
/// # Errors
///
/// When it isn't signed by enough of them, or isn't a manifest of that kind.
pub fn verified(text: &[u8], signatures: &str, kind: Kind) -> Result<Manifest, String> {
    verified_by(text, signatures, kind, &release_keys()?)
}

/// Like `verified`, with the keys to trust given.
///
/// # Errors
///
/// When it isn't signed by enough of `trusted`, or isn't a manifest of `kind`.
pub fn verified_by(
    text: &[u8],
    signatures: &str,
    kind: Kind,
    trusted: &[PublicKey],
) -> Result<Manifest, String> {
    let signers = sshsig::signers(signatures, kind.namespace(), text)?;
    // Each key counts once, however many times it signed or is listed.
    let mut vouched: Vec<&PublicKey> = Vec::new();
    for signer in signers.iter().filter(|signer| trusted.contains(signer)) {
        if !vouched.contains(&signer) {
            vouched.push(signer);
        }
    }
    let vouching = vouched.len();
    if vouching < kind.needs() {
        let keys = if trusted.len() == 1 { "key" } else { "keys" };
        return Err(format!(
            "the {} release is signed by {vouching} of the {} {keys} this build of Fennec trusts, and it needs {}",
            kind.word(),
            trusted.len(),
            kind.needs()
        ));
    }
    let text = std::str::from_utf8(text).map_err(|_| "the manifest isn't text")?;
    let manifest = Manifest::parse(text)?;
    if manifest.kind != kind {
        return Err(format!("the manifest is for the {}, not the {}", manifest.kind.word(), kind.word()));
    }
    Ok(manifest)
}
