//! Links the objects a Fennec build compiled into images for memory, without writing an
//! executable: macOS holds a new executable's first launch for seconds while it checks it, and
//! code mapped into memory isn't a file. This crate does everything that can be done safely:
//! it checks each object is the one the build made, reads it with every offset and size checked,
//! lays out its sections, resolves its symbols and applies its relocations into plain bytes. It
//! never maps or runs anything.
//!
//! Anything it doesn't know is refused rather than guessed at, with the reason, so a run can fall
//! back to a linked executable: an object it can't check, a load command, section, symbol or
//! relocation it doesn't take, a reference outside its section, a target it can't reach. Only
//! arm64 Mach-O objects are taken, as LLVM writes them for macOS.

mod apply;
mod link;
mod macho;

pub use link::{Image, Linked, PlacedLink, Placement, Plan, SectionInfo, Unwind};

/// Why objects weren't linked, said in one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Refuses with a reason.
pub(crate) fn refuse<T>(why: impl Into<String>) -> Result<T, Refused> {
    Err(Refused(why.into()))
}

/// An object's bytes, read once and checked against the hash the build gave for it. Everything
/// after works from these bytes, never from the file again, so a file changed after the check
/// can't be what's linked.
#[derive(Clone, Debug)]
pub struct Checked {
    bytes: Vec<u8>,
}

impl Checked {
    /// The bytes, if their hash is `expected`.
    pub fn new(bytes: Vec<u8>, expected: &fennec_blake3::Hash) -> Result<Checked, Refused> {
        if fennec_blake3::hash(&bytes) != *expected {
            return refuse("an object isn't the one the build made: its hash doesn't match");
        }
        Ok(Checked { bytes })
    }

    /// The file at `path`, read once, if its hash is `expected`.
    pub fn read(path: &std::path::Path, expected: &fennec_blake3::Hash) -> Result<Checked, Refused> {
        let bytes =
            std::fs::read(path).map_err(|err| Refused(format!("couldn't read {}: {err}", path.display())))?;
        Checked::new(bytes, expected)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// What a build hands a run to load: which build of Fennec made it, so objects from another
/// build are never mixed with this one's runtime, and each object with its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub build: String,
    pub objects: Vec<(std::path::PathBuf, fennec_blake3::Hash)>,
}

impl Manifest {
    /// Reads each object once and checks it, if this manifest is from `build`.
    pub fn checked(&self, build: &str) -> Result<Vec<Checked>, Refused> {
        if self.build != build {
            return refuse(format!("the objects are from build {}, not this one, {build}", self.build));
        }
        self.objects.iter().map(|(path, hash)| Checked::read(path, hash)).collect()
    }
}
