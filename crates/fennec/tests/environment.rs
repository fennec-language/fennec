//! `docs/environment.md` lists every environment variable the code uses, and nothing else.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The repository, two folders up from this crate.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Folders that hold no code of the repository's own.
const SKIPPED: [&str; 7] = ["target", ".git", "node_modules", "__pycache__", ".claude", "dist", ".astro"];

/// The kinds of file that hold code, configuration or scripts.
const CODE: [&str; 11] = ["rs", "py", "sh", "yml", "yaml", "js", "mjs", "ts", "fen", "toml", "ps1"];

/// Every file under `folder` that holds code: by its extension, or any file of `.githooks`.
fn code_files(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIPPED.contains(&name.as_str()) {
                code_files(&path, found);
            }
        } else if path.extension().is_some_and(|ext| CODE.iter().any(|code| ext == *code))
            || folder.file_name().is_some_and(|parent| parent == ".githooks")
        {
            found.push(path);
        }
    }
}

/// Each name in `text` that starts with the prefix, as far as its capitals, digits and
/// underscores go. A prefix with nothing after it, as prose writes it, isn't a name.
fn names_in(text: &str) -> BTreeSet<String> {
    let prefix = ["FENNEC", "_"].concat();
    let mut names = BTreeSet::new();
    for (at, _) in text.match_indices(&prefix) {
        let before = text.get(..at).and_then(|head| head.chars().next_back());
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let rest = text.get(at + prefix.len()..).unwrap_or_default();
        let tail: String =
            rest.chars().take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_').collect();
        let tail = tail.trim_end_matches('_');
        if !tail.is_empty() {
            names.insert(format!("{prefix}{tail}"));
        }
    }
    names
}

#[test]
fn every_fennec_name_in_the_code_is_listed() {
    let root = repository();
    let page = std::fs::read_to_string(root.join("docs/environment.md")).expect("docs/environment.md");
    let listed = names_in(&page);
    // What's decided but not built is listed before any code uses it.
    let planned = page.split("## Decided, not built yet").nth(1).map(names_in).unwrap_or_default();
    let mut files = Vec::new();
    code_files(&root, &mut files);
    let mut used = BTreeSet::new();
    for file in &files {
        // A file that isn't text, like a compiled script, holds no code to read.
        if let Ok(text) = std::fs::read_to_string(file) {
            for name in names_in(&text) {
                used.insert(name);
            }
        }
    }
    assert!(files.len() > 100, "only {} code files were found under {}", files.len(), root.display());
    let missing: Vec<&String> = used.difference(&listed).collect();
    assert!(missing.is_empty(), "docs/environment.md doesn't list {missing:?}: give each a line there");
    let stale: Vec<&String> = listed.difference(&used).filter(|name| !planned.contains(*name)).collect();
    assert!(stale.is_empty(), "docs/environment.md lists {stale:?}, which no code uses any more");
}
