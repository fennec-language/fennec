//! What changed between two runs, file by file and definition by definition.

use crate::snapshot::{FileRecord, Snapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Added,
    Removed,
    Changed,
    /// The file's bytes changed but none of its definitions did: only its formatting,
    /// comments or order changed, so nothing needs building again.
    Reworded,
}

/// One change: a whole file, or one definition in it when `definition` names one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: Kind,
    pub file: String,
    pub definition: Option<String>,
}

/// Every change from `old` to `new`, sorted by file, then by definition.
pub fn changes(old: &Snapshot, new: &Snapshot) -> Vec<Change> {
    let mut found = Vec::new();
    for (file, record) in &new.files {
        match old.files.get(file) {
            None => found.push(Change { kind: Kind::Added, file: file.clone(), definition: None }),
            Some(before) if before.hash != record.hash => found.extend(within(file, before, record)),
            Some(_) => {}
        }
    }
    for file in old.files.keys().filter(|file| !new.files.contains_key(*file)) {
        found.push(Change { kind: Kind::Removed, file: file.clone(), definition: None });
    }
    found.sort_by(|a, b| (&a.file, &a.definition).cmp(&(&b.file, &b.definition)));
    found
}

/// The changed definitions of a file whose bytes changed.
fn within(file: &str, before: &FileRecord, after: &FileRecord) -> Vec<Change> {
    let change = |kind, name: &String| Change { kind, file: file.to_owned(), definition: Some(name.clone()) };
    let mut found: Vec<Change> = after
        .definitions
        .iter()
        .filter_map(|(name, hash)| match before.definitions.get(name) {
            None => Some(change(Kind::Added, name)),
            Some(old) if old != hash => Some(change(Kind::Changed, name)),
            Some(_) => None,
        })
        .collect();
    let removed = before.definitions.keys().filter(|name| !after.definitions.contains_key(*name));
    found.extend(removed.map(|name| change(Kind::Removed, name)));
    if found.is_empty() {
        found.push(Change { kind: Kind::Reworded, file: file.to_owned(), definition: None });
    }
    found
}
