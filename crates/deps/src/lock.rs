//! The lines of `kit.lock` that pin dependencies: `kit <name> <hash> <url> <commit> [folder]`, one
//! for each kit from git, beside the `toolchain` line the launcher writes. The hash is of the kit's
//! files, so a commit can't come back with other content than it had when it was pinned.
//!
//! And the lines that say what each kit holds: `holds <name> <capability>...`, one for each kit the
//! program hands a capability to, its own dependencies' included. The compiler works that out, and
//! a check stops where a kit would hold more than its line says, until `fennec update` accepts it.

use std::fmt::Write as _;

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

/// What `kit.lock` says one kit holds, and the source that was accepted for: a line belongs to
/// where the kit comes from, not to its name, so a kit from somewhere else holds nothing until
/// its grants are accepted again.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Holding {
    pub kit: String,
    /// Where the kit comes from, as `source_of` writes it: `path:../shapes`, `git:<address>`.
    pub source: String,
    pub holds: Vec<String>,
}

/// What each kit `kit.lock` says it holds, from its `holds <kit> <source> <capability>...` lines:
/// a kit with none holds nothing.
///
/// # Errors
///
/// When a `holds` line names no kit or no source, or something that isn't a capability.
pub fn holds(lock: &str) -> Result<Vec<Holding>, String> {
    let mut holds: Vec<Holding> = Vec::new();
    for line in lock.lines() {
        let mut words = line.split(' ');
        if words.next() != Some("holds") {
            continue;
        }
        let bad = || format!("kit.lock's `{line}` isn't `holds <kit> <source> <capability>...`");
        let kit = words.next().filter(|kit| crate::valid_name(kit)).ok_or_else(bad)?;
        let source = words
            .next()
            .filter(|source| source.starts_with("path:") || source.starts_with("git:"))
            .ok_or_else(bad)?;
        let held: Vec<String> = words.map(str::to_owned).collect();
        if held.is_empty()
            || !held.iter().all(|capability| fennec_syntax::HOLDABLE.contains(&capability.as_str()))
        {
            return Err(bad());
        }
        match holds.iter_mut().find(|had| had.kit == kit) {
            Some(had) if had.source == source => had.holds.extend(held),
            Some(_) => return Err(format!("kit.lock says `{kit}` comes from two places")),
            None => holds.push(Holding { kit: kit.to_owned(), source: source.to_owned(), holds: held }),
        }
    }
    Ok(holds)
}

/// `kit.lock` saying each kit holds exactly what `holds` gives it, in order of kit, with every line
/// that isn't a `holds` line as it was. A kit holding nothing has no line.
pub fn with_holds(lock: &str, holds: &[Holding]) -> String {
    let mut kept: String = lock
        .split_inclusive('\n')
        .filter(|line| !line.starts_with("holds "))
        .map(|line| if line.ends_with('\n') { line.to_owned() } else { format!("{line}\n") })
        .collect();
    let mut sorted: Vec<&Holding> = holds.iter().filter(|holding| !holding.holds.is_empty()).collect();
    sorted.sort();
    if kept.is_empty() && !sorted.is_empty() {
        kept.push_str("# What this kit builds with, by hash, and what each kit it depends on holds. Only `fennec update` changes it.\n");
    }
    for holding in sorted {
        let mut held = holding.holds.clone();
        held.sort();
        held.dedup();
        let _ = writeln!(kept, "holds {} {} {}", holding.kit, holding.source, held.join(" "));
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::{Holding, holds, with_holds};

    fn holding(kit: &str, source: &str, held: &[&str]) -> Holding {
        Holding {
            kit: kit.to_owned(),
            source: source.to_owned(),
            holds: held.iter().map(|one| (*one).to_owned()).collect(),
        }
    }

    /// `holds` lines say what each kit holds and where it comes from, and writing them back keeps
    /// every other line, puts them in order, and leaves out a kit that holds nothing.
    #[test]
    fn holds_lines_read_and_write_back_in_order() {
        let lock = "toolchain 0.4.2\nholds net_kit git:https://example.com/net Net\nholds saves path:../saves Fs Clock\n";
        assert_eq!(
            holds(lock).unwrap(),
            [
                holding("net_kit", "git:https://example.com/net", &["Net"]),
                holding("saves", "path:../saves", &["Fs", "Clock"])
            ]
        );
        let written = with_holds(
            lock,
            &[
                holding("saves", "path:../saves", &["Fs", "Clock"]),
                holding("empty", "path:../empty", &[]),
                holding("http", "git:https://example.com/http#kits/http", &["Net"]),
            ],
        );
        assert_eq!(
            written,
            "toolchain 0.4.2\nholds http git:https://example.com/http#kits/http Net\nholds saves path:../saves Clock Fs\n"
        );
        for bad in [
            "holds",
            "holds http",
            "holds http Net",
            "holds http path:../http",
            "holds http path:../http Network",
            "holds Http path:../http Net",
            "holds http path:../http Secret",
            "holds http path:../a Net\nholds http path:../b Fs",
        ] {
            assert!(holds(bad).is_err(), "{bad}");
        }
    }
}
