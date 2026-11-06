//! What `fennec fix --mut-to-ref` writes, and that it means the same as what it read.

use std::path::{Path, PathBuf};

use fennec_syntax::{mut_to_ref, parse};

/// The source in each of the spans a file's result leaves for a person to read.
fn left_in(source: &str) -> Vec<&str> {
    let left = mut_to_ref(source).map(|result| result.left).unwrap_or_default();
    left.iter().filter_map(|span| source.get(span.start as usize..span.end as usize)).collect()
}

fn renamed(source: &str) -> String {
    mut_to_ref(source).map_or_else(|err| err.message, |result| result.text)
}

#[test]
fn each_place_but_a_local_becomes_ref() {
    for (before, after) in [
        ("for mut unit in units:\n    pass\n", "for ref unit in units:\n    pass\n"),
        ("for mut a, b in pairs:\n    pass\n", "for ref a, b in pairs:\n    pass\n"),
        ("def f(mut self):\n    pass\n", "def f(ref self):\n    pass\n"),
        ("def f(a: int, mut b: int):\n    pass\n", "def f(a: int, ref b: int):\n    pass\n"),
        (
            "def f(\n    mut a: int,\n    mut b: int,\n):\n    pass\n",
            "def f(\n    ref a: int,\n    ref b: int,\n):\n    pass\n",
        ),
        ("f(mut a, b, mut c)\n", "f(ref a, b, ref c)\n"),
        ("f(\n    mut a,\n)\n", "f(\n    ref a,\n)\n"),
        ("with a as mut b:\n    pass\n", "with a as ref b:\n    pass\n"),
        ("with a, b as mut c, mut d:\n    pass\n", "with a, b as ref c, ref d:\n    pass\n"),
    ] {
        assert_eq!(renamed(before), after);
    }
}

#[test]
fn a_local_stays_mut() {
    for source in [
        "mut count = 0\n",
        "mut count: int = 0\n",
        "def f():\n    mut count = 0\n",
        "if a: mut count = 0\n",
        "mut target = units[0]\n",
    ] {
        assert_eq!(renamed(source), source);
    }
}

#[test]
fn only_code_changes() {
    for source in [
        "name = \"for mut unit in (mut self), mut a\"\n",
        "# for mut unit in units\nx = 1\n",
        "## Takes (mut self).\ndef f():\n    pass\n",
    ] {
        assert_eq!(renamed(source), source);
    }
}

#[test]
fn what_runs_in_a_doc_comment_changes_and_other_fences_dont() {
    let source = "## ```\n## mut total = 0\n## for mut item in items:\n##     grow(mut total)\n## ```\n##\n## ```text\n## for mut item\n## ```\ndef f():\n    pass\n";
    let expected = "## ```\n## mut total = 0\n## for ref item in items:\n##     grow(ref total)\n## ```\n##\n## ```text\n## for mut item\n## ```\ndef f():\n    pass\n";
    assert_eq!(renamed(source), expected);
    assert_eq!(left_in(source), ["## for mut item"]);
}

#[test]
fn it_points_at_what_a_person_has_to_read() {
    let source = "# a `mut` parameter\nx = f\"{g(mut y)}\"\nname = \"mut\"\n# nothing here, like mutable\n";
    assert_eq!(left_in(source), ["# a `mut` parameter", "f\"{g(mut y)}\""]);
}

#[test]
fn what_cant_be_read_is_an_error() {
    assert!(mut_to_ref("x = \"never closed\n").is_err());
}

/// Every Fennec file under `dir`, leaving out build output and hidden folders.
fn fennec_files(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                fennec_files(&path, found)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "fen") {
            found.push(path);
        }
    }
    Ok(())
}

/// On every Fennec file in the repository, without changing any: the tree parsed from the new text
/// is the old one's, spans and all, and running it again changes nothing. The repository has been
/// renamed, so a file it would change still says `mut` where `ref` goes.
#[test]
fn every_file_in_the_repository_means_the_same_afterwards() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    fennec_files(&root, &mut files).unwrap();
    assert!(files.len() > 100, "found only {} files", files.len());
    let mut changed = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        let Ok(result) = mut_to_ref(&source) else { continue };
        if result.text != source {
            changed.push(file.display().to_string());
        }
        assert_eq!(result.text.len(), source.len(), "{}", file.display());
        assert_eq!(
            format!("{:?}", parse(&result.text)),
            format!("{:?}", parse(&source)),
            "{}",
            file.display()
        );
        assert_eq!(renamed(&result.text), result.text, "{}", file.display());
    }
    assert!(
        changed.is_empty(),
        "these still say `mut` where `ref` goes; run `cargo run -p fennec -- fix --mut-to-ref .`:\n{}",
        changed.join("\n")
    );
}
