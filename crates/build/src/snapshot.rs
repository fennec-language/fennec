//! A record of one run: each file's hash and the hashes of its definitions. The next run
//! compares itself with it, and doesn't parse a file whose bytes hash the same as before.

use std::collections::BTreeMap;

use fennec_blake3::Hash;
use fennec_syntax::Diagnostic;

use crate::tree::definitions;

/// Every file of a program at one run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub files: BTreeMap<String, FileRecord>,
}

/// One file: the hash of its bytes, and each of its definitions' tree hashes by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRecord {
    pub hash: Hash,
    pub definitions: BTreeMap<String, Hash>,
}

/// A run's snapshot, and what it took to make it.
#[derive(Debug, Default)]
pub struct Run {
    pub snapshot: Snapshot,
    /// The files that were new or had changed, so were parsed.
    pub parsed: Vec<String>,
    /// The files whose bytes hadn't changed, so weren't parsed again.
    pub reused: Vec<String>,
    /// The files that don't parse. Each keeps its last good record in the snapshot, whose
    /// hash no longer matches, so it's parsed again next time and compared with that.
    pub errors: Vec<(String, Diagnostic)>,
}

impl Snapshot {
    /// Takes a snapshot of `files`, each a path and its text. A file whose bytes hash the same
    /// as in `previous` keeps its definitions from there instead of being parsed.
    pub fn take(files: &[(String, String)], previous: &Snapshot) -> Run {
        let mut run = Run::default();
        for (path, text) in files {
            let hash = fennec_blake3::hash(text.as_bytes());
            if let Some(record) = previous.files.get(path).filter(|record| record.hash == hash) {
                run.snapshot.files.insert(path.clone(), record.clone());
                run.reused.push(path.clone());
                continue;
            }
            match fennec_syntax::parse(text) {
                Ok(module) => {
                    let definitions = definitions(&module).into_iter().collect();
                    run.snapshot.files.insert(path.clone(), FileRecord { hash, definitions });
                    run.parsed.push(path.clone());
                }
                Err(diagnostic) => {
                    if let Some(record) = previous.files.get(path) {
                        run.snapshot.files.insert(path.clone(), record.clone());
                    }
                    run.errors.push((path.clone(), diagnostic));
                }
            }
        }
        run
    }

    /// The snapshot as text: a line per file, then a line per definition in it.
    pub fn to_text(&self) -> String {
        self.files
            .iter()
            .flat_map(|(path, record)| {
                let definitions = record.definitions.iter().map(|(name, hash)| format!("  {hash} {name}\n"));
                std::iter::once(format!("file {} {path}\n", record.hash)).chain(definitions)
            })
            .collect()
    }

    /// Reads a snapshot written by `to_text`. Returns None if it's damaged.
    pub fn from_text(text: &str) -> Option<Snapshot> {
        let mut snapshot = Snapshot::default();
        let mut current: Option<&mut FileRecord> = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("file ") {
                let (hash, path) = rest.split_once(' ')?;
                let record = FileRecord { hash: Hash::from_hex(hash)?, definitions: BTreeMap::new() };
                current = Some(snapshot.files.entry(path.to_owned()).or_insert(record));
            } else {
                let (hash, name) = line.strip_prefix("  ")?.split_once(' ')?;
                current.as_mut()?.definitions.insert(name.to_owned(), Hash::from_hex(hash)?);
            }
        }
        Some(snapshot)
    }
}
