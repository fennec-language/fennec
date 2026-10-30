//! The build engine's content hashes: what changes a definition's hash and what doesn't,
//! reusing unchanged files, reporting changes, and storing snapshots.

mod cache;

use std::collections::BTreeMap;

use fennec_build::{Change, Kind, Snapshot, changes, definitions, load, save};

fn hashes(source: &str) -> Result<BTreeMap<String, String>, fennec_syntax::Diagnostic> {
    let module = fennec_syntax::parse(source)?;
    Ok(definitions(&module).into_iter().map(|(name, hash)| (name, hash.to_hex())).collect())
}

const GAME: &str = "from std.math import Vec3\nimport std.game\n\ntype Id = int\n\n\
## An enemy.\nstruct Enemy:\n    health: int\n\n    def hurt(mut self, amount: int):\n        self.health -= amount\n\n\
enum Shape:\n    Point\n\nerror Gone:\n    id: int\n\ninterface Named:\n    def name(self) -> str\n\n\
def main(stdout: Stdout):\n    stdout.print(\"hi\")\n\ntest \"it works\":\n    assert True\n";

#[test]
fn every_definition_has_a_hash() {
    let names: Vec<String> = hashes(GAME).unwrap().into_keys().collect();
    assert_eq!(
        names,
        [
            "def Enemy.hurt",
            "def main",
            "enum Shape",
            "error Gone",
            "from std.math import Vec3",
            "import std.game",
            "interface Named",
            "struct Enemy",
            "test \"it works\"",
            "type Id",
        ]
    );
}

#[test]
fn layout_comments_and_order_change_no_hash() {
    let moved = "def main(stdout: Stdout):\n    # hello\n    stdout.print( \"hi\" )\n\n\n\
test \"it works\":\n  assert True\ninterface Named:\n    def name(self) -> str\nstruct Enemy:\n    health: int\n    def hurt(mut self, amount: int):\n        self.health -= amount\n\
enum Shape:\n    Point\nerror Gone:\n    id: int\ntype Id = int\nimport std.game\nfrom std.math import Vec3\n";
    assert_eq!(hashes(GAME).unwrap(), hashes(moved).unwrap());
}

#[test]
fn changing_a_body_changes_only_its_hash() {
    let before = hashes(GAME).unwrap();
    let after = hashes(&GAME.replace("self.health -= amount", "self.health -= amount * 2")).unwrap();
    let changed: Vec<&String> = before.keys().filter(|name| before.get(*name) != after.get(*name)).collect();
    assert_eq!(changed, ["def Enemy.hurt"]);
}

#[test]
fn strings_that_look_like_positions_still_count() {
    let one = "def main(stdout: Stdout):\n    stdout.print(\"Span { start: 1, end: 2 }\")\n";
    let two = "def main(stdout: Stdout):\n    stdout.print(\"Span { start: 5, end: 9 }\")\n";
    assert_ne!(hashes(one).unwrap(), hashes(two).unwrap());
}

fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(path, text)| ((*path).to_owned(), (*text).to_owned())).collect()
}

#[test]
fn unchanged_files_arent_parsed_again() {
    let first =
        Snapshot::take(&files(&[("a.fen", GAME), ("b.fen", "def b():\n    pass\n")]), &Snapshot::default());
    assert_eq!(first.parsed, ["a.fen", "b.fen"]);
    let second =
        Snapshot::take(&files(&[("a.fen", GAME), ("b.fen", "def b():\n    return\n")]), &first.snapshot);
    assert_eq!((second.parsed, second.reused), (vec!["b.fen".to_owned()], vec!["a.fen".to_owned()]));
    assert_eq!(second.snapshot.files.get("a.fen"), first.snapshot.files.get("a.fen"));
}

#[test]
fn a_file_that_doesnt_parse_keeps_its_last_good_record() {
    let good = Snapshot::take(&files(&[("a.fen", GAME)]), &Snapshot::default()).snapshot;
    let broken = Snapshot::take(&files(&[("a.fen", "def (:\n"), ("b.fen", "def (:\n")]), &good);
    assert_eq!(broken.errors.len(), 2);
    assert_eq!(broken.snapshot, good);
    assert_eq!(changes(&good, &broken.snapshot), []);
    let fixed = Snapshot::take(&files(&[("a.fen", GAME)]), &broken.snapshot);
    assert_eq!((fixed.parsed, fixed.reused), (vec![], vec!["a.fen".to_owned()]));
}

fn change(kind: Kind, file: &str, definition: Option<&str>) -> Change {
    Change { kind, file: file.to_owned(), definition: definition.map(str::to_owned) }
}

#[test]
fn changes_are_reported_by_file_and_definition() {
    let old = Snapshot::take(
        &files(&[
            ("a.fen", GAME),
            ("gone.fen", "def gone():\n    pass\n"),
            ("same.fen", "def same():\n    pass\n"),
        ]),
        &Snapshot::default(),
    )
    .snapshot;
    let edited = GAME.replace("stdout.print(\"hi\")", "stdout.print(\"bye\")").replace("type Id = int\n", "")
        + "\ndef extra():\n    pass\n";
    let new = Snapshot::take(
        &files(&[
            ("a.fen", &edited),
            ("new.fen", "def new():\n    pass\n"),
            ("same.fen", "# reworded\ndef same():\n    pass\n"),
        ]),
        &old,
    )
    .snapshot;
    assert_eq!(
        changes(&old, &new),
        [
            change(Kind::Added, "a.fen", Some("def extra")),
            change(Kind::Changed, "a.fen", Some("def main")),
            change(Kind::Removed, "a.fen", Some("type Id")),
            change(Kind::Removed, "gone.fen", None),
            change(Kind::Added, "new.fen", None),
            change(Kind::Reworded, "same.fen", None),
        ]
    );
    assert_eq!(changes(&new, &new), []);
}

#[test]
fn snapshots_are_stored_and_read_back() {
    let home = std::env::temp_dir().join(format!("fennec-build-test-{}", std::process::id()));
    let program = home.join("game.fen");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(&program, GAME).unwrap();
    let snapshot = Snapshot::take(&files(&[("game.fen", GAME)]), &Snapshot::default()).snapshot;

    assert_eq!(load(&home, &program), Snapshot::default());
    save(&home, &program, &snapshot).unwrap();
    assert_eq!(load(&home, &program), snapshot);
    assert_eq!(Snapshot::from_text(&snapshot.to_text()), Some(snapshot.clone()));
    assert_eq!(Snapshot::from_text("file nothex game.fen\n"), None);
    assert_eq!(Snapshot::from_text("  0000 def stray\n"), None);

    let runs = home.join("cache").join(fennec_build::VERSION).join("runs");
    let stored = std::fs::read_dir(runs).unwrap().next().unwrap().unwrap().path();
    let text = std::fs::read_to_string(&stored).unwrap();
    std::fs::write(&stored, text.replacen("fennec snapshot", "fennec snapshot 9.9.9 was", 1)).unwrap();
    assert_eq!(load(&home, &program), Snapshot::default(), "another version's snapshot is ignored");
    std::fs::remove_dir_all(&home).unwrap();
}
