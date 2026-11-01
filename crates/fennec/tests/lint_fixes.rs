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

fn output_of(source: &str) -> Result<String, String> {
    let mut module = fennec_syntax::parse(source).map_err(|err| format!("doesn't parse: {err:?}"))?;
    fennec_check::check_and_resolve(&mut module).map_err(|err| format!("doesn't check: {err:?}"))?;
    let mut out = Vec::new();
    fennec_interp::run(&module, &mut out).map_err(|err| format!("fails: {err:?}"))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The interpreter recurses for each call, so it runs on a big stack, as `fennec` gives it.
fn on_big_stack(test: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let worker = std::thread::Builder::new().stack_size(1 << 30).spawn(test)?;
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
    Ok(())
}

#[test]
fn every_example_with_every_fix_does_the_same() {
    let compared = std::sync::Arc::new(std::sync::Mutex::new(Err(String::new())));
    let result = std::sync::Arc::clone(&compared);
    on_big_stack(move || *result.lock().unwrap() = every_example()).unwrap();
    let compared = compared.lock().unwrap().clone().unwrap();
    assert!(compared >= 14, "only {compared} programs had fixes to try");
}

/// How many programs had fixes to try, each of which still did the same, or the first that didn't.
fn every_example() -> Result<usize, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unlinted");
    let mut compared = 0;
    for entry in std::fs::read_dir(&dir).map_err(|err| err.to_string())? {
        let path = entry.map_err(|err| err.to_string())?.path();
        let Ok(expected) = std::fs::read_to_string(path.with_extension("out")) else { continue };
        if path.extension().is_none_or(|extension| extension != "fen") {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
        let changed = fixed(&source);
        let before = output_of(&source);
        if changed == source || before.as_ref().ok() != Some(&expected) {
            continue;
        }
        if output_of(&changed) != before {
            return Err(format!("{} does something else once fixed:\n{changed}", path.display()));
        }
        compared += 1;
    }
    Ok(compared)
}
