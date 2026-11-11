//! The linter's fixes never change what a program does: every program in `fixtures/unlinted`, with
//! every fix made, still checks and prints exactly its `.out` file. They're examples as they were
//! before they were lint-clean, kept so the fixes have something to fix.

use std::path::Path;

use fennec_syntax::{Sources, apply_fixes, parse_at};

/// How many rounds of fixes to make, since one fix can show another.
const ROUNDS: usize = 8;

/// A source with the linter's fixes made until it has none left.
fn fixed(source: &str) -> String {
    let mut text = source.to_owned();
    for _ in 0..ROUNDS {
        let mut sources = Sources::new();
        let base = sources.add("example.fen", text.as_str());
        let Ok(module) = parse_at(&text, base) else { return text };
        let diagnostics: Vec<_> =
            fennec_lint::lint(&module, &sources).into_iter().map(|lint| lint.diagnostic).collect();
        match apply_fixes(&text, base, &diagnostics) {
            Some(next) if next != text => text = next,
            _ => return text,
        }
    }
    text
}

/// What a program prints when it's run, or why it couldn't be.
fn output_of(source: &str, dir: &Path) -> Result<String, String> {
    let program = dir.join("program.fen");
    std::fs::write(&program, source).map_err(|err| err.to_string())?;
    let ran = crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .arg(&program)
        .env("FENNEC_HOME", dir.join("home"))
        .output()
        .map_err(|err| err.to_string())?;
    if !ran.status.success() {
        return Err(String::from_utf8_lossy(&ran.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
}

#[test]
fn every_example_with_every_fix_does_the_same() {
    if !crate::llvm::ready() {
        return;
    }
    let compared = every_example().unwrap();
    // Fixtures whose only fixes were `positional-arguments` have none left: it's a compile error now,
    // so they're written with names.
    assert!(compared >= 6, "only {compared} programs had fixes to try");
}

/// How many programs had fixes to try, each of which still did the same, or the first that didn't.
fn every_example() -> Result<usize, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unlinted");
    let work =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-lint-fixes-{}", std::process::id())));
    std::fs::create_dir_all(&work).map_err(|err| err.to_string())?;
    let mut compared = 0;
    for entry in std::fs::read_dir(&dir).map_err(|err| err.to_string())? {
        let path = entry.map_err(|err| err.to_string())?.path();
        let Ok(expected) = std::fs::read_to_string(path.with_extension("out")) else { continue };
        if path.extension().is_none_or(|extension| extension != "fen") {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
        let changed = fixed(&source);
        let before = output_of(&source, &work);
        if changed == source || before.as_ref().ok() != Some(&expected) {
            continue;
        }
        if output_of(&changed, &work) != before {
            return Err(format!("{} does something else once fixed:\n{changed}", path.display()));
        }
        compared += 1;
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(compared)
}
