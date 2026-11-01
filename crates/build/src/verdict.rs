//! What an edit to a kit can have changed, without looking at the rest of the kit.
//!
//! A kit's files only reach each other through imports, so an edit that changes only the bodies
//! of functions can't break anything outside them. Each file's definitions are kept by the
//! file's text, and each kit's last passing state is the text of its files. An edit then means
//! comparing the files that changed, and nothing more.
//!
//! Anything that can reach further makes `changed_bodies` give up: a function's signature, what
//! it can fail with, a type or constant, or a definition that came or went.

use std::collections::{HashMap, HashSet};

use fennec_blake3::Hash;
use fennec_syntax::{FnDef, Item, Span};

use crate::Cache;
use crate::traced::from_hex;
use crate::tree::{item_definitions, tree_hash};

/// One definition of a file: what it's keyed by, its whole tree, and the part of it other
/// definitions can depend on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    hash: Hash,
    signature: Hash,
}

/// The part of a function that what calls it can depend on. Whether a function fails, and with
/// what, comes from its body, so a body that can fail or yield counts whole.
fn signature(def: &FnDef) -> Hash {
    let body = format!("{:?}", def.body);
    if ["Fail(", "Try {", "Yield("].iter().any(|effect| body.contains(effect)) {
        return tree_hash(def);
    }
    tree_hash(&(&def.name, &def.generics, &def.params, &def.ret))
}

/// The definitions in some items.
pub fn rows<'i>(items: impl Iterator<Item = &'i Item>) -> Vec<Row> {
    let mut found = Vec::new();
    for item in items {
        let mut keyed = Vec::new();
        item_definitions(item, &mut keyed);
        let methods: Vec<&FnDef> = match item {
            Item::Function(def) => vec![def],
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.methods.iter().collect(),
            Item::Extend(extension) => extension.methods.iter().collect(),
            _ => Vec::new(),
        };
        let signatures: Vec<Hash> = methods.into_iter().map(signature).collect();
        // A type's own entry comes first, then its methods in order, or there's only a function.
        let first_method = keyed.len() - signatures.len();
        for (at, (key, hash)) in keyed.into_iter().enumerate() {
            let own = at.checked_sub(first_method).and_then(|at| signatures.get(at)).copied();
            // Nothing depends on a test, so all of it is its body.
            let test = if let Item::Test(test) = item { Some(tree_hash(&test.name)) } else { None };
            found.push(Row { key, hash, signature: own.or(test).unwrap_or(hash) });
        }
    }
    found
}

/// Where an item is written.
pub fn item_span(item: &Item) -> Span {
    match item {
        Item::Function(def) => def.span,
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.span,
        Item::Interface(interface) => interface.span,
        Item::Extend(extension) => extension.span,
        Item::TypeAlias { span, .. } | Item::Const { span, .. } => *span,
        Item::Import(import) => import.span,
        Item::Test(test) => test.span,
        Item::Stmt(stmt) => stmt.span,
    }
}

/// The functions and tests whose bodies changed, by key, or None if an edit reaches further.
pub fn changed_bodies(old: &[Row], new: &[Row]) -> Option<Vec<String>> {
    let keys = |rows: &[Row]| rows.iter().map(|row| row.key.clone()).collect::<HashSet<_>>();
    if old.len() != new.len() || keys(old) != keys(new) {
        return None;
    }
    let before: HashMap<&str, &Row> = old.iter().map(|row| (row.key.as_str(), row)).collect();
    let mut changed = Vec::new();
    for row in new {
        let was = before.get(row.key.as_str())?;
        if was.hash == row.hash {
            continue;
        }
        let body = row.key.starts_with("def ") || row.key.starts_with("test \"");
        if !body || was.signature != row.signature {
            return None;
        }
        changed.push(row.key.clone());
    }
    Some(changed)
}

const RECORD: char = '\u{1}';
const FIELD: char = '\u{0}';

/// One of a kit's files when its tests last passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KitFile {
    pub module: String,
    pub text: Hash,
    pub size: u64,
    /// When it was last changed, in nanoseconds.
    pub changed_at: u128,
}

/// A kit when all its tests last passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KitState {
    pub files: HashMap<String, KitFile>,
    /// When this was noted, in nanoseconds.
    pub noted_at: u128,
}

/// How close to when it's noted a file's change can't be told apart from an edit just after,
/// on a filesystem with coarse times. Such a file is read again, as `git` does.
const TOO_CLOSE: u128 = 2_000_000_000;

impl KitState {
    /// A file's text, if a file of this size changed at this time can only be what was noted.
    pub fn same_text(&self, module: &str, size: u64, changed_at: u128) -> Option<Hash> {
        let file = self.files.get(module)?;
        let settled = changed_at.saturating_add(TOO_CLOSE) < self.noted_at;
        (file.size == size && file.changed_at == changed_at && settled).then_some(file.text)
    }
}

impl Cache {
    fn state_name(program: &str, build: &str) -> String {
        format!("{}.state", fennec_blake3::hash(format!("{program}{FIELD}{build}").as_bytes()).to_hex())
    }

    /// A kit when all its tests last passed, for this build.
    pub fn kit_state(&self, program: &str, build: &str) -> Option<KitState> {
        let bytes = self.read("kit-state", &Self::state_name(program, build))?;
        let text = String::from_utf8(bytes).ok()?;
        let (noted_at, records) = text.split_once(RECORD)?;
        let mut state = KitState { files: HashMap::new(), noted_at: noted_at.parse().ok()? };
        for record in records.split(RECORD).filter(|record| !record.is_empty()) {
            let mut fields = record.split(FIELD);
            let (text, size, changed_at, module) =
                (fields.next()?, fields.next()?, fields.next()?, fields.next()?);
            let file = KitFile {
                module: module.to_owned(),
                text: from_hex(text)?,
                size: size.parse().ok()?,
                changed_at: changed_at.parse().ok()?,
            };
            state.files.insert(file.module.clone(), file);
        }
        Some(state)
    }

    pub fn store_kit_state(&self, program: &str, build: &str, files: &[KitFile]) -> std::io::Result<()> {
        let noted_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let mut text = format!("{noted_at}{RECORD}");
        for file in files {
            text.push_str(&file.text.to_hex());
            text.push(FIELD);
            text.push_str(&file.size.to_string());
            text.push(FIELD);
            text.push_str(&file.changed_at.to_string());
            text.push(FIELD);
            text.push_str(&file.module);
            text.push(RECORD);
        }
        self.write("kit-state", &Self::state_name(program, build), text.as_bytes())
    }

    /// Forgets a kit's state, once a test has failed.
    pub fn clear_kit_state(&self, program: &str, build: &str) {
        let _ = std::fs::remove_file(self.path("kit-state", &Self::state_name(program, build)));
    }

    /// The definitions of a file with this text, if they've been kept.
    pub fn file_rows(&self, text: Hash) -> Option<Vec<Row>> {
        let bytes = self.read("file-rows", &format!("{}.rows", text.to_hex()))?;
        let text = String::from_utf8(bytes).ok()?;
        let mut rows = Vec::new();
        for record in text.split(RECORD).filter(|record| !record.is_empty()) {
            let mut fields = record.split(FIELD);
            let (hash, signature, key) = (fields.next()?, fields.next()?, fields.next()?);
            rows.push(Row { key: key.to_owned(), hash: from_hex(hash)?, signature: from_hex(signature)? });
        }
        Some(rows)
    }

    pub fn has_file_rows(&self, text: Hash) -> bool {
        self.path("file-rows", &format!("{}.rows", text.to_hex())).is_file()
    }

    pub fn store_file_rows(&self, text: Hash, rows: &[Row]) -> std::io::Result<()> {
        let mut written = String::new();
        for row in rows {
            written.push_str(&row.hash.to_hex());
            written.push(FIELD);
            written.push_str(&row.signature.to_hex());
            written.push(FIELD);
            written.push_str(&row.key);
            written.push(RECORD);
        }
        self.write("file-rows", &format!("{}.rows", text.to_hex()), written.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use fennec_syntax::parse;

    use super::{changed_bodies, rows};

    fn of(source: &str) -> Vec<super::Row> {
        rows(parse(source).unwrap().items.iter())
    }

    const SOURCE: &str = "struct P:\n    x: int\n\n    def get(self) -> int:\n        self.x\n\n\
                          LIMIT = 1\n\ndef f(a: int) -> int:\n    a + 1\n\ntest \"t\":\n    assert f(1) == 2\n";

    fn changed(edited: &str) -> Option<Vec<String>> {
        changed_bodies(&of(SOURCE), &of(edited))
    }

    #[test]
    fn a_changed_body_is_named() {
        assert_eq!(changed(SOURCE), Some(Vec::new()));
        assert_eq!(changed(&SOURCE.replace("a + 1", "a + 2")), Some(vec!["def f".to_owned()]));
        assert_eq!(changed(&SOURCE.replace("self.x", "self.x + 0")), Some(vec!["def P.get".to_owned()]));
        assert_eq!(changed(&SOURCE.replace("f(1) == 2", "f(2) == 3")), Some(vec!["test \"t\"".to_owned()]));
    }

    #[test]
    fn what_can_reach_further_gives_up() {
        for (from, to, what) in [
            ("a: int", "a: float", "a parameter"),
            ("-> int:\n    a", "-> float:\n    a", "a return type"),
            ("x: int", "x: float", "a field"),
            ("LIMIT = 1", "LIMIT = 2", "a constant"),
            ("def f(", "def renamed(", "a name"),
            ("a + 1", "a + other()?", "a body that can now fail"),
            ("a + 1", "fail Oops()", "a body that can now fail"),
            ("test \"t\"", "test \"t2\"", "a test's name"),
        ] {
            assert_eq!(changed(&SOURCE.replace(from, to)), None, "{what}");
        }
        let added = format!("{SOURCE}\ndef g() -> int:\n    1\n");
        assert_eq!(changed(&added), None, "a new function");
    }

    #[test]
    fn a_files_text_is_only_trusted_when_its_time_can_be_told_apart() {
        let text = fennec_blake3::hash(b"text");
        let file = super::KitFile { module: "m".to_owned(), text, size: 4, changed_at: 1_000 };
        let noted_at = 1_000 + super::TOO_CLOSE + 1;
        let state = super::KitState { files: [("m".to_owned(), file)].into(), noted_at };
        assert_eq!(state.same_text("m", 4, 1_000), Some(text));
        assert_eq!(state.same_text("m", 5, 1_000), None, "its size changed");
        assert_eq!(state.same_text("m", 4, 1_001), None, "its time changed");
        assert_eq!(state.same_text("other", 4, 1_000), None, "it's new");
        let close = super::KitState { noted_at: noted_at - 1, ..state };
        assert_eq!(close.same_text("m", 4, 1_000), None, "an edit could hide in that time");
    }

    #[test]
    fn a_body_that_could_already_fail_counts_whole() {
        let source = "def f(a: int) -> int:\n    if a > 1: fail Oops()\n    a\n";
        let edited = source.replace("a > 1", "a > 2");
        assert_eq!(changed_bodies(&of(source), &of(&edited)), None);
    }
}
