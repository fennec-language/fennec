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
    let paths = fen_files(&examples_dir()).unwrap();
    // Finding none would pass with nothing checked.
    assert!(paths.len() > 100, "only {} examples were found in {}", paths.len(), examples_dir().display());
    for path in paths {
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

/// A program of about `lines` lines, made of the examples over and over.
fn big_program(lines: usize) -> std::io::Result<String> {
    let sources =
        fen_files(&examples_dir())?.iter().map(fs::read_to_string).collect::<Result<Vec<_>, _>>()?;
    let mut program = String::new();
    while program.lines().count() < lines {
        for source in &sources {
            program.push_str(source);
            program.push('\n');
        }
    }
    Ok(program)
}

/// The quickest of three formattings of `program`, which a busy machine slows least.
fn quickest(program: &str) -> Duration {
    (0..3)
        .map(|_| {
            let started = Instant::now();
            assert!(fennec_fmt::format(program).is_ok());
            started.elapsed()
        })
        .min()
        .unwrap_or_default()
}

/// Formatting takes time in proportion to the program: four times the lines takes about four
/// times as long, where work that grows with the square would take sixteen. A busy machine slows
/// both sizes alike, so their ratio holds where a limit in seconds wouldn't. It reads a clock, so
/// it's no required check: the nightly Proofs run runs it, in release.
#[test]
#[ignore = "timing: the nightly Proofs run runs it"]
fn formats_a_big_program_in_time_in_proportion() {
    let (small, big) = (big_program(10_000).unwrap(), big_program(40_000).unwrap());
    assert!(fennec_fmt::format(&big).unwrap().lines().count() >= 40_000);
    let (small_took, big_took) = (quickest(&small), quickest(&big));
    let lines = |program: &str| f64::from(u32::try_from(program.lines().count()).unwrap_or(u32::MAX));
    let times = big_took.as_secs_f64() / small_took.as_secs_f64().max(1e-6);
    let grown = lines(&big) / lines(&small);
    assert!(
        times < grown * 2.0,
        "{} lines took {big_took:?} and {} took {small_took:?}: {times:.1} times as long for {grown:.1} times the lines",
        lines(&big),
        lines(&small)
    );
}
