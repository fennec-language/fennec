//! Every Fennec file in the repo formats without changing its syntax tree, formatting twice
//! gives the same text, and the examples are formatted already.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn fen_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Formats `source`, checking the tree is unchanged and a second pass changes nothing.
/// Returns None if the source doesn't parse.
fn round_trip(source: &str, name: &str) -> Option<String> {
    let before = fennec_syntax::parse(source).ok()?;
    let formatted = fennec_fmt::format(source);
    let Ok(formatted) = formatted else {
        assert_eq!(formatted, Ok(String::new()), "{name} didn't format");
        return None;
    };
    let after = fennec_syntax::parse(&formatted).map(|after| fennec_fmt::without_spans(&after));
    assert_eq!(Ok(fennec_fmt::without_spans(&before)), after, "{name}: the tree changed");
    assert_eq!(fennec_fmt::format(&formatted), Ok(formatted.clone()), "{name}: formatting twice changed it");
    Some(formatted)
}

#[test]
fn examples_are_formatted() {
    for path in fen_files(&examples_dir()).unwrap() {
        let source = fs::read_to_string(&path).unwrap();
        let name = path.display().to_string();
        let formatted = round_trip(&source, &name);
        assert!(formatted.is_some(), "{name} doesn't parse");
        let formatted = formatted.unwrap();
        assert_eq!(source, formatted, "{name} isn't formatted; run `fennec fmt examples`");
    }
}

#[test]
fn error_examples_keep_their_meaning() {
    let mut formatted = 0;
    for path in fen_files(&examples_dir().join("errors")).unwrap() {
        let source = fs::read_to_string(&path).unwrap();
        let name = path.display().to_string();
        if let Some(output) = round_trip(&source, &name) {
            assert_eq!(source, output, "{name} isn't formatted; run `fennec fmt examples`");
            formatted += 1;
        } else {
            assert!(
                matches!(fennec_fmt::format(&source), Err(fennec_fmt::Error::Syntax(_))),
                "{name} doesn't parse, so formatting it should say so"
            );
        }
    }
    assert!(formatted > 30, "only {formatted} error examples parsed");
}

#[test]
fn the_prelude_keeps_its_meaning() {
    for source in fennec_syntax::prelude() {
        let formatted = round_trip(source, "the prelude").unwrap();
        assert_eq!(
            formatted, source,
            "the prelude isn't formatted; run `fennec fmt crates/syntax/src/stdlib`"
        );
    }
}

/// A program of about 40,000 lines, made of the examples over and over.
fn big_program() -> std::io::Result<String> {
    let sources =
        fen_files(&examples_dir())?.iter().map(fs::read_to_string).collect::<Result<Vec<_>, _>>()?;
    let mut program = String::new();
    while program.lines().count() < 40_000 {
        for source in &sources {
            program.push_str(source);
            program.push('\n');
        }
    }
    Ok(program)
}

#[test]
fn formats_a_big_program_quickly() {
    let program = big_program().unwrap();
    let started = Instant::now();
    let formatted = fennec_fmt::format(&program).unwrap();
    let took = started.elapsed();
    // Release builds take a small fraction of this; debug builds and busy machines are slower.
    let limit = if cfg!(debug_assertions) { Duration::from_secs(10) } else { Duration::from_secs(1) };
    assert!(took < limit, "formatting {} lines took {took:?}", program.lines().count());
    assert!(formatted.lines().count() >= 40_000);
}
