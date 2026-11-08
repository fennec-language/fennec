//! The lines of `kit.lock` that pin dependencies: `kit <name> <hash> <url> <commit> [folder]`, one
//! for each kit from git, beside the `toolchain` line the launcher writes. The hash is of the kit's
//! files, so a commit can't come back with other content than it had when it was pinned.

use fennec_blake3::Hash;

/// What `kit.lock` pins one dependency from git to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub hash: Hash,
    pub url: String,
    pub commit: String,
    pub folder: Option<String>,
}

impl Pin {
    fn line(&self) -> String {
        let folder = self.folder.as_deref().map(|folder| format!(" {folder}")).unwrap_or_default();
        format!("kit {} {} {} {}{folder}\n", self.name, self.hash.to_hex(), self.url, self.commit)
    }

    /// Whether this pins the kit at `url`, `commit` and `folder`.
    pub(crate) fn is(&self, url: &str, commit: &str, folder: Option<&str>) -> bool {
        self.url == url && self.commit == commit && self.folder.as_deref() == folder
    }

    /// Where the kit is, as messages show it: its address, its folder, and its commit, shortened.
    pub(crate) fn shown(&self) -> String {
        shown(&self.url, &self.commit, self.folder.as_deref())
    }
}

/// A kit's place, as messages show it.
pub(crate) fn shown(url: &str, commit: &str, folder: Option<&str>) -> String {
    let folder = folder.map(|folder| format!(" {folder}")).unwrap_or_default();
    format!("{url}{folder} at {}", commit.get(..12).unwrap_or(commit))
}

/// Every dependency `kit.lock` pins.
///
/// # Errors
///
/// When a `kit` line can't be read.
pub fn pins(lock: &str) -> Result<Vec<Pin>, String> {
    let mut pins = Vec::new();
    for line in lock.lines() {
        let mut words = line.split(' ');
        if words.next() != Some("kit") {
            continue;
        }
        let words: Vec<&str> = words.collect();
        let bad = || format!("kit.lock's `{line}` isn't `kit <name> <hash> <address> <commit> [folder]`");
        let (name, hash, url, commit, folder) = match words.as_slice() {
            [name, hash, url, commit] => (name, hash, url, commit, None),
            [name, hash, url, commit, folder] => (name, hash, url, commit, Some((*folder).to_owned())),
            _ => return Err(bad()),
        };
        let hash = Hash::from_hex(hash)
            .filter(|_| hash.bytes().all(|byte| !byte.is_ascii_uppercase()))
            .ok_or_else(bad)?;
        pins.push(Pin {
            name: (*name).to_owned(),
            hash,
            url: (*url).to_owned(),
            commit: (*commit).to_owned(),
            folder,
        });
    }
    Ok(pins)
}

/// `kit.lock` pinning exactly `pins`, in the order given, with every line that isn't a `kit` line as
/// it was.
pub fn with_pins(lock: &str, pins: &[Pin]) -> String {
    let kept: String = lock
        .split_inclusive('\n')
        .filter(|line| !line.starts_with("kit "))
        .map(|line| if line.ends_with('\n') { line.to_owned() } else { format!("{line}\n") })
        .collect();
    let pinned: String = pins.iter().map(Pin::line).collect();
    if kept.is_empty() && !pinned.is_empty() {
        return format!("# What this kit builds with, by hash. Only `fennec update` changes it.\n{pinned}");
    }
    format!("{kept}{pinned}")
}
